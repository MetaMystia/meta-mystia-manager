//! 非 Windows 平台的编译桩实现。
//!
//! 程序本体只在 Windows 上运行；这里用与 Windows 实现相同的签名提供最小实现，
//! 让 macOS / Linux 上的 `cargo check` / `cargo clippy` 覆盖全部源码。

use crate::error::{ManagerError, Result};
use crate::platform::SystemProxySettings;

use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};

#[cfg(target_os = "macos")]
use std::process::Command;

const SHA256_LENGTH: usize = 32;

/// 非 Windows 平台：返回默认代理设置（未启用）。
pub fn read_system_proxy_settings() -> SystemProxySettings {
    SystemProxySettings::default()
}

/// 非 Windows 平台：不解析 PAC。
pub const fn resolve_pac_proxy(
    _target_url: &str,
    _pac_url: &str,
) -> Option<(String, Option<String>)> {
    None
}

/// 控制台事件处理器签名（与 Windows 实现保持一致）。
pub type ConsoleHandler = unsafe extern "system" fn(u32) -> i32;

/// 非 Windows 平台：空操作。
pub const fn set_console_ctrl_handler(_handler: ConsoleHandler) {}

/// 非 Windows 平台：空操作。
pub const fn focus_manager_window() {}

/// 读取 macOS 的 `IOPlatformUUID`。
#[cfg(target_os = "macos")]
pub fn machine_id() -> Option<String> {
    let out = Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|line| line.contains("IOPlatformUUID"))
        .and_then(|line| line.split('=').nth(1))
        .map(|value| value.trim().trim_matches('"').to_string())
        .filter(|id| !id.is_empty())
}

/// 读取 Linux 的 `/etc/machine-id`。
#[cfg(all(unix, not(target_os = "macos")))]
pub fn machine_id() -> Option<String> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .iter()
        .find_map(|path| fs::read_to_string(path).ok())
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

/// 非 Windows/Unix 平台：无机器标识。
#[cfg(not(any(windows, unix)))]
pub const fn machine_id() -> Option<String> {
    None
}

/// 非 Windows 平台：不提供磁盘空间信息。
#[cfg(not(windows))]
pub const fn free_space(_path: &Path) -> Option<u64> {
    None
}

/// 非 Windows 平台：恒为管理员（仅用于编译检查）。
pub const fn is_elevated() -> bool {
    true
}

/// 非 Windows 平台：不支持提权。
pub fn elevate_and_restart() -> Result<()> {
    Err(ManagerError::Other(
        "非 Windows 平台不支持提权重启".to_string(),
    ))
}

/// 非 Windows 平台：不启用自更新。
pub const fn is_self_update_enabled() -> bool {
    false
}

/// 非 Windows 平台：不进入演练模式。
pub const fn is_fs_dry_run() -> bool {
    false
}

/// 非 Windows 平台：无法读取 PE 版本信息。
pub const fn file_product_version(_path: &Path) -> Option<String> {
    None
}

/// 非 Windows 平台：恒为未运行。
#[allow(clippy::unnecessary_wraps, reason = "与 Windows 实现保持相同签名")]
pub const fn is_game_running() -> Result<bool> {
    Ok(false)
}

/// 非 Windows 平台：不支持打开浏览器。
pub fn open_url(_url: &str) -> Result<()> {
    Err(ManagerError::SsoLoginFailed(
        "非 Windows 平台不支持打开浏览器".to_string(),
    ))
}

/// 读取系统随机数；非 Windows 平台使用 `/dev/urandom`。
pub fn random_bytes(buffer: &mut [u8]) -> Result<()> {
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(buffer))
        .map_err(|e| ManagerError::SsoLoginFailed(format!("读取系统随机数失败：{e}")))
}

/// 计算 SHA-256；非 Windows 平台使用纯 Rust 实现。
#[allow(clippy::unnecessary_wraps, reason = "与 Windows 实现保持相同签名")]
pub fn sha256(data: &[u8]) -> Result<[u8; SHA256_LENGTH]> {
    let mut hasher = Sha256::new();
    hasher.update(data);

    Ok(hasher.finalize().into())
}
