//! 界面适配层：把后台流程对 `Ui` trait 的调用转换为窗口事件（`PostMessage`）。
//! 需要用户确认的请求会排队，由界面线程弹出模态框后回传结果。

use crate::config::BEPINEX_CORE_DLL;
use crate::env::{check_game_directory, check_game_running_cached};
use crate::error::Result;
use crate::mode::{OperationMode, UninstallMode};
use crate::net::downloader::{Downloader, MAX_PARALLEL_DOWNLOADS};
use crate::net::remote_config;
use crate::ops::flow::self_update;
use crate::ops::installer::is_bepinex_console_enabled;
use crate::ops::rollback;
use crate::ops::toggle::doorstop_enabled;
use crate::ops::upgrader::Upgrader;
use crate::shutdown::run_shutdown;
use crate::telemetry::report_event;
use crate::ui::{JobOutcome, Ui, UiEvent, UiReply};
use crate::version::VersionInfo;

use std::{
    collections::HashMap,
    mem,
    path::{Path, PathBuf},
    process,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering},
        mpsc::{Sender, channel},
    },
    time::{Duration, Instant},
};

use windows_sys::Win32::{Foundation::HWND, UI::WindowsAndMessaging::PostMessageW};

// 窗口消息与进度槽位
/// 进度页上的组件槽位数；下载并发也受这个上限约束。
pub const JOB_SLOTS: usize = MAX_PARALLEL_DOWNLOADS;
/// 自定义窗口消息：后台线程用它通知界面刷新。
pub const WM_UI_EVENT: u32 = 0x8000 + 1;

// 进度采样
/// 速度采样间隔：到点才更新一次平滑速度，避免每块都抖动。
const SPEED_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
/// 单个下载任务的最新进度：槽位、已下载字节、平滑速度（字节/秒）。
pub type JobProgress = (usize, u64, f64);

/// 向导当前所处的阶段。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// 清理旧组件
    Cleanup,
    /// 部署组件
    Deploy,
    /// 下载
    Download,
    /// 登录
    Login,
}

/// 后台线程发给界面的事件。
pub enum Event {
    /// 请求弹框确认
    Confirm(ConfirmRequest),
    /// 操作结束：`None` 表示成功，`Some` 是错误信息
    Done(Option<String>),
    /// 展示错误信息
    Error(String),
    /// 单个下载任务结束
    JobFinish {
        /// 展示文本
        message: String,
        /// 任务结果
        outcome: JobOutcome,
        /// 进度槽位
        slot: usize,
    },
    /// 本次下载计划（槽位与名称）
    JobPlan(Vec<(usize, String)>),
    /// 单个下载任务开始
    JobStart {
        /// 文件说明
        name: String,
        /// 进度槽位
        slot: usize,
        /// 总大小（未知时为 `None`）。
        total: Option<u64>,
    },
    /// 追加一行日志
    Log(String),
    /// 本地环境探测结果
    Local(LocalInfo),
    /// 启用 / 禁用完成（BepInEx、MetaMystia、ResourceExample 的启用状态）
    ManageCompleted {
        bepinex: bool,
        dll: bool,
        resourceex: bool,
    },
    /// 没有可用更新
    NoUpdate,
    /// 版本更新说明（版本号与说明三元组）
    Notes {
        notes: Option<(String, String, String)>,
        version: String,
    },
    /// 远端版本信息获取结果
    Remote(Result<RemoteInfo>),
    /// 管理工具自升级未完成：`None` 表示没有执行替换（平台跳过或版本已一致），`Some` 是错误信息
    SelfUpdate(Option<String>),
    /// 向导阶段变化
    Stage(Stage),
}

/// 需要界面弹框确认的请求；后台线程会阻塞等待回复。
pub struct ConfirmRequest {
    /// 取消按钮文案
    pub cancel_label: String,
    /// 确认按钮文案
    pub confirm_label: String,
    /// 正文
    pub content: String,
    /// 说明文字
    pub instruction: String,
    /// 回传确认结果
    pub reply: Sender<bool>,
}

/// 本地环境探测结果。
#[derive(Clone, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "字段直接对应界面上的组件状态"
)]
pub struct LocalInfo {
    /// 是否启用 BepInEx 控制台
    pub bepinex_console: bool,
    /// BepInEx 是否被禁用（`doorstop_config.ini` 的 `[General] enabled`）
    pub bepinex_disabled: bool,
    /// 是否安装 BepInEx
    pub bepinex_installed: bool,
    /// 已安装的 BepInEx 构建号
    pub bepinex_version: Option<String>,
    /// 探测过程是否失败
    pub detect_failed: bool,
    /// MetaMystia 是否被禁用
    pub dll_disabled: bool,
    /// 已安装的 MetaMystia 版本
    pub dll_version: Option<String>,
    /// 游戏目录
    pub game_root: Option<PathBuf>,
    /// ResourceExample 是否被禁用
    pub resourceex_disabled: bool,
    /// 已安装的 ResourceExample 版本
    pub resourceex_version: Option<String>,
}

