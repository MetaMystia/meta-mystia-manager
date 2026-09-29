// release 构建不带控制台窗口；debug 保留控制台便于看 panic
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod diagnostics;
mod downloader;
mod env_check;
mod error;
mod extractor;
mod file_ops;
mod flow;
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
mod window;

#[cfg(windows)]
mod gui;
#[cfg(windows)]
mod gui_ui;
#[cfg(windows)]
mod win32;

use std::process::ExitCode;

#[cfg(windows)]
fn main() -> ExitCode {
    gui::run();
    ExitCode::SUCCESS
}

/// 界面只在 Windows 上提供，其它平台保留最小实现以便编译与测试
#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("MetaMystia Mod 管理工具目前只支持 Windows。");
    ExitCode::from(1)
}
