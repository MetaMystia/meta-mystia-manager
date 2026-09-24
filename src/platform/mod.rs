//! 平台相关能力
//!
//! Windows 宿主上提供真实实现（提权、进程枚举、系统浏览器、CNG 加密、控制台事件钩子）；
//! 非 Windows 宿主上提供开发模拟实现（见 `dev` 模块），仅用于本地开发调试。
//!
//! 所有平台相关调用都收敛在这里，调用方只依赖 `crate::platform::*`：
//! - `cfg(windows)` 的实现会被完整编入 Windows 产物；
//! - `cfg(not(windows))` 的开发实现不会进入 Windows 产物。

#[cfg(windows)]
mod windows;

#[cfg(not(windows))]
pub mod dev;

#[cfg(windows)]
use windows as imp;

#[cfg(not(windows))]
use dev as imp;

pub use imp::{
    elevate_and_restart, fs_dry_run, init, is_elevated, is_game_running, open_url, random_bytes,
    self_update_enabled, sha256,
};

// 只有 Windows 需要抑制子进程的控制台窗口
#[cfg(windows)]
pub use imp::suppress_console_window;
