use crate::config::{
    METAMYSTIA_PLUGIN_GLOB, METAMYSTIA_PLUGIN_OLD_GLOB, METAMYSTIA_PLUGIN_PART_GLOB,
    RESOURCEEX_ZIP_GLOB, RESOURCEEX_ZIP_OLD_GLOB, RESOURCEEX_ZIP_PART_GLOB, TEMP_DIR_NAME,
    UninstallMode,
};
use crate::env_check::check_game_running;
use crate::error::ManagerError;
use crate::metrics::report_event;
use crate::model::VersionInfo;
use crate::platform::fs_dry_run;
use crate::ui::{Ui, UiEvent};

use glob::{MatchOptions, glob_with};
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const fn case_insensitive_match_options() -> MatchOptions {
    MatchOptions {
        case_sensitive: false,
        require_literal_leading_dot: false,
        require_literal_separator: false,
    }
}

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

fn ensure_owner_writable(metadata: &fs::Metadata) -> fs::Permissions {
    let mut perms = metadata.permissions();

    #[cfg(unix)]
    {
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

/// 将 `io::Error` 映射为更具体的 `ManagerError`（Windows 上识别“文件被占用”）
pub fn map_io_error_to_uninstall_error(err: &io::Error, path: &Path) -> ManagerError {
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

pub fn atomic_rename_or_copy(src: &Path, dst: &Path) -> Result<(), ManagerError> {
    if fs_dry_run() {
        eprintln!("[dev] 跳过文件写入（模拟）：{}", dst.display());
        return Ok(());
    }

    ensure_game_not_running_for_path(dst)?;

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(ManagerError::from)?;
    }

    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            let mut tmp_path = dst.with_extension("tmp");
            let mut tmp_idx = 0;
            while tmp_path.exists() {
                tmp_idx += 1;
                tmp_path = dst.with_extension(format!("tmp{tmp_idx}"));
            }

            fs::copy(src, &tmp_path).map_err(|e| {
                ManagerError::from(io::Error::other(format!(
                    "重命名 {} 失败：{}；复制到临时文件 {} 失败：{}",
                    src.display(),
                    rename_err,
                    tmp_path.display(),
                    e
                )))
            })?;

            if let Ok(f) = fs::OpenOptions::new().read(true).open(&tmp_path) {
                let _ = f.sync_all();
            }

            match fs::rename(&tmp_path, dst) {
                Ok(()) => {
                    let _ = fs::remove_file(src);
                    Ok(())
                }
                Err(e) => {
                    let _ = fs::remove_file(&tmp_path);
                    Err(ManagerError::from(io::Error::other(format!(
                        "重命名或替换目标 {} 失败：{}",
                        dst.display(),
                        e
                    ))))
                }
            }
        }
    }
}

/// 计算一个尚未占用的备份路径（`.old`、`.old.1`、…）
pub fn next_backup_path(path: &Path, ext_suffix: &str) -> PathBuf {
    let mut idx = 0;

    loop {
        let backup = if idx == 0 {
            path.with_extension(ext_suffix)
        } else {
            path.with_extension(format!("{ext_suffix}.{idx}"))
        };

        if !backup.exists() {
            return backup;
        }

        idx += 1;
    }
}

pub fn backup_to_path(path: &Path, backup: &Path) -> Result<(), ManagerError> {
    ensure_game_not_running_for_path(path)?;

    if !path.exists() {
        return Err(ManagerError::from(io::Error::new(
            io::ErrorKind::NotFound,
            format!("源路径不存在：{}", path.display()),
        )));
    }

    atomic_rename_or_copy(path, backup)
}

/// 把路径转成 glob 模式：目录部分按字面量转义，只有文件名部分保留通配符
fn glob_pattern_string(pattern: &Path) -> String {
    let file_name = pattern
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let Some(parent) = pattern
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return file_name;
    };

    let parent = glob::Pattern::escape(&parent.to_string_lossy().replace('\\', "/"));
    format!("{parent}/{file_name}")
}

