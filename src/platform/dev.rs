//! 非 Windows 宿主的编译桩实现
//!
//! 程序本体只在 Windows 上运行；这里用与 Windows 实现相同的签名提供最小实现，
//! 让 macOS / Linux 宿主上的 cargo check / clippy 覆盖全部源码。

use crate::error::{ManagerError, Result};

use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};

const SHA256_LENGTH: usize = 32;

pub const fn is_elevated() -> bool {
    true
}

pub fn elevate_and_restart() -> Result<()> {
    Err(ManagerError::Other(
        "非 Windows 宿主不支持提权重启".to_string(),
    ))
}

pub const fn self_update_enabled() -> bool {
    false
}

pub const fn fs_dry_run() -> bool {
    false
}

pub const fn file_product_version(_path: &Path) -> Option<String> {
    None
}

#[allow(clippy::unnecessary_wraps, reason = "与 Windows 实现保持相同签名")]
pub const fn is_game_running() -> Result<bool> {
    Ok(false)
}

pub fn open_url(_url: &str) -> Result<()> {
    Err(ManagerError::SsoLoginFailed(
        "非 Windows 宿主不支持打开浏览器".to_string(),
    ))
}

pub fn random_bytes(buffer: &mut [u8]) -> Result<()> {
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(buffer))
        .map_err(|e| ManagerError::SsoLoginFailed(format!("读取系统随机数失败：{e}")))
}

#[allow(clippy::unnecessary_wraps, reason = "与 Windows 实现保持相同签名")]
pub fn sha256(data: &[u8]) -> Result<[u8; SHA256_LENGTH]> {
    let mut hasher = Sha256::new();
    hasher.update(data);

    Ok(hasher.finalize().into())
}
