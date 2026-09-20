use crate::config::RetryConfig;
use crate::error::{ManagerError, Result};
use crate::file_ops::atomic_rename_or_copy;
use crate::metrics::report_event;
use crate::model::VersionInfo;
use crate::net::{
    JsonRequestError, build_agent, check_response_status, get_json_with_retry_stopping_on_status,
    get_response_with_retry, with_retry,
};
use crate::ui::Ui;

use percent_encoding::{NON_ALPHANUMERIC, percent_encode};
use std::{
    cmp,
    collections::HashMap,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
    thread::sleep,
    time::{Duration, Instant},
};
use ureq::ResponseExt;

const FILE_API: &str = "https://file.izakaya.cc/api/public/dl";
const REDIRECT_URL: &str = "https://url.izakaya.cc/getMetaMystia";
const VERSION_API: &str = "https://api.izakaya.cc/version/meta-mystia";

const BEPINEX_PRIMARY: &str = "https://builds.bepinex.dev/projects/bepinex_be";
const GITHUB_RELEASE_API_BASE: &str = "https://api.github.com/repos/MetaMikuAI/MetaMystia/releases";

const RATE_LIMIT: usize = 128 * 1024; // 128KB/s
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5); // 连接超时

const EXTERNAL_SOURCE_MIN_SPEED_BPS: usize = 128 * 1024; // 128KB/s，外部源最低速度阈值
const SPEED_CHECK_INTERVAL: Duration = Duration::from_secs(10); // 滑动窗口长度
const OVERALL_CHECK_INTERVAL: Duration = Duration::from_secs(5); // 整体均速采样间隔
const WARMUP_DURATION: Duration = Duration::from_secs(5); // 启动期豁免
const MAX_CONSECUTIVE_SLOW_WINDOWS: u32 = 2; // 滑动窗口连续低速换源阈值
const MAX_CONSECUTIVE_SLOW_OVERALL: u32 = 2; // 整体均速连续低速换源阈值
const TAIL_SKIP_RATIO: f64 = 0.90; // 已下载比例豁免阈值
const TAIL_SKIP_MIN_REMAINING_CAP: u64 = 384 * 1024; // 剩余字节豁免阈值上限

/// 平均速度低于阈值时返回 `(平均速度 KB/s, 阈值 KB/s)`
#[allow(
    clippy::cast_precision_loss,
    reason = "字节数与秒数都远小于 f64 的 2^53 精度上限"
)]
fn slow_speed(min_speed_bps: usize, bytes: u64, elapsed: Duration) -> Option<(f64, usize)> {
    let avg_speed = bytes as f64 / elapsed.as_secs_f64();

    if avg_speed < min_speed_bps as f64 {
        Some((avg_speed / 1024.0, min_speed_bps / 1024))
    } else {
        None
    }
}

/// 是否处于收尾豁免区间（剩余字节太少，不再判定低速）
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "仅按比例估算剩余字节，量级远小于 2^53"
)]
fn in_tail_skip(total_size: Option<u64>, downloaded: u64) -> bool {
    let Some(total) = total_size.filter(|total| *total > 0) else {
        return false;
    };

    let ratio_skip = (downloaded as f64 / total as f64) >= TAIL_SKIP_RATIO;
    let by_ratio_remaining = (total as f64 * (1.0 - TAIL_SKIP_RATIO)) as u64;
    let eff_tail_remaining = cmp::min(TAIL_SKIP_MIN_REMAINING_CAP, by_ratio_remaining);

    ratio_skip || total.saturating_sub(downloaded) <= eff_tail_remaining
}

/// 限速：按已下载字节数补齐应有的耗时
#[allow(
    clippy::cast_precision_loss,
    reason = "限速时长以 f64 近似表示，量级远小于 2^53"
)]
fn sleep_for_rate_limit(downloaded: u64, elapsed: Duration) {
    let expected_secs = downloaded as f64 / RATE_LIMIT as f64;
    let elapsed_secs = elapsed.as_secs_f64();

    if expected_secs <= elapsed_secs {
        return;
    }

    let sleep_dur = if cfg!(test) {
        Duration::from_millis(1)
    } else {
        Duration::from_secs_f64((expected_secs - elapsed_secs).max(0.001))
    };

    sleep(sleep_dur);
}

