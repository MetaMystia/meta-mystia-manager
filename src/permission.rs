use crate::error::{ManagerError, Result};
use crate::metrics::report_event;

use std::{
    mem::size_of, os::windows::process::CommandExt, path::PathBuf, process::Command, ptr::null_mut,
};
use winapi::um::{
    handleapi::CloseHandle,
    processthreadsapi::{GetCurrentProcess, OpenProcessToken},
    securitybaseapi::GetTokenInformation,
    winbase::CREATE_NO_WINDOW,
    winnt::{HANDLE, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
};

struct TokenHandle(HANDLE);

impl TokenHandle {
    fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for TokenHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct TempScript(PathBuf);

impl TempScript {
    fn new(path: PathBuf) -> Self {
        Self(path)
    }
}

impl Drop for TempScript {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// 检查当前进程是否具有管理员权限
pub fn is_elevated() -> Result<bool> {
    unsafe {
        let mut token: HANDLE = null_mut();

        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Ok(false);
        }

        let token_handle = TokenHandle::new(token);

        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut return_length = 0u32;

        let result = GetTokenInformation(
            token_handle.raw(),
            TokenElevation,
            &mut elevation as *mut TOKEN_ELEVATION as *mut _,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut return_length,
        );

        if result != 0 {
            Ok(elevation.TokenIsElevated != 0)
        } else {
            Ok(false)
        }
    }
}

/// 以管理员权限重新启动程序
pub fn elevate_and_restart() -> Result<()> {
    let current_dir = std::env::current_dir()?;
    let exe_path = std::env::current_exe()?;

    // 创建一个临时 PowerShell 脚本来执行 Start-Process -Verb RunAs
    let escape = |s: &str| s.replace('"', "\"\"");
    let dir_escaped = escape(&current_dir.display().to_string());
    let exe_escaped = escape(&exe_path.display().to_string());

    let script = format!(
        "Start-Process -FilePath \"{}\" -WorkingDirectory \"{}\" -Verb RunAs",
        exe_escaped, dir_escaped
    );

    let mut script_path = std::env::temp_dir();
    script_path.push(format!("meta_mystia_elevate_{}.ps1", std::process::id()));

    std::fs::write(&script_path, script.as_bytes()).map_err(|e| {
        ManagerError::from(std::io::Error::new(
            e.kind(),
            format!("写入提升脚本 {} 失败：{}", script_path.display(), e),
        ))
    })?;

    let _temp_script = TempScript::new(script_path.clone());

    // 尝试优先使用 pwsh（PowerShell Core），若不可用再回退到 powershell.exe
    let shells = ["pwsh.exe", "powershell.exe"];

    for shell in &shells {
        let res = Command::new(shell)
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&script_path)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();

        if res.is_ok() {
            report_event("Permission.Elevate.Scheduled", None);
            return Ok(());
        }
    }

    report_event("Permission.Elevate.Failed", None);

    Err(ManagerError::Other(
        "无法以管理员身份重新启动（未找到可用的 PowerShell 或启动失败）".to_string(),
    ))
}
