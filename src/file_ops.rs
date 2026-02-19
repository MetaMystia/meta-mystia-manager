use crate::config::{TEMP_DIR_NAME, UninstallMode};
use crate::env_check::check_game_running;
use crate::error::ManagerError;
use crate::ui::Ui;

use glob::glob;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn is_temp_path(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == TEMP_DIR_NAME)
}

fn ensure_game_not_running_for_path(path: &Path) -> Result<(), ManagerError> {
    if is_temp_path(path) {
        return Ok(());
    }
    if check_game_running()? {
        return Err(ManagerError::GameRunning);
    }

    Ok(())
}

fn ensure_owner_writable(metadata: &std::fs::Metadata) -> std::fs::Permissions {
    let mut perms = metadata.permissions();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = perms.mode() | 0o200;
        perms.set_mode(mode);
    }

    #[cfg(not(unix))]
    {
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
    }

    perms
}

#[cfg(windows)]
const ERROR_SHARING_VIOLATION: i32 = 32;

/// 将 io::Error 映射为更具体的 UninstallError
pub fn map_io_error_to_uninstall_error(err: &std::io::Error, path: &Path) -> ManagerError {
    #[cfg(windows)]
    if let Some(code) = err.raw_os_error()
        && code == ERROR_SHARING_VIOLATION
    {
        return ManagerError::FileInUse(path.display().to_string());
    }

    ManagerError::from(std::io::Error::new(err.kind(), err.to_string()))
}

/// 原子重命名或回退到 copy + remove
pub fn atomic_rename_or_copy(src: &Path, dst: &Path) -> Result<(), ManagerError> {
    ensure_game_not_running_for_path(dst)?;

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(ManagerError::from)?;
    }

    match std::fs::rename(src, dst) {
        Ok(_) => Ok(()),
        Err(rename_err) => {
            let mut tmp_path = dst.with_extension("tmp");
            let mut tmp_idx = 0;
            while tmp_path.exists() {
                tmp_idx += 1;
                tmp_path = dst.with_extension(format!("tmp{}", tmp_idx));
            }

            std::fs::copy(src, &tmp_path).map_err(|e| {
                ManagerError::from(std::io::Error::other(format!(
                    "重命名 {} 失败：{}；复制到临时文件 {} 失败：{}",
                    src.display(),
                    rename_err,
                    tmp_path.display(),
                    e
                )))
            })?;

            if let Ok(f) = std::fs::OpenOptions::new().read(true).open(&tmp_path) {
                let _ = f.sync_all();
            }

            match std::fs::rename(&tmp_path, dst) {
                Ok(_) => {
                    let _ = std::fs::remove_file(src);
                    Ok(())
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp_path);
                    Err(ManagerError::from(std::io::Error::other(format!(
                        "重命名或替换目标 {} 失败：{}",
                        dst.display(),
                        e
                    ))))
                }
            }
        }
    }
}