/// 预取到的远端信息。
pub struct RemoteInfo {
    /// 可用的管理工具新版本
    pub manager_update: Option<String>,
    /// Release 说明（tag，标题，正文）
    pub release_notes: Option<(String, String, String)>,
    /// 远端版本信息
    pub version_info: VersionInfo,
}

/// 界面收集的用户选择。
#[derive(Clone, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "每个开关对应界面上的一个勾选项"
)]
pub struct Choices {
    /// 选定的 MetaMystia 版本
    pub dll_version: Option<String>,
    /// 游戏目录
    pub game_root: PathBuf,
    /// 是否安装 ResourceExample
    pub install_resourceex: bool,
    /// 启用 / 禁用操作：BepInEx 是否启用（`None` 表示不处理）
    pub manage_bepinex: Option<bool>,
    /// 启用 / 禁用操作：MetaMystia 是否启用（`None` 表示不处理）
    pub manage_dll: Option<bool>,
    /// 启用 / 禁用操作：ResourceExample 是否启用（`None` 表示不处理）
    pub manage_resourceex: Option<bool>,
    /// 选定的操作
    pub operation: Option<OperationMode>,
    /// 选定的 ResourceExample 版本
    pub resourceex_version: Option<String>,
    /// 是否显示 BepInEx 控制台
    pub show_bepinex_console: bool,
    /// 是否完全卸载
    pub uninstall_full: bool,
    /// 是否升级 BepInEx
    pub upgrade_bepinex: bool,
    /// 是否升级 MetaMystia DLL
    pub upgrade_dll: bool,
}

/// [`crate::ui::Ui`] 的 GUI 实现：缓存事件与进度，通过窗口消息唤醒界面。
pub struct GuiUi {
    aborted: AtomicBool,
    cancelled: AtomicBool,
    diagnostics_path: Mutex<Option<String>>,
    download_slots: Mutex<HashMap<usize, usize>>,
    download_speeds: Mutex<HashMap<usize, SpeedTracker>>,
    events: Mutex<Vec<Event>>,
    hwnd: AtomicIsize,
    next_download_id: AtomicUsize,
    paused: AtomicBool,
    progress: Mutex<[Option<JobProgress>; JOB_SLOTS]>,
    progress_posted: AtomicBool,
}

struct SpeedTracker {
    bytes: u64,
    sampled_at: Instant,
    speed: f64,
}

impl GuiUi {
    /// 创建空的界面后端。
    pub fn new() -> Self {
        Self {
            aborted: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            diagnostics_path: Mutex::new(None),
            download_slots: Mutex::new(HashMap::new()),
            download_speeds: Mutex::new(HashMap::new()),
            events: Mutex::new(Vec::new()),
            hwnd: AtomicIsize::new(0),
            next_download_id: AtomicUsize::new(1),
            paused: AtomicBool::new(false),
            progress: Mutex::new([None; JOB_SLOTS]),
            progress_posted: AtomicBool::new(false),
        }
    }

    /// 绑定主窗口句柄。
    pub fn attach(&self, hwnd: HWND) {
        self.hwnd.store(hwnd as isize, Ordering::Relaxed);
    }

    /// 设置取消标记。
    pub fn set_cancelled(&self, cancelled: bool) {
        self.cancelled.store(cancelled, Ordering::Relaxed);
    }

