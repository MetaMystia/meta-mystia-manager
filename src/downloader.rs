use crate::config::RetryConfig;
use crate::error::{ManagerError, Result};
use crate::file_ops::atomic_rename_or_copy;
use crate::metrics::report_event;
use crate::model::VersionInfo;
use crate::net::{get_json_with_retry, get_response_with_retry, with_retry};
use crate::ui::Ui;

use percent_encoding::{NON_ALPHANUMERIC, percent_encode};
use reqwest::blocking::{Client, ClientBuilder};
use std::cmp;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant};

const FILE_API: &str = "https://file.izakaya.cc/api/public/dl";
const REDIRECT_URL: &str = "https://url.izakaya.cc/getMetaMystia";
const VERSION_API: &str = "https://api.izakaya.cc/version/meta-mystia";

const BEPINEX_PRIMARY: &str = "https://builds.bepinex.dev/projects/bepinex_be";
const GITHUB_RELEASE_API_BASE: &str = "https://api.github.com/repos/MetaMikuAI/MetaMystia/releases";

const RATE_LIMIT: usize = 128 * 1024; // 128KB/s
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5); // 连接超时

/// 下载器
pub struct Downloader<'a> {
    client: Client,
    ui: &'a dyn Ui,
    cached_github_releases: Mutex<HashMap<String, serde_json::Value>>,
    cached_version: Mutex<Option<VersionInfo>>,
}

impl<'a> Downloader<'a> {
    pub fn new(ui: &'a dyn Ui) -> Result<Self> {
        let client = Self::build_client(CONNECT_TIMEOUT)?;
        Ok(Self {
            client,
            ui,
            cached_github_releases: Mutex::new(HashMap::new()),
            cached_version: Mutex::new(None),
        })
    }

    fn build_client(connect_timeout: Duration) -> Result<Client> {
        ClientBuilder::new()
            .connect_timeout(connect_timeout)
            .user_agent(crate::config::USER_AGENT)
            .build()
            .map_err(|e| {
                report_event("Download.ClientBuildFailed", Some(&format!("{}", e)));
                ManagerError::NetworkError(format!("创建 HTTP 客户端失败：{}", e))
            })
    }

    fn retry<F, T>(&self, op_desc: &str, f: F) -> Result<T>
    where
        F: FnMut() -> Result<T>,
    {
        with_retry(self.ui, op_desc, None, f)
    }

