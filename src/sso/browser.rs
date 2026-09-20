//! 用系统默认浏览器打开授权地址
//!
//! 走 `shell32.dll` 的 `ShellExecuteW`：由系统按 `http:` 的默认关联选择浏览器。

use crate::error::{ManagerError, Result};

use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// `ShellExecuteW` 返回值不大于该值即视为失败（`0`、`SE_ERR_*`）
const SHELL_EXECUTE_ERROR_MAX: isize = 32;

pub fn open_url(url: &str) -> Result<()> {
    let operation: Vec<u16> = "open\0".encode_utf16().collect();
    let target: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();

    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = result as isize;

    if code <= SHELL_EXECUTE_ERROR_MAX {
        return Err(ManagerError::SsoLoginFailed(format!(
            "无法打开默认浏览器（ShellExecuteW 返回 {code}）"
        )));
    }

    Ok(())
}
