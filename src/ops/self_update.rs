//! 管理工具自更新。

#[cfg(windows)]
use crate::error::ManagerError;
use crate::error::Result;
use crate::fs::temp_dir::create_temp_dir_with_guard;
use crate::net::downloader::Downloader;
#[cfg(windows)]
use crate::platform::{UPDATE_RESTART_ARG, suppress_console_window};
use crate::telemetry::report_event;
use crate::ui::{Ui, UiEvent};
use crate::version::VersionInfo;

use std::path::Path;
#[cfg(windows)]
use std::{env, fs, io, path::PathBuf, process::Command, sync::OnceLock, thread, time::Duration};

#[cfg(windows)]
static REPLACED_EXE: OnceLock<PathBuf> = OnceLock::new();

/// 下载新版本，并以 `--self-update-restart` 模式启动它；旧 exe 由新版本删除。
#[cfg(windows)]
pub fn run_self_update(
    base_dir: &Path,
    ui: &dyn Ui,
    downloader: &Downloader,
    version_info: &VersionInfo,
) -> Result<String> {
    report_event("SelfUpdate.Start", version_info.manager_version());

    let (temp_dir, _guard) = create_temp_dir_with_guard(base_dir)?;
    let filename = version_info.manager_filename()?;
    let temp_path = temp_dir.join(&filename);
    let target_path = base_dir.join(&filename);

    if let Err(e) = downloader.download_manager(version_info, &temp_path) {
        ui.emit(UiEvent::ManagerPromptManualUpdate)?;
        ui.emit(UiEvent::ManagerUpdateFailed(&format!("下载失败：{e}")))?;
        report_event("SelfUpdate.Failed.Download", Some(&format!("{e}")));
        return Err(e);
    }

    if let Err(e) = move_new_version(&temp_path, &target_path) {
        ui.emit(UiEvent::ManagerPromptManualUpdate)?;
        ui.emit(UiEvent::ManagerUpdateFailed(&format!(
            "写入运行目录失败：{e}"
        )))?;
        report_event("SelfUpdate.Failed.Move", Some(&format!("{e}")));
        return Err(ManagerError::from(io::Error::new(
            e.kind(),
            format!("写入运行目录 {} 失败：{e}", target_path.display()),
        )));
    }

    let old_exe = env::current_exe()?;
    let mut command = Command::new(&target_path);
    command
        .arg(UPDATE_RESTART_ARG)
        .arg(&old_exe)
        .current_dir(base_dir);
    suppress_console_window(&mut command);

    if let Err(e) = command.spawn() {
        ui.emit(UiEvent::ManagerPromptManualUpdate)?;
        ui.emit(UiEvent::ManagerUpdateFailed(&format!(
            "启动新版本失败：{e}"
        )))?;
        report_event("SelfUpdate.Failed.Spawn", Some(&format!("{e}")));
        return Err(ManagerError::from(io::Error::new(
            e.kind(),
            format!("启动新版本 {} 失败：{e}", target_path.display()),
        )));
    }

    report_event("SelfUpdate.Scheduled", version_info.manager_version());
    ui.emit(UiEvent::ManagerUpdateStarting)?;

    Ok(filename)
}

#[cfg(windows)]
fn move_new_version(src: &Path, dst: &Path) -> io::Result<()> {
    if fs::rename(src, dst).is_ok() {
        return Ok(());
    }

    fs::copy(src, dst)?;
    let _ = fs::remove_file(src);

    Ok(())
}

/// 记录启动参数里的旧 exe 路径；界面就绪后由 [`remove_replaced_exe`] 删除。
#[cfg(windows)]
pub fn capture_restart_args() {
    let mut args = env::args_os().skip(1);

    while let Some(arg) = args.next() {
        if arg.to_str() != Some(UPDATE_RESTART_ARG) {
            continue;
        }

        if let Some(path) = args.next() {
            let _ = REPLACED_EXE.set(PathBuf::from(path));
        }
        return;
    }
}

/// 删除被替换掉的旧 exe；失败只上报，不影响新版本运行。
#[cfg(windows)]
pub fn remove_replaced_exe() {
    let Some(path) = REPLACED_EXE.get() else {
        return;
    };

    for attempt in 0..5 {
        match fs::remove_file(path) {
            Ok(()) => {
                report_event(
                    "SelfUpdate.Applied",
                    path.file_name().and_then(|name| name.to_str()),
                );
                return;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(_) if attempt < 4 => thread::sleep(Duration::from_millis(200)),
            Err(e) => {
                report_event("SelfUpdate.Failed.Cleanup", Some(&format!("{e}")));
                return;
            }
        }
    }
}

/// 非 Windows 平台：不支持自更新。
#[cfg(not(windows))]
pub fn run_self_update(
    base_dir: &Path,
    ui: &dyn Ui,
    downloader: &Downloader,
    version_info: &VersionInfo,
) -> Result<String> {
    report_event("SelfUpdate.Start", version_info.manager_version());

    let (temp_dir, _guard) = create_temp_dir_with_guard(base_dir)?;
    let filename = version_info.manager_filename()?;
    let temp_path = temp_dir.join(&filename);

    if let Err(e) = downloader.download_manager(version_info, &temp_path) {
        ui.emit(UiEvent::ManagerUpdateFailed(&format!("下载失败：{e}")))?;
        report_event("SelfUpdate.Failed.Download", Some(&format!("{e}")));
        return Err(e);
    }

    report_event("SelfUpdate.Simulated", version_info.manager_version());
    ui.emit(UiEvent::ManagerUpdateStarting)?;
    eprintln!("[dev] 已获取 {filename}，跳过替换正在运行的可执行文件（仅 Windows 支持）");

    Ok(filename)
}
