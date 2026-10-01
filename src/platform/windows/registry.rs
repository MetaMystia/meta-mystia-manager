//! Windows 注册表读取。

use std::{mem::size_of, ptr::null_mut};
use windows_sys::Win32::System::Registry::{HKEY, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};

/// 读取 `REG_SZ` 字符串；缺失或类型不符时返回 `None`。
pub fn read_string(hive: HKEY, subkey: &str, value: &str) -> Option<String> {
    let subkey: Vec<u16> = subkey.encode_utf16().chain([0]).collect();
    let value: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let mut size = 0u32;
    let status = unsafe {
        RegGetValueW(
            hive,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &raw mut size,
        )
    };
    if status != 0 || size == 0 {
        return None;
    }

    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    let status = unsafe {
        RegGetValueW(
            hive,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &raw mut size,
        )
    };
    if status != 0 {
        return None;
    }

    let len = (size as usize / 2).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..len]);
    let text = text.trim_end_matches('\0').trim().to_string();

    (!text.is_empty()).then_some(text)
}

/// 读取 `REG_DWORD` 数值；缺失或类型不符时返回 `None`。
pub fn read_dword(hive: HKEY, subkey: &str, value: &str) -> Option<u32> {
    let subkey: Vec<u16> = subkey.encode_utf16().chain([0]).collect();
    let value: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let mut data = 0u32;
    let mut size = u32::try_from(size_of::<u32>()).unwrap_or(u32::MAX);
    let status = unsafe {
        RegGetValueW(
            hive,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&raw mut data).cast(),
            &raw mut size,
        )
    };

    (status == 0 && size >= u32::try_from(size_of::<u32>()).unwrap_or(u32::MAX)).then_some(data)
}