fn backup_with_index(path: &Path, ext_suffix: &str) -> Result<PathBuf, ManagerError> {
    ensure_game_not_running_for_path(path)?;

    if !path.exists() {
        return Err(ManagerError::from(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("源路径不存在：{}", path.display()),
        )));
    }

    let mut idx = 0;
    loop {
        let backup = if idx == 0 {
            path.with_extension(ext_suffix)
        } else {
            path.with_extension(format!("{}.{}", ext_suffix, idx))
        };

        if backup.exists() {
            idx += 1;
            continue;
        }

        match atomic_rename_or_copy(path, &backup) {
            Ok(_) => return Ok(backup),
            Err(e) => {
                if backup.exists() {
                    idx += 1;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

fn normalize_path_for_glob(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub struct RemoveGlobResult {
    pub removed: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, ManagerError)>,
}

/// 删除匹配 glob 模式的文件/目录
pub fn remove_glob_files(pattern: &Path) -> RemoveGlobResult {
    let mut removed = Vec::new();
    let mut failed = Vec::new();

    let pattern_str = normalize_path_for_glob(pattern);
    if let Ok(entries) = glob(&pattern_str) {
        for entry in entries.flatten() {
            if let Err(e) = ensure_game_not_running_for_path(&entry) {
                failed.push((entry, e));
                continue;
            }

            if entry.exists() {
                let res = if entry.is_dir() {
                    std::fs::remove_dir_all(&entry)
                } else {
                    std::fs::remove_file(&entry)
                };

                match res {
                    Ok(_) => removed.push(entry),
                    Err(e) => failed.push((entry, ManagerError::from(e))),
                }
            }
        }
    }

    RemoveGlobResult { removed, failed }
}

/// 备份一组路径（使用 backup_with_index）
pub fn backup_paths_with_index(
    paths: &[PathBuf],
    ext_suffix: &str,
) -> Vec<Result<PathBuf, ManagerError>> {
    paths
        .iter()
        .map(|p| backup_with_index(p, ext_suffix))
        .collect()
}

/// 根据 glob 模式获取匹配的路径列表
pub fn glob_matches(pattern: &Path) -> Vec<PathBuf> {
    let mut matches = Vec::new();
    let s = normalize_path_for_glob(pattern);

    if let Ok(entries) = glob(&s) {
        for entry in entries.flatten() {
            if entry.exists() {
                matches.push(entry);
            }
        }
    }

    matches
}

#[derive(Clone)]
pub enum DeletionStatus {
    Success,
    Failed(Arc<ManagerError>),
    Skipped,
}

#[derive(Clone)]
pub struct DeletionResult {
    pub path: PathBuf,
    pub status: DeletionStatus,
}

/// 扫描实际存在的文件
pub fn scan_existing_files(base: &Path, mode: UninstallMode) -> Vec<PathBuf> {
    let targets = mode.targets();
    let mut existing_files = Vec::new();

    for &(pattern, is_dir) in targets {
        scan_target(base, pattern, is_dir, &mut existing_files);
    }

    existing_files
}

/// 扫描单个删除目标
fn scan_target(base: &Path, pattern: &str, is_directory: bool, existing_files: &mut Vec<PathBuf>) {
    let target_path = base.join(pattern);
    let path_str = normalize_path_for_glob(&target_path);

    if path_str.contains('*') {
        if let Ok(entries) = glob(&path_str) {
            for entry in entries.flatten() {
                if entry.exists()
                    && ((is_directory && entry.is_dir()) || (!is_directory && entry.is_file()))
                {
                    existing_files.push(entry);
                }
            }
        }
    } else if target_path.exists() {
        let is_dir = target_path.is_dir();
        if is_dir == is_directory {
            existing_files.push(target_path);
        }
    }
}

/// 执行删除操作
pub fn execute_deletion(files: &[PathBuf], ui: &dyn Ui) -> Vec<DeletionResult> {
    let total = files.len();
    let mut results = Vec::new();

    let _ = ui.deletion_start();

    for (index, path) in files.iter().enumerate() {
        let _ = ui.deletion_display_progress(index + 1, total, &path.display().to_string());

        let result = if path.is_dir() {
            delete_directory(path)
        } else {
            delete_file(path)
        };

        match &result.status {
            DeletionStatus::Success => {
                let _ = ui.deletion_display_success(&path.display().to_string());
            }
            DeletionStatus::Failed(error) => {
                let _ =
                    ui.deletion_display_failure(&path.display().to_string(), &error.to_string());
            }
            DeletionStatus::Skipped => {
                let _ = ui.deletion_display_skipped(&path.display().to_string());
            }
        }

        results.push(result);
    }

    results
}

/// 删除单个文件
fn delete_file(path: &Path) -> DeletionResult {
    if let Err(e) = ensure_game_not_running_for_path(path) {
        return DeletionResult {
            path: path.to_path_buf(),
            status: DeletionStatus::Failed(Arc::new(e)),
        };
    }

    if !path.exists() {
        return DeletionResult {
            path: path.to_path_buf(),
            status: DeletionStatus::Skipped,
        };
    }

    match std::fs::remove_file(path) {
        Ok(_) => {
            if path.exists() {
                DeletionResult {
                    path: path.to_path_buf(),
                    status: DeletionStatus::Failed(Arc::new(ManagerError::Other(
                        "执行删除后文件仍存在".to_string(),
                    ))),
                }
            } else {
                DeletionResult {
                    path: path.to_path_buf(),
                    status: DeletionStatus::Success,
                }
            }
        }
        Err(e) => {
            // 先检测是否为“文件被占用”类错误
            if let ManagerError::FileInUse(_) = map_io_error_to_uninstall_error(&e, path) {
                return DeletionResult {
                    path: path.to_path_buf(),
                    status: DeletionStatus::Failed(Arc::new(ManagerError::FileInUse(
                        path.display().to_string(),
                    ))),
                };
            }

            // 若为权限错误，尝试清除只读属性并重试一次
            if e.kind() == std::io::ErrorKind::PermissionDenied
                && let Ok(metadata) = std::fs::metadata(path)
            {
                let perms = ensure_owner_writable(&metadata);
                let _ = std::fs::set_permissions(path, perms);
                if std::fs::remove_file(path).is_ok() {
                    return DeletionResult {
                        path: path.to_path_buf(),
                        status: DeletionStatus::Success,
                    };
                }
            }

            let error = match e.kind() {
                std::io::ErrorKind::PermissionDenied => {
                    ManagerError::PermissionDenied(path.display().to_string())
                }
                std::io::ErrorKind::NotFound => {
                    return DeletionResult {
                        path: path.to_path_buf(),
                        status: DeletionStatus::Skipped,
                    };
                }
                _ => map_io_error_to_uninstall_error(&e, path),
            };

            DeletionResult {
                path: path.to_path_buf(),
                status: DeletionStatus::Failed(Arc::new(error)),
            }
        }
    }
}

/// 删除目录
fn delete_directory(path: &Path) -> DeletionResult {
    if let Err(e) = ensure_game_not_running_for_path(path) {
        return DeletionResult {
            path: path.to_path_buf(),
            status: DeletionStatus::Failed(Arc::new(e)),
        };
    }

    if !path.exists() {
        return DeletionResult {
            path: path.to_path_buf(),
            status: DeletionStatus::Skipped,
        };
    }

    match std::fs::remove_dir_all(path) {
        Ok(_) => {
            if path.exists() {
                DeletionResult {
                    path: path.to_path_buf(),
                    status: DeletionStatus::Failed(Arc::new(ManagerError::Other(
                        "执行删除后文件夹仍存在".to_string(),
                    ))),
                }
            } else {
                DeletionResult {
                    path: path.to_path_buf(),
                    status: DeletionStatus::Success,
                }
            }
        }
        Err(e) => {
            // 先检测是否为“文件/目录被占用”类错误
            if let ManagerError::FileInUse(_) = map_io_error_to_uninstall_error(&e, path) {
                return DeletionResult {
                    path: path.to_path_buf(),
                    status: DeletionStatus::Failed(Arc::new(ManagerError::FileInUse(
                        path.display().to_string(),
                    ))),
                };
            }

            // 权限错误时尝试清除只读并重试一次
            if e.kind() == std::io::ErrorKind::PermissionDenied
                && let Ok(metadata) = std::fs::metadata(path)
            {
                let perms = ensure_owner_writable(&metadata);
                let _ = std::fs::set_permissions(path, perms);
                if std::fs::remove_dir_all(path).is_ok() {
                    return DeletionResult {
                        path: path.to_path_buf(),
                        status: DeletionStatus::Success,
                    };
                }
            }

            let error = match e.kind() {
                std::io::ErrorKind::PermissionDenied => {
                    ManagerError::PermissionDenied(path.display().to_string())
                }
                std::io::ErrorKind::NotFound => {
                    return DeletionResult {
                        path: path.to_path_buf(),
                        status: DeletionStatus::Skipped,
                    };
                }
                _ => map_io_error_to_uninstall_error(&e, path),
            };

            DeletionResult {
                path: path.to_path_buf(),
                status: DeletionStatus::Failed(Arc::new(error)),
            }
        }
    }
}

/// 从结果中提取失败的项目
pub fn extract_failed_files(results: &[DeletionResult]) -> Vec<PathBuf> {
    results
        .iter()
        .filter_map(|r| match &r.status {
            DeletionStatus::Failed(_) => Some(r.path.clone()),
            _ => None,
        })
        .collect()
}

/// 统计删除结果
pub fn count_results(results: &[DeletionResult]) -> (usize, usize, usize) {
    let mut success = 0;
    let mut failed = 0;
    let mut skipped = 0;

    for result in results {
        match &result.status {
            DeletionStatus::Success => success += 1,
            DeletionStatus::Failed(_) => failed += 1,
            DeletionStatus::Skipped => skipped += 1,
        }
    }

    (success, failed, skipped)
}