/// 创建下载用的临时文件，返回临时路径与文件句柄
fn create_download_temp_file(dest: &Path) -> Result<(PathBuf, File)> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            ManagerError::from(io::Error::new(
                e.kind(),
                format!("创建目录 {} 失败：{}", parent.display(), e),
            ))
        })?;
    }

    let mut tmp_path = dest.with_extension("dl.tmp");
    let mut tmp_idx = 0;
    while tmp_path.exists() {
        tmp_idx += 1;
        tmp_path = dest.with_extension(format!("dl.tmp{tmp_idx}"));
    }

    let tmp_file = fs::File::create(&tmp_path).map_err(|e| {
        ManagerError::from(io::Error::new(
            e.kind(),
            format!("创建临时文件 {} 失败：{}", tmp_path.display(), e),
        ))
    })?;

    Ok((tmp_path, tmp_file))
}

/// 下载器
pub struct Downloader<'a> {
    agent: ureq::Agent,
    ui: &'a dyn Ui,
    cached_github_releases: Mutex<HashMap<String, serde_json::Value>>,
    cached_version: Mutex<Option<VersionInfo>>,
}

impl<'a> Downloader<'a> {
    pub fn new(ui: &'a dyn Ui) -> Self {
        let agent = build_agent(Some(CONNECT_TIMEOUT), None);
        Self {
            agent,
            ui,
            cached_github_releases: Mutex::new(HashMap::new()),
            cached_version: Mutex::new(None),
        }
    }

    fn retry<F, T>(&self, op_desc: &str, f: F) -> Result<T>
    where
        F: FnMut() -> Result<T>,
    {
        with_retry(self.ui, op_desc, None, f)
    }

    fn convert_ureq_error(e: &ureq::Error) -> String {
        match e {
            ureq::Error::Timeout(_) => "请求超时".to_string(),
            ureq::Error::Io(err) => match err.kind() {
                io::ErrorKind::TimedOut => "请求超时".to_string(),
                io::ErrorKind::ConnectionRefused
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::NotConnected
                | io::ErrorKind::AddrNotAvailable
                | io::ErrorKind::AddrInUse => "连接失败".to_string(),
                _ => format!("请求失败：{e}"),
            },
            ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => "连接失败".to_string(),
            _ => format!("请求失败：{e}"),
        }
    }

    /// 分享目录 base；`category` 为 `None` 表示扁平路径
    fn file_api_base(share_code: &str, category: Option<&str>) -> String {
        category.map_or_else(
            || format!("{FILE_API}/{share_code}"),
            |category| format!("{FILE_API}/{share_code}/{category}"),
        )
    }

    fn parse_share_code_from_url(url: &str) -> Option<String> {
        url.trim_end_matches('/')
            .split('/')
            .next_back()
            .and_then(|s| s.split(&['?', '#'][..]).next())
            .map(ToString::to_string)
    }

    /// 获取版本信息
    pub fn get_version_info(&self) -> Result<VersionInfo> {
        if let Ok(guard) = self.cached_version.lock()
            && let Some(cached) = guard.clone()
        {
            return Ok(cached);
        }

        let vi = self.retry("获取版本信息", || self.try_get_version_info())?;
        *self
            .cached_version
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(vi.clone());

        Ok(vi)
    }