fn matches_target_filename(pattern: &str, path: &Path) -> bool {
    let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    match pattern {
        METAMYSTIA_PLUGIN_GLOB => VersionInfo::is_metamystia_filename(filename),
        METAMYSTIA_PLUGIN_OLD_GLOB => {
            VersionInfo::is_canonical_metamystia_backup_filename(filename)
        }
        RESOURCEEX_ZIP_GLOB => VersionInfo::is_resourceex_filename(filename),
        RESOURCEEX_ZIP_OLD_GLOB => VersionInfo::is_canonical_resourceex_backup_filename(filename),
        METAMYSTIA_PLUGIN_PART_GLOB => VersionInfo::is_metamystia_part_filename(filename),
        RESOURCEEX_ZIP_PART_GLOB => VersionInfo::is_resourceex_part_filename(filename),
        _ => true,
    }
}

#[derive(Default)]
pub struct RemoveResult {
    pub failed: Vec<(PathBuf, ManagerError)>,
    pub removed: Vec<PathBuf>,
}

pub fn remove_paths(paths: &[PathBuf]) -> RemoveResult {
    let mut result = RemoveResult::default();

    for entry in paths {
        let deletion = if entry.is_dir() {
            delete_directory(entry)
        } else {
            delete_file(entry)
        };

        match deletion.status {
            DeletionStatus::Success => result.removed.push(entry.clone()),
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

/// 去掉 `.tmp` / `.tmp<数字>` 后缀
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

/// 只清理同目录存在对应目标文件的 `<文件主名>.tmp[数字]`
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

/// 收集崩溃/断电后残留的部署临时文件（只匹配我们自己创建的命名形状）
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

/// 删除残留的部署临时文件；返回实际删除的数量
pub fn cleanup_tmp_residue(game_root: &Path) -> usize {
    let files = collect_tmp_residue(game_root);
    if files.is_empty() {
        return 0;
    }

    let result = remove_paths(&files);
    for (path, error) in &result.failed {
        report_event(
            "TempFile.CleanupFailed",
            Some(&format!("{};err={error}", path.display())),
        );
    }

    result.removed.len()
}

pub fn glob_matches_filtered<F>(pattern: &Path, matcher: F) -> Vec<PathBuf>
where
    F: Fn(&Path) -> bool,
{
    let mut matched_paths = Vec::new();
    let s = glob_pattern_string(pattern);

    if let Ok(entries) = glob_with(&s, case_insensitive_match_options()) {
        for entry in entries.flatten() {
            if entry.exists() && matcher(&entry) {
                matched_paths.push(entry);
            }
        }
    }

    matched_paths
}

/// 根据 glob 模式获取匹配的路径列表，并通过 matcher 进行额外过滤（仅对文件名部分进行过滤）
pub fn glob_matches_by_filename(pattern: &Path, matcher: fn(&str) -> bool) -> Vec<PathBuf> {
    glob_matches_filtered(pattern, |path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(matcher)
    })
}

#[derive(Clone)]
pub enum DeletionStatus {
    Failed(Arc<ManagerError>),
    Skipped,
    Success,
}

#[derive(Clone)]
pub struct DeletionResult {
    pub path: PathBuf,
    pub status: DeletionStatus,
}

pub fn scan_existing_files(base: &Path, mode: UninstallMode) -> Vec<PathBuf> {
    let targets = mode.targets();
    let mut existing_files = Vec::new();

    for &(pattern, is_dir) in targets {
        scan_target(base, pattern, is_dir, &mut existing_files);
    }

    existing_files
}

fn scan_target(base: &Path, pattern: &str, is_directory: bool, existing_files: &mut Vec<PathBuf>) {
    let target_path = base.join(pattern);

    if pattern.contains('*') {
        existing_files.extend(glob_matches_filtered(&target_path, |entry| {
            ((is_directory && entry.is_dir()) || (!is_directory && entry.is_file()))
                && matches_target_filename(pattern, entry)
        }));
    } else if target_path.exists() {
        let is_dir = target_path.is_dir();
        if is_dir == is_directory {
            existing_files.push(target_path);
        }
    }
}

pub fn execute_deletion(files: &[PathBuf], ui: &dyn Ui) -> Vec<DeletionResult> {
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
    if fs_dry_run() {
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
            // 先检测是否为“文件/目录被占用”类错误
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

pub fn extract_failed_files(results: &[DeletionResult]) -> Vec<PathBuf> {
    results
        .iter()
        .filter_map(|r| match &r.status {
            DeletionStatus::Failed(_) => Some(r.path.clone()),
            _ => None,
        })
        .collect()
}

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
