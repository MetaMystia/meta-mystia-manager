//! 部署回滚：部署前备份将被覆盖的文件、记录将被新建的文件，
//! 部署失败时还原到操作前状态，成功时丢弃备份。

use crate::error::{ManagerError, Result};
use crate::extractor::Extractor;
use crate::metrics::report_event;

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// 回滚上下文；生命周期内只用于一次部署
pub struct Rollback {
    game_root: PathBuf,
    backup_dir: PathBuf,
    created: Vec<PathBuf>,
    overwritten: Vec<PathBuf>,
}

impl Rollback {
    pub fn new(game_root: &Path, temp_dir: &Path) -> Self {
        Self {
            backup_dir: temp_dir.join("rollback"),
            created: Vec::new(),
            game_root: game_root.to_path_buf(),
            overwritten: Vec::new(),
        }
    }

    /// 备份一个部署目标（不存在则记录为本次新建）
    pub fn plan(&mut self, target: &Path) -> Result<()> {
        let Ok(relative) = target.strip_prefix(&self.game_root) else {
            return Ok(());
        };

        if target.is_file() {
            let backup = self.backup_dir.join(relative);

            if let Some(parent) = backup.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    ManagerError::from(io::Error::new(
                        e.kind(),
                        format!("创建回滚备份目录 {} 失败：{}", parent.display(), e),
                    ))
                })?;
            }

            fs::copy(target, &backup).map_err(|e| {
                ManagerError::from(io::Error::new(
                    e.kind(),
                    format!("备份文件 {} 失败：{}", target.display(), e),
                ))
            })?;
            self.overwritten.push(relative.to_path_buf());
        } else if !target.exists() {
            self.created.push(relative.to_path_buf());
        }

        Ok(())
    }

    /// 按 ZIP 内容备份所有会被覆盖的文件
    pub fn plan_zip(&mut self, zip_path: &Path, exclude_patterns: &[&str]) -> Result<()> {
        for entry in Extractor::list_zip_entries(zip_path, exclude_patterns)? {
            self.plan(&self.game_root.join(entry))?;
        }

        Ok(())
    }

    /// 部署成功：丢弃备份
    pub fn discard(self) {
        let _ = fs::remove_dir_all(&self.backup_dir);
    }

    /// 部署失败：删除本次新建的文件并还原被覆盖的文件
    pub fn restore(self) -> Result<()> {
        for relative in &self.created {
            let target = self.game_root.join(relative);
            if target.is_file() {
                let _ = fs::remove_file(&target);
            }
        }

        for relative in &self.overwritten {
            let backup = self.backup_dir.join(relative);
            let target = self.game_root.join(relative);

            fs::copy(&backup, &target).map_err(|e| {
                ManagerError::from(io::Error::new(
                    e.kind(),
                    format!("还原文件 {} 失败：{}", target.display(), e),
                ))
            })?;
        }

        let _ = fs::remove_dir_all(&self.backup_dir);
        report_event(
            "Rollback.Restored",
            Some(&format!(
                "created={};overwritten={}",
                self.created.len(),
                self.overwritten.len()
            )),
        );

        Ok(())
    }
}
