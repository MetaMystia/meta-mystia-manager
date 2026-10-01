//! 部署回滚：部署前备份将被覆盖/删除的文件、记录将被新建的文件，
//! 部署失败时还原到操作前状态，成功时丢弃备份。
//! 真正修改磁盘前会把计划写入 journal，进程被中断后可在下次启动时恢复。

use crate::config::TEMP_DIR_NAME;
use crate::error::{ManagerError, Result};
use crate::fs::extractor::Extractor;
use crate::fs::file_ops::atomic_rename_or_copy;
use crate::telemetry::report_event;

use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
struct RollbackJournal {
    game_root: PathBuf,
    created: Vec<PathBuf>,
    overwritten: Vec<PathBuf>,
    removed: Vec<PathBuf>,
    renamed: Vec<(PathBuf, PathBuf)>,
}

/// 回滚上下文；生命周期内只用于一次部署。
pub struct Rollback {
    backup_dir: PathBuf,
    created: Vec<PathBuf>,
    game_root: PathBuf,
    journal_path: PathBuf,
    overwritten: Vec<PathBuf>,
    /// 清理前拷走的路径（相对于游戏根目录），恢复时拷回
    removed: Vec<PathBuf>,
    /// 改名备份：`(备份路径, 原路径)`，恢复时改回原名
    renamed: Vec<(PathBuf, PathBuf)>,
}

impl Rollback {
    /// 为一次部署创建回滚上下文。
    pub fn new(game_root: &Path, temp_dir: &Path) -> Self {
        let backup_dir = temp_dir.join("rollback");

        Self {
            backup_dir: backup_dir.clone(),
            created: Vec::new(),
            game_root: game_root.to_path_buf(),
            journal_path: backup_dir.join("journal.json"),
            overwritten: Vec::new(),
            removed: Vec::new(),
            renamed: Vec::new(),
        }
    }

    /// 备份一个部署目标（不存在则记录为本次新建）。
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

    /// 按 ZIP 内容备份所有会被覆盖的文件。
    pub fn plan_zip(&mut self, zip_path: &Path, exclude_patterns: &[&str]) -> Result<()> {
        for entry in Extractor::list_zip_entries(zip_path, exclude_patterns)? {
            self.plan(&self.game_root.join(entry))?;
        }

        Ok(())
    }

    /// 备份一个即将被清理删除的路径（文件或目录树），恢复时原样拷回。
    pub fn plan_removal(&mut self, target: &Path) -> Result<()> {
        let Ok(relative) = target.strip_prefix(&self.game_root) else {
            return Ok(());
        };

        if !target.exists() {
            return Ok(());
        }

        let backup = self.backup_dir.join("removed").join(relative);
        if let Some(parent) = backup.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                ManagerError::from(io::Error::new(
                    e.kind(),
                    format!("创建回滚备份目录 {} 失败：{}", parent.display(), e),
                ))
            })?;
        }

        copy_tree(target, &backup).map_err(|e| {
            ManagerError::from(io::Error::new(
                e.kind(),
                format!("备份待清理路径 {} 失败：{}", target.display(), e),
            ))
        })?;
        self.removed.push(relative.to_path_buf());

        Ok(())
    }

    /// 记录一次改名备份（如旧版本文件改成 `.old`），恢复时改回原名。
    pub fn plan_rename(&mut self, original: &Path, backup: &Path) {
        let Ok(original_relative) = original.strip_prefix(&self.game_root) else {
            return;
        };
        let Ok(backup_relative) = backup.strip_prefix(&self.game_root) else {
            return;
        };

        self.renamed.push((
            backup_relative.to_path_buf(),
            original_relative.to_path_buf(),
        ));
    }

    /// 在真正修改磁盘前把当前计划落盘；恢复时不依赖进程内状态。
    pub fn arm(&self) -> Result<()> {
        if self.created.is_empty()
            && self.overwritten.is_empty()
            && self.removed.is_empty()
            && self.renamed.is_empty()
        {
            return Ok(());
        }

        let journal = RollbackJournal {
            game_root: self.game_root.clone(),
            created: self.created.clone(),
            overwritten: self.overwritten.clone(),
            removed: self.removed.clone(),
            renamed: self.renamed.clone(),
        };
        let text = serde_json::to_vec_pretty(&journal)
            .map_err(|e| ManagerError::Other(format!("写入回滚日志失败：{e}")))?;

        fs::create_dir_all(&self.backup_dir).map_err(|e| {
            ManagerError::from(io::Error::new(
                e.kind(),
                format!("创建回滚目录 {} 失败：{}", self.backup_dir.display(), e),
            ))
        })?;

        let tmp_path = self.journal_path.with_extension("json.tmp");
        fs::write(&tmp_path, text).map_err(|e| {
            ManagerError::from(io::Error::new(
                e.kind(),
                format!("写入回滚日志 {} 失败：{}", tmp_path.display(), e),
            ))
        })?;
        fs::rename(&tmp_path, &self.journal_path).map_err(|e| {
            ManagerError::from(io::Error::new(
                e.kind(),
                format!("替换回滚日志 {} 失败：{}", self.journal_path.display(), e),
            ))
        })?;

        report_event(
            "Rollback.Armed",
            Some(&format!(
                "created={};overwritten={};removed={};renamed={}",
                self.created.len(),
                self.overwritten.len(),
                self.removed.len(),
                self.renamed.len()
            )),
        );

        Ok(())
    }

    /// 部署成功：丢弃备份。
    pub fn discard(self) {
        // 先让 journal 失效，避免清理失败时下次启动误恢复已成功的操作
        let done = self.journal_path.with_extension("json.done");
        let _ = fs::write(&done, b"done");
        let _ = fs::remove_file(&self.journal_path);

        let _ = fs::remove_dir_all(&self.backup_dir);
    }

    /// 部署失败：删除本次新建的文件、恢复清理删除与改名备份、还原被覆盖的文件。
    /// 单项失败不会中断其余还原，最终把失败原因汇总返回。
    pub fn restore(self) -> Result<()> {
        let result = restore_plan(
            &self.game_root,
            &self.backup_dir,
            &self.created,
            &self.overwritten,
            &self.removed,
            &self.renamed,
        );

        if result.is_ok() {
            let _ = fs::remove_dir_all(&self.backup_dir);
        }

        result
    }
}