    fn try_get_version_info(&self) -> Result<VersionInfo> {
        self.ui.download_version_info_start()?;

        let response = self.agent.get(VERSION_API).call().map_err(|e| {
            let msg = Self::convert_ureq_error(&e);
            let _ = self.ui.download_version_info_failed(&msg);
            ManagerError::NetworkError(msg)
        })?;

        if let Some(err) = check_response_status(&response, self.ui, "获取版本信息") {
            let _ = self.ui.download_version_info_failed(&err.to_string());
            return Err(err);
        }

        let text = response
            .into_body()
            .read_to_string()
            .map_err(|e| ManagerError::NetworkError(format!("读取响应失败：{e}")))?;

        let mut vi: VersionInfo = serde_json::from_str(&text).map_err(|e| {
            let snippet: String = text.chars().take(200).collect();

            let _ = self
                .ui
                .download_version_info_parse_failed(&format!("{e}"), &snippet);
            report_event(
                "Download.VersionInfo.ParseFailed",
                Some(&format!("err={e};snippet={snippet}")),
            );

            ManagerError::Other(format!("解析版本信息失败：{e}"))
        })?;

        vi.normalize_versions();
        vi.validate()?;

        self.ui.download_version_info_success()?;
        report_event("Download.VersionInfo.Success", Some(&vi.to_string()));

        Ok(vi)
    }

    /// 获取分享码
    pub fn get_share_code(&self) -> Result<String> {
        self.retry("获取下载链接", || self.try_get_share_code())
    }

    fn try_get_share_code(&self) -> Result<String> {
        self.ui.download_share_code_start()?;

        let response = self.agent.get(REDIRECT_URL).call().map_err(|e| {
            let msg = Self::convert_ureq_error(&e);
            let _ = self.ui.download_share_code_failed(&msg);
            ManagerError::NetworkError(msg)
        })?;

        if let Some(err) = check_response_status(&response, self.ui, "获取下载链接") {
            let _ = self.ui.download_share_code_failed(&err.to_string());
            return Err(err);
        }

        let final_uri = response.get_uri().to_string();
        if let Some(code) = Self::parse_share_code_from_url(&final_uri) {
            self.ui.download_share_code_success()?;
            report_event("Download.ShareCode.Success", Some(&code));
            Ok(code)
        } else {
            report_event(
                "Download.ShareCode.ParseFailed",
                Some(&format!("final_uri={final_uri}")),
            );
            Err(ManagerError::NetworkError(
                "无法从下载链接中解析分享码".to_string(),
            ))
        }
    }

    fn download_file_with_progress(
        &self,
        url: &str,
        dest: &Path,
        file_size: Option<u64>,
        rate_limit: bool,
    ) -> Result<()> {
        self.download_file_with_progress_and_speed_check(url, dest, file_size, rate_limit, None)
    }

    fn download_file_with_progress_and_speed_check(
        &self,
        url: &str,
        dest: &Path,
        file_size: Option<u64>,
        rate_limit: bool,
        min_speed_bps: Option<usize>,
    ) -> Result<()> {
        self.retry("下载文件", || {
            self.try_download(url, dest, file_size, rate_limit, min_speed_bps)
        })
    }

