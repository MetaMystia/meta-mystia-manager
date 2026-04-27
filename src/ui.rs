use crate::config::{OperationMode, UninstallMode};
use crate::error::Result;
use crate::model::VersionInfo;

use std::path::{Path, PathBuf};

/// UI 抽象接口
pub trait Ui: Send + Sync {
    fn display_welcome(&self) -> Result<()>;
    fn display_version(&self, manager_version: Option<&str>) -> Result<()>;
    fn display_game_running_warning(&self) -> Result<()>;
    fn display_available_updates(
        &self,
        bepinex_available: bool,
        dll_available: bool,
        resourceex_available: bool,
    ) -> Result<()>;
    fn select_operation_mode(&self) -> Result<OperationMode>;

    fn blank_line(&self) -> Result<()>;
    fn wait_for_key(&self) -> Result<()>;

    // 通用输出
    fn message(&self, text: &str) -> Result<()>;
    #[allow(dead_code)]
    fn warn(&self, text: &str) -> Result<()>;
    #[allow(dead_code)]
    fn error(&self, text: &str) -> Result<()>;

    // 目录相关
    fn path_display_steam_found(&self, app_id: u32, name: Option<&str>, path: &Path) -> Result<()>;
    fn path_confirm_use_steam_found(&self) -> Result<bool>;

    // 安装相关
    fn install_display_step(&self, step: usize, description: &str) -> Result<()>;
    fn install_display_version_info(&self, version_info: &VersionInfo) -> Result<()>;
    fn install_warn_existing(
        &self,
        bepinex_installed: bool,
        metamystia_installed: bool,
        resourceex_installed: bool,
    ) -> Result<()>;
    fn install_confirm_overwrite(&self) -> Result<bool>;
    fn install_ask_install_resourceex(&self) -> Result<bool>;
    fn install_ask_show_bepinex_console(&self) -> Result<bool>;
    fn install_downloads_completed(&self) -> Result<()>;
    fn install_start_cleanup(&self) -> Result<()>;
    fn install_cleanup_result(&self, success_count: usize, failed_count: usize) -> Result<()>;
    fn install_finished(&self, show_bepinex_console: bool) -> Result<()>;

    // 升级相关
    fn upgrade_backup_failed(&self, err: &str) -> Result<()>;
    fn upgrade_deleted(&self, path: &Path) -> Result<()>;
    fn upgrade_delete_failed(&self, path: &Path, err: &str) -> Result<()>;
    fn upgrade_checking_installed_version(&self) -> Result<()>;
    fn upgrade_detected_resourceex(&self) -> Result<()>;
    fn upgrade_display_current_and_latest_bepinex(&self, current: &str, latest: &str)
    -> Result<()>;
    fn upgrade_display_current_and_latest_dll(&self, current: &str, latest: &str) -> Result<()>;
    fn upgrade_display_current_and_latest_resourceex(
        &self,
        current: &str,
        latest: &str,
    ) -> Result<()>;
    fn upgrade_no_update_needed(&self) -> Result<()>;
    fn upgrade_bepinex_needs_upgrade(&self) -> Result<()>;
    fn upgrade_bepinex_already_latest(&self) -> Result<()>;
    fn upgrade_detected_new_dll(&self, current: &str, new: &str) -> Result<()>;
    fn upgrade_dll_already_latest(&self) -> Result<()>;
    fn upgrade_resourceex_needs_upgrade(&self) -> Result<()>;
    fn upgrade_downloading_bepinex(&self) -> Result<()>;
    fn upgrade_downloading_dll(&self) -> Result<()>;
    fn upgrade_downloading_resourceex(&self) -> Result<()>;
    fn upgrade_installing_bepinex(&self) -> Result<()>;
    fn upgrade_installing_dll(&self) -> Result<()>;
    fn upgrade_installing_resourceex(&self) -> Result<()>;
    fn upgrade_install_success(&self, path: &Path) -> Result<()>;
    fn upgrade_cleanup_start(&self) -> Result<()>;
    fn upgrade_done(&self) -> Result<()>;

