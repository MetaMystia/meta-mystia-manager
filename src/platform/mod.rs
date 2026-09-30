//! 平台相关能力
//!
//! Windows 宿主上提供真实实现（提权、进程枚举、系统浏览器、CNG 加密、控制台事件钩子）；
//! 非 Windows 宿主上提供编译桩实现（见 `dev` 模块），仅用于编译检查。
//!
//! 所有平台相关调用都收敛在这里，调用方只依赖 `crate::platform::*`：
//! - `cfg(windows)` 的实现会被完整编入 Windows 产物；
//! - `cfg(not(windows))` 的开发实现不会进入 Windows 产物。

#[cfg(windows)]
mod windows;

#[cfg(not(windows))]
mod dev;

#[cfg(windows)]
use windows as imp;

#[cfg(not(windows))]
use dev as imp;

pub use imp::{
    elevate_and_restart, file_product_version, fs_dry_run, is_elevated, is_game_running, open_url,
    random_bytes, self_update_enabled, sha256,
};

#[cfg(windows)]
pub use imp::init;

#[cfg(windows)]
pub use imp::suppress_console_window;

#[cfg(windows)]
pub use imp::acquire_single_instance;
