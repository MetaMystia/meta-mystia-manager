//! 界面与后台流程之间的桥。
//!
//! 后台线程通过 [`GuiUi`] 调用 `Ui` trait，这里把调用转换成窗口事件（`PostMessage`）；
//! 需要用户拍板的问题则把请求排进队列，由界面线程弹模态框后回传结果。

use crate::config::{OperationMode, UninstallMode};
use crate::downloader::Downloader;
use crate::env_check::check_game_directory;
use crate::error::Result;
use crate::flow::self_update;
use crate::installer::bepinex_console_enabled;
use crate::metrics::report_event;
use crate::model::VersionInfo;
use crate::remote_config;
use crate::shutdown::run_shutdown;
use crate::ui::Ui;
use crate::upgrader::Upgrader;

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

/// 工作线程有事件时发给主窗口的消息
pub const WM_UI_EVENT: u32 = 0x8000 + 1;
/// 进度页上的组件槽位数（与三行进度条一一对应）
pub const JOB_SLOTS: usize = 3;
/// 速度采样间隔：到点才更新一次平滑速度，避免每块都抖动
const SPEED_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
/// 单个下载任务的最新进度：槽位、已下载字节、平滑速度（字节/秒）
pub type JobProgress = (usize, u64, f64);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Cleanup,
    Deploy,
    Download,
    Login,
}

pub enum Event {
    Confirm(ConfirmRequest),
    /// 操作结束：`None` 表示成功，`Some` 是错误信息
    Done(Option<String>),
    Error(String),
    JobFinish {
        message: String,
        slot: usize,
    },
    JobPlan(Vec<(usize, String)>),
    JobStart {
        name: String,
        slot: usize,
        total: Option<u64>,
    },
    Log(String),
    Notes {
        notes: Option<(String, String, String)>,
        version: String,
    },
    Prefetch(Box<PrefetchOutcome>),
    /// 管理工具自升级未完成：`None` 表示没有执行替换（平台跳过或版本已一致），`Some` 是错误信息
    SelfUpdate(Option<String>),
    Stage(Stage),
}

/// 需要界面弹框确认的请求；后台线程会阻塞等待回复
pub struct ConfirmRequest {
    pub cancel_label: String,
    pub confirm_label: String,
    pub content: String,
    pub instruction: String,
    pub reply: Sender<bool>,
}

#[derive(Clone, Default)]
pub struct LocalInfo {
    pub bepinex_console: bool,
    pub bepinex_version: Option<String>,
    pub dll_version: Option<String>,
    pub game_root: Option<PathBuf>,
    pub resourceex_version: Option<String>,
}

pub struct PrefetchOutcome {
    pub local: LocalInfo,
    pub remote: Result<Prefetched>,
}

pub struct Prefetched {
    pub manager_update: Option<String>,
    pub release_notes: Option<(String, String, String)>,
    pub version_info: VersionInfo,
}

#[derive(Clone, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "每个开关对应界面上的一个勾选项"
)]
pub struct Choices {
    pub dll_version: Option<String>,
    pub game_root: PathBuf,
    pub install_resourceex: bool,
    pub operation: Option<OperationMode>,
    pub resourceex_version: Option<String>,
    pub show_bepinex_console: bool,
    pub uninstall_full: bool,
    pub upgrade_dll: bool,
}

pub struct GuiUi {
    cancelled: AtomicBool,
    choices: Mutex<Choices>,
    diagnostics_path: Mutex<Option<String>>,
    download_slots: Mutex<HashMap<usize, usize>>,
    download_speeds: Mutex<HashMap<usize, SpeedTracker>>,
    events: Mutex<Vec<Event>>,
    hwnd: AtomicIsize,
    next_download_id: AtomicUsize,
    paused: AtomicBool,
    progress: Mutex<Vec<JobProgress>>,
    progress_posted: AtomicBool,
}

struct SpeedTracker {
    bytes: u64,
    sampled_at: Instant,
    speed: f64,
}

