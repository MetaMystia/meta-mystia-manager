use crate::config::GAME_EXECUTABLE;
use crate::error::{ManagerError, Result};
use crate::metrics::report_event;
use crate::platform;
use crate::ui::Ui;

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

/// 定位游戏根目录：开发模拟模式用沙箱目录，Windows 上先查 Steam 安装位置，再回落到当前目录
pub fn check_game_directory(ui: &dyn Ui) -> Result<PathBuf> {
    // 开发模拟模式使用沙箱目录，不探测本机 Steam
    #[cfg(not(windows))]
    if platform::dev::dev_mode() {
        let root = platform::dev::ensure_sandbox_root()?;
        ui.message(&format!("[dev] 使用沙箱游戏目录：{}", root.display()))?;
        report_event("Env.DevSandbox", Some(&root.display().to_string()));
        return Ok(root);
    }

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
            ui.path_display_steam_found(app.app_id, app.name.as_deref(), &candidate)?;
            if ui.path_confirm_use_steam_found()? {
                ui.blank_line()?;
                report_event("Env.SteamFound", Some(&candidate.display().to_string()));
                return Ok(candidate);
            }
            ui.blank_line()?;
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

static GAME_RUNNING_CACHE: OnceLock<Mutex<(bool, Instant)>> = OnceLock::new();
const CACHE_DURATION: Duration = Duration::from_secs(1);

/// 游戏进程是否正在运行（结果缓存 1 秒，避免短时间内反复枚举进程）
pub fn check_game_running() -> Result<bool> {
    let cache = GAME_RUNNING_CACHE
        .get_or_init(|| Mutex::new((false, Instant::now().checked_sub(CACHE_DURATION).unwrap())));

    let (cached_result, last_check) = *cache.lock().unwrap_or_else(PoisonError::into_inner);

    if last_check.elapsed() < CACHE_DURATION {
        return Ok(cached_result);
    }

    let result = platform::is_game_running()?;
    *cache.lock().unwrap_or_else(PoisonError::into_inner) = (result, Instant::now());

    Ok(result)
}
