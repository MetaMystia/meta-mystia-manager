//! MetaMystia Mod 管理工具。

// release 构建不带控制台窗口；debug 保留控制台便于看 panic
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
// 非 Windows 入口只保留编译检查，界面与流程代码不会被执行
#![cfg_attr(not(windows), allow(dead_code))]

mod config;
mod diagnostics;
mod env;
mod error;
mod format;
mod fs;
mod http;
mod mode;
mod net;
mod ops;
mod platform;
mod shutdown;
mod telemetry;
mod ui;
mod version;

#[cfg(windows)]
mod gui;

use std::process::ExitCode;

#[cfg(windows)]
use crate::ops::self_update::capture_restart_args;

#[cfg(windows)]
fn main() -> ExitCode {
    capture_restart_args();
    gui::run();
    ExitCode::SUCCESS
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("MetaMystia Mod 管理工具目前只支持 Windows。");
    ExitCode::from(1)
}
