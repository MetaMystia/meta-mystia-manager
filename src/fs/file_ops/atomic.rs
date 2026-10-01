//! 原子写入与备份。

use super::ensure_game_not_running_for_path;
use crate::error::ManagerError;
use crate::platform::is_fs_dry_run;

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// 原子地改名；跨盘失败时退化为复制后删除。
pub fn atomic_rename_or_copy(src: &Path, dst: &Path) -> Result<(), ManagerError> {
    if is_fs_dry_run() {
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

/// 计算一个尚未占用的备份路径（`.old`、`.old.1`、…）。
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

/// 把文件改名到备份路径。
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
