use crate::config::{OperationMode, UninstallMode};
use crate::error::{ManagerError, Result};
use crate::model::VersionInfo;
use crate::ui::Ui;

use std::{
    cmp::min,
    path::{Path, PathBuf},
};

/// CLI UI 实现
pub struct CliUI {
    quiet: bool,
}

impl CliUI {
    pub fn new(quiet: bool) -> Self {
        Self { quiet }
    }

    fn fixed_choice(&self, choice: bool) -> Result<bool> {
        Ok(choice)
    }

    fn unsupported_interaction<T>(&self, action: &str) -> Result<T> {
        Err(ManagerError::Ui(format!(
            "CLI UI 不支持交互操作：{}",
            action
        )))
    }

    fn stderr(&self, msg: &str) {
        eprintln!("{}", msg);
    }

    fn stdout(&self, msg: &str) {
        if !self.quiet {
            println!("{}", msg);
        }
    }
}

impl Ui for CliUI {
    fn display_welcome(&self) -> Result<()> {
        Ok(())
    }

    fn display_version(&self, manager_version: Option<&str>) -> Result<()> {
        if let Some(version) = manager_version {
            self.stdout(&format!("Manager latest version: {}", version));
        }
        Ok(())
    }

    fn display_game_running_warning(&self) -> Result<()> {
        self.stderr("Game is currently running. Please close the game and try again.");
        Ok(())
    }

    fn display_available_updates(
        &self,
        bepinex_available: bool,
        dll_available: bool,
        resourceex_available: bool,
    ) -> Result<()> {
        if bepinex_available {
            self.stdout("BepInEx update available.");
        }
        if dll_available {
            self.stdout("MetaMystia DLL update available.");
        }
        if resourceex_available {
            self.stdout("ResourceExample ZIP update available.");
        }
        Ok(())
    }

    fn select_operation_mode(&self) -> Result<OperationMode> {
        self.unsupported_interaction("select_operation_mode")
    }

    fn blank_line(&self) -> Result<()> {
        Ok(())
    }

    fn wait_for_key(&self) -> Result<()> {
        Ok(())
    }

    fn message(&self, text: &str) -> Result<()> {
        self.stdout(text);
        Ok(())
    }

    fn warn(&self, text: &str) -> Result<()> {
        self.stderr(&format!("Warning: {}", text));
        Ok(())
    }

    fn error(&self, text: &str) -> Result<()> {
        self.stderr(&format!("Error: {}", text));
        Ok(())
    }

    fn path_display_steam_found(&self, app_id: u32, name: Option<&str>, path: &Path) -> Result<()> {
        self.stdout(&format!(
            "Found Steam game: {} (AppID: {}) at {}",
            name.unwrap_or("Unknown"),
            app_id,
            path.display()
        ));
        Ok(())
    }

    fn path_confirm_use_steam_found(&self) -> Result<bool> {
        self.fixed_choice(true)
    }

    fn install_display_step(&self, step: usize, description: &str) -> Result<()> {
        self.stdout(&format!("[Step {}] {}", step, description));
        Ok(())
    }

    fn install_display_version_info(&self, version_info: &VersionInfo) -> Result<()> {
        self.stdout(&format!(
            "Versions - MetaMystia DLL: {}, ResourceExample ZIP: {}, BepInEx: {}",
            version_info.latest_dll(),
            version_info.latest_resourceex(),
            version_info.bepinex_version()?
        ));
        Ok(())
    }

    fn install_warn_existing(
        &self,
        bepinex_installed: bool,
        metamystia_installed: bool,
        resourceex_installed: bool,
    ) -> Result<()> {
        if bepinex_installed || metamystia_installed || resourceex_installed {
            self.stdout("Existing installation detected, will overwrite.");
        }
        Ok(())
    }

    fn install_confirm_overwrite(&self) -> Result<bool> {
        self.fixed_choice(true)
    }

    fn install_ask_install_resourceex(&self) -> Result<bool> {
        self.unsupported_interaction("install_ask_install_resourceex")
    }

    fn install_ask_show_bepinex_console(&self) -> Result<bool> {
        self.unsupported_interaction("install_ask_show_bepinex_console")
    }

