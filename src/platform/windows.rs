//! Windows 真实实现：提权、进程枚举、系统浏览器、CNG 加密与控制台事件钩子。

use crate::config::GAME_PROCESS_NAME;
use crate::error::{ManagerError, Result};
use crate::telemetry::report_event;

use std::{
    env, io,
    mem::{size_of, zeroed},
    os::windows::{ffi::OsStrExt, process::CommandExt},
    path::Path,
    process::Command,
    ptr::{null, null_mut},
    slice, thread,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, INVALID_HANDLE_VALUE, NTSTATUS,
    },
    Security::{
        Cryptography::{
            BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE, BCRYPT_SHA256_ALGORITHM,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptCloseAlgorithmProvider, BCryptCreateHash,
            BCryptDestroyHash, BCryptFinishHash, BCryptGenRandom, BCryptHashData,
            BCryptOpenAlgorithmProvider,
        },
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    },
    Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    },
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        Threading::{CREATE_NO_WINDOW, CreateMutexW, GetCurrentProcess, OpenProcessToken},
    },
    UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
};

mod machine_id;
mod proxy;
mod registry;
mod window;

pub use machine_id::machine_id;
pub use proxy::{read_system_proxy_settings, resolve_pac_proxy};
pub use window::{
    MAIN_WINDOW_CLASS, focus_existing_manager_window, focus_manager_window, set_main_window,
};

// CNG 加密
const SHA256_LENGTH: usize = 32;
const STATUS_SUCCESS: NTSTATUS = 0;

// 调用方缓冲区远小于 4 GiB，超出 u32 时按上限截断，由 API 自行报错。
fn dword_len(bytes: usize) -> u32 {
    u32::try_from(bytes).unwrap_or(u32::MAX)
}

/// 返回指定路径所在磁盘的剩余空间（字节）。
pub fn free_space(path: &Path) -> Option<u64> {
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<u16>>();
    let mut free: u64 = 0;

    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &raw mut free, null_mut(), null_mut()) };

    (ok != 0).then_some(free)
}

// 提权与单实例
/// 提权重启时传给新进程的参数。
const ELEVATED_RESTART_ARG: &str = "--elevated-restart";
/// 单实例互斥体名称（每个登录会话一个）。
const INSTANCE_MUTEX_NAME: &str = "Local\\meta-mystia-manager";
/// 提权续任进程等待旧进程释放互斥体的间隔。
const INSTANCE_RETRY_INTERVAL: Duration = Duration::from_millis(250);
/// 提权续任进程等待旧进程释放互斥体的次数（合计约 10 秒）。
const INSTANCE_WAIT_ATTEMPTS: usize = 40;
/// `ShellExecuteW` 返回值不大于该值即视为失败（`0`、`SE_ERR_*`）。
const MAX_SHELL_EXECUTE_ERROR: isize = 32;

/// 申请单实例互斥体；已有实例在运行时返回 `false`（调用方把已有窗口带到前台）。
/// 提权重启的续任进程会等旧进程退出、释放互斥体后再继续。
pub fn acquire_single_instance() -> bool {
    let name: Vec<u16> = INSTANCE_MUTEX_NAME.encode_utf16().chain([0]).collect();
    let attempts = if env::args().any(|arg| arg == ELEVATED_RESTART_ARG) {
        INSTANCE_WAIT_ATTEMPTS
    } else {
        1
    };

    for attempt in 0..attempts {
        let handle = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };

        if handle.is_null() {
            // 创建失败时按“没有其它实例”处理，不阻断启动
            return true;
        }

        if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
            // 句柄故意不关闭：作为本进程持有互斥体的凭据，随进程退出释放
            return true;
        }

        unsafe { CloseHandle(handle) };

        if attempt + 1 < attempts {
            thread::sleep(INSTANCE_RETRY_INTERVAL);
        }
    }

    false
}

/// Windows 构建启用管理工具自更新。
pub const fn is_self_update_enabled() -> bool {
    true
}

/// 正式构建不进入演练模式。
pub const fn is_fs_dry_run() -> bool {
    false
}

/// 让外部命令不弹出控制台窗口。
pub fn suppress_console_window(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

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

/// 当前进程是否以管理员权限运行。
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

/// 以管理员权限重新启动管理工具。
pub fn elevate_and_restart() -> Result<()> {
    let exe_path = env::current_exe()?;
    let directory = exe_path.parent().map(Path::to_path_buf).unwrap_or_default();

    let operation: Vec<u16> = "runas\0".encode_utf16().collect();
    let file: Vec<u16> = exe_path.as_os_str().encode_wide().chain([0]).collect();
    let parameters: Vec<u16> = ELEVATED_RESTART_ARG.encode_utf16().chain([0]).collect();
    let directory: Vec<u16> = directory.as_os_str().encode_wide().chain([0]).collect();

    // ShellExecuteW 会弹出 UAC；用户取消时返回 SE_ERR_ACCESSDENIED（5）
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            directory.as_ptr(),
            SW_SHOWNORMAL,
        )
    };
    let code = result as isize;

    if code <= MAX_SHELL_EXECUTE_ERROR {
        report_event("Permission.Elevate.Failed", Some(&code.to_string()));

        return Err(ManagerError::PermissionDenied(format!(
            "无法以管理员身份重新启动（ShellExecuteW 返回 {code}）"
        )));
    }

    report_event("Permission.Elevate.Scheduled", None);

    Ok(())
}

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

