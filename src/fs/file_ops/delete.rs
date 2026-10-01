//! 删除与残留清理。

use super::{ensure_game_not_running_for_path, ensure_owner_writable};
use crate::error::ManagerError;
use crate::platform::is_fs_dry_run;
use crate::telemetry::report_event;
use crate::ui::{Ui, UiEvent};
use crate::version::VersionInfo;

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

// 共享冲突错误码
#[cfg(windows)]
const ERROR_SHARING_VIOLATION: i32 = 32;

/// 将 `io::Error` 映射为更具体的 `ManagerError`（Windows 上识别“文件被占用”）。
fn map_io_error_to_uninstall_error(err: &io::Error, path: &Path) -> ManagerError {
    #[cfg(windows)]
    if let Some(code) = err.raw_os_error()
        && code == ERROR_SHARING_VIOLATION
    {
        return ManagerError::FileInUse(path.display().to_string());
    }

    // 非 Windows 平台没有共享冲突错误码
    #[cfg(not(windows))]
    let _ = path;

    ManagerError::from(io::Error::new(err.kind(), err.to_string()))
}

/// 一次批量删除的结果。
#[derive(Default)]
pub struct DeletionBatch {
    /// 删除失败的路径与原因
    pub failed: Vec<(PathBuf, ManagerError)>,
    /// 已删除的路径
    pub deleted: Vec<PathBuf>,
}

/// 静默删除多个路径，返回成功与失败明细。
pub fn delete_paths(paths: &[PathBuf]) -> DeletionBatch {
    let mut result = DeletionBatch::default();

    for entry in paths {
        let deletion = if entry.is_dir() {
            delete_directory(entry)
        } else {
            delete_file(entry)
        };

        match deletion.status {
            DeletionStatus::Success => result.deleted.push(entry.clone()),
            DeletionStatus::Failed(error) => {
                let error = Arc::try_unwrap(error)
                    .unwrap_or_else(|error| ManagerError::Other(error.to_string()));
                result.failed.push((entry.clone(), error));
            }
            DeletionStatus::Skipped => {}
        }
    }

    result
}

fn strip_tmp_suffix(name: &str) -> Option<&str> {
    if let Some(base) = name.strip_suffix(".tmp") {
        return (!base.is_empty()).then_some(base);
    }

    let (base, digits) = name.rsplit_once(".tmp")?;

    (!base.is_empty() && !digits.is_empty() && digits.chars().all(|ch| ch.is_ascii_digit()))
        .then_some(base)
}

fn is_metamystia_residue_name(name: &str) -> bool {
    let Some(base) = strip_tmp_suffix(name) else {
        return false;
    };

    VersionInfo::is_metamystia_filename(base)
        || VersionInfo::is_metamystia_filename(&format!("{base}.dll"))
}

fn is_resourceex_residue_name(name: &str) -> bool {
    let Some(base) = strip_tmp_suffix(name) else {
        return false;
    };

    VersionInfo::is_resourceex_filename(base)
        || VersionInfo::is_resourceex_filename(&format!("{base}.zip"))
}

/// 只清理同目录存在对应目标文件的 `<文件主名>.tmp[数字]`。
fn collect_bepinex_core_residue(dir: &Path, residue: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let entries = entries.flatten().collect::<Vec<_>>();
    let mut stems = HashSet::new();

    for entry in &entries {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if strip_tmp_suffix(name).is_some() {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            stems.insert(stem.to_ascii_lowercase());
        }
    }

    for entry in entries {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        if path.is_file()
            && let Some(base) = strip_tmp_suffix(name)
            && stems.contains(&base.to_ascii_lowercase())
        {
            residue.push(path);
        }
    }
}

fn is_bepinex_config_residue(name: &str) -> bool {
    strip_tmp_suffix(name).is_some_and(|base| {
        base.eq_ignore_ascii_case("BepInEx") || base.eq_ignore_ascii_case("BepInEx.cfg")
    })
}

// 根目录残留识别
const ROOT_RESIDUE_STEMS: [&str; 5] = [
    ".doorstop_version",
    "changelog",
    "doorstop_config",
    "MinHook.x64",
    "winhttp",
];

fn collect_residue_from(dir: &Path, matcher: fn(&str) -> bool, residue: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        if path.is_file() && matcher(name) {
            residue.push(path);
        }
    }
}

fn collect_root_residue(game_root: &Path, residue: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(game_root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(base) = strip_tmp_suffix(name) else {
            continue;
        };

        if ROOT_RESIDUE_STEMS
            .iter()
            .any(|stem| base.eq_ignore_ascii_case(stem))
        {
            residue.push(path);
        }
    }
}

/// 收集崩溃/断电后残留的部署临时文件（只匹配我们自己创建的命名形状）。
pub fn collect_tmp_residue(game_root: &Path) -> Vec<PathBuf> {
    let mut residue = Vec::new();

    collect_residue_from(
        &game_root.join("BepInEx/plugins"),
        is_metamystia_residue_name,
        &mut residue,
    );
    collect_residue_from(
        &game_root.join("ResourceEx"),
        is_resourceex_residue_name,
        &mut residue,
    );
    collect_bepinex_core_residue(&game_root.join("BepInEx/core"), &mut residue);
    collect_residue_from(
        &game_root.join("BepInEx/config"),
        is_bepinex_config_residue,
        &mut residue,
    );
    collect_root_residue(game_root, &mut residue);

    residue
}