    fn install_downloads_completed(&self) -> Result<()> {
        Ok(())
    }

    fn install_start_cleanup(&self) -> Result<()> {
        self.stdout("Cleaning up old files...");
        Ok(())
    }

    fn install_cleanup_result(&self, success_count: usize, failed_count: usize) -> Result<()> {
        self.stdout(&format!(
            "Cleanup: {} succeeded, {} failed.",
            success_count, failed_count
        ));
        Ok(())
    }

    fn install_finished(&self, show_bepinex_console: bool) -> Result<()> {
        if show_bepinex_console {
            self.stdout("BepInEx console will be shown on game startup.");
        }

        self.stdout("Installation completed successfully.");

        Ok(())
    }

    fn upgrade_warn_unparse_version(&self, filename: &str) -> Result<()> {
        self.stderr(&format!(
            "Warning: Unable to parse version from {}",
            filename
        ));
        Ok(())
    }

    fn upgrade_backup_failed(&self, err: &str) -> Result<()> {
        self.stderr(&format!("Backup failed: {}", err));
        Ok(())
    }

    fn upgrade_deleted(&self, path: &Path) -> Result<()> {
        self.stdout(&format!("Deleted: {}", path.display()));
        Ok(())
    }

    fn upgrade_delete_failed(&self, path: &Path, err: &str) -> Result<()> {
        self.stderr(&format!("Failed to delete {}: {}", path.display(), err));
        Ok(())
    }

    fn upgrade_checking_installed_version(&self) -> Result<()> {
        self.stdout("Checking installed version...");
        Ok(())
    }

    fn upgrade_detected_resourceex(&self) -> Result<()> {
        self.stdout("ResourceExample ZIP detected.");
        Ok(())
    }

    fn upgrade_display_current_and_latest_bepinex(
        &self,
        current: &str,
        latest: &str,
    ) -> Result<()> {
        self.stdout(&format!(
            "BepInEx - Current: {}, Latest: {}",
            current, latest
        ));
        Ok(())
    }

    fn upgrade_display_current_and_latest_dll(&self, current: &str, latest: &str) -> Result<()> {
        self.stdout(&format!(
            "MetaMystia DLL - Current: {}, Latest: {}",
            current, latest
        ));
        Ok(())
    }

    fn upgrade_display_current_and_latest_resourceex(
        &self,
        current: &str,
        latest: &str,
    ) -> Result<()> {
        self.stdout(&format!(
            "ResourceExample ZIP - Current: {}, Latest: {}",
            current, latest
        ));
        Ok(())
    }

    fn upgrade_no_update_needed(&self) -> Result<()> {
        self.stdout("All components are up to date.");
        Ok(())
    }

    fn upgrade_bepinex_needs_upgrade(&self) -> Result<()> {
        self.stdout("BepInEx needs upgrade.");
        Ok(())
    }

    fn upgrade_bepinex_already_latest(&self) -> Result<()> {
        self.stdout("BepInEx is already the latest version.");
        Ok(())
    }

    fn upgrade_detected_new_dll(&self, current: &str, new: &str) -> Result<()> {
        self.stdout(&format!(
            "New MetaMystia DLL version available: {} -> {}",
            current, new
        ));
        Ok(())
    }

    fn upgrade_dll_already_latest(&self) -> Result<()> {
        self.stdout("MetaMystia DLL is already the latest version.");
        Ok(())
    }

    fn upgrade_resourceex_needs_upgrade(&self) -> Result<()> {
        self.stdout("ResourceExample ZIP needs upgrade.");
        Ok(())
    }

    fn upgrade_downloading_bepinex(&self) -> Result<()> {
        self.stdout("Downloading BepInEx...");
        Ok(())
    }

    fn upgrade_downloading_dll(&self) -> Result<()> {
        self.stdout("Downloading MetaMystia DLL...");
        Ok(())
    }

    fn upgrade_downloading_resourceex(&self) -> Result<()> {
        self.stdout("Downloading ResourceExample ZIP...");
        Ok(())
    }

    fn upgrade_installing_bepinex(&self) -> Result<()> {
        self.stdout("Installing BepInEx...");
        Ok(())
    }

