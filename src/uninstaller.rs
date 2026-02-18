use crate::config::{RetryConfig, UninstallMode};
use crate::error::{ManagerError, Result};
use crate::file_ops::{
    DeletionStatus, count_results, execute_deletion, extract_failed_files, scan_existing_files,
};
use crate::metrics::report_event;
use crate::permission::{elevate_and_restart, is_elevated};
use crate::shutdown::run_shutdown;
use crate::ui::Ui;

use std::collections::HashSet;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

/// 卸载管理器
pub struct Uninstaller<'a> {
    game_root: PathBuf,
    ui: &'a dyn Ui,
}

impl<'a> Uninstaller<'a> {
    pub fn new(game_root: PathBuf, ui: &'a dyn Ui) -> Result<Self> {
        Ok(Self { game_root, ui })
    }

    /// 执行卸载流程
    pub fn uninstall(&self, mode: Option<UninstallMode>) -> Result<()> {
        report_event("Uninstall.Start", None);

        // 1. 选择卸载模式（如果 mode 存在则使用，否则询问用户）
        let mode = if let Some(m) = mode {
            m
        } else {
            self.ui.uninstall_select_mode()?
        };
        let mode_desc = mode.description();
        report_event("Uninstall.ModeSelected", Some(mode_desc));

        // 2. 扫描实际存在的文件（相对于游戏目录）
        let existing_files = scan_existing_files(&self.game_root, mode);

        if existing_files.is_empty() {
            self.ui.uninstall_no_files_found()?;
            report_event("Uninstall.NoFiles", None);
            return Ok(());
        }

        // 3. 显示将要删除的文件列表
        self.ui.uninstall_display_target_files(&existing_files)?;

        // 4. 确认删除
        if !self.ui.uninstall_confirm_deletion()? {
            report_event("Uninstall.Cancelled", Some(mode_desc));
            return Err(ManagerError::UserCancelled);
        }
        report_event("Uninstall.Confirmed", Some(mode_desc));

        // 5. 检查当前权限状态
        let is_elevated = is_elevated()?;

        // 6. 执行删除操作
        let mut all_results = execute_deletion(&existing_files, self.ui);

        // 7. 处理失败项
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

            if !in_use_failures.is_empty() {
                self.ui.uninstall_files_in_use_warning()?;

                let cfg = RetryConfig::uninstall();
                let mut still_in_use = in_use_failures.clone();

                for attempt in 0..cfg.attempts {
                    if still_in_use.is_empty() {
                        break;
                    }

                    let raw = (cfg.base_delay_secs as f64) * cfg.multiplier.powi(attempt as i32);
                    let delay_secs = raw.min(cfg.max_delay_secs as f64).ceil() as u64;

                    self.ui
                        .uninstall_wait_before_retry(delay_secs, attempt + 1, cfg.attempts)?;

                    sleep(Duration::from_secs(delay_secs));

                    let retry_results = execute_deletion(&still_in_use, self.ui);

                    all_results.retain(|r| !still_in_use.contains(&r.path));
                    all_results.extend(retry_results.clone());

                    still_in_use = extract_failed_files(&all_results)
                        .into_iter()
                        .filter(|p| {
                            if let Some(r) = all_results.iter().find(|r| &r.path == p) {
                                match &r.status {
                                    DeletionStatus::Failed(err) => {
                                        matches!(&**err, ManagerError::FileInUse(_))
                                    }
                                    _ => false,
                                }
                            } else {
                                false
                            }
                        })
                        .collect();
                }

                let failed_files_after_in_use = extract_failed_files(&all_results);
                if failed_files_after_in_use.is_empty() {
                    break;
                }
            }

            let has_permission_issue = all_results.iter().any(|r| match &r.status {
                DeletionStatus::Failed(e) => matches!(&**e, ManagerError::PermissionDenied(_)),
                _ => false,
            });

            if has_permission_issue && !is_elevated && self.ui.uninstall_ask_elevate_permission()? {
                elevate_and_restart()?;
                self.ui.uninstall_restarting_elevated()?;
                run_shutdown();
                std::process::exit(0);
            }

            if !self.ui.uninstall_ask_retry_failures()? {
                break;
            }

            self.ui.uninstall_retrying_failed_items()?;

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

        // 8. 显示操作摘要
        let (success, failed, skipped) = count_results(&all_results);
        self.ui.deletion_display_summary(success, failed, skipped)?;
        report_event(
            "Uninstall.Finished",
            Some(&format!(
                "success:{};failed:{};skipped:{}",
                success, failed, skipped
            )),
        );

        Ok(())
    }
}
