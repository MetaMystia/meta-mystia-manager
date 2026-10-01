//! 文件操作：原子写入、备份、删除、glob 与残留清理。

mod atomic;
mod delete;
mod glob;

use crate::config::TEMP_DIR_NAME;
use crate::env::check_game_running_cached;
use crate::error::ManagerError;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{fs, path::Path};

pub use atomic::{atomic_rename_or_copy, backup_to_path, next_backup_path};
pub use delete::{
    DeletionResult, DeletionStatus, cleanup_tmp_residue, collect_tmp_residue, count_results,
    delete_paths, extract_failed_files, run_deletion,
};
pub use glob::{glob_matches_by_filename, glob_matches_filtered};

fn is_temp_path(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == TEMP_DIR_NAME)
}

fn ensure_game_not_running_for_path(path: &Path) -> Result<(), ManagerError> {
    if is_temp_path(path) {
        return Ok(());
    }
    if check_game_running_cached()? {
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
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "删除只读文件前需要先清除只读属性"
        )]
        perms.set_readonly(false);
    }

    perms
}
