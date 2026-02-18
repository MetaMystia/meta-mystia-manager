use crate::config::{GAME_EXECUTABLE, GAME_PROCESS_NAME, GAME_STEAM_APP_ID};
use crate::error::{ManagerError, Result};
use crate::metrics::report_event;
use crate::ui::Ui;

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use steamlocate::SteamDir;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

struct SnapshotHandle(HANDLE);

impl SnapshotHandle {
    fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    fn as_raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for SnapshotHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// 检查游戏根目录
pub fn check_game_directory(ui: &dyn Ui) -> Result<PathBuf> {
    if let Ok(steam_dir) = SteamDir::locate()
        && let Ok(Some((app, library))) = steam_dir.find_app(GAME_STEAM_APP_ID)
    {
        let install_dir = app.install_dir;
        let candidate = library
            .path()
            .join("steamapps")
            .join("common")
            .join(&install_dir);
        if candidate.join(GAME_EXECUTABLE).is_file() {
            ui.path_display_steam_found(app.app_id, app.name.as_deref(), &candidate)?;
            if ui.path_confirm_use_steam_found()? {
                ui.blank_line()?;
                report_event("Env.SteamFound", Some(&candidate.display().to_string()));
                return Ok(candidate);
            }
            ui.blank_line()?;
        }
    }

    let current_dir = std::env::current_dir()?;
    let game_exe = current_dir.join(GAME_EXECUTABLE);
    if game_exe.is_file() {
        report_event(
            "Env.CurrentDirFound",
            Some(&current_dir.display().to_string()),
        );
        return Ok(current_dir);
    }

    report_event("Env.GameNotFound", None);

    Err(ManagerError::GameNotFound)
}

static GAME_RUNNING_CACHE: OnceLock<Mutex<(bool, Instant)>> = OnceLock::new();
const CACHE_DURATION: Duration = Duration::from_secs(1);

/// 检查游戏进程是否正在运行
pub fn check_game_running() -> Result<bool> {
    let cache =
        GAME_RUNNING_CACHE.get_or_init(|| Mutex::new((false, Instant::now() - CACHE_DURATION)));

    let mut guard = match cache.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    let (cached_result, last_check) = *guard;

    if last_check.elapsed() < CACHE_DURATION {
        return Ok(cached_result);
    }

    let result = check_game_running_impl()?;
    *guard = (result, Instant::now());

    Ok(result)
}

fn check_game_running_impl() -> Result<bool> {
    unsafe {
        let snapshot_handle = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(handle) => SnapshotHandle::new(handle),
            Err(e) => {
                report_event(
                    "Env.GameRunning.CheckFailed.CreateToolhelp32Snapshot",
                    Some(&format!("{:?}", e)),
                );
                return Err(ManagerError::ProcessListError(format!(
                    "无法获取进程列表：{:?}",
                    e
                )));
            }
        };
        let snapshot = snapshot_handle.as_raw();

        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        match Process32FirstW(snapshot, &mut entry) {
            Ok(()) => {}
            Err(e) => {
                report_event(
                    "Env.GameRunning.CheckFailed.Process32FirstW",
                    Some(&format!("{:?}", e)),
                );
                return Err(ManagerError::ProcessListError(format!(
                    "读取进程列表失败：{:?}",
                    e
                )));
            }
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

            if Process32NextW(snapshot, &mut entry).is_err() {
                break;
            }
        }

        Ok(false)
    }
}
