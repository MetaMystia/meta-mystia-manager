//! 卸载流程。

use crate::config::{
    METAMYSTIA_PLUGIN_GLOB, METAMYSTIA_PLUGIN_OLD_GLOB, METAMYSTIA_PLUGIN_PART_GLOB,
    RESOURCEEX_ZIP_GLOB, RESOURCEEX_ZIP_OLD_GLOB, RESOURCEEX_ZIP_PART_GLOB, TEMP_DIR_NAME,
};
use crate::error::{ManagerError, Result};
use crate::fs::file_ops::{
    DeletionResult, DeletionStatus, collect_tmp_residue, count_results, extract_failed_files,
    glob_matches_filtered, run_deletion,
};
use crate::mode::UninstallMode;
use crate::net::retry::RetryConfig;
use crate::platform::{elevate_and_restart, is_elevated};
use crate::shutdown::run_shutdown;
use crate::telemetry::report_event;
use crate::ui::{Ui, UiEvent};
use crate::version::VersionInfo;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process,
    thread::sleep,
};

fn is_failed_in_use(results: &[DeletionResult], path: &Path) -> bool {
    results
        .iter()
        .find(|r| r.path == path)
        .is_some_and(|r| match &r.status {
            DeletionStatus::Failed(err) => matches!(&**err, ManagerError::FileInUse(_)),
            _ => false,
        })
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

/// 按卸载模式扫描游戏中现有的目标文件。
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

/// 卸载流程。
pub struct Uninstaller<'a> {
    game_root: PathBuf,
    mode: UninstallMode,
    ui: &'a dyn Ui,
}

impl<'a> Uninstaller<'a> {
    /// 创建卸载器，默认使用轻量模式。
    pub const fn new(game_root: PathBuf, ui: &'a dyn Ui) -> Self {
        Self {
            game_root,
            mode: UninstallMode::Light,
            ui,
        }
    }

    /// 设置卸载模式。
    #[must_use]
    pub const fn with_mode(mut self, mode: UninstallMode) -> Self {
        self.mode = mode;
        self
    }

