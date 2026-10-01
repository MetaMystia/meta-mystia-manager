//! 平台相关能力。
//!
//! Windows 平台上提供真实实现（提权、进程枚举、系统浏览器、CNG 加密、控制台事件钩子）；
//! 非 Windows 平台提供编译桩实现（见 `dev` 模块），仅用于编译检查。
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
    elevate_and_restart, file_product_version, focus_manager_window, free_space, is_elevated,
    is_fs_dry_run, is_game_running, is_self_update_enabled, machine_id, open_url, random_bytes,
    read_system_proxy_settings, resolve_pac_proxy, sha256,
};

#[cfg(windows)]
pub use imp::{MAIN_WINDOW_CLASS, focus_existing_manager_window, set_main_window};

pub use imp::set_console_ctrl_handler;

/// 系统代理设置快照。
#[derive(Default)]
pub struct SystemProxySettings {
    /// 是否启用系统代理
    pub enabled: bool,
    /// PAC 自动配置脚本地址
    pub auto_config_url: Option<String>,
    /// 绕过代理的地址列表
    pub bypass: Option<String>,
    /// 静态代理服务器
    pub server: Option<String>,
}

#[cfg(windows)]
pub use imp::suppress_console_window;

#[cfg(windows)]
pub use imp::acquire_single_instance;
