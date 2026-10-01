//! 游戏目录定位与游戏进程检测。

use crate::config::GAME_EXECUTABLE;
use crate::error::{ManagerError, Result};
use crate::platform::is_game_running;
use crate::telemetry::report_event;
use crate::ui::Ui;

#[cfg(windows)]
use crate::ui::UiEvent;

use std::{
    env,
    path::PathBuf,
    sync::{Mutex, OnceLock, PoisonError},
    time::{Duration, Instant},
};

#[cfg(windows)]
use crate::config::GAME_STEAM_APP_ID;
#[cfg(windows)]
use steamlocate::SteamDir;

/// 定位游戏根目录：Windows 上先查 Steam 安装位置，再回落到当前目录。
pub fn check_game_directory(ui: &dyn Ui) -> Result<PathBuf> {
    #[cfg(not(windows))]
    let _ = ui;

    #[cfg(windows)]
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
            ui.emit(UiEvent::SteamFound {
                app_id: app.app_id,
                name: app.name.as_deref(),
                path: &candidate,
            })?;
            report_event("Env.SteamFound", Some(&candidate.display().to_string()));

            return Ok(candidate);
        }
    }

    let current_dir = env::current_dir()?;
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

const CACHE_DURATION: Duration = Duration::from_secs(1);
static GAME_RUNNING_CACHE: OnceLock<Mutex<Option<(bool, Instant)>>> = OnceLock::new();

/// 游戏进程是否正在运行（结果缓存 1 秒，避免短时间内反复枚举进程）。
pub fn check_game_running_cached() -> Result<bool> {
    let cache = GAME_RUNNING_CACHE.get_or_init(|| Mutex::new(None));
    let cached = *cache.lock().unwrap_or_else(PoisonError::into_inner);

    if let Some((result, checked_at)) = cached
        && checked_at.elapsed() < CACHE_DURATION
    {
        return Ok(result);
    }

    let result = query_game_running()?;
    *cache.lock().unwrap_or_else(PoisonError::into_inner) = Some((result, Instant::now()));

    Ok(result)
}

/// 强制重新检测游戏进程是否正在运行，跳过缓存。
pub fn check_game_running() -> Result<bool> {
    let result = query_game_running()?;
    let cache = GAME_RUNNING_CACHE.get_or_init(|| Mutex::new(None));
    *cache.lock().unwrap_or_else(PoisonError::into_inner) = Some((result, Instant::now()));

    Ok(result)
}

fn query_game_running() -> Result<bool> {
    let running = is_game_running().inspect_err(|e| {
        report_event("Env.GameRunning.CheckFailed", Some(&e.to_string()));
    })?;

    if running {
        report_event("Env.GameRunning", None);
    }

    Ok(running)
}
