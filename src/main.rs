mod cli;
mod cli_ui;
mod config;
mod console_ui;
mod downloader;
mod env_check;
mod error;
mod extractor;
mod file_ops;
mod installer;
mod metrics;
mod model;
mod net;
mod permission;
mod shutdown;
mod temp_dir;
mod ui;
mod uninstaller;
mod updater;
mod upgrader;

use crate::cli::{Cli, CliConfig, CliOperation, InstallConfig};
use crate::cli_ui::CliUI;
use crate::config::{GAME_EXECUTABLE, OperationMode, UninstallMode};
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

use clap::Parser;
use std::{path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let cli_args = Cli::parse();
    let cli_config = cli_args.to_config();

    if !cfg!(windows) {
        if let Some(ref config) = cli_config {
            let cli_ui = CliUI::new(config.quiet);
            let _ = cli_ui.error("Windows platform is required");
            return ExitCode::from(1);
        }
        let console_ui = ConsoleUI::new();
        let _ = console_ui.error("错误：仅支持 Windows 平台");
        console_ui.wait_for_key().ok();
        return ExitCode::from(1);
    }

    let res = if let Some(ref config) = cli_config {
        let cli_ui = CliUI::new(config.quiet);
        match run_with_cli(&cli_ui, config) {
            Ok(exit_code) => ExitCode::from(exit_code),
            Err(e) => {
                eprintln!("Error: {}", e);
                ExitCode::from(1)
            }
        }
    } else {
        let console_ui = ConsoleUI::new();
        match run(&console_ui) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                let _ = console_ui.error(&format!("错误：{}", e));
                console_ui.wait_for_key().ok();
                ExitCode::from(1)
            }
        }
    };

    // 执行清理回调
    run_shutdown();

    res
}

fn run(ui: &dyn Ui) -> Result<()> {
    report_event("Run", Some(env!("CARGO_PKG_VERSION")));

    // 1. 显示欢迎信息
    ui.display_welcome()?;

    let mut version_info = None;
    let downloader = match Downloader::new(ui) {
        Ok(dl) => match dl.get_version_info() {
            Ok(vi) => {
                version_info = Some(vi);
                Some(dl)
            }
            Err(e) => {
                let _ = ui.message(&format!("无法获取版本信息：{}", e));
                None
            }
        },
        _ => None,
    };

    ui.display_version(version_info.as_ref().map(|vi| vi.manager.as_str()))?;

    // 自升级提示
    if let (Some(downloader), Some(vi)) = (&downloader, &version_info) {
        let current_version = env!("CARGO_PKG_VERSION");
        if current_version != vi.manager
            && ui.manager_ask_self_update(current_version, &vi.manager)?
        {
            match perform_self_update(&std::env::current_dir()?, ui, downloader, vi, true) {
                Ok(_) => {
                    run_shutdown();
                    std::process::exit(0);
                }
                Err(e) => ui.manager_update_failed(&format!("{}", e))?,
            }
        }
    }

    // 2. 目录环境检查
    let game_root = match check_game_directory(ui) {
        Ok(path) => path,
        Err(e) => {
            ui.message(&format!("当前目录：{}", std::env::current_dir()?.display()))?;
            ui.message(&format!(
                "请在游戏根目录（包含 {} 的文件夹）下运行本程序。",
                GAME_EXECUTABLE
            ))?;
            return Err(e);
        }
    };

    // 3. 游戏进程检查
    if check_game_running()? {
        ui.display_game_running_warning()?;
        return Err(ManagerError::GameRunning);
    }

    // 4. 显示可升级项
    if let Some(vi) = &version_info
        && let Ok(upgrader) = Upgrader::new(game_root.clone(), ui)
        && let Ok((bep_needs, dll_needs, res_needs)) = upgrader.has_updates(vi)
    {
        ui.display_available_updates(bep_needs, dll_needs, res_needs)?;
    }

    // 5. 选择操作模式
    let operation = ui.select_operation_mode()?;
    match operation {
        OperationMode::Install => run_install(game_root.clone(), ui, None),
        OperationMode::Upgrade => run_upgrade(game_root.clone(), ui),
        OperationMode::Uninstall => run_uninstall(game_root.clone(), ui, None),
    }
}