    // 卸载相关
    fn uninstall_select_mode(&self) -> Result<UninstallMode>;
    fn uninstall_no_files_found(&self) -> Result<()>;
    fn uninstall_display_target_files(&self, files: &[PathBuf]) -> Result<()>;
    fn uninstall_confirm_deletion(&self) -> Result<bool>;
    fn uninstall_files_in_use_warning(&self) -> Result<()>;
    fn uninstall_wait_before_retry(
        &self,
        delay_secs: u64,
        attempt: usize,
        attempts: usize,
    ) -> Result<()>;
    fn uninstall_ask_elevate_permission(&self) -> Result<bool>;
    fn uninstall_restarting_elevated(&self) -> Result<()>;
    fn uninstall_ask_retry_failures(&self) -> Result<bool>;
    fn uninstall_retrying_failed_items(&self) -> Result<()>;

    // 删除相关
    fn deletion_start(&self) -> Result<()>;
    fn deletion_display_progress(&self, current: usize, total: usize, path: &str) -> Result<()>;
    fn deletion_display_success(&self, path: &str) -> Result<()>;
    fn deletion_display_failure(&self, path: &str, error: &str) -> Result<()>;
    fn deletion_display_skipped(&self, path: &str) -> Result<()>;
    fn deletion_display_summary(
        &self,
        success_count: usize,
        failed_count: usize,
        skipped_count: usize,
    ) -> Result<()>;

    // 下载相关
    /// 开始一个下载任务，返回一个用于后续更新的 id
    fn download_start(&self, filename: &str, total: Option<u64>) -> Result<usize>;
    /// 更新下载进度（传入 download_start 返回的 id）
    fn download_update(&self, id: usize, downloaded: u64) -> Result<()>;
    /// 完成下载任务（并显示完成信息）
    fn download_finish(&self, id: usize, message: &str) -> Result<()>;
    fn download_version_info_start(&self) -> Result<()>;
    fn download_version_info_failed(&self, err: &str) -> Result<()>;
    fn download_version_info_success(&self) -> Result<()>;
    fn download_version_info_parse_failed(&self, err: &str, snippet: &str) -> Result<()>;
    fn download_share_code_start(&self) -> Result<()>;
    fn download_share_code_failed(&self, err: &str) -> Result<()>;
    fn download_share_code_success(&self) -> Result<()>;
    fn download_attempt_github_dll(&self) -> Result<()>;
    fn download_found_github_asset(&self, name: &str) -> Result<()>;
    fn download_github_dll_not_found(&self) -> Result<()>;
    fn download_display_github_release_notes(
        &self,
        tag: &str,
        name: &str,
        body: &str,
    ) -> Result<()>;
    fn download_ask_continue_after_release_notes(&self) -> Result<bool>;
    fn download_switch_to_fallback(&self, reason: &str) -> Result<()>;
    fn download_try_fallback_metamystia(&self) -> Result<()>;
    fn download_bepinex_attempt_primary(&self) -> Result<()>;
    fn download_bepinex_primary_failed(&self, err: &str) -> Result<()>;

    // 网络相关
    fn network_retrying(
        &self,
        op_desc: &str,
        delay_secs: u64,
        attempt: usize,
        attempts: usize,
        err: &str,
    ) -> Result<()>;
    fn network_rate_limited(&self, secs: u64) -> Result<()>;

    // 自升级相关
    fn manager_ask_self_update(&self, current_version: &str, latest_version: &str) -> Result<bool>;
    fn manager_update_starting(&self) -> Result<()>;
    fn manager_update_failed(&self, err: &str) -> Result<()>;
    fn manager_prompt_manual_update(&self) -> Result<()>;

    // 版本选择相关
    fn select_version_ask_select(&self, component: &str) -> Result<bool>;
    fn select_version_from_list(&self, component: &str, versions: &[String]) -> Result<usize>;
    fn select_version_not_available(
        &self,
        component: &str,
        version: &str,
        available: &[String],
    ) -> Result<()>;
}
