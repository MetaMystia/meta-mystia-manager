//! 版本信息获取与文件下载。

use crate::error::{ManagerError, Result};
use crate::fs::file_ops::atomic_rename_or_copy;
use crate::http::{build_download_agent, build_metadata_agent, host_key};
use crate::net::remote_config::{self, RemoteConfig};
use crate::net::retry::RetryConfig;
use crate::net::sso;
use crate::net::{
    JsonRequestError, check_response_status, fetch_json_with_retry_stopping_on_status,
    fetch_response_with_retry, with_retry,
};
use crate::telemetry::report_event;
use crate::ui::{Ui, UiEvent};
use crate::version::VersionInfo;

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use serde::Deserialize;
use std::{
    cmp,
    collections::HashMap,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, PoisonError},
    thread::{self, sleep},
    time::{Duration, Instant},
};
use ureq::ResponseExt;

mod github;
mod keys;
mod transfer;

// 版本信息接口
const VERSION_API: &str = "https://api.izakaya.cc/version/meta-mystia";

// 下载并发
/// 同时下载的任务数上限。
pub const MAX_PARALLEL_DOWNLOADS: usize = 3;

// 下载缓冲与重试
const DOWNLOAD_BUFFER_SIZE: usize = 32 * 1024;
const KEY_ATTEMPTS: usize = 2;

// 路径编码
const PATH_SEGMENT_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'+')
    .add(b'/')
    .add(b'?')
    .add(b'<')
    .add(b'>')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'\\')
    .add(b'^')
    .add(b'|');

// 低速换源判定
const EXTERNAL_SOURCE_MIN_SPEED_BPS: usize = 128 * 1024;
const MAX_CONSECUTIVE_SLOW_OVERALL: u32 = 2;
const MAX_CONSECUTIVE_SLOW_WINDOWS: u32 = 2;
const MAX_TAIL_SKIP_REMAINING_BYTES: u64 = 384 * 1024;
const OVERALL_CHECK_INTERVAL: Duration = Duration::from_secs(5);
const SPEED_CHECK_INTERVAL: Duration = Duration::from_secs(10);
const TAIL_SKIP_RATIO: f64 = 0.90;
const WARMUP_DURATION: Duration = Duration::from_secs(5);

/// 一个下载任务：文件说明与进度回调。
pub type DownloadJob<'a> = (&'a str, Box<dyn Fn(usize) -> Result<()> + Send + Sync + 'a>);

/// 版本信息获取与文件下载的统一入口。
pub struct Downloader<'a> {
    agents: Mutex<HashMap<String, ureq::Agent>>,
    metadata_agents: Mutex<HashMap<String, ureq::Agent>>,
    ui: &'a dyn Ui,
}

static VERSION_CACHE: OnceLock<Mutex<Option<VersionInfo>>> = OnceLock::new();