    /// 执行卸载，必要时请求提权或重试。
    #[allow(
        clippy::too_many_lines,
        reason = "卸载流程按步骤线性推进，拆分会让上下文参数来回传递"
    )]
    pub fn uninstall(&self) -> Result<()> {
        report_event("Uninstall.Start", None);

        let mode = self.mode;
        let mode_desc = mode.description();
        report_event("Uninstall.ModeSelected", Some(mode_desc));

        let mut existing_files = scan_existing_files(&self.game_root, mode);

        let temp_dir = self.game_root.join(TEMP_DIR_NAME);
        if temp_dir.exists() && !existing_files.contains(&temp_dir) {
            existing_files.push(temp_dir);
        }

        // 完全卸载会整目录删除，只有根目录残留需要单独列出
        for path in collect_tmp_residue(&self.game_root) {
            if matches!(mode, UninstallMode::Full)
                && path.parent() != Some(self.game_root.as_path())
            {
                continue;
            }
            if !existing_files.contains(&path) {
                existing_files.push(path);
            }
        }

        if existing_files.is_empty() {
            self.ui.emit(UiEvent::UninstallNoFilesFound)?;
            report_event("Uninstall.NoFiles", None);
            return Ok(());
        }

        self.ui
            .emit(UiEvent::UninstallTargetFiles(&existing_files))?;

        if !self
            .ui
            .emit(UiEvent::UninstallConfirmDeletion(mode))?
            .bool()?
        {
            report_event("Uninstall.Cancelled", Some(mode_desc));
            return Err(ManagerError::UserCancelled);
        }
        report_event("Uninstall.Confirmed", Some(mode_desc));

        let is_elevated = is_elevated();

        let mut all_results = run_deletion(&existing_files, self.ui);

        loop {
            let failed_files = extract_failed_files(&all_results);
            if failed_files.is_empty() {
                break;
            }

            let mut in_use_failures = Vec::new();
            let mut perm_failures = Vec::new();
            let mut other_failures = Vec::new();

            for p in &failed_files {
                if let Some(r) = all_results.iter().find(|r| &r.path == p) {
                    match &r.status {
                        DeletionStatus::Failed(e) => match &**e {
                            ManagerError::FileInUse(_) => in_use_failures.push(p.clone()),
                            ManagerError::PermissionDenied(_) => perm_failures.push(p.clone()),
                            _ => other_failures.push(p.clone()),
                        },
                        _ => other_failures.push(p.clone()),
                    }
                } else {
                    other_failures.push(p.clone());
                }
            }

            if !in_use_failures.is_empty()
                && self.retry_in_use_files(&in_use_failures, &mut all_results)?
            {
                break;
            }

            let has_permission_issue = all_results.iter().any(|r| match &r.status {
                DeletionStatus::Failed(e) => matches!(&**e, ManagerError::PermissionDenied(_)),
                _ => false,
            });

            if has_permission_issue
                && !is_elevated
                && self.ui.emit(UiEvent::UninstallAskElevate)?.bool()?
            {
                if let Err(e) = elevate_and_restart() {
                    report_event("Permission.Elevate.Failed", Some(&e.to_string()));
                    return Err(e);
                }
                report_event("Permission.Elevate.Scheduled", None);
                self.ui.emit(UiEvent::UninstallRestartingElevated)?;
                run_shutdown();
                process::exit(0);
            }

            if !self.ui.emit(UiEvent::UninstallAskRetryFailures)?.bool()? {
                break;
            }

            self.ui.emit(UiEvent::UninstallRetryingFailedItems)?;

            let mut seen = HashSet::new();
            let mut retry_list = Vec::new();

            let order = if is_elevated {
                vec![&perm_failures, &other_failures]
            } else {
                vec![&other_failures, &perm_failures]
            };

            for group in order {
                for p in group {
                    if seen.insert(p.clone()) {
                        retry_list.push(p.clone());
                    }
                }
            }

            if !retry_list.is_empty() {
                let retry_results = run_deletion(&retry_list, self.ui);
                all_results.retain(|r| !retry_list.contains(&r.path));
                all_results.extend(retry_results.clone());
            }
        }

        let (success, failed, skipped) = count_results(&all_results);
        self.ui
            .emit(UiEvent::DeletionSummary(success, failed, skipped))?;
        report_event(
            "Uninstall.Finished",
            Some(&format!(
                "success:{success};failed:{failed};skipped:{skipped}"
            )),
        );

        if failed > 0 {
            report_event("Uninstall.Finished.WithFailures", Some(&failed.to_string()));

            return Err(ManagerError::UninstallIncomplete(format!(
                "{failed} 项未能删除，请关闭占用文件的程序或以管理员身份重试"
            )));
        }

        Ok(())
    }

    /// 对「文件被占用」的失败项按退避策略重试删除。
    ///
    /// 返回是否已无任何失败项。
    fn retry_in_use_files(
        &self,
        files: &[PathBuf],
        all_results: &mut Vec<DeletionResult>,
    ) -> Result<bool> {
        self.ui.emit(UiEvent::UninstallFilesInUse)?;

        let cfg = RetryConfig::uninstall();
        let mut still_in_use = files.to_vec();

        for attempt in 0..cfg.attempts {
            if still_in_use.is_empty() {
                break;
            }

            let delay = cfg.delay(attempt);
            self.ui.emit(UiEvent::UninstallWaitBeforeRetry(
                delay.as_secs(),
                attempt + 1,
                cfg.attempts,
            ))?;
            sleep(delay);

            let retry_results = run_deletion(&still_in_use, self.ui);

            all_results.retain(|r| !still_in_use.contains(&r.path));
            all_results.extend(retry_results);

            still_in_use = extract_failed_files(all_results)
                .into_iter()
                .filter(|p| is_failed_in_use(all_results, p))
                .collect();
        }

        Ok(extract_failed_files(all_results).is_empty())
    }
}
