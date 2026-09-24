use crate::config::RetryConfig;
use crate::error::{ManagerError, Result, service_error};
use crate::file_ops::atomic_rename_or_copy;
use crate::metrics::report_event;
use crate::model::VersionInfo;
use crate::net::{
    JsonRequestError, build_agent, check_response_status, get_json_with_retry_stopping_on_status,
    get_response_with_retry, with_retry,
};
#[cfg(not(windows))]
use crate::platform;
use crate::remote_config::{self, RemoteConfig};
use crate::sso;
use crate::ui::Ui;

use serde::Deserialize;
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

mod github;
mod keys;
mod transfer;

/// 唯一的引导地址：配置与其余端点都由它下发
const VERSION_API: &str = "https://api.izakaya.cc/version/meta-mystia";

const DOWNLOAD_BUFFER_SIZE: usize = 8192;
const KEY_ATTEMPTS: usize = 2; // 密钥失效（410）后重新申请的上限
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5); // 连接超时

const EXTERNAL_SOURCE_MIN_SPEED_BPS: usize = 128 * 1024; // 128KB/s，外部源最低速度阈值
const SPEED_CHECK_INTERVAL: Duration = Duration::from_secs(10); // 滑动窗口长度
const OVERALL_CHECK_INTERVAL: Duration = Duration::from_secs(5); // 整体均速采样间隔
const WARMUP_DURATION: Duration = Duration::from_secs(5); // 启动期豁免
const MAX_CONSECUTIVE_SLOW_WINDOWS: u32 = 2; // 滑动窗口连续低速换源阈值
const MAX_CONSECUTIVE_SLOW_OVERALL: u32 = 2; // 整体均速连续低速换源阈值
const TAIL_SKIP_RATIO: f64 = 0.90; // 已下载比例豁免阈值
const TAIL_SKIP_MIN_REMAINING_CAP: u64 = 384 * 1024; // 剩余字节豁免阈值上限

