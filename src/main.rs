mod config;
mod console_ui;
mod diagnostics;
mod downloader;
mod env_check;
mod error;
mod extractor;
mod file_ops;
mod installer;
mod metrics;
mod model;
mod net;
mod platform;
mod preflight;
mod remote_config;
mod rollback;
mod shutdown;
mod sso;
mod temp_dir;
mod ui;
mod uninstaller;
mod updater;
mod upgrader;
#[cfg(windows)]
mod win32;
mod window;

use crate::config::{GAME_EXECUTABLE, OfflineMode, OperationMode};
use crate::console_ui::ConsoleUI;
use crate::downloader::Downloader;
use crate::env_check::{check_game_directory, check_game_running};
use crate::error::{ManagerError, Result};
use crate::installer::Installer;
use crate::metrics::report_event;
use crate::shutdown::run_shutdown;
use crate::ui::Ui;
use crate::uninstaller::Uninstaller;
use crate::updater::perform_self_update;
use crate::upgrader::Upgrader;

use std::{
    env,
    path::{Path, PathBuf},
    process::{self, ExitCode},
};

fn main() -> ExitCode {
    platform::init();

    let res = run_console_ui();

    run_shutdown();

    res
}

/// 以交互式控制台模式运行，返回进程退出码
fn run_console_ui() -> ExitCode {
    let console_ui = ConsoleUI::new();

    match run(&console_ui) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = console_ui.error(&format!("错误：{e}"));
            console_ui.wait_for_key().ok();
            ExitCode::from(1)
        }
    }
}

fn run(ui: &dyn Ui) -> Result<()> {
    report_event("Run", Some(env!("CARGO_PKG_VERSION")));

    // 1. 显示欢迎信息
    ui.display_welcome()?;

    // 拿不到版本信息时无法安装/升级，转入只提供卸载与诊断包的离线模式
    let downloader = Downloader::new(ui);
    let version_info = match downloader.get_version_info() {
        Ok(version_info) => version_info,
        Err(e) => {
            ui.error(&format!("无法获取版本信息：{e}"))?;

            return run_offline(ui);
        }
    };

    ui.display_version(Some(version_info.manager.as_str()))?;

    // 自升级提示
    if platform::self_update_enabled() {
        let current_version = env!("CARGO_PKG_VERSION");
        if current_version != version_info.manager
            && ui.manager_ask_self_update(current_version, &version_info.manager)?
        {
            match perform_self_update(&env::current_dir()?, ui, &downloader, &version_info, true) {
                Ok(_) => {
                    run_shutdown();
                    process::exit(0);
                }
                Err(e) => ui.manager_update_failed(&format!("{e}"))?,
            }
        }
    }

    // 2. 目录环境检查
    let game_root = resolve_game_root(ui)?;

    // 3. 游戏进程检查
    if check_game_running()? {
        ui.display_game_running_warning()?;
        return Err(ManagerError::GameRunning);
    }

    // 4. 显示可升级项
    if let Ok((bep_needs, dll_needs, res_needs)) =
        Upgrader::new(game_root.clone(), ui).has_updates(&version_info)
    {
        ui.display_available_updates(bep_needs, dll_needs, res_needs)?;
    }

    // 5. 选择操作模式（安装/升级需要先完成账号登录；用户放弃登录时回到菜单）
    let operation = loop {
        let operation = ui.select_operation_mode()?;

        if matches!(operation, OperationMode::Install | OperationMode::Upgrade)
            && !sso::ensure_logged_in(ui, &version_info.config_url)?
        {
            continue;
        }

        break operation;
    };

    match operation {
        OperationMode::Install => run_install(game_root, ui),
        OperationMode::Upgrade => run_upgrade(game_root, ui),
        OperationMode::Uninstall => run_uninstall(game_root, ui),
        OperationMode::Diagnostics => run_diagnostics(&game_root, ui),
    }
}

/// 定位游戏根目录（当前目录或 Steam 安装路径）
fn resolve_game_root(ui: &dyn Ui) -> Result<PathBuf> {
    match check_game_directory(ui) {
        Ok(path) => Ok(path),
        Err(e) => {
            ui.message(&format!("当前目录：{}", env::current_dir()?.display()))?;
            ui.message(&format!(
                "请在游戏根目录（包含 {GAME_EXECUTABLE} 的文件夹）下运行本程序。"
            ))?;

            Err(e)
        }
    }
}

/// 离线模式：版本信息拿不到时只提供不依赖服务端的功能
fn run_offline(ui: &dyn Ui) -> Result<()> {
    let game_root = resolve_game_root(ui)?;

    if check_game_running()? {
        ui.display_game_running_warning()?;
        return Err(ManagerError::GameRunning);
    }

    match ui.select_offline_mode()? {
        OfflineMode::Uninstall => run_uninstall(game_root, ui),
        OfflineMode::Diagnostics => run_diagnostics(&game_root, ui),
    }
}

fn run_install(game_root: PathBuf, ui: &dyn Ui) -> Result<()> {
    let installer = Installer::new(game_root, ui);

    // 检查是否已安装组件
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

        let confirmed = ui.install_confirm_overwrite()?;
        if !confirmed {
            return Err(ManagerError::UserCancelled);
        }
    }

    installer.install(has_installed)?;

    ui.wait_for_key()?;
    Ok(())
}

fn run_upgrade(game_root: PathBuf, ui: &dyn Ui) -> Result<()> {
    let upgrader = Upgrader::new(game_root, ui);

    upgrader.upgrade()?;

    ui.wait_for_key()?;
    Ok(())
}

fn run_uninstall(game_root: PathBuf, ui: &dyn Ui) -> Result<()> {
    let uninstaller = Uninstaller::new(game_root, ui);

    uninstaller.uninstall()?;

    ui.wait_for_key()?;
    Ok(())
}

fn run_diagnostics(game_root: &Path, ui: &dyn Ui) -> Result<()> {
    match diagnostics::export(ui, game_root)? {
        path if path.as_os_str().is_empty() => {}
        path => ui.message(&format!("诊断包已生成：{}", path.display()))?,
    }

    ui.wait_for_key()?;
    Ok(())
}