/// 游戏进程是否正在运行。
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

        let mut entry: PROCESSENTRY32W = zeroed();
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

/// 用系统默认浏览器打开 URL。
pub fn open_url(url: &str) -> Result<()> {
    let operation: Vec<u16> = "open\0".encode_utf16().collect();
    let target: Vec<u16> = url.encode_utf16().chain([0]).collect();

    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            operation.as_ptr(),
            target.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    let code = result as isize;

    if code <= MAX_SHELL_EXECUTE_ERROR {
        return Err(ManagerError::SsoLoginFailed(format!(
            "无法打开默认浏览器（ShellExecuteW 返回 {code}）"
        )));
    }

    Ok(())
}

/// 读取 PE 文件的产品版本号。
pub fn file_product_version(path: &Path) -> Option<String> {
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut handle = 0u32;
    let size = unsafe { GetFileVersionInfoSizeW(path_wide.as_ptr(), &raw mut handle) };
    if size == 0 {
        return None;
    }

    let mut block = vec![0u8; size as usize];
    let loaded =
        unsafe { GetFileVersionInfoW(path_wide.as_ptr(), 0, size, block.as_mut_ptr().cast()) };
    if loaded == 0 {
        return None;
    }

    let translation_query: Vec<u16> = "\\VarFileInfo\\Translation\0".encode_utf16().collect();
    let mut translations = null_mut();
    let mut translations_len = 0u32;
    let has_translations = unsafe {
        VerQueryValueW(
            block.as_ptr().cast(),
            translation_query.as_ptr(),
            &raw mut translations,
            &raw mut translations_len,
        )
    };
    if has_translations == 0 {
        return None;
    }

    let translations = unsafe {
        slice::from_raw_parts(
            translations.cast::<u16>(),
            translations_len as usize / size_of::<u16>(),
        )
    };

    for language in translations.as_chunks::<2>().0 {
        let product_query: Vec<u16> = format!(
            "\\StringFileInfo\\{:04x}{:04x}\\ProductVersion\0",
            language[0], language[1]
        )
        .encode_utf16()
        .collect();

        let mut value = null_mut();
        let mut value_len = 0u32;
        let has_value = unsafe {
            VerQueryValueW(
                block.as_ptr().cast(),
                product_query.as_ptr(),
                &raw mut value,
                &raw mut value_len,
            )
        };
        if has_value == 0 || value.is_null() {
            continue;
        }

        let value = unsafe { slice::from_raw_parts(value.cast::<u16>(), value_len as usize) };
        let end = value.iter().position(|&c| c == 0).unwrap_or(value.len());
        let text = String::from_utf16_lossy(&value[..end]);

        if !text.is_empty() {
            return Some(text);
        }
    }

    None
}

/// 用 CNG 的系统首选随机数发生器填充缓冲区。
pub fn random_bytes(buffer: &mut [u8]) -> Result<()> {
    let len = u32::try_from(buffer.len())
        .map_err(|_| ManagerError::SsoLoginFailed("随机数长度超出限制".to_string()))?;

    let status = unsafe {
        BCryptGenRandom(
            null_mut(),
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

/// 使用 Windows CNG 计算 SHA-256。
pub fn sha256(data: &[u8]) -> Result<[u8; SHA256_LENGTH]> {
    let len = u32::try_from(data.len())
        .map_err(|_| ManagerError::SsoLoginFailed("待哈希数据长度超出限制".to_string()))?;

    let mut algorithm: BCRYPT_ALG_HANDLE = null_mut();
    let mut hash: BCRYPT_HASH_HANDLE = null_mut();
    let mut digest = [0u8; SHA256_LENGTH];

    unsafe {
        let status =
            BCryptOpenAlgorithmProvider(&raw mut algorithm, BCRYPT_SHA256_ALGORITHM, null(), 0);
        if status != STATUS_SUCCESS {
            return Err(ManagerError::SsoLoginFailed(format!(
                "打开 SHA-256 算法提供程序失败：NTSTATUS {status:#x}"
            )));
        }

        // 默认不提供哈希对象缓冲区，由 CNG 自行分配
        let status = BCryptCreateHash(algorithm, &raw mut hash, null_mut(), 0, null(), 0, 0);
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

unsafe extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}

/// 控制台事件处理器签名。
pub type ConsoleHandler = unsafe extern "system" fn(u32) -> i32;

/// 注册控制台退出事件处理器。
pub fn set_console_ctrl_handler(handler: ConsoleHandler) {
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(handler), 1);
    }
}
