//! 安装 / 升级 / 卸载 / 诊断的执行流程。
//!
//! 界面负责收集用户选择并展示进度，这里按顺序调用底层能力；
//! 需要用户当场确认的少数问题仍然通过 [`Ui`] 回调界面。

use crate::config::{OperationMode, UninstallMode};
use crate::diagnostics::export;
use crate::downloader::Downloader;
use crate::env_check::check_game_running;
use crate::error::{ManagerError, Result};
use crate::installer::Installer;
use crate::metrics::report_event;
use crate::platform::self_update_enabled;
use crate::rollback;
use crate::sso::ensure_logged_in;
use crate::ui::{Ui, UiEvent};
use crate::uninstaller::Uninstaller;
use crate::updater::perform_self_update;
use crate::upgrader::Upgrader;

use std::{env, path::PathBuf};

#[allow(
    clippy::struct_excessive_bools,
    reason = "字段直接对应界面选项与流程分支"
)]
pub struct Input {
    /// 选定的 MetaMystia DLL 版本；`None` 表示最新
    pub dll_version: Option<String>,
    pub game_root: PathBuf,
    /// 升级/安装时是否处理 ResourceExample ZIP（界面可让用户跳过）
    pub install_resourceex: bool,
    pub needs_download: bool,
    pub operation: OperationMode,
    /// 选定的 ResourceExample ZIP 版本；`None` 表示最新
    pub resourceex_version: Option<String>,
    pub show_bepinex_console: bool,
    pub uninstall_full: bool,
    /// 升级时是否升级 BepInEx（界面可让用户跳过）
    pub upgrade_bepinex: bool,
    /// 升级时是否升级 MetaMystia DLL（界面可让用户跳过）
    pub upgrade_dll: bool,
}

/// 执行一次完整操作；界面在此之前已经拿到版本信息与游戏目录
pub fn run(ui: &dyn Ui, input: &Input) -> Result<()> {
    report_event("Run", Some(env!("CARGO_PKG_VERSION")));

    let downloader = Downloader::new(ui);

    if matches!(
        input.operation,
        OperationMode::Install | OperationMode::Upgrade
    ) {
        if check_game_running()? {
            ui.emit(UiEvent::GameRunningWarning)?;
            return Err(ManagerError::GameRunning);
        }

        if rollback::recover_interrupted(&input.game_root)? {
            ui.emit(UiEvent::Message(
                "检测到上次安装/升级未完成，已自动恢复到操作前的状态",
            ))?;
        }

        let version_info = downloader.get_version_info()?;
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

            if installer.check_metamystia_installed() {
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

    let has_installed = installer.check_bepinex_installed()
        || installer.check_metamystia_installed()
        || installer.check_resourceex_installed();

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

/// 管理工具自升级：服务端版本比当前新时下载并替换自身；返回是否真的替换了
pub fn self_update(ui: &dyn Ui) -> Result<bool> {
    if !self_update_enabled() {
        return Ok(false);
    }

    let downloader = Downloader::new(ui);
    let version_info = downloader.get_version_info()?;

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

    perform_self_update(&exe_dir, ui, &downloader, &version_info)?;

    Ok(true)
}