impl GuiUi {
    pub fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            choices: Mutex::new(Choices::default()),
            diagnostics_path: Mutex::new(None),
            download_slots: Mutex::new(HashMap::new()),
            download_speeds: Mutex::new(HashMap::new()),
            events: Mutex::new(Vec::new()),
            hwnd: AtomicIsize::new(0),
            next_download_id: AtomicUsize::new(1),
            paused: AtomicBool::new(false),
            progress: Mutex::new(Vec::new()),
            progress_posted: AtomicBool::new(false),
        }
    }

    /// 主窗口创建后登记句柄
    pub fn attach(&self, hwnd: HWND) {
        self.hwnd.store(hwnd as isize, Ordering::Relaxed);
    }

    pub fn set_choices(&self, choices: Choices) {
        *self.choices.lock().unwrap_or_else(PoisonError::into_inner) = choices;
    }

    pub fn choices(&self) -> Choices {
        self.choices
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set_cancelled(&self, cancelled: bool) {
        self.cancelled.store(cancelled, Ordering::Relaxed);
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// 用户是否在下载过程中点了停止
    pub fn was_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// 最近一次导出的诊断包路径
    pub fn diagnostics_path(&self) -> Option<String> {
        self.diagnostics_path
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn take_events(&self) -> Vec<Event> {
        mem::take(&mut *self.events.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// 取出累计的下载进度；同时复位“有新进度”标记
    pub fn take_progress(&self) -> Vec<JobProgress> {
        self.progress_posted.store(false, Ordering::Relaxed);
        mem::take(&mut *self.progress.lock().unwrap_or_else(PoisonError::into_inner))
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

    /// 供后台线程投递事件（等同内部 `push`）
    pub fn push_event(&self, event: Event) {
        self.push(event);
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

    fn slot_for(filename: &str) -> usize {
        let lower = filename.to_ascii_lowercase();

        if lower.contains("bepinex") {
            0
        } else if lower.contains("metamystia") {
            1
        } else if lower.contains("resourceexample") {
            2
        } else {
            0
        }
    }
}

pub fn prefetch(ui: &GuiUi) -> PrefetchOutcome {
    let local = detect_local(ui);
    let remote = fetch_remote(ui);

    PrefetchOutcome { local, remote }
}

pub fn detect_local(ui: &GuiUi) -> LocalInfo {
    match check_game_directory(ui) {
        Ok(root) => detect_local_at(ui, root),
        Err(e) => {
            ui.log(format!("未自动找到游戏目录：{e}"));
            LocalInfo::default()
        }
    }
}

pub fn detect_local_at(ui: &GuiUi, root: PathBuf) -> LocalInfo {
    let upgrader = Upgrader::new(root.clone(), ui);
    let (dll_version, resourceex_version) =
        upgrader.get_installed_versions().unwrap_or((None, None));

    LocalInfo {
        bepinex_console: bepinex_console_enabled(&root),
        bepinex_version: upgrader.read_bepinex_version(),
        dll_version,
        game_root: Some(root),
        resourceex_version,
    }
}

fn fetch_remote(ui: &GuiUi) -> Result<Prefetched> {
    let downloader = Downloader::new(ui);
    let version_info = downloader.get_version_info()?;

    remote_config::get(ui, &version_info.config_url)?;

    let release_notes = downloader
        .get_github_release_notes(version_info.latest_dll().ok())
        .ok()
        .flatten();
    let manager_update =
        (version_info.manager != env!("CARGO_PKG_VERSION")).then(|| version_info.manager.clone());

    Ok(Prefetched {
        manager_update,
        release_notes,
        version_info,
    })
}

/// 自升级：下载新版本并交给替换脚本；替换成功后本进程退出，由脚本启动新版本
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

/// 拉取指定版本的发行说明（切换安装版本时用）
pub fn fetch_release_notes(ui: &GuiUi, version: String) {
    let downloader = Downloader::new(ui);

    let notes = downloader.get_version_info().ok().and_then(|_| {
        downloader
            .get_github_release_notes(Some(&version))
            .ok()
            .flatten()
    });

    ui.push_event(Event::Notes { notes, version });
}

impl Ui for GuiUi {
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

    fn install_warn_existing(
        &self,
        bepinex_installed: bool,
        metamystia_installed: bool,
        resourceex_installed: bool,
    ) -> Result<()> {
        let mut items = Vec::new();

        if bepinex_installed {
            items.push("BepInEx");
        }
        if metamystia_installed {
            items.push("MetaMystia");
        }
        if resourceex_installed {
            items.push("ResourceExample");
        }

        self.log(format!("检测到已安装：{}", items.join("、")));
        Ok(())
    }

    fn install_confirm_overwrite(&self) -> Result<bool> {
        let confirmed = self.confirm(
            "覆盖安装？",
            "检测到已安装的组件，继续会先备份再覆盖同名文件。",
            "继续安装",
            "取消",
        );
        report_event("UI.Install.Confirm", Some(yes_no(confirmed)));

        Ok(confirmed)
    }

    fn install_ask_install_resourceex(&self) -> Result<bool> {
        Ok(self.choices().install_resourceex)
    }

    fn install_ask_show_bepinex_console(&self) -> Result<bool> {
        Ok(self.choices().show_bepinex_console)
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
        self.log("所有组件都是最新版本");
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

    fn uninstall_select_mode(&self) -> Result<UninstallMode> {
        Ok(if self.choices().uninstall_full {
            UninstallMode::Full
        } else {
            UninstallMode::Light
        })
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

    fn uninstall_confirm_deletion(&self) -> Result<bool> {
        let confirmed = self.confirm(
            "确认卸载？",
            "删除后无法恢复，游戏本体文件不会被修改。",
            "开始卸载",
            "取消",
        );
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

    fn download_start(&self, filename: &str, total: Option<u64>) -> Result<usize> {
        let id = self.next_download_id.fetch_add(1, Ordering::Relaxed);
        let slot = Self::slot_for(filename);
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
        let slot = self
            .download_slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .copied()
            .unwrap_or(0);

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

        self.progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((slot, downloaded, speed));

        if !self.progress_posted.swap(true, Ordering::Relaxed) {
            self.post();
        }

        Ok(())
    }

    fn download_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    fn download_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    fn download_plan(&self, names: &[&str]) -> Result<()> {
        let items = names
            .iter()
            .map(|name| {
                let slot = Self::slot_for(name);
                let label = name.trim_start_matches("下载 ").trim_end_matches(" DLL");

                (slot, label.to_string())
            })
            .collect();

        self.push(Event::JobPlan(items));
        Ok(())
    }

    fn download_finish(&self, id: usize, message: &str) -> Result<()> {
        let slot = self
            .download_slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .copied()
            .unwrap_or(0);
        self.download_speeds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        self.push(Event::JobFinish {
            message: message.to_string(),
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

    fn download_ask_continue_after_release_notes(&self) -> Result<bool> {
        Ok(true)
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
            "{op_desc}失败：{err}；{delay_secs} 秒后重试（{attempt}/{attempts}）"
        ));
        Ok(())
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

    fn select_version_ask_select(&self, component: &str) -> Result<bool> {
        let choices = self.choices();

        Ok(if component.contains("ResourceEx") {
            choices.resourceex_version.is_some()
        } else {
            choices.dll_version.is_some()
        })
    }

    fn select_version_from_list(&self, component: &str, versions: &[String]) -> Result<usize> {
        let choices = self.choices();
        let selected = if component.contains("ResourceEx") {
            choices.resourceex_version.as_deref()
        } else {
            choices.dll_version.as_deref()
        };

        Ok(selected
            .and_then(|version| {
                versions.iter().position(|item| {
                    VersionInfo::normalize_version(item) == VersionInfo::normalize_version(version)
                })
            })
            .unwrap_or(0))
    }

    fn sso_ask_open_browser(&self) -> Result<bool> {
        self.stage(Stage::Login);
        report_event("UI.Sso.OpenBrowser.Confirm", Some("yes"));

        Ok(true)
    }

    fn diagnostics_confirm_export(&self, entries: &[String]) -> Result<bool> {
        let confirmed = self.confirm("导出诊断包？", &entries.join("\r\n"), "开始导出", "取消");
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

/// 埋点用的“是/否”
pub const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