    fn upgrade_installing_dll(&self) -> Result<()> {
        self.stdout("Installing MetaMystia DLL...");
        Ok(())
    }

    fn upgrade_installing_resourceex(&self) -> Result<()> {
        self.stdout("Installing ResourceExample ZIP...");
        Ok(())
    }

    fn upgrade_install_success(&self, path: &Path) -> Result<()> {
        self.stdout(&format!("Successfully installed: {}", path.display()));
        Ok(())
    }

    fn upgrade_cleanup_start(&self) -> Result<()> {
        self.stdout("Cleaning up old files...");
        Ok(())
    }

    fn upgrade_done(&self) -> Result<()> {
        self.stdout("Upgrade completed successfully");
        Ok(())
    }

    fn uninstall_select_mode(&self) -> Result<UninstallMode> {
        self.unsupported_interaction("uninstall_select_mode")
    }

    fn uninstall_no_files_found(&self) -> Result<()> {
        self.stdout("No files to uninstall.");
        Ok(())
    }

    fn uninstall_display_target_files(&self, files: &[PathBuf]) -> Result<()> {
        self.stdout(&format!("Files to be deleted: {}", files.len()));
        Ok(())
    }

    fn uninstall_confirm_deletion(&self) -> Result<bool> {
        self.fixed_choice(true)
    }

    fn uninstall_files_in_use_warning(&self) -> Result<()> {
        self.stderr("Warning: Some files are in use, will retry.");
        Ok(())
    }

    fn uninstall_wait_before_retry(
        &self,
        delay_secs: u64,
        attempt: usize,
        attempts: usize,
    ) -> Result<()> {
        self.stdout(&format!(
            "Waiting {} seconds before retry {}/{}...",
            delay_secs, attempt, attempts
        ));
        Ok(())
    }

    fn uninstall_ask_elevate_permission(&self) -> Result<bool> {
        self.fixed_choice(true)
    }

    fn uninstall_restarting_elevated(&self) -> Result<()> {
        self.stdout("Restarting with elevated permissions...");
        Ok(())
    }

    fn uninstall_ask_retry_failures(&self) -> Result<bool> {
        self.fixed_choice(true)
    }

    fn uninstall_retrying_failed_items(&self) -> Result<()> {
        self.stdout("Retrying failed items...");
        Ok(())
    }

    fn deletion_start(&self) -> Result<()> {
        Ok(())
    }

    fn deletion_display_progress(&self, current: usize, total: usize, path: &str) -> Result<()> {
        self.stdout(&format!("[{}/{}] Deleting: {}", current, total, path));
        Ok(())
    }

    fn deletion_display_success(&self, path: &str) -> Result<()> {
        self.stdout(&format!("Deleted: {}", path));
        Ok(())
    }

    fn deletion_display_failure(&self, path: &str, error: &str) -> Result<()> {
        self.stderr(&format!("Failed to delete {}: {}", path, error));
        Ok(())
    }

    fn deletion_display_skipped(&self, path: &str) -> Result<()> {
        self.stdout(&format!("Skipped: {}", path));
        Ok(())
    }

    fn deletion_display_summary(
        &self,
        success_count: usize,
        failed_count: usize,
        skipped_count: usize,
    ) -> Result<()> {
        self.stdout(&format!(
            "Summary: {} succeeded, {} failed, {} skipped.",
            success_count, failed_count, skipped_count
        ));
        Ok(())
    }

    fn download_start(&self, filename: &str, total: Option<u64>) -> Result<usize> {
        if let Some(size) = total {
            self.stdout(&format!("Downloading {} ({} bytes)...", filename, size));
        } else {
            self.stdout(&format!("Downloading {}...", filename));
        }
        Ok(0)
    }

    fn download_update(&self, _id: usize, _downloaded: u64) -> Result<()> {
        Ok(())
    }

    fn download_finish(&self, _id: usize, message: &str) -> Result<()> {
        self.stdout(message);
        Ok(())
    }

    fn download_version_info_start(&self) -> Result<()> {
        self.stdout("Fetching version info...");
        Ok(())
    }

    fn download_version_info_failed(&self, err: &str) -> Result<()> {
        self.stderr(&format!("Failed to fetch version info: {}", err));
        Ok(())
    }