/// 本次会话已获取的远端版本信息；诊断包会附带这份快照。
pub fn cached_version_info() -> Option<VersionInfo> {
    VERSION_CACHE
        .get()?
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn store_version_info(version_info: &VersionInfo) {
    *VERSION_CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(version_info.clone());
}

fn encode_path_segment(value: &str) -> String {
    utf8_percent_encode(value, PATH_SEGMENT_ENCODE_SET).to_string()
}

impl<'a> Downloader<'a> {
    /// 创建一个使用给定界面回调的下载器。
    pub fn new(ui: &'a dyn Ui) -> Self {
        Self {
            agents: Mutex::new(HashMap::new()),
            metadata_agents: Mutex::new(HashMap::new()),
            ui,
        }
    }

    /// 按主机缓存并复用下载 Agent。
    pub(super) fn agent_for(&self, url: &str) -> ureq::Agent {
        let key = host_key(url);
        let mut agents = self.agents.lock().unwrap_or_else(PoisonError::into_inner);

        agents
            .entry(key)
            .or_insert_with(|| build_download_agent(url))
            .clone()
    }

    /// 按主机缓存并复用元数据 Agent。
    pub(super) fn metadata_agent(&self, url: &str) -> ureq::Agent {
        let key = host_key(url);
        let mut agents = self
            .metadata_agents
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        agents
            .entry(key)
            .or_insert_with(|| build_metadata_agent(url))
            .clone()
    }

    /// 请求中止同批次的其它下载。
    pub(super) fn request_abort(&self) {
        self.ui.set_download_aborted(true);
    }

    /// 用户是否已取消或中止当前下载。
    pub(super) fn is_cancelled(&self) -> bool {
        self.ui.is_download_cancelled() || self.ui.is_download_aborted()
    }

    /// 按 `maxConcurrentDownloads` 并发执行下载任务，任一失败即中止。
    pub fn download_files(&self, jobs: &[DownloadJob<'_>]) -> Result<()> {
        if jobs.is_empty() {
            return Ok(());
        }

        self.ui.set_download_aborted(false);

        let names = jobs.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        self.ui.emit(UiEvent::DownloadPlan(&names))?;

        let concurrency = self.max_concurrent_downloads()?;

        for (chunk_index, chunk) in jobs.chunks(concurrency).enumerate() {
            let base = chunk_index * concurrency;
            let failed = thread::scope(|scope| {
                let handles = chunk
                    .iter()
                    .enumerate()
                    .map(|(offset, (name, job))| {
                        let slot = base + offset;

                        (
                            *name,
                            scope.spawn(move || {
                                let result = job(slot);

                                if result.is_err() {
                                    self.request_abort();
                                }

                                result
                            }),
                        )
                    })
                    .collect::<Vec<_>>();
                // 优先保留真正的失败原因，只有全部是取消时才回退为取消提示
                let mut failed: Option<(bool, String)> = None;

                for (name, handle) in handles {
                    let result = handle.join().unwrap_or_else(|_| {
                        Err(ManagerError::Other("下载线程异常结束".to_string()))
                    });

                    if let Err(e) = result {
                        self.request_abort();

                        let cancelled = matches!(e, ManagerError::UserCancelled);
                        let message = format!("{name}：{e}");
                        let replace = failed
                            .as_ref()
                            .is_none_or(|(was_cancelled, _)| *was_cancelled && !cancelled);

                        if replace {
                            failed = Some((cancelled, message));
                        }
                    }
                }

                failed.map(|(_, message)| message)
            });

            if let Some(message) = failed {
                report_event("Download.Job.Failed", Some(&message));
                return Err(ManagerError::ServiceError(message));
            }
        }

        if self.is_cancelled() {
            return Err(ManagerError::UserCancelled);
        }

        Ok(())
    }

    /// 获取版本信息：进程内缓存，并在返回前规范化、校验版本号。
    pub fn fetch_version_info(&self) -> Result<VersionInfo> {
        if let Some(cached) = cached_version_info() {
            return Ok(cached);
        }

        let vi = self.retry("获取版本信息", || self.try_fetch_version_info())?;
        store_version_info(&vi);

        Ok(vi)
    }

    /// 下载 MetaMystia DLL；`try_github` 为真时先试 GitHub，失败再回落到镜像源。
    pub fn download_metamystia(
        &self,
        version: &str,
        dest: &Path,
        category: Option<&str>,
        try_github: bool,
        slot: usize,
    ) -> Result<()> {
        if self.is_cancelled() {
            return Err(ManagerError::UserCancelled);
        }

        report_event("Download.Metamystia.Start", Some(version));

        let filename = VersionInfo::metamystia_filename(version);

        if try_github {
            match self.fetch_dll_download_url_from_github(version) {
                Ok(url) => match self.download_file_with_progress_and_speed_check(
                    &url,
                    dest,
                    None,
                    None,
                    Some(EXTERNAL_SOURCE_MIN_SPEED_BPS),
                    slot,
                ) {
                    Ok(()) => {
                        report_event("Download.Metamystia.Success.GitHub", Some(version));
                        return Ok(());
                    }
                    Err(e) if matches!(e, ManagerError::UserCancelled) => return Err(e),
                    Err(e) => {
                        self.ui.emit(UiEvent::DownloadSwitchToFallback(&format!(
                            "从 GitHub 下载 MetaMystia DLL 失败：{e}，切换到备用源..."
                        )))?;
                        report_event("Download.Metamystia.Failed.GitHub", Some(&format!("{e}")));
                    }
                },
                Err(e) if matches!(e, ManagerError::UserCancelled) => return Err(e),
                Err(e) => {
                    self.ui.emit(UiEvent::DownloadSwitchToFallback(
                        "从 GitHub 获取 MetaMystia DLL 下载链接失败，切换到备用源...",
                    ))?;
                    report_event("Download.Metamystia.GitHubUrlFailed", Some(&format!("{e}")));
                }
            }

            self.ui.emit(UiEvent::DownloadTryFallbackMetamystia)?;
        }

        match self.download_asset_with_key(category, &filename, dest, "下载 MetaMystia DLL", slot)
        {
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

    /// 下载 ResourceExample ZIP（可选组件，仅在用户选择安装时调用）。
    pub fn download_resourceex(
        &self,
        version: &str,
        dest: &Path,
        category: Option<&str>,
        slot: usize,
    ) -> Result<()> {
        if self.is_cancelled() {
            return Err(ManagerError::UserCancelled);
        }

        report_event("Download.ResourceEx.Start", Some(version));

        let filename = VersionInfo::resourceex_filename(version);

        match self.download_asset_with_key(category, &filename, dest, "下载 ResourceExample", slot)
        {
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

    /// 下载 BepInEx；返回是否来自上游主源。
    pub fn download_bepinex(
        &self,
        version_info: &VersionInfo,
        dest: &Path,
        slot: usize,
    ) -> Result<bool> {
        if self.is_cancelled() {
            return Err(ManagerError::UserCancelled);
        }

        let filename = version_info.bepinex_filename()?;
        let build = version_info.bepinex_version()?;
        let config = self.remote_config()?;

        report_event("Download.BepInEx.Start", Some(build));

        if let Some(primary) = config
            .sources
            .as_ref()
            .map(|sources| sources.bep_in_ex_primary.trim())
            .filter(|primary| !primary.is_empty())
        {
            self.ui.emit(UiEvent::DownloadBepinexAttemptPrimary)?;

            let primary_url = format!(
                "{}/{build}/{}",
                primary.trim_end_matches('/'),
                encode_path_segment(filename)
            );
            let primary_result = fetch_response_with_retry(
                &self.agent_for(&primary_url),
                self.ui,
                &primary_url,
                "请求 BepInEx 主源",
                None,
            );

            match primary_result {
                Ok(resp) => {
                    let total_size = Self::content_length(&resp);
                    let id = self
                        .ui
                        .emit(UiEvent::DownloadStart(
                            slot,
                            "BepInEx（bepinex.dev）",
                            total_size,
                        ))?
                        .download_id()?;

                    match self.write_response_to_file(
                        resp.into_body().into_reader(),
                        dest,
                        id,
                        total_size,
                        None,
                        Some(EXTERNAL_SOURCE_MIN_SPEED_BPS),
                    ) {
                        Ok(()) => {
                            report_event("Download.BepInEx.Success.Primary", Some(build));
                            return Ok(true);
                        }
                        Err(e) if matches!(e, ManagerError::UserCancelled) => return Err(e),
                        Err(e) => {
                            self.ui
                                .emit(UiEvent::DownloadBepinexPrimaryFailed(&format!(
                                    "从 bepinex.dev 下载失败 ({e}), 切换到备用源..."
                                )))?;
                            report_event("Download.BepInEx.Failed.Primary", Some(&format!("{e}")));
                        }
                    }
                }
                Err(e) if matches!(e, ManagerError::UserCancelled) => return Err(e),
                Err(_) => {
                    self.ui.emit(UiEvent::DownloadBepinexPrimaryFailed(
                        "从 bepinex.dev 下载失败或超时，切换到备用源...",
                    ))?;
                    report_event("Download.BepInEx.PrimaryRequestFailed", Some(build));
                }
            }
        }

        match self.download_asset_with_key(
            version_info.paths.bep_in_ex.as_deref(),
            filename,
            dest,
            "下载 BepInEx",
            slot,
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

    /// 下载管理工具可执行文件（自更新，不需要登录）。
    pub fn download_manager(&self, version_info: &VersionInfo, dest: &Path) -> Result<()> {
        let filename = version_info.manager_filename()?;

        report_event("Download.Manager.Start", version_info.manager_version());
        self.ui.emit(UiEvent::DownloadPlan(&["管理工具"]))?;

        let config = self.remote_config()?;
        let rate_limit_bps =
            remote_config::rate_limit_bytes_per_second(config.download.rate_limit_kb_per_second);
        let Some(self_update) = config.self_update.as_ref() else {
            report_event("Download.Manager.Disabled", None);
            return Err(ManagerError::Other(
                "服务端未配置管理工具自更新地址".to_string(),
            ));
        };

        let share_code = self.fetch_share_code(&self_update.entry_url)?;
        let url = format!(
            "{}/{share_code}/{filename}",
            self_update.api_base.trim_end_matches('/')
        );

        match self.download_file_with_progress(&url, dest, None, rate_limit_bps, 0) {
            Ok(()) => {
                report_event("Download.Manager.Success", version_info.manager_version());
                Ok(())
            }
            Err(e) => {
                report_event("Download.Manager.Failed", Some(&format!("{e}")));
                Err(e)
            }
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
            ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => "连接失败".to_string(),
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
            ureq::Error::Timeout(_) => "请求超时".to_string(),
            _ => format!("请求失败：{e}"),
        }
    }

    /// 运行期配置；地址由版本 API 随 `configUrl` 下发。
    fn remote_config(&self) -> Result<RemoteConfig> {
        let config_url = cached_version_info()
            .map(|info| info.config_url)
            .ok_or(ManagerError::InvalidVersionInfo)?;

        remote_config::get(self.ui, &config_url)
    }

    fn max_concurrent_downloads(&self) -> Result<usize> {
        let config = self.remote_config()?;

        Ok(usize::try_from(config.download.max_concurrent_downloads)
            .unwrap_or(usize::MAX)
            .clamp(1, MAX_PARALLEL_DOWNLOADS))
    }

    fn content_length(response: &ureq::http::Response<ureq::Body>) -> Option<u64> {
        response
            .headers()
            .get("Content-Length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
    }

    fn parse_share_code_from_url(url: &str) -> Option<String> {
        let path = url.split(['?', '#']).next()?;
        let (_, code) = path.trim_end_matches('/').rsplit_once("/share/")?;

        (!code.is_empty() && !code.contains('/')).then(|| code.to_string())
    }

    fn try_fetch_version_info(&self) -> Result<VersionInfo> {
        self.ui.emit(UiEvent::DownloadVersionInfoStart)?;

        let response = self
            .metadata_agent(VERSION_API)
            .get(VERSION_API)
            .call()
            .map_err(|e| {
                let msg = Self::convert_ureq_error(&e);
                let _ = self.ui.emit(UiEvent::DownloadVersionInfoFailed(&msg));
                ManagerError::NetworkError(msg)
            })?;

        if let Some(err) = check_response_status(&response, self.ui, "获取版本信息") {
            let _ = self
                .ui
                .emit(UiEvent::DownloadVersionInfoFailed(&err.to_string()));
            return Err(err);
        }

        let text = response
            .into_body()
            .read_to_string()
            .map_err(|e| ManagerError::NetworkError(format!("读取响应失败：{e}")))?;

        let mut vi: VersionInfo = serde_json::from_str(&text).map_err(|e| {
            let snippet: String = text.chars().take(200).collect();

            let _ = self.ui.emit(UiEvent::DownloadVersionInfoParseFailed(
                &format!("{e}"),
                &snippet,
            ));
            report_event(
                "Download.VersionInfo.ParseFailed",
                Some(&format!("err={e};snippet={snippet}")),
            );

            ManagerError::Other(format!("版本信息格式不符合预期：{e}"))
        })?;

        vi.normalize_versions();
        vi.validate()?;

        self.ui.emit(UiEvent::DownloadVersionInfoSuccess)?;
        report_event("Download.VersionInfo.Success", Some(&vi.to_string()));

        Ok(vi)
    }

    /// 获取自更新入口短链最终跳转到的分享码。
    fn fetch_share_code(&self, redirect_url: &str) -> Result<String> {
        self.retry("获取下载链接", || {
            self.try_fetch_share_code(redirect_url)
        })
    }

    fn try_fetch_share_code(&self, redirect_url: &str) -> Result<String> {
        self.ui.emit(UiEvent::DownloadShareCodeStart)?;

        let response = self
            .metadata_agent(redirect_url)
            .get(redirect_url)
            .call()
            .map_err(|e| {
                let msg = Self::convert_ureq_error(&e);
                let _ = self.ui.emit(UiEvent::DownloadShareCodeFailed(&msg));
                ManagerError::NetworkError(msg)
            })?;

        if let Some(err) = check_response_status(&response, self.ui, "获取下载链接") {
            let _ = self
                .ui
                .emit(UiEvent::DownloadShareCodeFailed(&err.to_string()));
            return Err(err);
        }

        let final_uri = response.get_uri().to_string();
        if let Some(code) = Self::parse_share_code_from_url(&final_uri) {
            self.ui.emit(UiEvent::DownloadShareCodeSuccess)?;
            report_event("Download.ShareCode.Success", Some(&code));
            Ok(code)
        } else {
            report_event(
                "Download.ShareCode.ParseFailed",
                Some(&format!("final_uri={final_uri}")),
            );
            Err(ManagerError::NetworkError(
                "下载链接未跳转到分享页，请稍后重试".to_string(),
            ))
        }
    }
}