    fn try_download(
        &self,
        url: &str,
        dest: &Path,
        file_size: Option<u64>,
        rate_limit: bool,
        min_speed_bps: Option<usize>,
    ) -> Result<()> {
        let response = self
            .agent
            .get(url)
            .call()
            .map_err(|e| ManagerError::NetworkError(Self::convert_ureq_error(&e)))?;

        if let Some(err) = check_response_status(&response, self.ui, "下载文件") {
            return Err(err);
        }

        let total_size = file_size.or_else(|| {
            response
                .headers()
                .get("Content-Length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
        });
        let filename = dest.file_name().map_or_else(
            || dest.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );

        let id = self.ui.download_start(&filename, total_size)?;

        let mut reader = response.into_body().into_reader();
        self.write_response_to_file(&mut reader, dest, id, total_size, rate_limit, min_speed_bps)
    }

    fn write_response_to_file<R: Read>(
        &self,
        resp: &mut R,
        dest: &Path,
        id: usize,
        total_size: Option<u64>,
        rate_limit: bool,
        min_speed_bps: Option<usize>,
    ) -> Result<()> {
        let (tmp_path, mut tmp_file) = create_download_temp_file(dest)?;

        let buf_len = cmp::min(RATE_LIMIT, 8192) as usize;
        let mut buffer = vec![0; buf_len];

        let mut downloaded = 0u64;
        let start = Instant::now();

        let mut window_start = Instant::now();
        let mut window_bytes = 0u64;
        let mut slow_window_count: u32 = 0;
        let mut last_overall_check = Instant::now();
        let mut slow_overall_count: u32 = 0;

        loop {
            let to_read = buffer.len();

            let n = resp
                .read(&mut buffer[..to_read])
                .map_err(|e| ManagerError::NetworkError(e.to_string()))?;
            if n == 0 {
                break;
            }

            tmp_file.write_all(&buffer[..n]).map_err(|e| {
                ManagerError::from(io::Error::new(
                    e.kind(),
                    format!("写入临时文件 {} 失败：{}", tmp_path.display(), e),
                ))
            })?;
            downloaded += n as u64;
            window_bytes += n as u64;

            self.ui.download_update(id, downloaded)?;

            if let Some(min_speed) = min_speed_bps {
                let elapsed = start.elapsed();

                // 启动期豁免
                if elapsed >= WARMUP_DURATION {
                    // 收尾豁免（同时作用于两条检测路径）
                    if !in_tail_skip(total_size, downloaded) {
                        // 路径 A：滑动窗口
                        let window_elapsed = window_start.elapsed();
                        if window_elapsed >= SPEED_CHECK_INTERVAL {
                            if let Some((speed_kbs, threshold_kbs)) =
                                slow_speed(min_speed, window_bytes, window_elapsed)
                            {
                                slow_window_count += 1;
                                if slow_window_count >= MAX_CONSECUTIVE_SLOW_WINDOWS {
                                    let _ = fs::remove_file(&tmp_path);
                                    report_event(
                                        "Download.SlowSpeed.Triggered.Window",
                                        Some(&format!("{speed_kbs:.1}KB/s<{threshold_kbs}KB/s")),
                                    );
                                    return Err(ManagerError::SlowDownload(format!(
                                        "{speed_kbs:.1} KB/s < {threshold_kbs} KB/s"
                                    )));
                                }
                            } else {
                                slow_window_count = 0;
                            }
                            window_start = Instant::now();
                            window_bytes = 0;
                        }

                        // 路径 B：整体均速
                        if last_overall_check.elapsed() >= OVERALL_CHECK_INTERVAL {
                            if let Some((speed_kbs, threshold_kbs)) =
                                slow_speed(min_speed, downloaded, elapsed)
                            {
                                slow_overall_count += 1;
                                if slow_overall_count >= MAX_CONSECUTIVE_SLOW_OVERALL {
                                    let _ = fs::remove_file(&tmp_path);
                                    report_event(
                                        "Download.SlowSpeed.Triggered.Overall",
                                        Some(&format!("{speed_kbs:.1}KB/s<{threshold_kbs}KB/s")),
                                    );
                                    return Err(ManagerError::SlowDownload(format!(
                                        "整体均速 {speed_kbs:.1} KB/s < {threshold_kbs} KB/s"
                                    )));
                                }
                            } else {
                                slow_overall_count = 0;
                            }
                            last_overall_check = Instant::now();
                        }
                    }
                }
            }

            if rate_limit {
                sleep_for_rate_limit(downloaded, start.elapsed());
            }
        }

        tmp_file.flush().map_err(|e| {
            ManagerError::from(io::Error::new(
                e.kind(),
                format!("同步临时文件 {} 失败：{}", tmp_path.display(), e),
            ))
        })?;

        self.finish_download(&tmp_path, dest, id)
    }

    /// 将临时文件落到目标路径，并报告下载完成
    fn finish_download(&self, tmp_path: &Path, dest: &Path, id: usize) -> Result<()> {
        if let Err(e) = atomic_rename_or_copy(tmp_path, dest) {
            let _ = fs::remove_file(tmp_path);
            return Err(ManagerError::from(io::Error::other(format!(
                "重命名或复制临时文件 {} 失败：{}",
                tmp_path.display(),
                e
            ))));
        }

        let _ = fs::remove_file(tmp_path);
        let filename = dest.file_name().map_or_else(
            || dest.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );

        self.ui
            .download_finish(id, &format!("下载完成：{filename}"))
    }

    fn download_share_code_candidates(
        &self,
        base_url: &str,
        filenames: &[String],
        dest: &Path,
    ) -> Result<String> {
        let mut last_err = None;

        for filename in filenames {
            let url = format!("{base_url}/{filename}");

            match self.download_file_with_progress(&url, dest, None, true) {
                Ok(()) => return Ok(filename.clone()),
                Err(e) => last_err = Some(e),
            }
        }

        Err(last_err
            .unwrap_or_else(|| ManagerError::NetworkError("没有可用的下载文件名候选".to_string())))
    }

    fn download_share_code_asset_with_events(
        &self,
        base_url: &str,
        filenames: &[String],
        dest: &Path,
        version: &str,
        success_event: &str,
        failed_event: &str,
    ) -> Result<String> {
        match self.download_share_code_candidates(base_url, filenames, dest) {
            Ok(filename) => {
                report_event(
                    success_event,
                    Some(&format!("version={version};file={filename}")),
                );
                Ok(filename)
            }
            Err(e) => {
                report_event(failed_event, Some(&format!("{e}")));
                Err(e)
            }
        }
    }

    fn github_release_fallback_tag(version: &str) -> Option<String> {
        let normalized = VersionInfo::normalize_version(version);
        let parts: Vec<_> = normalized.split('.').collect();

        match parts.as_slice() {
            [major, minor, patch]
                if major.parse::<u64>().is_ok()
                    && minor.parse::<u64>().is_ok()
                    && patch.parse::<u64>().ok() == Some(0) =>
            {
                Some(format!("{major}.{minor}"))
            }
            _ => None,
        }
    }

    fn github_release_fallback_tag_for_status(version: &str, status_code: u16) -> Option<String> {
        if status_code == 404 {
            Self::github_release_fallback_tag(version)
        } else {
            None
        }
    }

    fn github_release_tag_matches_requested_version(requested: &str, tag: &str) -> bool {
        let requested = VersionInfo::normalize_version(requested);
        let tag = VersionInfo::normalize_version(tag);

        !tag.is_empty()
            && (requested == tag
                || Self::github_release_fallback_tag(&requested)
                    .as_deref()
                    .is_some_and(|fallback| fallback == tag))
    }

    fn github_release_api_url(version: Option<&str>) -> String {
        version.map_or_else(
            || format!("{GITHUB_RELEASE_API_BASE}/latest"),
            |version| format!("{GITHUB_RELEASE_API_BASE}/tags/v{version}"),
        )
    }

    fn github_release_not_found_error() -> ManagerError {
        ManagerError::NetworkError("请求 GitHub API 返回错误：HTTP 404".to_string())
    }

    fn fetch_github_release_json_from_url(
        &self,
        api_url: &str,
    ) -> std::result::Result<serde_json::Value, JsonRequestError> {
        get_json_with_retry_stopping_on_status(
            &self.agent,
            self.ui,
            api_url,
            Some("application/vnd.github+json"),
            "请求 GitHub API ",
            Some(RetryConfig::github_release_note()),
            &[404],
        )
    }

    fn fetch_github_release_json(&self, version: Option<&str>) -> Result<serde_json::Value> {
        let cache_key = version.unwrap_or("latest").to_string();

        if let Ok(guard) = self.cached_github_releases.lock()
            && let Some(json) = guard.get(&cache_key)
        {
            return Ok(json.clone());
        }

        let api_url = Self::github_release_api_url(version);

        let json = match self.fetch_github_release_json_from_url(&api_url) {
            Ok(json) => json,
            Err(JsonRequestError::HttpStatus(404)) => {
                if let Some(v) = version
                    && let Some(fallback_tag) = Self::github_release_fallback_tag_for_status(v, 404)
                {
                    let fallback_url = Self::github_release_api_url(Some(&fallback_tag));
                    match self.fetch_github_release_json_from_url(&fallback_url) {
                        Ok(fallback_json) => {
                            if let Ok(mut guard) = self.cached_github_releases.lock() {
                                guard.insert(cache_key, fallback_json.clone());
                            }
                            return Ok(fallback_json);
                        }
                        Err(JsonRequestError::HttpStatus(404)) => {
                            return Err(Self::github_release_not_found_error());
                        }
                        Err(JsonRequestError::Other(err)) => {
                            return Err(err);
                        }
                        Err(JsonRequestError::HttpStatus(status)) => {
                            return Err(ManagerError::NetworkError(format!(
                                "请求 GitHub API 返回错误：HTTP {status}"
                            )));
                        }
                    }
                }
                return Err(Self::github_release_not_found_error());
            }
            Err(JsonRequestError::Other(err)) => {
                return Err(err);
            }
            Err(JsonRequestError::HttpStatus(status)) => {
                return Err(ManagerError::NetworkError(format!(
                    "请求 GitHub API 返回错误：HTTP {status}"
                )));
            }
        };

        if let Ok(mut guard) = self.cached_github_releases.lock() {
            guard.insert(cache_key, json.clone());
        }

        Ok(json)
    }

    fn github_metamystia_asset_candidates_for_tag(tag: &str) -> Vec<String> {
        let normalized = VersionInfo::normalize_version(tag);
        let parts: Vec<_> = normalized.split('.').collect();

        match parts.as_slice() {
            [major, minor] if major.parse::<u64>().is_ok() && minor.parse::<u64>().is_ok() => {
                vec![
                    format!("MetaMystia-v{}.{}.dll", major, minor),
                    format!("MetaMystia-v{}.{}.0.dll", major, minor),
                ]
            }
            [major, minor, patch]
                if major.parse::<u64>().is_ok()
                    && minor.parse::<u64>().is_ok()
                    && patch.parse::<u64>().is_ok() =>
            {
                vec![format!("MetaMystia-v{}.{}.{}.dll", major, minor, patch)]
            }
            _ => Vec::new(),
        }
    }

    fn get_dll_download_url_from_github(&self, version: &str) -> Result<String> {
        self.ui.download_attempt_github_dll()?;

        let json = self.fetch_github_release_json(Some(version))?;
        let tag = json["tag_name"].as_str().unwrap_or("");

        if !Self::github_release_tag_matches_requested_version(version, tag) {
            report_event(
                "Download.GitHub.Dll.TagMismatch",
                Some(&format!("requested={version};tag={tag}")),
            );
            return Err(ManagerError::NetworkError(format!(
                "GitHub Release 标签与目标版本不一致：目标 {}，实际 {}",
                version,
                if tag.is_empty() { "<empty>" } else { tag }
            )));
        }

        let candidates = Self::github_metamystia_asset_candidates_for_tag(tag);

        if candidates.is_empty() {
            report_event(
                "Download.GitHub.Dll.InvalidTag",
                Some(&format!("requested={version};tag={tag}")),
            );
            return Err(ManagerError::NetworkError(format!(
                "无法从 GitHub Release 标签解析 MetaMystia 文件名：{}",
                if tag.is_empty() { "<empty>" } else { tag }
            )));
        }

        if let Some(assets) = json["assets"].as_array() {
            for asset in assets {
                if let (Some(name), Some(url)) = (
                    asset["name"].as_str(),
                    asset["browser_download_url"].as_str(),
                ) && candidates
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(name))
                {
                    self.ui.download_found_github_asset(name)?;
                    report_event(
                        "Download.GitHub.Dll.Found",
                        Some(&format!("requested={version};tag={tag};file={name}")),
                    );
                    return Ok(url.to_string());
                }
            }
        }

        self.ui.download_github_dll_not_found()?;
        report_event("Download.GitHub.Dll.NotFound", None);

        Err(ManagerError::NetworkError(
            "未找到 MetaMystia DLL 文件".to_string(),
        ))
    }