/// 删除残留的部署临时文件；返回实际删除的数量。
pub fn cleanup_tmp_residue(game_root: &Path) -> usize {
    let files = collect_tmp_residue(game_root);
    if files.is_empty() {
        return 0;
    }

    let result = delete_paths(&files);
    for (path, error) in &result.failed {
        report_event(
            "TempFile.CleanupFailed",
            Some(&format!("{};err={error}", path.display())),
        );
    }

    result.deleted.len()
}

/// 单个路径的删除结果。
#[derive(Clone)]
pub enum DeletionStatus {
    /// 删除失败
    Failed(Arc<ManagerError>),
    /// 目标已不存在，跳过
    Skipped,
    /// 删除成功
    Success,
}

/// 单个路径的删除结果与路径。
#[derive(Clone)]
pub struct DeletionResult {
    /// 目标路径
    pub path: PathBuf,
    /// 删除状态
    pub status: DeletionStatus,
}

/// 逐个删除并上报进度，返回每个路径的结果。
pub fn run_deletion(files: &[PathBuf], ui: &dyn Ui) -> Vec<DeletionResult> {
    let total = files.len();
    let mut results = Vec::new();

    let _ = ui.emit(UiEvent::DeletionStart);

    for (index, path) in files.iter().enumerate() {
        let label = if path.is_dir() {
            format!("{}（目录，文件较多时请稍候）", path.display())
        } else {
            path.display().to_string()
        };

        let _ = ui.emit(UiEvent::DeletionProgress(index + 1, total, &label));

        let result = if path.is_dir() {
            delete_directory(path)
        } else {
            delete_file(path)
        };

        match &result.status {
            DeletionStatus::Failed(error) => {
                let _ = ui.emit(UiEvent::DeletionFailure(
                    &path.display().to_string(),
                    &error.to_string(),
                ));
            }
            DeletionStatus::Skipped => {
                let _ = ui.emit(UiEvent::DeletionSkipped(&path.display().to_string()));
            }
            DeletionStatus::Success => {
                let _ = ui.emit(UiEvent::DeletionSuccess(&path.display().to_string()));
            }
        }

        results.push(result);
    }

    results
}

fn delete_file(path: &Path) -> DeletionResult {
    delete_path(path, |path| fs::remove_file(path), "执行删除后文件仍存在")
}

fn delete_directory(path: &Path) -> DeletionResult {
    delete_path(
        path,
        |path| fs::remove_dir_all(path),
        "执行删除后文件夹仍存在",
    )
}

fn delete_path<F>(path: &Path, remove: F, still_exists_message: &str) -> DeletionResult
where
    F: Fn(&Path) -> io::Result<()>,
{
    if is_fs_dry_run() {
        eprintln!("[dev] 跳过删除（模拟）：{}", path.display());
        return deletion_success(path);
    }

    if let Err(e) = ensure_game_not_running_for_path(path) {
        return deletion_failed(path, e);
    }

    if !path.exists() {
        return deletion_skipped(path);
    }

    match remove(path) {
        Ok(()) => {
            if path.exists() {
                deletion_failed(path, ManagerError::Other(still_exists_message.to_string()))
            } else {
                deletion_success(path)
            }
        }
        Err(e) => {
            if let ManagerError::FileInUse(_) = map_io_error_to_uninstall_error(&e, path) {
                return deletion_failed(path, ManagerError::FileInUse(path.display().to_string()));
            }

            // 权限错误时尝试清除只读并重试一次
            if e.kind() == io::ErrorKind::PermissionDenied
                && let Ok(metadata) = fs::metadata(path)
            {
                let perms = ensure_owner_writable(&metadata);
                let _ = fs::set_permissions(path, perms);
                if remove(path).is_ok() {
                    return deletion_success(path);
                }
            }

            let error = match e.kind() {
                io::ErrorKind::PermissionDenied => {
                    ManagerError::PermissionDenied(path.display().to_string())
                }
                io::ErrorKind::NotFound => {
                    return deletion_skipped(path);
                }
                _ => map_io_error_to_uninstall_error(&e, path),
            };

            deletion_failed(path, error)
        }
    }
}

fn deletion_success(path: &Path) -> DeletionResult {
    DeletionResult {
        path: path.to_path_buf(),
        status: DeletionStatus::Success,
    }
}

fn deletion_skipped(path: &Path) -> DeletionResult {
    DeletionResult {
        path: path.to_path_buf(),
        status: DeletionStatus::Skipped,
    }
}

fn deletion_failed(path: &Path, error: ManagerError) -> DeletionResult {
    DeletionResult {
        path: path.to_path_buf(),
        status: DeletionStatus::Failed(Arc::new(error)),
    }
}

/// 从删除结果中提取失败的路径。
pub fn extract_failed_files(results: &[DeletionResult]) -> Vec<PathBuf> {
    results
        .iter()
        .filter_map(|r| match &r.status {
            DeletionStatus::Failed(_) => Some(r.path.clone()),
            _ => None,
        })
        .collect()
}

/// 统计删除结果（成功数，失败数，跳过数）。
pub fn count_results(results: &[DeletionResult]) -> (usize, usize, usize) {
    let mut success = 0;
    let mut failed = 0;
    let mut skipped = 0;

    for result in results {
        match &result.status {
            DeletionStatus::Failed(_) => failed += 1,
            DeletionStatus::Skipped => skipped += 1,
            DeletionStatus::Success => success += 1,
        }
    }

    (success, failed, skipped)
}
