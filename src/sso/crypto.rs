//! SSO 需要的密码学原语

use crate::error::{ManagerError, Result};

use base64::Engine;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use windows_sys::Win32::Foundation::NTSTATUS;
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE, BCRYPT_SHA256_ALGORITHM,
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptCloseAlgorithmProvider, BCryptCreateHash,
    BCryptDestroyHash, BCryptFinishHash, BCryptGenRandom, BCryptHashData,
    BCryptOpenAlgorithmProvider,
};

const STATUS_SUCCESS: NTSTATUS = 0;
const SHA256_LENGTH: usize = 32;

/// base64url 编码（URL 安全字母表、无填充）
pub fn base64url_encode(bytes: &[u8]) -> String {
    BASE64_URL_SAFE_NO_PAD.encode(bytes)
}

/// 用 CNG 的系统首选随机数发生器填充缓冲区
pub fn random_bytes(buffer: &mut [u8]) -> Result<()> {
    let len = u32::try_from(buffer.len())
        .map_err(|_| ManagerError::SsoLoginFailed("随机数长度超出限制".to_string()))?;

    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buffer.as_mut_ptr(),
            len,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status != STATUS_SUCCESS {
        return Err(ManagerError::SsoLoginFailed(format!(
            "生成随机数失败：NTSTATUS {status:#x}"
        )));
    }

    Ok(())
}

/// SHA-256 摘要
pub fn sha256(data: &[u8]) -> Result<[u8; SHA256_LENGTH]> {
    let len = u32::try_from(data.len())
        .map_err(|_| ManagerError::SsoLoginFailed("待哈希数据长度超出限制".to_string()))?;

    let mut algorithm: BCRYPT_ALG_HANDLE = std::ptr::null_mut();
    let mut hash: BCRYPT_HASH_HANDLE = std::ptr::null_mut();

    let mut digest = [0u8; SHA256_LENGTH];

    unsafe {
        let status = BCryptOpenAlgorithmProvider(
            &raw mut algorithm,
            BCRYPT_SHA256_ALGORITHM,
            std::ptr::null(),
            0,
        );
        if status != STATUS_SUCCESS {
            return Err(ManagerError::SsoLoginFailed(format!(
                "打开 SHA-256 算法提供程序失败：NTSTATUS {status:#x}"
            )));
        }

        // 默认不提供哈希对象缓冲区，由 CNG 自行分配
        let status = BCryptCreateHash(
            algorithm,
            &raw mut hash,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            0,
            0,
        );
        if status != STATUS_SUCCESS {
            BCryptCloseAlgorithmProvider(algorithm, 0);
            return Err(ManagerError::SsoLoginFailed(format!(
                "创建哈希对象失败：NTSTATUS {status:#x}"
            )));
        }

        let mut status = BCryptHashData(hash, data.as_ptr(), len, 0);
        if status == STATUS_SUCCESS {
            status = BCryptFinishHash(
                hash,
                digest.as_mut_ptr(),
                u32::try_from(SHA256_LENGTH).unwrap_or(u32::MAX),
                0,
            );
        }

        BCryptDestroyHash(hash);
        BCryptCloseAlgorithmProvider(algorithm, 0);

        if status != STATUS_SUCCESS {
            return Err(ManagerError::SsoLoginFailed(format!(
                "计算 SHA-256 失败：NTSTATUS {status:#x}"
            )));
        }
    }

    Ok(digest)
}