    fn get_github_release_notes(
        &self,
        version: Option<&str>,
    ) -> Result<Option<(String, String, String)>> {
        let json = self.fetch_github_release_json(version)?;

        let tag = json["tag_name"].as_str().unwrap_or("").to_string();
        let name = json["name"].as_str().unwrap_or("").to_string();
        let body = json["body"].as_str().unwrap_or("").to_string();

        if tag.is_empty() && name.is_empty() && body.trim().is_empty() {
            report_event("Download.GitHub.ReleaseNotes.Empty", version);
            Ok(None)
        } else {
            report_event("Download.GitHub.ReleaseNotes.Found", Some(&tag));
            Ok(Some((tag, name, body)))
        }
    }

    /// 获取并显示 GitHub Release Notes
    ///
    /// # 参数
    /// - `version`: 版本号（不含 'v' 前缀），例如 "1.0.0"。如果为 None，则获取最新版本的 notes
    pub fn fetch_and_display_github_release_notes(
        &self,
        version: Option<&str>,
    ) -> Result<Option<(String, String, String)>> {
        match self.get_github_release_notes(version) {
            Ok(Some((tag, name, body))) => {
                self.ui
                    .download_display_github_release_notes(&tag, &name, &body)?;
                Ok(Some((tag, name, body)))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                report_event(
                    "Download.GitHub.ReleaseNotes.Failed",
                    Some(&format!("version={version:?};error={e}")),
                );
                Ok(None)
            }
        }
    }