    fn download_version_info_success(&self) -> Result<()> {
        Ok(())
    }

    fn download_version_info_parse_failed(&self, err: &str, snippet: &str) -> Result<()> {
        self.stderr(&format!(
            "Failed to parse version info: {}\nSnippet: {}",
            err, snippet
        ));
        Ok(())
    }

    fn download_share_code_start(&self) -> Result<()> {
        self.stdout("Fetching share code...");
        Ok(())
    }

    fn download_share_code_failed(&self, err: &str) -> Result<()> {
        self.stderr(&format!("Failed to fetch share code: {}", err));
        Ok(())
    }

    fn download_share_code_success(&self) -> Result<()> {
        Ok(())
    }

    fn download_attempt_github_dll(&self) -> Result<()> {
        self.stdout("Attempting to download from GitHub...");
        Ok(())
    }

    fn download_found_github_asset(&self, name: &str) -> Result<()> {
        self.stdout(&format!("Found GitHub asset: {}", name));
        Ok(())
    }

    fn download_github_dll_not_found(&self) -> Result<()> {
        self.stderr("MetaMystia DLL not found on GitHub.");
        Ok(())
    }

    fn download_display_github_release_notes(
        &self,
        _tag: &str,
        _name: &str,
        _body: &str,
    ) -> Result<()> {
        Ok(())
    }

    fn download_ask_continue_after_release_notes(&self) -> Result<bool> {
        self.fixed_choice(true)
    }

    fn download_switch_to_fallback(&self, reason: &str) -> Result<()> {
        self.stdout(&format!("Switching to fallback source: {}", reason));
        Ok(())
    }

    fn download_try_fallback_metamystia(&self) -> Result<()> {
        self.stdout("Trying fallback source for MetaMystia...");
        Ok(())
    }

    fn download_bepinex_attempt_primary(&self) -> Result<()> {
        self.stdout("Downloading BepInEx from primary source...");
        Ok(())
    }

    fn download_bepinex_primary_failed(&self, err: &str) -> Result<()> {
        self.stderr(&format!(
            "Failed to download BepInEx from primary source: {}",
            err
        ));
        Ok(())
    }

    fn network_retrying(
        &self,
        op_desc: &str,
        delay_secs: u64,
        attempt: usize,
        attempts: usize,
        err: &str,
    ) -> Result<()> {
        self.stdout(&format!(
            "Retrying {} ({}/{}) after {} seconds: {}",
            op_desc, attempt, attempts, delay_secs, err
        ));
        Ok(())
    }

    fn network_rate_limited(&self, secs: u64) -> Result<()> {
        self.stdout(&format!("Rate limited, waiting {} seconds...", secs));
        Ok(())
    }

    fn manager_ask_self_update(&self, current_version: &str, latest_version: &str) -> Result<bool> {
        self.stdout(&format!(
            "Manager update available: {} -> {}",
            current_version, latest_version
        ));
        self.fixed_choice(true)
    }

    fn manager_update_starting(&self) -> Result<()> {
        self.stdout("Starting manager self-update...");
        Ok(())
    }

    fn manager_update_failed(&self, err: &str) -> Result<()> {
        self.stderr(&format!("Manager self-update failed: {}", err));
        Ok(())
    }

    fn manager_prompt_manual_update(&self) -> Result<()> {
        self.stderr("Please update the manager manually.");
        Ok(())
    }

    fn select_version_ask_select(&self, _component: &str) -> Result<bool> {
        self.fixed_choice(false)
    }

    fn select_version_from_list(&self, _component: &str, _versions: &[String]) -> Result<usize> {
        Ok(0)
    }

    fn select_version_not_available(
        &self,
        component: &str,
        version: &str,
        available: &[String],
    ) -> Result<()> {
        self.stderr(&format!(
            "Error: {} version \"{}\" is not available",
            component, version
        ));

        let display_count = min(10, available.len());
        let header = if available.len() < 10 {
            "Available versions:"
        } else {
            "Latest 10 available versions:"
        };

        self.stderr(&format!(
            "{} {}",
            header,
            available[..display_count].join(", ")
        ));
        Ok(())
    }
}
