//! SSO 需要的密码学原语
//!
//! 随机数与 SHA-256 由平台层提供（Windows 走 CNG，开发模拟走系统随机数与纯 Rust 实现）。

use crate::error::Result;
use crate::platform;

use base64::Engine;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;

const SHA256_LENGTH: usize = 32;

/// base64url 编码（URL 安全字母表、无填充）
pub fn base64url_encode(bytes: &[u8]) -> String {
    BASE64_URL_SAFE_NO_PAD.encode(bytes)
}

/// 用系统随机数发生器填充缓冲区（Windows 走 CNG，开发模拟走 `/dev/urandom`）
pub fn random_bytes(buffer: &mut [u8]) -> Result<()> {
    platform::random_bytes(buffer)
}

/// SHA-256 摘要
pub fn sha256(data: &[u8]) -> Result<[u8; SHA256_LENGTH]> {
    platform::sha256(data)
}