    /// 下载 MetaMystia DLL
    pub fn download_metamystia(
        &self,
        share_code: &str,
        version: &str,
        dest: &Path,
        category: Option<&str>,
        try_github: bool,
    ) -> Result<()> {
        report_event("Download.Metamystia.Start", Some(version));

        let fallback_filenames = [VersionInfo::metamystia_filename(version)];
        let base_url = Self::file_api_base(share_code, category);

        if !try_github {
            return self
                .download_share_code_asset_with_events(
                    &base_url,
                    &fallback_filenames,
                    dest,
                    version,
                    "Download.Metamystia.Success.Fallback",
                    "Download.Metamystia.Failed.Fallback",
                )
                .map(|_| ());
        }

        if let Ok(url) = self.get_dll_download_url_from_github(version) {
            if let Err(e) = self.download_file_with_progress_and_speed_check(
                &url,
                dest,
                None,
                false,
                Some(EXTERNAL_SOURCE_MIN_SPEED_BPS),
            ) {
                self.ui.download_switch_to_fallback(&format!(
                    "从 GitHub 下载 MetaMystia DLL 失败：{e}，切换到备用源..."
                ))?;
                self.ui.download_try_fallback_metamystia()?;
                report_event("Download.Metamystia.Failed.GitHub", Some(&format!("{e}")));

                self.download_share_code_asset_with_events(
                    &base_url,
                    &fallback_filenames,
                    dest,
                    version,
                    "Download.Metamystia.Success.Fallback",
                    "Download.Metamystia.Failed.Fallback",
                )
                .map(|_| ())
            } else {
                report_event("Download.Metamystia.Success.GitHub", Some(version));
                Ok(())
            }
        } else {
            self.ui.download_switch_to_fallback(
                "从 GitHub 获取 MetaMystia DLL 下载链接失败，切换到备用源...",
            )?;
            self.ui.download_try_fallback_metamystia()?;
            report_event("Download.Metamystia.GitHubUrlFailed", None);

            self.download_share_code_asset_with_events(
                &base_url,
                &fallback_filenames,
                dest,
                version,
                "Download.Metamystia.Success.Fallback",
                "Download.Metamystia.Failed.Fallback",
            )
            .map(|_| ())
        }
    }