/// 上次安装/升级被中断时，用落盘的回滚计划恢复；返回是否执行了恢复。
pub fn recover_interrupted(game_root: &Path) -> Result<bool> {
    let backup_dir = game_root.join(TEMP_DIR_NAME).join("rollback");
    let journal_path = backup_dir.join("journal.json");

    if backup_dir.join("journal.json.done").is_file() {
        return Ok(false);
    }

    if !journal_path.is_file() {
        return Ok(false);
    }

    let text = fs::read_to_string(&journal_path).map_err(|e| {
        ManagerError::from(io::Error::new(
            e.kind(),
            format!("读取回滚日志 {} 失败：{}", journal_path.display(), e),
        ))
    })?;
    let journal: RollbackJournal = serde_json::from_str(&text).map_err(|e| {
        ManagerError::Other(format!(
            "解析回滚日志 {} 失败：{}",
            journal_path.display(),
            e
        ))
    })?;

    if !same_path(&journal.game_root, game_root) {
        report_event(
            "Rollback.Recovery.Skipped",
            Some(&format!(
                "journal={};target={}",
                journal.game_root.display(),
                game_root.display()
            )),
        );

        return Ok(false);
    }

    restore_plan(
        game_root,
        &backup_dir,
        &journal.created,
        &journal.overwritten,
        &journal.removed,
        &journal.renamed,
    )?;
    let _ = fs::remove_dir_all(&backup_dir);
    report_event("Rollback.Recovery.Success", None);

    Ok(true)
}

fn restore_plan(
    game_root: &Path,
    backup_dir: &Path,
    created: &[PathBuf],
    overwritten: &[PathBuf],
    removed: &[PathBuf],
    renamed: &[(PathBuf, PathBuf)],
) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    for relative in created {
        let target = game_root.join(relative);
        if target.is_file()
            && let Err(e) = fs::remove_file(&target)
        {
            errors.push(format!("删除新建文件 {} 失败：{e}", target.display()));
        }
    }

    for relative in removed.iter().rev() {
        let backup = backup_dir.join("removed").join(relative);
        let target = game_root.join(relative);
        if !backup.exists() {
            continue;
        }

        if target.exists() {
            let removal = if target.is_dir() {
                fs::remove_dir_all(&target)
            } else {
                fs::remove_file(&target)
            };

            if let Err(e) = removal {
                errors.push(format!("清理回滚目标 {} 失败：{e}", target.display()));
                continue;
            }
        }

        if let Err(e) = copy_tree(&backup, &target) {
            errors.push(format!("恢复 {} 失败：{e}", target.display()));
        }
    }

    for (backup_relative, original_relative) in renamed.iter().rev() {
        let backup = game_root.join(backup_relative);
        let original = game_root.join(original_relative);
        if !backup.exists() {
            continue;
        }

        if let Err(e) = atomic_rename_or_copy(&backup, &original) {
            errors.push(format!("恢复 {} 失败：{e}", original.display()));
        }
    }

    for relative in overwritten {
        let backup = backup_dir.join(relative);
        let target = game_root.join(relative);

        if let Err(e) = fs::copy(&backup, &target) {
            errors.push(format!("还原文件 {} 失败：{e}", target.display()));
        }
    }

    if errors.is_empty() {
        report_event(
            "Rollback.Restored",
            Some(&format!(
                "created={};overwritten={};removed={};renamed={}",
                created.len(),
                overwritten.len(),
                removed.len(),
                renamed.len()
            )),
        );

        return Ok(());
    }

    let summary = errors.join("；");
    report_event("Rollback.RestoreFailed", Some(&summary));

    Err(ManagerError::Other(summary))
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    if source.is_dir() {
        fs::create_dir_all(destination)?;

        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }

        return Ok(());
    }

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, destination).map(|_| ())
}
