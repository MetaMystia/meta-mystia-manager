//! 安装 / 升级 / 卸载 / 诊断的执行流程。
//!
//! 界面负责收集用户选择并展示进度，这里按顺序调用底层能力；
//! 需要用户当场确认的少数问题仍然通过 [`Ui`] 回调界面。

use crate::diagnostics::export;
use crate::env::check_game_running_cached;
use crate::error::{ManagerError, Result};
use crate::mode::{OperationMode, UninstallMode};
use crate::net::downloader::Downloader;
use crate::net::sso::ensure_logged_in;
use crate::ops::installer::Installer;
use crate::ops::rollback;
use crate::ops::self_update::run_self_update;
use crate::ops::uninstaller::Uninstaller;
use crate::ops::upgrader::Upgrader;
use crate::platform::is_self_update_enabled;
use crate::telemetry::report_event;
use crate::ui::{Ui, UiEvent};

use std::{env, path::PathBuf};

/// 一次操作的输入：用户选择与游戏目录。
#[allow(
    clippy::struct_excessive_bools,
    reason = "字段直接对应界面选项与流程分支"
)]
pub struct Input {
    /// 选定的 MetaMystia DLL 版本；`None` 表示最新
    pub dll_version: Option<String>,
    /// 游戏目录
    pub game_root: PathBuf,
    /// 升级/安装时是否处理 ResourceExample ZIP（界面可让用户跳过）
    pub install_resourceex: bool,
    /// 是否需要下载文件
    pub needs_download: bool,
    /// 本次操作类型
    pub operation: OperationMode,
    /// 选定的 ResourceExample ZIP 版本；`None` 表示最新
    pub resourceex_version: Option<String>,
    /// 是否显示 BepInEx 控制台
    pub show_bepinex_console: bool,
    /// 是否完全卸载
    pub uninstall_full: bool,
    /// 升级时是否升级 BepInEx（界面可让用户跳过）
    pub upgrade_bepinex: bool,
    /// 升级时是否升级 MetaMystia DLL（界面可让用户跳过）
    pub upgrade_dll: bool,
}

/// 执行一次完整操作；界面在此之前已经拿到版本信息与游戏目录。
pub fn run(ui: &dyn Ui, input: &Input) -> Result<()> {
    report_event("Run.Start", Some(env!("CARGO_PKG_VERSION")));
    report_event("Run.Options", Some(&options_summary(input)));

    let result = run_inner(ui, input);

    match &result {
        Ok(()) => report_event("Run.Finished", None),
        Err(ManagerError::UserCancelled) => report_event("Run.Cancelled", None),
        Err(e) => report_event("Run.Failed", Some(&e.to_string())),
    }

    result
}

fn options_summary(input: &Input) -> String {
    match input.operation {
        OperationMode::Diagnostics => "op=diagnostics".to_string(),
        OperationMode::Uninstall => {
            format!("op=uninstall;full_uninstall={}", input.uninstall_full)
        }
        OperationMode::Install | OperationMode::Upgrade => format!(
            "op={};dll={};resourceex={};download={};bepinex={};dll_update={};console={}",
            input.operation.name(),
            input.dll_version.as_deref().unwrap_or("latest"),
            input.resourceex_version.as_deref().unwrap_or("latest"),
            input.needs_download,
            input.upgrade_bepinex,
            input.upgrade_dll,
            input.show_bepinex_console,
        ),
    }
}

fn run_inner(ui: &dyn Ui, input: &Input) -> Result<()> {
    let downloader = Downloader::new(ui);

    if matches!(
        input.operation,
        OperationMode::Install | OperationMode::Upgrade
    ) {
        if check_game_running_cached()? {
            ui.emit(UiEvent::GameRunningWarning)?;
            return Err(ManagerError::GameRunning);
        }

        if rollback::recover_interrupted(&input.game_root)? {
            ui.emit(UiEvent::Message(
                "检测到上次安装/升级未完成，已自动恢复到操作前的状态",
            ))?;
        }

        let version_info = downloader.fetch_version_info()?;
        ui.emit(UiEvent::DisplayVersion(version_info.manager_version()))?;

        let needs_download = match input.operation {
            OperationMode::Install => true,
            OperationMode::Upgrade => input.needs_download,
            _ => false,
        };
        if needs_download && !ensure_logged_in(ui, &version_info.config_url)? {
            return Err(ManagerError::UserCancelled);
        }
    }

    match input.operation {
        OperationMode::Diagnostics => run_diagnostics(input, ui),
        OperationMode::Install => {
            let installer = Installer::new(input.game_root.clone(), ui);

            if installer.is_metamystia_installed() {
                run_upgrade(input, ui)
            } else {
                run_install(input, ui)
            }
        }
        OperationMode::Uninstall => run_uninstall(input, ui),
        OperationMode::Upgrade => run_upgrade(input, ui),
    }
}

fn run_install(input: &Input, ui: &dyn Ui) -> Result<()> {
    let installer = Installer::new(input.game_root.clone(), ui)
        .with_dll_version(input.dll_version.clone())
        .with_resourceex(input.resourceex_version.clone(), input.install_resourceex)
        .with_console(input.show_bepinex_console);

    let has_installed = installer.is_bepinex_installed()
        || installer.is_metamystia_installed()
        || installer.is_resourceex_installed();

    installer.install(has_installed)
}

fn run_upgrade(input: &Input, ui: &dyn Ui) -> Result<()> {
    let upgrader = Upgrader::new(input.game_root.clone(), ui)
        .with_dll_version(input.dll_version.clone())
        .with_resourceex_version(input.resourceex_version.clone())
        .with_skips(
            !input.upgrade_bepinex,
            !input.upgrade_dll,
            !input.install_resourceex,
        )
        .with_console(input.show_bepinex_console);

    upgrader.upgrade()
}

fn run_uninstall(input: &Input, ui: &dyn Ui) -> Result<()> {
    let mode = if input.uninstall_full {
        UninstallMode::Full
    } else {
        UninstallMode::Light
    };
    let uninstaller = Uninstaller::new(input.game_root.clone(), ui).with_mode(mode);

    uninstaller.uninstall()
}

fn run_diagnostics(input: &Input, ui: &dyn Ui) -> Result<()> {
    let path = export(ui, &input.game_root)?;

    if path.as_os_str().is_empty() {
        return Err(ManagerError::UserCancelled);
    }

    ui.emit(UiEvent::DiagnosticsExported(&path))?;
    ui.emit(UiEvent::Message(&format!(
        "诊断包已生成：{}",
        path.display()
    )))?;

    Ok(())
}

/// 管理工具自升级：服务端版本比当前新时下载并替换自身；返回是否真的替换了。
pub fn self_update(ui: &dyn Ui) -> Result<bool> {
    if !is_self_update_enabled() {
        return Ok(false);
    }

    let downloader = Downloader::new(ui);
    let version_info = downloader.fetch_version_info()?;

    let Some(remote_version) = version_info.manager_version() else {
        return Ok(false);
    };

    if env!("CARGO_PKG_VERSION") == remote_version {
        return Ok(false);
    }

    let exe_dir = env::current_exe()?
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| ManagerError::Other("无法确定管理工具所在目录".to_string()))?;

    run_self_update(&exe_dir, ui, &downloader, &version_info)?;

    Ok(true)
}