    /// 下载 ResourceExample ZIP
    pub fn download_resourceex(
        &self,
        share_code: &str,
        version: &str,
        dest: &Path,
        category: Option<&str>,
    ) -> Result<()> {
        report_event("Download.ResourceEx.Start", Some(version));

        let filenames = [VersionInfo::resourceex_filename(version)];
        let base_url = Self::file_api_base(share_code, category);

        self.download_share_code_asset_with_events(
            &base_url,
            &filenames,
            dest,
            version,
            "Download.ResourceEx.Success",
            "Download.ResourceEx.Failed",
        )
        .map(|_| ())
    }

    /// 下载 BepInEx
    pub fn download_bepinex(&self, version_info: &VersionInfo, dest: &Path) -> Result<bool> {
        let filename = version_info.bepinex_filename()?;
        let version = version_info.bepinex_version()?;
        let filename_with_version =
            percent_encode(format!("{version}#{filename}").as_bytes(), NON_ALPHANUMERIC)
                .to_string();

        self.ui.download_bepinex_attempt_primary()?;
        report_event("Download.BepInEx.Start", Some(version));

        let primary_url = format!("{BEPINEX_PRIMARY}/{version}/{filename}");
        let primary_result = get_response_with_retry(
            &self.agent,
            self.ui,
            &primary_url,
            "请求 BepInEx 主源",
            None,
        );

        if let Ok(resp) = primary_result {
            let total_size = resp
                .headers()
                .get("Content-Length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok());
            let id = self
                .ui
                .download_start("BepInEx（bepinex.dev）", total_size)?;

            if let Err(e) = self.write_response_to_file(
                &mut resp.into_body().into_reader(),
                dest,
                id,
                total_size,
                false,
                Some(EXTERNAL_SOURCE_MIN_SPEED_BPS),
            ) {
                self.ui.download_finish(id, "从 bepinex.dev 下载失败")?;
                self.ui.download_bepinex_primary_failed(&format!(
                    "从 bepinex.dev 下载失败 ({e}), 切换到备用源..."
                ))?;
                report_event("Download.BepInEx.Failed.Primary", Some(&format!("{e}")));

                let share_code = self.get_share_code()?;
                let fallback_filenames = [filename_with_version];
                let base_url =
                    Self::file_api_base(&share_code, version_info.paths.bep_in_ex.as_deref());

                self.download_share_code_asset_with_events(
                    &base_url,
                    &fallback_filenames,
                    dest,
                    version,
                    "Download.BepInEx.Success.Fallback",
                    "Download.BepInEx.Failed.Fallback",
                )
                .map(|_| false)
            } else {
                report_event("Download.BepInEx.Success.Primary", Some(version));
                Ok(true)
            }
        } else {
            self.ui.download_bepinex_primary_failed(
                "从 bepinex.dev 下载失败或超时，切换到备用源...",
            )?;
            report_event("Download.BepInEx.PrimaryRequestFailed", Some(version));

            let share_code = self.get_share_code()?;
            let fallback_filenames = [filename_with_version];
            let base_url =
                Self::file_api_base(&share_code, version_info.paths.bep_in_ex.as_deref());

            self.download_share_code_asset_with_events(
                &base_url,
                &fallback_filenames,
                dest,
                version,
                "Download.BepInEx.Success.Fallback",
                "Download.BepInEx.Failed.Fallback",
            )
            .map(|_| false)
        }
    }

    /// 下载管理工具可执行文件
    pub fn download_manager(&self, version_info: &VersionInfo, dest: &Path) -> Result<()> {
        let filename = version_info.manager_filename();

        report_event("Download.Manager.Start", Some(&version_info.manager));

        let share_code = self.get_share_code()?;
        let base_url = Self::file_api_base(&share_code, version_info.paths.manager.as_deref());
        let url = format!("{base_url}/{filename}");

        match self.download_file_with_progress(&url, dest, None, true) {
            Ok(()) => {
                report_event("Download.Manager.Success", Some(&version_info.manager));
                Ok(())
            }
            Err(e) => {
                report_event("Download.Manager.Failed", Some(&format!("{e}")));
                Err(e)
            }
        }
    }
}