    /// 设置暂停标记。
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// 是否已被用户取消。
    pub fn was_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// 取最近一次诊断包路径。
    pub fn diagnostics_path(&self) -> Option<String> {
        self.diagnostics_path
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// 取出并清空缓存的界面事件。
    pub fn take_events(&self) -> Vec<Event> {
        mem::take(&mut *self.events.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// 取出累计的下载进度；同时复位“有新进度”标记。
    pub fn take_progress(&self) -> Vec<JobProgress> {
        self.progress_posted.store(false, Ordering::Relaxed);

        let mut progress = self.progress.lock().unwrap_or_else(PoisonError::into_inner);
        let mut pending = Vec::with_capacity(JOB_SLOTS);

        for slot in 0..JOB_SLOTS {
            if let Some(entry) = progress[slot].take() {
                pending.push(entry);
            }
        }

        pending
    }

    /// 供后台线程投递事件（等同内部 `push`）。
    pub fn push_event(&self, event: Event) {
        self.push(event);
    }

    fn post(&self) {
        let hwnd = self.hwnd.load(Ordering::Relaxed) as HWND;
        if !hwnd.is_null() {
            unsafe {
                PostMessageW(hwnd, WM_UI_EVENT, 0, 0);
            }
        }
    }

    fn push(&self, event: Event) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
        self.post();
    }

    fn log(&self, text: impl Into<String>) {
        self.push(Event::Log(text.into()));
    }

    fn error(&self, text: impl Into<String>) {
        self.push(Event::Error(text.into()));
    }

    fn stage(&self, stage: Stage) {
        self.push(Event::Stage(stage));
    }

    fn confirm(
        &self,
        instruction: &str,
        content: &str,
        confirm_label: &str,
        cancel_label: &str,
    ) -> bool {
        let (tx, rx) = channel();
        self.push(Event::Confirm(ConfirmRequest {
            cancel_label: cancel_label.to_string(),
            confirm_label: confirm_label.to_string(),
            content: content.to_string(),
            instruction: instruction.to_string(),
            reply: tx,
        }));

        rx.recv().unwrap_or(false)
    }
}

/// 启动时的本地探测（含上次未完成操作的自动恢复）。
pub fn prefetch_local(ui: &GuiUi) -> LocalInfo {
    ui.set_download_aborted(false);

    let mut local = detect_local(ui);

    if let Some(root) = local.game_root.clone() {
        match check_game_running_cached() {
            Ok(false) => match rollback::recover_interrupted(&root) {
                Ok(true) => {
                    ui.log("检测到上次安装/升级未完成，已自动恢复到操作前的状态");
                    local = detect_local_at(ui, root);
                }
                Ok(false) => {}
                Err(e) => ui.log(format!("自动恢复上次操作失败：{e}")),
            },
            Ok(true) => ui.log("游戏正在运行，暂不自动恢复上次未完成的操作"),
            Err(e) => ui.log(format!(
                "检查游戏进程失败，暂不自动恢复上次未完成的操作：{e}"
            )),
        }
    }

    local
}

/// 自动定位游戏目录并探测本地安装状态。
pub fn detect_local(ui: &GuiUi) -> LocalInfo {
    match check_game_directory(ui) {
        Ok(root) => detect_local_at(ui, root),
        Err(e) => {
            ui.log(format!("未自动找到游戏目录：{e}"));
            LocalInfo::default()
        }
    }
}

/// 探测指定游戏目录下的本地安装状态。
pub fn detect_local_at(ui: &GuiUi, root: PathBuf) -> LocalInfo {
    let upgrader = Upgrader::new(root.clone(), ui);
    let (dll_disabled, dll_version, resourceex_disabled, resourceex_version, detect_failed) =
        match upgrader.installed_versions() {
            Ok(installed) => {
                let dll_disabled = installed.dll.as_ref().is_some_and(|state| state.disabled);
                let dll_version = installed.dll.map(|state| state.version);
                let resourceex_disabled = installed
                    .resourceex
                    .as_ref()
                    .is_some_and(|state| state.disabled);
                let resourceex_version = installed.resourceex.map(|state| state.version);

                (
                    dll_disabled,
                    dll_version,
                    resourceex_disabled,
                    resourceex_version,
                    false,
                )
            }
            Err(e) => {
                ui.log(format!("检测已安装组件失败：{e}"));
                report_event("Env.DetectInstalled.Failed", Some(&format!("{e}")));

                (false, None, false, None, true)
            }
        };
    let bepinex_installed = root.join(BEPINEX_CORE_DLL).is_file();
    let bepinex_disabled = bepinex_installed && !doorstop_enabled(&root).unwrap_or(true);

    LocalInfo {
        bepinex_console: is_bepinex_console_enabled(&root),
        bepinex_disabled,
        bepinex_installed,
        bepinex_version: upgrader.read_bepinex_version(),
        detect_failed,
        dll_disabled,
        dll_version,
        game_root: Some(root),
        resourceex_disabled,
        resourceex_version,
    }
}

/// 拉取远端版本信息与发行说明。
pub fn fetch_remote(ui: &GuiUi) -> Result<RemoteInfo> {
    let downloader = Downloader::new(ui);
    let version_info = downloader.fetch_version_info()?;

    let config = remote_config::get(ui, &version_info.config_url)?;

    let release_notes = downloader
        .fetch_github_release_notes(version_info.latest_dll().ok())
        .ok()
        .flatten();
    let manager_update = config
        .self_update
        .as_ref()
        .and_then(|_| version_info.manager_version())
        .filter(|version| *version != env!("CARGO_PKG_VERSION"))
        .map(ToString::to_string);

    Ok(RemoteInfo {
        manager_update,
        release_notes,
        version_info,
    })
}

/// 自升级：下载新版本并交给替换脚本；替换成功后本进程退出，由脚本启动新版本。
pub fn run_self_update(ui: &GuiUi) {
    match self_update(ui) {
        Ok(false) => ui.push_event(Event::SelfUpdate(None)),
        Ok(true) => {
            run_shutdown();
            process::exit(0);
        }
        Err(e) => {
            ui.push_event(Event::SelfUpdate(Some(e.to_string())));
        }
    }
}

/// 拉取指定版本的发行说明（切换安装版本时用）。
pub fn fetch_release_notes(ui: &GuiUi, version: String) {
    ui.set_download_aborted(false);

    let downloader = Downloader::new(ui);

    let notes = downloader.fetch_version_info().ok().and_then(|_| {
        downloader
            .fetch_github_release_notes(Some(&version))
            .ok()
            .flatten()
    });

    ui.push_event(Event::Notes { notes, version });
}

impl Ui for GuiUi {
    #[allow(clippy::too_many_lines, reason = "全部界面事件的唯一分发点")]
    fn emit(&self, event: UiEvent<'_>) -> Result<UiReply> {
        match event {
            UiEvent::DisplayVersion(version) => {
                self.display_version(version)?;
            }
            UiEvent::GameRunningWarning => {
                self.display_game_running_warning()?;
            }
            UiEvent::Message(text) => {
                self.message(text)?;
            }
            UiEvent::Warn(text) => {
                self.warn(text)?;
            }
            UiEvent::SteamFound { app_id, name, path } => {
                self.path_display_steam_found(app_id, name, path)?;
            }
            UiEvent::InstallStep(step, description) => {
                self.install_display_step(step, description)?;
            }
            UiEvent::InstallVersionInfo(info) => {
                self.install_display_version_info(info)?;
            }
            UiEvent::InstallDownloadsCompleted => {
                self.install_downloads_completed()?;
            }
            UiEvent::InstallStartCleanup => {
                self.install_start_cleanup()?;
            }
            UiEvent::InstallCleanupResult(success, failed) => {
                self.install_cleanup_result(success, failed)?;
            }
            UiEvent::InstallFinished(show_console) => {
                self.install_finished(show_console)?;
            }
            UiEvent::UpgradeDeleted(path) => {
                self.upgrade_deleted(path)?;
            }
            UiEvent::UpgradeDeleteFailed(path, err) => {
                self.upgrade_delete_failed(path, err)?;
            }
            UiEvent::UpgradeCheckingInstalledVersion => {
                self.upgrade_checking_installed_version()?;
            }
            UiEvent::UpgradeDetectedResourceex => {
                self.upgrade_detected_resourceex()?;
            }
            UiEvent::UpgradeBepinexVersions(current, latest) => {
                self.upgrade_display_current_and_latest_bepinex(current, latest)?;
            }
            UiEvent::UpgradeDllVersions(current, latest) => {
                self.upgrade_display_current_and_latest_dll(current, latest)?;
            }
            UiEvent::UpgradeResourceexVersions(current, latest) => {
                self.upgrade_display_current_and_latest_resourceex(current, latest)?;
            }
            UiEvent::UpgradeNoUpdateNeeded => {
                self.upgrade_no_update_needed()?;
            }
            UiEvent::UpgradeBepinexNeedsUpgrade(installed) => {
                self.upgrade_bepinex_needs_upgrade(installed)?;
            }
            UiEvent::UpgradeBepinexAlreadyLatest => {
                self.upgrade_bepinex_already_latest()?;
            }
            UiEvent::UpgradeDllDetected(current, new) => {
                self.upgrade_detected_new_dll(current, new)?;
            }
            UiEvent::UpgradeDllAlreadyLatest => {
                self.upgrade_dll_already_latest()?;
            }
            UiEvent::UpgradeResourceexNeedsUpgrade(installed) => {
                self.upgrade_resourceex_needs_upgrade(installed)?;
            }
            UiEvent::UpgradeDownloadingBepinex => {
                self.upgrade_downloading_bepinex()?;
            }
            UiEvent::UpgradeDownloadingDll => {
                self.upgrade_downloading_dll()?;
            }
            UiEvent::UpgradeDownloadingResourceex => {
                self.upgrade_downloading_resourceex()?;
            }
            UiEvent::UpgradeInstallingBepinex => {
                self.upgrade_installing_bepinex()?;
            }
            UiEvent::UpgradeInstallingDll => {
                self.upgrade_installing_dll()?;
            }
            UiEvent::UpgradeInstallingResourceex => {
                self.upgrade_installing_resourceex()?;
            }
            UiEvent::UpgradeInstallSuccess(path) => {
                self.upgrade_install_success(path)?;
            }
            UiEvent::UpgradeCleanupStart => {
                self.upgrade_cleanup_start()?;
            }
            UiEvent::UpgradeDone => {
                self.upgrade_done()?;
            }
            UiEvent::UninstallNoFilesFound => {
                self.uninstall_no_files_found()?;
            }
            UiEvent::UninstallTargetFiles(files) => {
                self.uninstall_display_target_files(files)?;
            }
            UiEvent::UninstallConfirmDeletion(mode) => {
                return Ok(UiReply::Bool(self.uninstall_confirm_deletion(mode)?));
            }
            UiEvent::UninstallFilesInUse => {
                self.uninstall_files_in_use_warning()?;
            }
            UiEvent::UninstallWaitBeforeRetry(delay, attempt, attempts) => {
                self.uninstall_wait_before_retry(delay, attempt, attempts)?;
            }
            UiEvent::UninstallAskElevate => {
                return Ok(UiReply::Bool(self.uninstall_ask_elevate_permission()?));
            }
            UiEvent::UninstallRestartingElevated => {
                self.uninstall_restarting_elevated()?;
            }
            UiEvent::UninstallAskRetryFailures => {
                return Ok(UiReply::Bool(self.uninstall_ask_retry_failures()?));
            }
            UiEvent::UninstallRetryingFailedItems => {
                self.uninstall_retrying_failed_items()?;
            }
            UiEvent::ManageCompleted {
                bepinex,
                dll,
                resourceex,
            } => {
                self.push(Event::ManageCompleted {
                    bepinex,
                    dll,
                    resourceex,
                });
            }
            UiEvent::DiagnosticsConfirmExport(entries) => {
                return Ok(UiReply::Bool(self.diagnostics_confirm_export(entries)?));
            }
            UiEvent::DiagnosticsExported(path) => {
                self.diagnostics_exported(path)?;
            }
            UiEvent::DeletionStart => {
                self.deletion_start()?;
            }
            UiEvent::DeletionProgress(current, total, path) => {
                self.deletion_display_progress(current, total, path)?;
            }
            UiEvent::DeletionSuccess(path) => {
                self.deletion_display_success(path)?;
            }
            UiEvent::DeletionFailure(path, error) => {
                self.deletion_display_failure(path, error)?;
            }
            UiEvent::DeletionSkipped(path) => {
                self.deletion_display_skipped(path)?;
            }
            UiEvent::DeletionSummary(success, failed, skipped) => {
                self.deletion_display_summary(success, failed, skipped)?;
            }
            UiEvent::DownloadPlan(names) => {
                self.download_plan(names)?;
            }
            UiEvent::DownloadStart(slot, filename, total) => {
                return Ok(UiReply::DownloadId(
                    self.download_start(slot, filename, total)?,
                ));
            }
            UiEvent::DownloadUpdate(id, downloaded) => {
                self.download_update(id, downloaded)?;
            }
            UiEvent::DownloadFinish(id, message, outcome) => {
                self.download_finish(id, message, outcome)?;
            }
            UiEvent::DownloadVersionInfoStart => {
                self.download_version_info_start()?;
            }
            UiEvent::DownloadVersionInfoFailed(err) => {
                self.download_version_info_failed(err)?;
            }
            UiEvent::DownloadVersionInfoSuccess => {
                self.download_version_info_success()?;
            }
            UiEvent::DownloadVersionInfoParseFailed(err, snippet) => {
                self.download_version_info_parse_failed(err, snippet)?;
            }
            UiEvent::DownloadShareCodeStart => {
                self.download_share_code_start()?;
            }
            UiEvent::DownloadShareCodeFailed(err) => {
                self.download_share_code_failed(err)?;
            }
            UiEvent::DownloadShareCodeSuccess => {
                self.download_share_code_success()?;
            }
            UiEvent::DownloadAttemptGithubDll => {
                self.download_attempt_github_dll()?;
            }
            UiEvent::DownloadFoundGithubAsset(name) => {
                self.download_found_github_asset(name)?;
            }
            UiEvent::DownloadGithubDllNotFound => {
                self.download_github_dll_not_found()?;
            }
            UiEvent::DownloadReleaseNotes(tag, name, body) => {
                self.download_display_github_release_notes(tag, name, body)?;
            }
            UiEvent::DownloadSwitchToFallback(reason) => {
                self.download_switch_to_fallback(reason)?;
            }
            UiEvent::DownloadTryFallbackMetamystia => {
                self.download_try_fallback_metamystia()?;
            }
            UiEvent::DownloadBepinexAttemptPrimary => {
                self.download_bepinex_attempt_primary()?;
            }
            UiEvent::DownloadBepinexPrimaryFailed(err) => {
                self.download_bepinex_primary_failed(err)?;
            }
            UiEvent::NetworkRetrying(op_desc, delay, attempt, attempts, err) => {
                self.network_retrying(op_desc, delay, attempt, attempts, err)?;
            }
            UiEvent::NetworkRetryFailed(op_desc, attempts, err) => {
                self.network_retry_failed(op_desc, attempts, err);
            }
            UiEvent::NetworkRateLimited(secs) => {
                self.network_rate_limited(secs)?;
            }
            UiEvent::ManagerUpdateStarting => {
                self.manager_update_starting()?;
            }
            UiEvent::ManagerUpdateFailed(err) => {
                self.manager_update_failed(err)?;
            }
            UiEvent::ManagerPromptManualUpdate => {
                self.manager_prompt_manual_update()?;
            }
            UiEvent::SsoAskOpenBrowser => {
                return Ok(UiReply::Bool(self.sso_ask_open_browser()?));
            }
        }

        Ok(UiReply::Ack)
    }

    fn is_download_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    fn is_download_aborted(&self) -> bool {
        self.aborted.load(Ordering::Relaxed)
    }

    fn set_download_aborted(&self, aborted: bool) {
        self.aborted.store(aborted, Ordering::Relaxed);
    }

    fn is_download_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
}

/// 把布尔值转成埋点用的 `yes`/`no`。
pub const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[allow(
    clippy::missing_const_for_fn,
    clippy::unnecessary_wraps,
    clippy::unused_self,
    reason = "事件方法保持统一签名，便于 emit 分发"
)]
impl GuiUi {
    fn display_version(&self, manager_version: Option<&str>) -> Result<()> {
        if let Some(version) = manager_version {
            self.log(format!("管理工具最新版本：v{version}"));
        }

        Ok(())
    }

    fn display_game_running_warning(&self) -> Result<()> {
        self.error("检测到游戏正在运行，请先退出游戏再重试。");
        Ok(())
    }

    fn message(&self, text: &str) -> Result<()> {
        self.log(text);
        Ok(())
    }

    fn warn(&self, text: &str) -> Result<()> {
        self.log(text);
        Ok(())
    }

    fn path_display_steam_found(&self, app_id: u32, name: Option<&str>, path: &Path) -> Result<()> {
        self.log(format!(
            "检测到 Steam 安装：{}（AppID {}，{}）",
            path.display(),
            app_id,
            name.unwrap_or("未知名称")
        ));
        Ok(())
    }

    fn install_display_step(&self, step: usize, description: &str) -> Result<()> {
        match step {
            2 => self.stage(Stage::Download),
            3 => self.stage(Stage::Deploy),
            _ => {}
        }

        self.log(format!("第 {step} 步：{description}"));
        Ok(())
    }

    fn install_display_version_info(&self, version_info: &VersionInfo) -> Result<()> {
        self.log(format!("将安装：{version_info}"));
        Ok(())
    }

    fn install_downloads_completed(&self) -> Result<()> {
        Ok(())
    }

    fn install_start_cleanup(&self) -> Result<()> {
        self.stage(Stage::Cleanup);
        Ok(())
    }

    fn install_cleanup_result(&self, success_count: usize, failed_count: usize) -> Result<()> {
        self.log(format!(
            "清理旧文件：成功 {success_count}，失败 {failed_count}"
        ));
        Ok(())
    }

    fn install_finished(&self, show_bepinex_console: bool) -> Result<()> {
        if show_bepinex_console {
            self.log("已开启 BepInEx 日志窗口");
        }
        Ok(())
    }

    fn upgrade_deleted(&self, path: &Path) -> Result<()> {
        self.log(format!("已删除旧文件：{}", path.display()));
        Ok(())
    }

    fn upgrade_delete_failed(&self, path: &Path, err: &str) -> Result<()> {
        self.log(format!("删除失败：{}（{err}）", path.display()));
        Ok(())
    }

    fn upgrade_checking_installed_version(&self) -> Result<()> {
        self.log("正在检查已安装版本…");
        Ok(())
    }

    fn upgrade_detected_resourceex(&self) -> Result<()> {
        self.log("检测到已安装 ResourceExample");
        Ok(())
    }

    fn upgrade_display_current_and_latest_bepinex(
        &self,
        current: &str,
        latest: &str,
    ) -> Result<()> {
        self.log(format!("BepInEx：{current} → {latest}"));
        Ok(())
    }

    fn upgrade_display_current_and_latest_dll(&self, current: &str, latest: &str) -> Result<()> {
        self.log(format!("MetaMystia：{current} → {latest}"));
        Ok(())
    }

    fn upgrade_display_current_and_latest_resourceex(
        &self,
        current: &str,
        latest: &str,
    ) -> Result<()> {
        self.log(format!("ResourceExample：{current} → {latest}"));
        Ok(())
    }

    fn upgrade_no_update_needed(&self) -> Result<()> {
        self.log("没有需要执行的组件更新");
        self.push(Event::NoUpdate);
        Ok(())
    }

    fn upgrade_bepinex_needs_upgrade(&self, installed: bool) -> Result<()> {
        self.log(if installed {
            "将升级 BepInEx"
        } else {
            "将安装 BepInEx"
        });
        Ok(())
    }

    fn upgrade_bepinex_already_latest(&self) -> Result<()> {
        self.log("BepInEx 已是最新版本");
        Ok(())
    }

    fn upgrade_detected_new_dll(&self, current: &str, new: &str) -> Result<()> {
        self.log(format!("将升级 MetaMystia：{current} → {new}"));
        Ok(())
    }

    fn upgrade_dll_already_latest(&self) -> Result<()> {
        self.log("MetaMystia 已是最新版本");
        Ok(())
    }

    fn upgrade_resourceex_needs_upgrade(&self, installed: bool) -> Result<()> {
        self.log(if installed {
            "将升级 ResourceExample"
        } else {
            "将安装 ResourceExample"
        });
        Ok(())
    }

    fn upgrade_downloading_bepinex(&self) -> Result<()> {
        self.stage(Stage::Download);
        Ok(())
    }

    fn upgrade_downloading_dll(&self) -> Result<()> {
        self.stage(Stage::Download);
        Ok(())
    }

    fn upgrade_downloading_resourceex(&self) -> Result<()> {
        self.stage(Stage::Download);
        Ok(())
    }

    fn upgrade_installing_bepinex(&self) -> Result<()> {
        self.stage(Stage::Deploy);
        Ok(())
    }

    fn upgrade_installing_dll(&self) -> Result<()> {
        self.stage(Stage::Deploy);
        Ok(())
    }

    fn upgrade_installing_resourceex(&self) -> Result<()> {
        self.stage(Stage::Deploy);
        Ok(())
    }

    fn upgrade_install_success(&self, path: &Path) -> Result<()> {
        self.log(format!("已安装：{}", path.display()));
        Ok(())
    }

    fn upgrade_cleanup_start(&self) -> Result<()> {
        self.stage(Stage::Cleanup);
        Ok(())
    }

    fn upgrade_done(&self) -> Result<()> {
        self.log("升级完成");
        Ok(())
    }

    fn uninstall_no_files_found(&self) -> Result<()> {
        self.log("没有找到需要删除的 Mod 文件");
        Ok(())
    }

    fn uninstall_display_target_files(&self, files: &[PathBuf]) -> Result<()> {
        self.log(format!("待删除 {} 个文件/目录", files.len()));
        for file in files {
            self.log(format!("  · {}", file.display()));
        }
        Ok(())
    }

    fn uninstall_confirm_deletion(&self, mode: UninstallMode) -> Result<bool> {
        let content = match mode {
            UninstallMode::Full => {
                "删除 BepInEx、其他 Mod 与配置文件；游戏本体文件保持不变。删除后无法恢复。"
            }
            UninstallMode::Light => {
                "只删除 MetaMystia 相关文件；BepInEx 与其他 Mod 保持不变。删除后无法恢复。"
            }
        };
        let confirmed = self.confirm("确认卸载？", content, "开始卸载", "取消");
        report_event("UI.Uninstall.Confirm.Choice", Some(yes_no(confirmed)));

        Ok(confirmed)
    }

    fn uninstall_files_in_use_warning(&self) -> Result<()> {
        self.error("有文件被占用，请关闭游戏与相关程序后重试。");
        Ok(())
    }

    fn uninstall_wait_before_retry(
        &self,
        delay_secs: u64,
        attempt: usize,
        attempts: usize,
    ) -> Result<()> {
        self.log(format!(
            "文件仍被占用，{delay_secs} 秒后重试（{attempt}/{attempts}）"
        ));
        Ok(())
    }

    fn uninstall_ask_elevate_permission(&self) -> Result<bool> {
        let confirmed = self.confirm(
            "需要管理员权限",
            "删除这些文件需要管理员权限，是否以管理员身份重新运行？",
            "以管理员运行",
            "取消",
        );
        report_event("UI.Uninstall.Elevate.Choice", Some(yes_no(confirmed)));

        Ok(confirmed)
    }

    fn uninstall_restarting_elevated(&self) -> Result<()> {
        self.log("正在以管理员身份重新启动…");
        Ok(())
    }

    fn uninstall_ask_retry_failures(&self) -> Result<bool> {
        let retry = self.confirm(
            "重试失败项？",
            "部分文件删除失败，通常是被游戏或杀毒软件占用。",
            "重试",
            "跳过",
        );
        report_event("UI.Uninstall.Retry.Choice", Some(yes_no(retry)));

        Ok(retry)
    }

    fn uninstall_retrying_failed_items(&self) -> Result<()> {
        self.log("正在重试删除失败的文件…");
        Ok(())
    }

    fn deletion_start(&self) -> Result<()> {
        self.log("开始删除文件…");
        Ok(())
    }

    fn deletion_display_progress(&self, current: usize, total: usize, path: &str) -> Result<()> {
        self.log(format!("[{current}/{total}] {path}"));
        Ok(())
    }

    fn deletion_display_success(&self, path: &str) -> Result<()> {
        self.log(format!("已删除：{path}"));
        Ok(())
    }

    fn deletion_display_failure(&self, path: &str, error: &str) -> Result<()> {
        self.log(format!("删除失败：{path}（{error}）"));
        Ok(())
    }

    fn deletion_display_skipped(&self, path: &str) -> Result<()> {
        self.log(format!("已跳过：{path}"));
        Ok(())
    }

    fn deletion_display_summary(
        &self,
        success_count: usize,
        failed_count: usize,
        skipped_count: usize,
    ) -> Result<()> {
        self.log(format!(
            "删除结果：成功 {success_count}，失败 {failed_count}，跳过 {skipped_count}"
        ));
        Ok(())
    }

    fn download_start(&self, slot: usize, filename: &str, total: Option<u64>) -> Result<usize> {
        let id = self.next_download_id.fetch_add(1, Ordering::Relaxed);
        self.download_slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, slot);

        self.push(Event::JobStart {
            name: filename.to_string(),
            slot,
            total,
        });

        Ok(id)
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "采样区间内的字节数远小于 f64 的 2^53 精度上限"
    )]
    fn download_update(&self, id: usize, downloaded: u64) -> Result<()> {
        let Some(slot) = self
            .download_slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .copied()
        else {
            return Ok(());
        };

        let speed = {
            let mut tracks = self
                .download_speeds
                .lock()
                .unwrap_or_else(PoisonError::into_inner);

            if let Some(track) = tracks.get_mut(&id) {
                let now = Instant::now();
                let elapsed = now.duration_since(track.sampled_at).as_secs_f64();

                if elapsed >= SPEED_SAMPLE_INTERVAL.as_secs_f64() {
                    let instant = downloaded.saturating_sub(track.bytes) as f64 / elapsed;
                    track.speed = if track.speed == 0.0 {
                        instant
                    } else {
                        track.speed.mul_add(0.6, instant * 0.4)
                    };
                    track.bytes = downloaded;
                    track.sampled_at = now;
                }

                track.speed
            } else {
                tracks.insert(
                    id,
                    SpeedTracker {
                        bytes: downloaded,
                        sampled_at: Instant::now(),
                        speed: 0.0,
                    },
                );

                0.0
            }
        };

        if slot < JOB_SLOTS {
            self.progress.lock().unwrap_or_else(PoisonError::into_inner)[slot] =
                Some((slot, downloaded, speed));
        }

        if !self.progress_posted.swap(true, Ordering::Relaxed) {
            self.post();
        }

        Ok(())
    }