    fn convert_reqwest_error(&self, e: reqwest::Error) -> String {
        if e.is_timeout() {
            "请求超时".to_string()
        } else if e.is_connect() {
            "连接失败".to_string()
        } else if e.is_status() {
            format!(
                "服务器返回错误：HTTP {}",
                e.status()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "未知".to_string())
            )
        } else {
            format!("请求失败：{}", e)
        }
    }

    fn file_api_url(share_code: &str, filename: &str) -> String {
        format!("{}/{}/{}", FILE_API, share_code, filename)
    }

    fn parse_share_code_from_url(url: &str) -> Option<String> {
        url.trim_end_matches('/')
            .split('/')
            .next_back()
            .and_then(|s| s.split(&['?', '#'][..]).next())
            .map(|s| s.to_string())
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
            .unwrap_or_else(|e| e.into_inner()) = Some(vi.clone());

        Ok(vi)
    }

    fn try_get_version_info(&self) -> Result<VersionInfo> {
        self.ui.download_version_info_start()?;

        let response = self.client.get(VERSION_API).send().map_err(|e| {
            let msg = self.convert_reqwest_error(e);
            let _ = self.ui.download_version_info_failed(&msg);
            ManagerError::NetworkError(msg)
        })?;

        if !response.status().is_success() {
            return Err(ManagerError::NetworkError(format!(
                "获取版本信息失败：HTTP {}",
                response.status()
            )));
        }

        let text = response
            .text()
            .map_err(|e| ManagerError::NetworkError(format!("读取响应失败：{}", e)))?;

        let vi: VersionInfo = serde_json::from_str(&text).map_err(|e| {
            let snippet: String = text.chars().take(200).collect();

            let _ = self
                .ui
                .download_version_info_parse_failed(&format!("{}", e), &snippet);
            report_event(
                "Download.VersionInfo.ParseFailed",
                Some(&format!("err={};snippet={}", e, snippet)),
            );

            ManagerError::Other(format!("解析版本信息失败：{}", e))
        })?;

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

        let response = self.client.get(REDIRECT_URL).send().map_err(|e| {
            let msg = self.convert_reqwest_error(e);
            let _ = self.ui.download_share_code_failed(&msg);
            ManagerError::NetworkError(msg)
        })?;

        if !response.status().is_success() {
            return Err(ManagerError::NetworkError(format!(
                "获取下载链接失败：HTTP {}",
                response.status()
            )));
        }

        let final_url = response.url().as_str();
        if let Some(code) = Self::parse_share_code_from_url(final_url) {
            self.ui.download_share_code_success()?;
            report_event("Download.ShareCode.Success", Some(&code));
            Ok(code)
        } else {
            report_event(
                "Download.ShareCode.ParseFailed",
                Some(&format!("url={}", final_url)),
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
        self.retry("下载文件", || {
            self.try_download(url, dest, file_size, rate_limit)
        })
    }

    fn try_download(
        &self,
        url: &str,
        dest: &Path,
        file_size: Option<u64>,
        rate_limit: bool,
    ) -> Result<()> {
        let mut response = self
            .client
            .get(url)
            .send()
            .map_err(|e| ManagerError::NetworkError(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ManagerError::NetworkError(format!(
                "HTTP {}",
                response.status()
            )));
        }

        let total_size = file_size.or_else(|| response.content_length());
        let filename = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dest.display().to_string());

        let id = self.ui.download_start(&filename, total_size)?;

        self.write_response_to_file(&mut response, dest, id, rate_limit)
    }

    fn write_response_to_file<R: Read>(
        &self,
        resp: &mut R,
        dest: &Path,
        id: usize,
        rate_limit: bool,
    ) -> Result<()> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ManagerError::from(std::io::Error::new(
                    e.kind(),
                    format!("创建目录 {} 失败：{}", parent.display(), e),
                ))
            })?;
        }

        let mut tmp_path = dest.with_extension("dl.tmp");
        let mut tmp_idx = 0;
        while tmp_path.exists() {
            tmp_idx += 1;
            tmp_path = dest.with_extension(format!("dl.tmp{}", tmp_idx));
        }

        let mut tmp_file = std::fs::File::create(&tmp_path).map_err(|e| {
            ManagerError::from(std::io::Error::new(
                e.kind(),
                format!("创建临时文件 {} 失败：{}", tmp_path.display(), e),
            ))
        })?;

        let buf_len = cmp::min(RATE_LIMIT, 8192) as usize;
        let mut buffer = vec![0; buf_len];

        let mut downloaded = 0u64;
        let start = Instant::now();

        loop {
            let to_read = buffer.len();

            let n = resp
                .read(&mut buffer[..to_read])
                .map_err(|e| ManagerError::NetworkError(e.to_string()))?;
            if n == 0 {
                break;
            }

            tmp_file.write_all(&buffer[..n]).map_err(|e| {
                ManagerError::from(std::io::Error::new(
                    e.kind(),
                    format!("写入临时文件 {} 失败：{}", tmp_path.display(), e),
                ))
            })?;
            downloaded += n as u64;

            self.ui.download_update(id, downloaded)?;

            if rate_limit {
                let expected_secs = (downloaded as f64) / (RATE_LIMIT as f64);
                let elapsed = start.elapsed().as_secs_f64();
                if expected_secs > elapsed {
                    let to_sleep = expected_secs - elapsed;
                    let sleep_dur = if cfg!(test) {
                        Duration::from_millis(1)
                    } else {
                        let ms = (to_sleep * 1000.0).max(1.0);
                        Duration::from_millis(ms.ceil() as u64)
                    };
                    sleep(sleep_dur);
                }
            }
        }

        tmp_file.flush().map_err(|e| {
            ManagerError::from(std::io::Error::new(
                e.kind(),
                format!("同步临时文件 {} 失败：{}", tmp_path.display(), e),
            ))
        })?;

        match atomic_rename_or_copy(&tmp_path, dest) {
            Ok(_) => {
                let _ = std::fs::remove_file(&tmp_path);
                self.ui.download_finish(
                    id,
                    &format!(
                        "下载完成：{}",
                        dest.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| dest.display().to_string())
                    ),
                )?;
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp_path);
                Err(ManagerError::from(std::io::Error::other(format!(
                    "重命名或复制临时文件 {} 失败：{}",
                    tmp_path.display(),
                    e
                ))))
            }
        }
    }

    fn fetch_github_release_json(&self, version: Option<&str>) -> Result<serde_json::Value> {
        let cache_key = version.unwrap_or("latest").to_string();

        if let Ok(guard) = self.cached_github_releases.lock()
            && let Some(json) = guard.get(&cache_key)
        {
            return Ok(json.clone());
        }

        let api_url = if let Some(v) = version {
            format!("{}/tags/v{}", GITHUB_RELEASE_API_BASE, v)
        } else {
            format!("{}/latest", GITHUB_RELEASE_API_BASE)
        };

        let result = get_json_with_retry::<serde_json::Value>(
            &self.client,
            self.ui,
            &api_url,
            Some("application/vnd.github+json"),
            "请求 GitHub API ",
            Some(RetryConfig::github_release_note()),
        );

        let json = match result {
            Ok(json) => json,
            Err(e) => {
                if let Some(v) = version
                    && v.ends_with(".0")
                {
                    let trimmed_version = &v[..v.len() - 2];
                    let fallback_url =
                        format!("{}/tags/v{}", GITHUB_RELEASE_API_BASE, trimmed_version);
                    if let Ok(fallback_json) = get_json_with_retry::<serde_json::Value>(
                        &self.client,
                        self.ui,
                        &fallback_url,
                        Some("application/vnd.github+json"),
                        "请求 GitHub API ",
                        Some(RetryConfig::github_release_note()),
                    ) {
                        if let Ok(mut guard) = self.cached_github_releases.lock() {
                            guard.insert(cache_key, fallback_json.clone());
                        }
                        return Ok(fallback_json);
                    }
                }
                return Err(e);
            }
        };

        if let Ok(mut guard) = self.cached_github_releases.lock() {
            guard.insert(cache_key, json.clone());
        }

        Ok(json)
    }

    fn get_dll_download_url_from_github(&self) -> Result<String> {
        self.ui.download_attempt_github_dll()?;

        let json = self.fetch_github_release_json(None)?;

        if let Some(assets) = json["assets"].as_array() {
            for asset in assets {
                match (
                    asset["name"].as_str(),
                    asset["browser_download_url"].as_str(),
                ) {
                    (Some(name), Some(url))
                        if name.starts_with("MetaMystia-v") && name.ends_with(".dll") =>
                    {
                        self.ui.download_found_github_asset(name)?;
                        report_event("Download.GitHub.Dll.Found", Some(name));
                        return Ok(url.to_string());
                    }
                    _ => {}
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
                    Some(&format!("version={:?};error={}", version, e)),
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
        try_github: bool,
    ) -> Result<()> {
        report_event("Download.Metamystia.Start", Some(version));

        if !try_github {
            let filename = VersionInfo::metamystia_filename(version);
            let url = Self::file_api_url(share_code, &filename);

            return match self.download_file_with_progress(&url, dest, None, true) {
                Ok(()) => {
                    report_event("Download.Metamystia.Success.Fallback", Some(version));
                    Ok(())
                }
                Err(e) => {
                    report_event(
                        "Download.Metamystia.Failed.Fallback",
                        Some(&format!("{}", e)),
                    );
                    Err(e)
                }
            };
        }

        match self.get_dll_download_url_from_github() {
            Ok(url) => {
                if let Err(e) = self.download_file_with_progress(&url, dest, None, false) {
                    self.ui.download_switch_to_fallback(&format!(
                        "从 GitHub 下载 MetaMystia DLL 失败：{}，切换到备用源...",
                        e
                    ))?;
                    self.ui.download_try_fallback_metamystia()?;
                    report_event("Download.Metamystia.Failed.GitHub", Some(&format!("{}", e)));

                    let filename = VersionInfo::metamystia_filename(version);
                    let fallback_url = Self::file_api_url(share_code, &filename);

                    match self.download_file_with_progress(&fallback_url, dest, None, true) {
                        Ok(()) => {
                            report_event("Download.Metamystia.Success.Fallback", Some(version));
                            Ok(())
                        }
                        Err(e) => {
                            report_event(
                                "Download.Metamystia.Failed.Fallback",
                                Some(&format!("{}", e)),
                            );
                            Err(e)
                        }
                    }
                } else {
                    report_event("Download.Metamystia.Success.GitHub", Some(version));
                    Ok(())
                }
            }
            Err(_) => {
                self.ui.download_switch_to_fallback(
                    "从 GitHub 获取 MetaMystia DLL 下载链接失败，切换到备用源...",
                )?;
                self.ui.download_try_fallback_metamystia()?;
                report_event("Download.Metamystia.GitHubUrlFailed", None);

                let filename = VersionInfo::metamystia_filename(version);
                let url = Self::file_api_url(share_code, &filename);

                match self.download_file_with_progress(&url, dest, None, true) {
                    Ok(()) => {
                        report_event("Download.Metamystia.Success.Fallback", Some(version));
                        Ok(())
                    }
                    Err(e) => {
                        report_event(
                            "Download.Metamystia.Failed.Fallback",
                            Some(&format!("{}", e)),
                        );
                        Err(e)
                    }
                }
            }
        }
    }

    /// 下载 ResourceExample ZIP
    pub fn download_resourceex(&self, share_code: &str, version: &str, dest: &Path) -> Result<()> {
        report_event("Download.ResourceEx.Start", Some(version));

        let filename = VersionInfo::resourceex_filename(version);
        let url = Self::file_api_url(share_code, &filename);

        match self.download_file_with_progress(&url, dest, None, true) {
            Ok(()) => {
                report_event("Download.ResourceEx.Success", Some(version));
                Ok(())
            }
            Err(e) => {
                report_event("Download.ResourceEx.Failed", Some(&format!("{}", e)));
                Err(e)
            }
        }
    }

    /// 下载 BepInEx
    pub fn download_bepinex(&self, version_info: &VersionInfo, dest: &Path) -> Result<bool> {
        let filename = version_info.bepinex_filename()?;
        let version = version_info.bepinex_version()?;
        let filename_with_version = percent_encode(
            format!("{}#{}", version, filename).as_bytes(),
            NON_ALPHANUMERIC,
        )
        .to_string();

        self.ui.download_bepinex_attempt_primary()?;
        report_event("Download.BepInEx.Start", Some(version));

        let primary_url = format!("{}/{}/{}", BEPINEX_PRIMARY, version, filename);
        let primary_result = get_response_with_retry(
            &self.client,
            self.ui,
            &primary_url,
            "请求 BepInEx 主源",
            None,
        );

        match primary_result {
            Ok(mut resp) => {
                let total_size = resp.content_length();
                let id = self
                    .ui
                    .download_start("BepInEx（bepinex.dev）", total_size)?;

                if let Err(e) = self.write_response_to_file(&mut resp, dest, id, false) {
                    self.ui.download_finish(id, "从 bepinex.dev 下载失败")?;
                    self.ui.download_bepinex_primary_failed(&format!(
                        "从 bepinex.dev 下载失败 ({}), 切换到备用源...",
                        e
                    ))?;
                    report_event("Download.BepInEx.Failed.Primary", Some(&format!("{}", e)));

                    let share_code = self.get_share_code()?;
                    let fallback_url = Self::file_api_url(&share_code, &filename_with_version);

                    match self.download_file_with_progress(&fallback_url, dest, None, true) {
                        Ok(()) => {
                            report_event("Download.BepInEx.Success.Fallback", Some(version));
                            Ok(false)
                        }
                        Err(e) => {
                            report_event(
                                "Download.BepInEx.Failed.Fallback",
                                Some(&format!("{}", e)),
                            );
                            Err(e)
                        }
                    }
                } else {
                    report_event("Download.BepInEx.Success.Primary", Some(version));
                    Ok(true)
                }
            }
            Err(_) => {
                self.ui.download_bepinex_primary_failed(
                    "从 bepinex.dev 下载失败或超时，切换到备用源...",
                )?;
                report_event("Download.BepInEx.PrimaryRequestFailed", Some(version));

                let share_code = self.get_share_code()?;
                let fallback_url = Self::file_api_url(&share_code, &filename_with_version);

                match self.download_file_with_progress(&fallback_url, dest, None, true) {
                    Ok(()) => {
                        report_event("Download.BepInEx.Success.Fallback", Some(version));
                        Ok(false)
                    }
                    Err(e) => {
                        report_event("Download.BepInEx.Failed.Fallback", Some(&format!("{}", e)));
                        Err(e)
                    }
                }
            }
        }
    }

    /// 下载管理工具可执行文件
    pub fn download_manager(&self, version_info: &VersionInfo, dest: &Path) -> Result<()> {
        let filename = version_info.manager_filename();

        report_event("Download.Manager.Start", Some(&version_info.manager));

        let share_code = self.get_share_code()?;
        let url = Self::file_api_url(&share_code, &filename);

        match self.download_file_with_progress(&url, dest, None, true) {
            Ok(()) => {
                report_event("Download.Manager.Success", Some(&version_info.manager));
                Ok(())
            }
            Err(e) => {
                report_event("Download.Manager.Failed", Some(&format!("{}", e)));
                Err(e)
            }
        }
    }
}
