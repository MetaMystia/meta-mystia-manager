use crate::config::{RetryConfig, TEMP_DIR_NAME, UninstallMode};
use crate::error::{ManagerError, Result};
use crate::file_ops::{
    DeletionResult, DeletionStatus, collect_tmp_residue, count_results, execute_deletion,
    extract_failed_files, scan_existing_files,
};
use crate::metrics::report_event;
use crate::platform::{elevate_and_restart, is_elevated};
use crate::shutdown::run_shutdown;
use crate::ui::{Ui, UiEvent};

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process,
    thread::sleep,
};

fn failed_in_use(results: &[DeletionResult], path: &Path) -> bool {
    results
        .iter()
        .find(|r| r.path == path)
        .is_some_and(|r| match &r.status {
            DeletionStatus::Failed(err) => matches!(&**err, ManagerError::FileInUse(_)),
            _ => false,
        })
}

pub struct Uninstaller<'a> {
    game_root: PathBuf,
    mode: UninstallMode,
    ui: &'a dyn Ui,
}

impl<'a> Uninstaller<'a> {
    pub const fn new(game_root: PathBuf, ui: &'a dyn Ui) -> Self {
        Self {
            game_root,
            mode: UninstallMode::Light,
            ui,
        }
    }

    #[must_use]
    pub const fn with_mode(mut self, mode: UninstallMode) -> Self {
        self.mode = mode;
        self
    }

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

        let mut all_results = execute_deletion(&existing_files, self.ui);

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
                elevate_and_restart()?;
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
                let retry_results = execute_deletion(&retry_list, self.ui);
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

    /// 对「文件被占用」的失败项按退避策略重试删除
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

            let retry_results = execute_deletion(&still_in_use, self.ui);

            all_results.retain(|r| !still_in_use.contains(&r.path));
            all_results.extend(retry_results);

            still_in_use = extract_failed_files(all_results)
                .into_iter()
                .filter(|p| failed_in_use(all_results, p))
                .collect();
        }

        Ok(extract_failed_files(all_results).is_empty())
    }
}