/// 一个下载任务：名称（用于错误提示）+ 执行体
pub type DownloadJob<'a> = (&'a str, Box<dyn Fn() -> Result<()> + Send + Sync + 'a>);

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

    /// 运行期配置；地址由版本 API 随 `configUrl` 下发
    fn remote_config(&self) -> Result<RemoteConfig> {
        let config_url = {
            let guard = self
                .cached_version
                .lock()
                .unwrap_or_else(PoisonError::into_inner);

            guard
                .as_ref()
                .map(|info| info.config_url.clone())
                .ok_or(ManagerError::InvalidVersionInfo)?
        };

        remote_config::get(self.ui, &config_url)
    }

    fn max_concurrent_downloads(&self) -> Result<usize> {
        let config = self.remote_config()?;

        Ok(usize::try_from(config.download.max_concurrent_downloads)
            .unwrap_or(usize::MAX)
            .max(1))
    }

    /// 按 `maxConcurrentDownloads` 并发执行下载任务，任一失败即中止
    pub fn download_files(&self, jobs: &[DownloadJob<'_>]) -> Result<()> {
        if jobs.is_empty() {
            return Ok(());
        }

        // 开发模拟模式：占位产物由各下载函数直接生成，跳过远程配置与并发调度
        #[cfg(not(windows))]
        if platform::dev::sim_download() {
            for (name, job) in jobs {
                if let Err(e) = job() {
                    let message = format!("{name}失败：{e}");
                    report_event("Download.Job.Failed", Some(&message));
                    return Err(ManagerError::ServiceError(message));
                }
            }

            return Ok(());
        }

        let concurrency = self.max_concurrent_downloads()?;

        for chunk in jobs.chunks(concurrency) {
            let failed = std::thread::scope(|scope| {
                let handles = chunk
                    .iter()
                    .map(|(name, job)| (*name, scope.spawn(job)))
                    .collect::<Vec<_>>();
                let mut failed = None;

                for (name, handle) in handles {
                    let result = handle.join().unwrap_or_else(|_| {
                        Err(ManagerError::Other("下载线程异常结束".to_string()))
                    });

                    if let Err(e) = result {
                        failed.get_or_insert_with(|| format!("{name}失败：{e}"));
                    }
                }

                failed
            });

            if let Some(message) = failed {
                report_event("Download.Job.Failed", Some(&message));
                return Err(ManagerError::ServiceError(message));
            }
        }

        Ok(())
    }

    fn content_length(response: &ureq::http::Response<ureq::Body>) -> Option<u64> {
        response
            .headers()
            .get("Content-Length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
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
        // 开发模拟模式：不联网，直接返回伪版本信息
        #[cfg(not(windows))]
        if platform::dev::sim_download() {
            self.ui.download_version_info_start()?;
            let version_info = platform::dev::fake_version_info();
            self.ui.download_version_info_success()?;
            return Ok(version_info);
        }

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

            ManagerError::Other(format!("版本信息格式不符合预期：{e}"))
        })?;

        vi.normalize_versions();
        vi.validate()?;

        self.ui.download_version_info_success()?;
        report_event("Download.VersionInfo.Success", Some(&vi.to_string()));

        Ok(vi)
    }

    /// 获取自更新入口短链最终跳转到的分享码
    fn get_share_code(&self, redirect_url: &str) -> Result<String> {
        // 开发模拟模式：不联网，直接返回占位分享码
        #[cfg(not(windows))]
        if platform::dev::sim_download() {
            self.ui.download_share_code_start()?;
            self.ui.download_share_code_success()?;
            return Ok("dev".to_string());
        }

        self.retry("获取下载链接", || {
            self.try_get_share_code(redirect_url)
        })
    }

    fn try_get_share_code(&self, redirect_url: &str) -> Result<String> {
        self.ui.download_share_code_start()?;

        let response = self.agent.get(redirect_url).call().map_err(|e| {
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

    /// 下载 MetaMystia DLL
    pub fn download_metamystia(
        &self,
        version: &str,
        dest: &Path,
        category: Option<&str>,
        try_github: bool,
    ) -> Result<()> {
        #[cfg(not(windows))]
        if platform::dev::sim_download() {
            report_event("Download.Metamystia.Success.Dev", Some(version));
            return platform::dev::write_fake_artifact(dest, &platform::dev::FakeArtifact::Dll);
        }

        report_event("Download.Metamystia.Start", Some(version));

        let filename = VersionInfo::metamystia_filename(version);

        if try_github {
            match self.get_dll_download_url_from_github(version) {
                Ok(url) => match self.download_file_with_progress_and_speed_check(
                    &url,
                    dest,
                    None,
                    None,
                    Some(EXTERNAL_SOURCE_MIN_SPEED_BPS),
                ) {
                    Ok(()) => {
                        report_event("Download.Metamystia.Success.GitHub", Some(version));
                        return Ok(());
                    }
                    Err(e) => {
                        self.ui.download_switch_to_fallback(&format!(
                            "从 GitHub 下载 MetaMystia DLL 失败：{e}，切换到备用源..."
                        ))?;
                        report_event("Download.Metamystia.Failed.GitHub", Some(&format!("{e}")));
                    }
                },
                Err(e) => {
                    self.ui.download_switch_to_fallback(
                        "从 GitHub 获取 MetaMystia DLL 下载链接失败，切换到备用源...",
                    )?;
                    report_event("Download.Metamystia.GitHubUrlFailed", Some(&format!("{e}")));
                }
            }

            self.ui.download_try_fallback_metamystia()?;
        }

        match self.download_asset_with_key(category, &filename, dest, "下载 MetaMystia DLL") {
            Ok(()) => {
                report_event("Download.Metamystia.Success.Fallback", Some(version));
                Ok(())
            }
            Err(e) => {
                report_event("Download.Metamystia.Failed.Fallback", Some(&format!("{e}")));
                Err(e)
            }
        }
    }

    /// 下载 ResourceExample ZIP
    pub fn download_resourceex(
        &self,
        version: &str,
        dest: &Path,
        category: Option<&str>,
    ) -> Result<()> {
        #[cfg(not(windows))]
        if platform::dev::sim_download() {
            report_event("Download.ResourceEx.Success.Dev", Some(version));
            return platform::dev::write_fake_artifact(dest, &platform::dev::FakeArtifact::Zip);
        }

        report_event("Download.ResourceEx.Start", Some(version));

        let filename = VersionInfo::resourceex_filename(version);

        match self.download_asset_with_key(category, &filename, dest, "下载 ResourceExample") {
            Ok(()) => {
                report_event("Download.ResourceEx.Success", Some(version));
                Ok(())
            }
            Err(e) => {
                report_event("Download.ResourceEx.Failed", Some(&format!("{e}")));
                Err(e)
            }
        }
    }

    /// 下载 BepInEx；返回是否来自上游主源
    pub fn download_bepinex(&self, version_info: &VersionInfo, dest: &Path) -> Result<bool> {
        #[cfg(not(windows))]
        if platform::dev::sim_download() {
            report_event("Download.BepInEx.Success.Dev", None);
            platform::dev::write_fake_artifact(dest, &platform::dev::FakeArtifact::Zip)?;
            return Ok(true);
        }

        let filename = version_info.bepinex_filename()?;
        let build = version_info.bepinex_version()?;

        self.ui.download_bepinex_attempt_primary()?;
        report_event("Download.BepInEx.Start", Some(build));

        // 上游直链：`{primary}/{构建号}/{文件名}`，文件名里的 `+` 需要编码
        let config = self.remote_config()?;
        let primary_url = format!(
            "{}/{build}/{}",
            config.sources.bep_in_ex_primary.trim_end_matches('/'),
            filename.replace('+', "%2B")
        );
        let primary_result = get_response_with_retry(
            &self.agent,
            self.ui,
            &primary_url,
            "请求 BepInEx 主源",
            None,
        );

        if let Ok(resp) = primary_result {
            let total_size = Self::content_length(&resp);
            let id = self
                .ui
                .download_start("BepInEx（bepinex.dev）", total_size)?;

            if let Err(e) = self.write_response_to_file(
                &mut resp.into_body().into_reader(),
                dest,
                id,
                total_size,
                None,
                Some(EXTERNAL_SOURCE_MIN_SPEED_BPS),
            ) {
                self.ui.download_finish(id, "从 bepinex.dev 下载失败")?;
                self.ui.download_bepinex_primary_failed(&format!(
                    "从 bepinex.dev 下载失败 ({e}), 切换到备用源..."
                ))?;
                report_event("Download.BepInEx.Failed.Primary", Some(&format!("{e}")));
            } else {
                report_event("Download.BepInEx.Success.Primary", Some(build));
                return Ok(true);
            }
        } else {
            self.ui.download_bepinex_primary_failed(
                "从 bepinex.dev 下载失败或超时，切换到备用源...",
            )?;
            report_event("Download.BepInEx.PrimaryRequestFailed", Some(build));
        }

        match self.download_asset_with_key(
            version_info.paths.bep_in_ex.as_deref(),
            filename,
            dest,
            "下载 BepInEx",
        ) {
            Ok(()) => {
                report_event("Download.BepInEx.Success.Fallback", Some(build));
                Ok(false)
            }
            Err(e) => {
                report_event("Download.BepInEx.Failed.Fallback", Some(&format!("{e}")));
                Err(e)
            }
        }
    }

    /// 下载管理工具可执行文件（自更新，不需要登录）
    pub fn download_manager(&self, version_info: &VersionInfo, dest: &Path) -> Result<()> {
        #[cfg(not(windows))]
        if platform::dev::sim_download() {
            report_event("Download.Manager.Success.Dev", Some(&version_info.manager));
            return platform::dev::write_fake_artifact(dest, &platform::dev::FakeArtifact::Exe);
        }

        let filename = version_info.manager_filename();

        report_event("Download.Manager.Start", Some(&version_info.manager));

        let config = self.remote_config()?;
        let rate_limit_bps =
            remote_config::rate_limit_bytes_per_second(config.download.rate_limit_kb_per_second);

        let share_code = self.get_share_code(&config.self_update.entry_url)?;
        let url = format!(
            "{}/{share_code}/{filename}",
            config.self_update.api_base.trim_end_matches('/')
        );

        match self.download_file_with_progress(&url, dest, None, rate_limit_bps) {
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