    fn download_plan(&self, names: &[&str]) -> Result<()> {
        let items: Vec<(usize, String)> = names
            .iter()
            .enumerate()
            .map(|(slot, name)| {
                let label = name.trim_start_matches("下载 ").trim_end_matches(" DLL");

                (slot, label.to_string())
            })
            .collect();

        self.push(Event::JobPlan(items));
        Ok(())
    }

    fn download_finish(&self, id: usize, message: &str, outcome: JobOutcome) -> Result<()> {
        let slot = self
            .download_slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id)
            .unwrap_or(0);
        self.download_speeds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        self.push(Event::JobFinish {
            message: message.to_string(),
            outcome,
            slot,
        });
        Ok(())
    }

    fn download_version_info_start(&self) -> Result<()> {
        Ok(())
    }

    fn download_version_info_failed(&self, err: &str) -> Result<()> {
        report_event("Download.VersionInfo.Failed", Some(err));
        Ok(())
    }

    fn download_version_info_success(&self) -> Result<()> {
        Ok(())
    }

    fn download_version_info_parse_failed(&self, err: &str, snippet: &str) -> Result<()> {
        self.log(format!("版本信息解析失败：{err}（{snippet}）"));
        Ok(())
    }

    fn download_share_code_start(&self) -> Result<()> {
        Ok(())
    }

    fn download_share_code_failed(&self, err: &str) -> Result<()> {
        self.log(format!("获取分享码失败：{err}"));
        Ok(())
    }

    fn download_share_code_success(&self) -> Result<()> {
        Ok(())
    }

    fn download_attempt_github_dll(&self) -> Result<()> {
        self.log("尝试从 GitHub 下载 MetaMystia…");
        Ok(())
    }

    fn download_found_github_asset(&self, name: &str) -> Result<()> {
        self.log(format!("找到文件：{name}"));
        Ok(())
    }

    fn download_github_dll_not_found(&self) -> Result<()> {
        self.log("GitHub 上未找到对应版本，改用备用源");
        Ok(())
    }

    fn download_display_github_release_notes(
        &self,
        tag: &str,
        name: &str,
        body: &str,
    ) -> Result<()> {
        self.log(format!("发行说明 {tag} {name}"));
        for line in body.lines() {
            self.log(line);
        }
        Ok(())
    }

    fn download_switch_to_fallback(&self, reason: &str) -> Result<()> {
        self.log(format!("切换备用源：{reason}"));
        Ok(())
    }

    fn download_try_fallback_metamystia(&self) -> Result<()> {
        self.log("尝试备用源下载 MetaMystia…");
        Ok(())
    }

    fn download_bepinex_attempt_primary(&self) -> Result<()> {
        Ok(())
    }

    fn download_bepinex_primary_failed(&self, err: &str) -> Result<()> {
        self.log(format!("BepInEx 主源失败：{err}"));
        Ok(())
    }

    fn network_retrying(
        &self,
        op_desc: &str,
        delay_secs: u64,
        attempt: usize,
        attempts: usize,
        err: &str,
    ) -> Result<()> {
        self.log(format!(
            "{op_desc} 失败：{err}；{delay_secs} 秒后重试（{attempt}/{attempts}）"
        ));
        Ok(())
    }

    fn network_retry_failed(&self, op_desc: &str, attempts: usize, err: &str) {
        self.log(format!("{op_desc} 失败：{err}；已尝试 {attempts} 次"));
    }

    fn network_rate_limited(&self, secs: u64) -> Result<()> {
        self.log(format!("请求过于频繁，等待 {secs} 秒后重试"));
        Ok(())
    }

    fn manager_update_starting(&self) -> Result<()> {
        self.log("正在更新管理工具…");
        Ok(())
    }

    fn manager_update_failed(&self, err: &str) -> Result<()> {
        self.error(format!("管理工具更新失败：{err}"));
        Ok(())
    }

    fn manager_prompt_manual_update(&self) -> Result<()> {
        self.log("请手动下载最新版管理工具后重试");
        Ok(())
    }

    fn sso_ask_open_browser(&self) -> Result<bool> {
        let confirmed = self.confirm(
            "需要登录",
            "安装/更新需要登录东方夜雀食堂小助手账号，接下来会在浏览器中完成授权。",
            "打开浏览器",
            "取消",
        );
        report_event("UI.Sso.OpenBrowser.Confirm", Some(yes_no(confirmed)));

        if confirmed {
            self.stage(Stage::Login);
        }

        Ok(confirmed)
    }

    fn diagnostics_confirm_export(&self, entries: &[String]) -> Result<bool> {
        let content = format!(
            "将收集以下内容，不会修改游戏文件：\r\n{}",
            entries.join("\r\n")
        );
        let confirmed = self.confirm("确认导出诊断包？", &content, "开始导出", "取消");
        report_event("UI.Diagnostics.ConfirmExport", Some(yes_no(confirmed)));

        if !confirmed {
            self.set_cancelled(true);
        }

        Ok(confirmed)
    }

    fn diagnostics_exported(&self, path: &Path) -> Result<()> {
        *self
            .diagnostics_path
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(path.display().to_string());
        Ok(())
    }
}
