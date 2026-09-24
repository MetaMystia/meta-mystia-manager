//! Windows 真实实现：提权、进程枚举、系统浏览器、CNG 加密与控制台事件钩子。

use crate::config::GAME_PROCESS_NAME;
use crate::error::{ManagerError, Result};
use crate::metrics::report_event;
use crate::shutdown::run_shutdown;
use crate::win32::dword_len;

use std::{
    env, fs, io,
    mem::size_of,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{self, Command},
    ptr::null_mut,
};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, GetCurrentProcess, OpenProcessToken,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use windows_sys::Win32::Foundation::NTSTATUS;
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE, BCRYPT_SHA256_ALGORITHM,
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptCloseAlgorithmProvider, BCryptCreateHash,
    BCryptDestroyHash, BCryptFinishHash, BCryptGenRandom, BCryptHashData,
    BCryptOpenAlgorithmProvider,
};

const STATUS_SUCCESS: NTSTATUS = 0;
const SHA256_LENGTH: usize = 32;

/// `ShellExecuteW` 返回值不大于该值即视为失败（`0`、`SE_ERR_*`）
const SHELL_EXECUTE_ERROR_MAX: isize = 32;

const CTRL_C_EVENT: u32 = 0;
const CTRL_BREAK_EVENT: u32 = 1;
const CTRL_CLOSE_EVENT: u32 = 2;
const CTRL_LOGOFF_EVENT: u32 = 5;
const CTRL_SHUTDOWN_EVENT: u32 = 6;

/// 初始化平台能力：注册控制台事件钩子，让中断/关机事件走统一清理流程
pub fn init() {
    install_shutdown_handler();
}

/// 真实平台上启用自更新
pub const fn self_update_enabled() -> bool {
    true
}

/// 真实平台上文件操作照常执行
pub const fn fs_dry_run() -> bool {
    false
}

/// 让子进程不弹出控制台窗口
pub fn suppress_console_window(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

// ---------------------------------------------------------------------------
// 提权
// ---------------------------------------------------------------------------

struct TokenHandle(HANDLE);

impl TokenHandle {
    const fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    const fn raw(&self) -> HANDLE {
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
    const fn new(path: PathBuf) -> Self {
        Self(path)
    }
}

impl Drop for TempScript {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// 检查当前进程是否具有管理员权限
pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = null_mut();

        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) == 0 {
            return false;
        }

        let token_handle = TokenHandle::new(token);

        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut return_length = 0u32;

        let result = GetTokenInformation(
            token_handle.raw(),
            TokenElevation,
            (&raw mut elevation).cast(),
            dword_len(size_of::<TOKEN_ELEVATION>()),
            &raw mut return_length,
        );

        if result != 0 {
            elevation.TokenIsElevated != 0
        } else {
            false
        }
    }
}

/// 以管理员权限重新启动程序
pub fn elevate_and_restart() -> Result<()> {
    let current_dir = env::current_dir()?;
    let exe_path = env::current_exe()?;

    // 创建一个临时 PowerShell 脚本来执行 Start-Process -Verb RunAs
    let escape = |s: &str| s.replace('"', "\"\"");
    let dir_escaped = escape(&current_dir.display().to_string());
    let exe_escaped = escape(&exe_path.display().to_string());

    let script = format!(
        "Start-Process -FilePath \"{exe_escaped}\" -WorkingDirectory \"{dir_escaped}\" -Verb RunAs"
    );

    let mut script_path = env::temp_dir();
    script_path.push(format!("meta_mystia_elevate_{}.ps1", process::id()));

    fs::write(&script_path, script.as_bytes()).map_err(|e| {
        ManagerError::from(io::Error::new(
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

// ---------------------------------------------------------------------------
// 进程枚举
// ---------------------------------------------------------------------------

struct SnapshotHandle(HANDLE);

impl SnapshotHandle {
    const fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    const fn as_raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for SnapshotHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// 检查游戏进程是否正在运行
pub fn is_game_running() -> Result<bool> {
    unsafe {
        let raw_snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if raw_snapshot == INVALID_HANDLE_VALUE {
            let e = io::Error::last_os_error();
            report_event(
                "Env.GameRunning.CheckFailed.CreateToolhelp32Snapshot",
                Some(&format!("{e}")),
            );
            return Err(ManagerError::ProcessListError(format!(
                "无法获取进程列表：{e}"
            )));
        }
        let snapshot_handle = SnapshotHandle::new(raw_snapshot);
        let snapshot = snapshot_handle.as_raw();

        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = dword_len(size_of::<PROCESSENTRY32W>());

        if Process32FirstW(snapshot, &raw mut entry) == 0 {
            let e = io::Error::last_os_error();
            report_event(
                "Env.GameRunning.CheckFailed.Process32FirstW",
                Some(&format!("{e}")),
            );
            return Err(ManagerError::ProcessListError(format!(
                "读取进程列表失败：{e}"
            )));
        }

        let target = GAME_PROCESS_NAME.to_lowercase();

        loop {
            let process_name = String::from_utf16_lossy(
                &entry.szExeFile[..entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len())],
            );

            if process_name.to_lowercase() == target {
                report_event("Env.GameRunning", None);
                return Ok(true);
            }

            if Process32NextW(snapshot, &raw mut entry) == 0 {
                break;
            }
        }

        Ok(false)
    }
}

// ---------------------------------------------------------------------------
// 系统浏览器
// ---------------------------------------------------------------------------

/// 用系统默认浏览器打开链接
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

// ---------------------------------------------------------------------------
// 密码学原语
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// 控制台事件钩子
// ---------------------------------------------------------------------------

unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> i32 {
    if matches!(
        ctrl_type,
        CTRL_C_EVENT
            | CTRL_BREAK_EVENT
            | CTRL_CLOSE_EVENT
            | CTRL_LOGOFF_EVENT
            | CTRL_SHUTDOWN_EVENT
    ) {
        run_shutdown();
        process::exit(0);
    } else {
        0
    }
}

unsafe extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}

/// 注册控制台事件钩子
pub fn install_shutdown_handler() {
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(console_ctrl_handler), 1);
    }
}
