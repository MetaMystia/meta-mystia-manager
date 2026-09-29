//! 安装 / 升级 / 卸载 / 诊断的执行流程。
//!
//! 界面负责收集用户选择并展示进度，这里按顺序调用底层能力；
//! 需要用户当场确认的少数问题仍然通过 [`Ui`] 回调界面。

use crate::config::OperationMode;
use crate::diagnostics::export;
use crate::downloader::Downloader;
use crate::env_check::check_game_running;
use crate::error::{ManagerError, Result};
use crate::installer::Installer;
use crate::metrics::report_event;
use crate::platform::self_update_enabled;
use crate::sso::ensure_logged_in;
use crate::ui::Ui;
use crate::uninstaller::Uninstaller;
use crate::updater::perform_self_update;
use crate::upgrader::Upgrader;

use std::{env, path::PathBuf};

/// 界面收集好的选择
pub struct Input {
    /// 选定的 MetaMystia DLL 版本；`None` 表示最新
    pub dll_version: Option<String>,
    pub game_root: PathBuf,
    /// 升级/安装时是否处理 ResourceExample ZIP（界面可让用户跳过）
    pub install_resourceex: bool,
    pub operation: OperationMode,
    /// 选定的 ResourceExample ZIP 版本；`None` 表示最新
    pub resourceex_version: Option<String>,
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
        let version_info = downloader.get_version_info()?;
        ui.display_version(Some(version_info.manager.as_str()))?;

        if check_game_running()? {
            ui.display_game_running_warning()?;
            return Err(ManagerError::GameRunning);
        }
        if !ensure_logged_in(ui, &version_info.config_url)? {
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
    let installer = Installer::new(input.game_root.clone(), ui);

    let bepinex_installed = installer.check_bepinex_installed();
    let metamystia_installed = installer.check_metamystia_installed();
    let resourceex_installed = installer.check_resourceex_installed();
    let has_installed = bepinex_installed || metamystia_installed || resourceex_installed;

    if has_installed {
        ui.install_warn_existing(
            bepinex_installed,
            metamystia_installed,
            resourceex_installed,
        )?;

        if !ui.install_confirm_overwrite()? {
            return Err(ManagerError::UserCancelled);
        }
    }

    installer.install(has_installed)
}

fn run_upgrade(input: &Input, ui: &dyn Ui) -> Result<()> {
    let upgrader = Upgrader::new(input.game_root.clone(), ui)
        .with_dll_version(input.dll_version.clone())
        .with_resourceex_version(input.resourceex_version.clone())
        .with_skips(!input.upgrade_dll, !input.install_resourceex);

    upgrader.upgrade()
}

fn run_uninstall(input: &Input, ui: &dyn Ui) -> Result<()> {
    let uninstaller = Uninstaller::new(input.game_root.clone(), ui);

    uninstaller.uninstall()
}

fn run_diagnostics(input: &Input, ui: &dyn Ui) -> Result<()> {
    let path = export(ui, &input.game_root)?;

    if path.as_os_str().is_empty() {
        return Err(ManagerError::UserCancelled);
    }

    ui.diagnostics_exported(&path)?;
    ui.message(&format!("诊断包已生成：{}", path.display()))?;

    Ok(())
}

/// 管理工具自升级：服务端版本比当前新时下载并替换自身；返回是否真的替换了
pub fn self_update(ui: &dyn Ui) -> Result<bool> {
    if !self_update_enabled() {
        return Ok(false);
    }

    let downloader = Downloader::new(ui);
    let version_info = downloader.get_version_info()?;

    if env!("CARGO_PKG_VERSION") == version_info.manager {
        return Ok(false);
    }

    perform_self_update(&env::current_dir()?, ui, &downloader, &version_info, true)?;

    Ok(true)
}