fn run_with_cli(ui: &dyn Ui, config: &CliConfig) -> Result<u8> {
    report_event("Run.CLI", Some(env!("CARGO_PKG_VERSION")));

    let skip_network = matches!(config.operation, CliOperation::Uninstall(_));

    let mut version_info = None;
    let downloader = if !skip_network {
        let dl = Downloader::new(ui)?;
        let vi = dl.get_version_info()?;
        version_info = Some(vi);
        Some(dl)
    } else {
        None
    };

    ui.display_version(version_info.as_ref().map(|vi| vi.manager.as_str()))?;

    // 执行自更新
    if !skip_network
        && !config.skip_self_update
        && let (Some(downloader), Some(vi)) = (&downloader, &version_info)
    {
        let current_version = env!("CARGO_PKG_VERSION");
        if current_version != vi.manager {
            match perform_self_update(&std::env::current_dir()?, ui, downloader, vi, false) {
                Ok(filename) => {
                    ui.message(&filename)?;
                    run_shutdown();
                    return Ok(100);
                }
                Err(e) => ui.manager_update_failed(&format!("{}", e))?,
            }
        }
    }

    // 1. 目录环境检查
    let game_root = if let Some(path) = &config.game_path {
        if !path.exists() {
            return Err(ManagerError::Other(format!(
                "Path does not exist: {}",
                path.display()
            )));
        }
        if !path.join(GAME_EXECUTABLE).exists() {
            return Err(ManagerError::Other(format!(
                "Game executable {} not found in {}",
                GAME_EXECUTABLE,
                path.display()
            )));
        }
        path.clone()
    } else {
        match check_game_directory(ui) {
            Ok(path) => path,
            Err(e) => {
                ui.message(&format!(
                    "Current directory: {}",
                    std::env::current_dir()?.display()
                ))?;
                ui.message(&format!(
                    "Please run this program in the game root directory (containing {}) or use --path to specify the directory.",
                    GAME_EXECUTABLE
                ))?;
                return Err(e);
            }
        }
    };

    // 2. 游戏进程检查
    if check_game_running()? {
        ui.display_game_running_warning()?;
        return Err(ManagerError::GameRunning);
    }

    // 3. 执行操作
    match &config.operation {
        CliOperation::Install(install_config) => {
            run_install(game_root, ui, Some(install_config))?;
        }
        CliOperation::Upgrade => {
            run_upgrade(game_root, ui)?;
        }
        CliOperation::Uninstall(mode) => {
            run_uninstall(game_root, ui, Some(*mode))?;
        }
    }

    Ok(0)
}

fn run_install(game_root: PathBuf, ui: &dyn Ui, config: Option<&InstallConfig>) -> Result<()> {
    // 创建安装器
    let installer = Installer::new(game_root, ui)?;

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

    // 执行安装
    installer.install(has_installed, config)?;

    ui.wait_for_key()?;
    Ok(())
}

fn run_upgrade(game_root: PathBuf, ui: &dyn Ui) -> Result<()> {
    // 创建升级器
    let upgrader = Upgrader::new(game_root, ui)?;

    // 执行升级
    upgrader.upgrade()?;

    ui.wait_for_key()?;
    Ok(())
}

fn run_uninstall(game_root: PathBuf, ui: &dyn Ui, mode: Option<UninstallMode>) -> Result<()> {
    // 创建卸载器
    let uninstaller = Uninstaller::new(game_root, ui)?;

    // 执行卸载
    uninstaller.uninstall(mode)?;

    ui.wait_for_key()?;
    Ok(())
}
