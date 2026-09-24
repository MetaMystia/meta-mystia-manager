use crate::config::{OfflineMode, OperationMode, UninstallMode};
use crate::error::{ManagerError, Result};
use crate::metrics::{get_user_id, report_event};
use crate::model::VersionInfo;
use crate::ui::Ui;

use console::{Term, style};
use dialoguer::{Confirm, Input, theme::ColorfulTheme};
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressStyle};
use std::{
    cmp::min,
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

/// 交互式控制台实现：基于 `dialoguer` 的选项交互与 `indicatif` 的进度条
pub struct ConsoleUI {
    bars: Mutex<HashMap<usize, ProgressBar>>,
    multi: MultiProgress,
    next_id: AtomicUsize,
    overall: ProgressBar,
    progress: Mutex<ProgressTotals>,
}

/// 下载总进度：按任务汇总已下载字节，并按采样估算总速度
#[derive(Default)]
struct ProgressTotals {
    /// id -> (已下载, 已知总量)
    active: HashMap<usize, (u64, Option<u64>)>,
    finished_bytes: u64,
    last_sample: Option<(Instant, u64)>,
    speed_bytes_per_second: f64,
}

impl ProgressTotals {
    fn downloaded(&self) -> u64 {
        self.finished_bytes
            + self
                .active
                .values()
                .map(|(downloaded, _)| *downloaded)
                .sum::<u64>()
    }

    fn known_total(&self) -> u64 {
        self.active
            .values()
            .filter_map(|(_, total)| *total)
            .sum::<u64>()
    }

    /// 按 0.3 秒以上的采样窗口估算总速度
    #[allow(
        clippy::cast_precision_loss,
        reason = "下载量远小于 f64 的 2^53 精度上限"
    )]
    fn sample(&mut self) {
        let downloaded = self.downloaded();
        let now = Instant::now();

        match self.last_sample {
            Some((at, bytes)) if now.duration_since(at).as_secs_f64() >= 0.3 => {
                let elapsed = now.duration_since(at).as_secs_f64();
                self.speed_bytes_per_second = downloaded.saturating_sub(bytes) as f64 / elapsed;
                self.last_sample = Some((now, downloaded));
            }
            None => self.last_sample = Some((now, downloaded)),
            _ => {}
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        reason = "速度与剩余量都是估算值，量级远小于 f64/u64 上限"
    )]
    fn summary(&self) -> String {
        let downloaded = self.downloaded();
        let total = self.known_total();
        let speed = format!("{}/s", HumanBytes(self.speed_bytes_per_second as u64));
        let remaining = if total > downloaded && self.speed_bytes_per_second > 0.0 {
            format_duration((total - downloaded) as f64 / self.speed_bytes_per_second)
        } else {
            "--".to_string()
        };

        if total > 0 {
            format!(
                "总进度：{} / {}　{speed}　剩余 {remaining}",
                HumanBytes(downloaded),
                HumanBytes(total)
            )
        } else {
            format!("总进度：{}　{speed}", HumanBytes(downloaded))
        }
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "剩余时间已取正数并按秒取整"
)]
fn format_duration(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;

    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

impl ConsoleUI {
    pub fn new() -> Self {
        let multi = MultiProgress::new();
        let overall = multi.add(ProgressBar::new_spinner());

        Self {
            bars: Mutex::new(HashMap::new()),
            multi,
            next_id: AtomicUsize::new(1),
            overall,
            progress: Mutex::new(ProgressTotals::default()),
        }
    }

    fn confirm_with_event(prompt: impl Into<String>, default: bool, event: &str) -> Result<bool> {
        let choice = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(prompt.into())
            .default(default)
            .interact_on_opt(&Term::stdout())?
            .unwrap_or(false);

        report_event(event, Some(if choice { "yes" } else { "no" }));

        Ok(choice)
    }
}

impl Ui for ConsoleUI {
    fn display_welcome(&self) -> Result<()> {
        let term = Term::stdout();
        term.clear_screen()?;

        println!("{}", style("═".repeat(60)).cyan());
        println!(
            "{}{}（v{}）",
            " ".repeat(7),
            style("MetaMystia Mod 一键安装/升级/卸载工具").cyan().bold(),
            env!("CARGO_PKG_VERSION")
        );

        let user_id = get_user_id();
        print!("{}", " ".repeat(14));
        println!("{}", style(user_id).dim());

        println!("{}", style("═".repeat(60)).cyan());
        println!();

        Ok(())
    }

    fn display_version(&self, manager_version: Option<&str>) -> Result<()> {
        if let Some(v) = manager_version {
            println!();
            println!("管理工具最新版本：{}", style(v).green());
            if v != env!("CARGO_PKG_VERSION") {
                println!(
                    "{}",
                    style("升级提醒：您当前使用的不是最新版本，建议升级至最新版本。").yellow()
                );
                println!(
                    "手动下载：https://doc.meta-mystia.izakaya.cc/user_guide/how_to_install.html#onclick_install"
                );
            }
            println!();
        }

        println!("{}", style("═".repeat(60)).cyan());
        println!();

        Ok(())
    }

    fn display_game_running_warning(&self) -> Result<()> {
        println!("请先关闭游戏，然后重新运行本程序。");
        Ok(())
    }

    fn display_available_updates(
        &self,
        bepinex_available: bool,
        dll_available: bool,
        resourceex_available: bool,
    ) -> Result<()> {
        if bepinex_available || dll_available || resourceex_available {
            println!("检测到可升级项：");
            if bepinex_available {
                println!("  • BepInEx 可升级");
            }
            if dll_available {
                println!("  • MetaMystia DLL 可升级");
            }
            if resourceex_available {
                println!("  • ResourceExample ZIP 可升级");
            }
            println!();
        }

        Ok(())
    }

    fn select_operation_mode(&self) -> Result<OperationMode> {
        println!("{}", style("请选择操作模式：").cyan().bold());
        println!();
        println!("  {} 安装 Mod", style("[1]").green());
        println!("  {} 升级 Mod", style("[2]").green());
        println!("  {} 卸载 Mod", style("[3]").green());
        println!("  {} 导出诊断包", style("[4]").green());
        println!("  {} 退出程序", style("[0]").dim());
        println!();

        loop {
            let input: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt(" 请输入选项")
                .interact_text()?;

            match input.trim() {
                "1" => return Ok(OperationMode::Install),
                "2" => return Ok(OperationMode::Upgrade),
                "3" => return Ok(OperationMode::Uninstall),
                "4" => return Ok(OperationMode::Diagnostics),
                "0" => {
                    return Err(ManagerError::UserCancelled);
                }
                _ => {
                    println!();
                    println!("{}", style("无效的选项，请输入 0 到 4 之间的数字").yellow());
                }
            }
        }
    }

    fn select_offline_mode(&self) -> Result<OfflineMode> {
        println!();
        println!(
            "{}",
            style("离线模式：安装与升级需要联网获取版本信息，当前只能卸载或导出诊断包。")
                .yellow()
                .bold()
        );
        println!();
        println!("  {} 卸载 Mod", style("[1]").green());
        println!("  {} 导出诊断包", style("[2]").green());
        println!("  {} 退出程序", style("[0]").dim());
        println!();

        loop {
            let input: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt(" 请输入选项")
                .interact_text()?;

            match input.trim() {
                "1" => return Ok(OfflineMode::Uninstall),
                "2" => return Ok(OfflineMode::Diagnostics),
                "0" => return Err(ManagerError::UserCancelled),
                _ => {
                    println!();
                    println!("{}", style("无效的选项，请输入 0、1 或 2").yellow());
                }
            }
        }
    }

    fn blank_line(&self) -> Result<()> {
        println!();
        Ok(())
    }

    fn wait_for_key(&self) -> Result<()> {
        println!("{}", style("按回车（Enter）键退出...").dim());

        let mut line = String::new();
        io::stdin().read_line(&mut line)?;

        Ok(())
    }

    fn message(&self, text: &str) -> Result<()> {
        println!("{text}");
        Ok(())
    }

    fn warn(&self, text: &str) -> Result<()> {
        println!("{}", style(text).yellow());
        Ok(())
    }

    fn error(&self, text: &str) -> Result<()> {
        println!();
        println!("{}", style(text).red());
        Ok(())
    }

    fn path_display_steam_found(&self, app_id: u32, name: Option<&str>, path: &Path) -> Result<()> {
        println!(
            "{}",
            style(format!(
                "检测到 Steam 上已安装的游戏：{}（AppID {}）",
                name.unwrap_or("未知"),
                app_id
            ))
            .cyan()
        );
        println!("路径：{}", path.display());
        println!();
        Ok(())
    }

    fn path_confirm_use_steam_found(&self) -> Result<bool> {
        Self::confirm_with_event(
            " 是否将此路径作为运行目录并继续？",
            true,
            "UI.SteamPath.Choice",
        )
    }

    fn install_display_step(&self, step: usize, description: &str) -> Result<()> {
        println!();
        println!(
            "{} {}",
            style(format!("[{step}/3]")).cyan().bold(),
            style(description).cyan()
        );
        println!();
        Ok(())
    }

    fn install_display_version_info(&self, version_info: &VersionInfo) -> Result<()> {
        println!("检测到的最新版本：");
        println!(
            "  • MetaMystia DLL：{}",
            style(version_info.latest_dll()).green()
        );
        println!(
            "  • ResourceExample ZIP：{}",
            style(version_info.latest_resourceex()).green()
        );
        println!(
            "  • BepInEx：{}",
            style(version_info.bepinex_version()?).green()
        );
        Ok(())
    }

    fn install_warn_existing(
        &self,
        bepinex_installed: bool,
        metamystia_installed: bool,
        resourceex_installed: bool,
    ) -> Result<()> {
        println!("{}", style("警告：检测到已安装的组件").yellow());
        println!();

        if bepinex_installed {
            println!("  • BepInEx 框架");
        }
        if metamystia_installed {
            println!("  • MetaMystia DLL");
        }
        if resourceex_installed {
            println!("  • ResourceExample ZIP");
        }

        println!();
        println!("继续安装将会执行以下操作：");
        println!("  • 覆盖 BepInEx 框架相关文件（不包含 plugins 文件夹）");
        println!("  • 覆盖 MetaMystia 相关文件");
        println!("  • 安装最新版本的 BepInEx 和 MetaMystia 相关文件");
        println!();

        Ok(())
    }

    fn install_confirm_overwrite(&self) -> Result<bool> {
        Self::confirm_with_event(" 是否继续安装？", false, "UI.Install.Confirm")
    }

    fn install_ask_install_resourceex(&self) -> Result<bool> {
        println!();
        println!(
            "{}",
            style("ResourceExample ZIP 是 MetaMystia 的可选组件").cyan()
        );
        println!("可以在游戏中加入由 MetaMystia 所提供的额外内容（如：新的稀客、料理和食材等）");
        println!("更多介绍：https://doc.meta-mystia.izakaya.cc/resource_ex/use_resource-ex.html");
        println!();

        Self::confirm_with_event(
            " 是否安装 ResourceExample ZIP？",
            true,
            "UI.Install.ResourceEx.Choice",
        )
    }

    fn install_ask_show_bepinex_console(&self) -> Result<bool> {
        println!();

        Self::confirm_with_event(
            " 是否在游戏启动时弹出 BepInEx 的控制台窗口用于显示日志？",
            false,
            "UI.Install.BepInExConsole.Choice",
        )
    }

    fn install_downloads_completed(&self) -> Result<()> {
        println!("所有文件下载完成");
        Ok(())
    }

    fn install_start_cleanup(&self) -> Result<()> {
        println!();
        println!("正在清理旧版本...");
        Ok(())
    }

    fn install_cleanup_result(&self, success_count: usize, failed_count: usize) -> Result<()> {
        if failed_count > 0 {
            println!("旧版本删除完成（成功：{success_count}，失败：{failed_count}）");
            println!("{}", style("  部分文件删除失败，将继续安装").yellow());
        } else {
            println!("旧版本删除完成（清理 {success_count} 项）");
        }

        Ok(())
    }

    fn install_finished(&self, show_bepinex_console: bool) -> Result<()> {
        println!("安装完成！");
        println!("现在可以启动游戏，Mod 将自动加载。");

        if show_bepinex_console {
            println!(
                "{}",
                style("注意：首次启动需要较长时间加载，请您耐心等待。").yellow()
            );
        } else {
            println!(
              "{}",
              style(
                  "注意：首次启动需要较长时间加载（可能需要几分钟且没有任何窗口弹出），请您耐心等待。"
              )
              .yellow()
          );
        }

        println!("祝您游戏愉快！");

        Ok(())
    }

    fn upgrade_deleted(&self, path: &Path) -> Result<()> {
        println!("已删除：{}", path.display());
        Ok(())
    }

    fn upgrade_delete_failed(&self, path: &Path, err: &str) -> Result<()> {
        println!(
            "{}",
            style(format!("删除失败：{}（{}）", path.display(), err)).yellow()
        );
        Ok(())
    }

    fn upgrade_checking_installed_version(&self) -> Result<()> {
        println!("正在检查当前安装的版本...");
        Ok(())
    }

    fn upgrade_detected_resourceex(&self) -> Result<()> {
        println!("检测到已安装 ResourceExample ZIP");
        Ok(())
    }

    fn upgrade_display_current_and_latest_bepinex(
        &self,
        current: &str,
        latest: &str,
    ) -> Result<()> {
        println!();
        println!("当前 BepInEx 版本：{}", style(current).green());
        println!("最新 BepInEx 版本：{}", style(latest).green());
        Ok(())
    }

    fn upgrade_display_current_and_latest_dll(&self, current: &str, latest: &str) -> Result<()> {
        println!("当前 MetaMystia DLL 版本：{}", style(current).green());
        println!("最新 MetaMystia DLL 版本：{}", style(latest).green());
        Ok(())
    }

    fn upgrade_display_current_and_latest_resourceex(
        &self,
        current: &str,
        latest: &str,
    ) -> Result<()> {
        println!("当前 ResourceExample ZIP 版本：{}", style(current).green());
        println!("最新 ResourceExample ZIP 版本：{}", style(latest).green());
        Ok(())
    }

    fn upgrade_no_update_needed(&self) -> Result<()> {
        println!();
        println!("✔  已是最新版本，无需升级！");
        Ok(())
    }

    fn upgrade_bepinex_needs_upgrade(&self) -> Result<()> {
        println!();
        println!("BepInEx 需要升级");
        Ok(())
    }

    fn upgrade_bepinex_already_latest(&self) -> Result<()> {
        println!();
        println!("BepInEx 已是最新版本");
        Ok(())
    }

    fn upgrade_detected_new_dll(&self, current: &str, new: &str) -> Result<()> {
        println!("发现新版本 MetaMystia DLL：v{current} -> v{new}");
        Ok(())
    }

    fn upgrade_dll_already_latest(&self) -> Result<()> {
        println!("MetaMystia DLL 已是最新版本");
        Ok(())
    }

    fn upgrade_resourceex_needs_upgrade(&self) -> Result<()> {
        println!("ResourceExample ZIP 需要升级");
        println!();
        Ok(())
    }

    fn upgrade_downloading_bepinex(&self) -> Result<()> {
        println!();
        println!("正在下载 BepInEx...");
        Ok(())
    }

    fn upgrade_downloading_dll(&self) -> Result<()> {
        println!();
        println!("正在下载 MetaMystia DLL...");
        Ok(())
    }

    fn upgrade_downloading_resourceex(&self) -> Result<()> {
        println!();
        println!("正在下载 ResourceExample ZIP...");
        Ok(())
    }

    fn upgrade_installing_bepinex(&self) -> Result<()> {
        println!();
        println!();
        println!("正在安装 BepInEx...");
        Ok(())
    }

    fn upgrade_installing_dll(&self) -> Result<()> {
        println!("正在安装 MetaMystia DLL...");
        Ok(())
    }

    fn upgrade_installing_resourceex(&self) -> Result<()> {
        println!("正在安装 ResourceExample ZIP...");
        Ok(())
    }

    fn upgrade_install_success(&self, path: &Path) -> Result<()> {
        println!("安装成功：{}", path.display());
        Ok(())
    }

    fn upgrade_cleanup_start(&self) -> Result<()> {
        println!();
        println!("正在清理临时文件...");
        Ok(())
    }

    fn upgrade_done(&self) -> Result<()> {
        println!();
        println!("✔  升级完成！");
        Ok(())
    }

    fn uninstall_select_mode(&self) -> Result<UninstallMode> {
        println!();
        println!("{}", style("请选择卸载模式：").cyan().bold());
        println!();
        println!(
            "  {} {}",
            style("[1]").green(),
            UninstallMode::Light.description()
        );
        println!(
            "  {} {}",
            style("[2]").green(),
            UninstallMode::Full.description()
        );
        println!("  {} 退出程序", style("[0]").dim());
        println!();

        loop {
            let input: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt(" 请输入选项")
                .interact_text()?;

            match input.trim() {
                "1" => return Ok(UninstallMode::Light),
                "2" => return Ok(UninstallMode::Full),
                "0" => {
                    return Err(ManagerError::UserCancelled);
                }
                _ => {
                    println!();
                    println!("{}", style("无效的选项，请输入 0、1 或 2").yellow());
                }
            }
        }
    }

    fn uninstall_no_files_found(&self) -> Result<()> {
        println!();
        println!("未找到需要删除的文件，可能已经卸载完成。");
        Ok(())
    }

    fn uninstall_display_target_files(&self, files: &[PathBuf]) -> Result<()> {
        println!();
        println!("{}", style("即将删除以下文件/文件夹：").yellow().bold());
        println!();

        for file in files {
            let file_type = if file.is_dir() { "📁" } else { "📄" };
            println!("  {} {} {}", style("•").cyan(), file_type, file.display());
        }

        println!();

        Ok(())
    }

    fn uninstall_confirm_deletion(&self) -> Result<bool> {
        Self::confirm_with_event(" 是否继续当前操作？", false, "UI.Uninstall.Confirm.Choice")
    }

    fn uninstall_files_in_use_warning(&self) -> Result<()> {
        println!();
        println!(
            "{}",
            style("部分文件被占用，请关闭相关程序后重试。正在短暂等待并自动重试这些文件...")
                .yellow()
        );
        Ok(())
    }

    fn uninstall_wait_before_retry(
        &self,
        delay_secs: u64,
        attempt: usize,
        attempts: usize,
    ) -> Result<()> {
        println!();
        println!("等待 {delay_secs} 秒后重试被占用文件（重试 {attempt}/{attempts}）...");
        Ok(())
    }

    fn uninstall_ask_elevate_permission(&self) -> Result<bool> {
        println!();
        println!(
            "{}",
            style("部分文件删除失败，可能需要管理员权限。").yellow()
        );
        println!();

        Self::confirm_with_event(
            " 是否以管理员权限重新运行？",
            false,
            "UI.Uninstall.Elevate.Choice",
        )
    }

    fn uninstall_restarting_elevated(&self) -> Result<()> {
        println!();
        println!("正在以管理员权限重新启动...");
        Ok(())
    }

    fn uninstall_ask_retry_failures(&self) -> Result<bool> {
        println!();

        Self::confirm_with_event(" 是否重试失败的项目？", false, "UI.Uninstall.Retry.Choice")
    }

    fn uninstall_retrying_failed_items(&self) -> Result<()> {
        println!();
        println!("正在重试失败的项目...");
        Ok(())
    }

    fn deletion_start(&self) -> Result<()> {
        println!();
        Ok(())
    }

    fn deletion_display_progress(&self, current: usize, total: usize, path: &str) -> Result<()> {
        println!(
            "{} [{}/{}] {}",
            style("正在删除").cyan(),
            current,
            total,
            path
        );
        Ok(())
    }

    fn deletion_display_success(&self, path: &str) -> Result<()> {
        println!("  {} {}", style("✔ ").green(), style(path).dim());
        Ok(())
    }

    fn deletion_display_failure(&self, path: &str, error: &str) -> Result<()> {
        println!(
            "  {} {} - {}",
            style("✗ ").red(),
            style(path).dim(),
            style(error).red()
        );
        Ok(())
    }

    fn deletion_display_skipped(&self, path: &str) -> Result<()> {
        println!("  {} {}", style("○ ").dim(), style(path).dim());
        Ok(())
    }

    fn deletion_display_summary(
        &self,
        success_count: usize,
        failed_count: usize,
        skipped_count: usize,
    ) -> Result<()> {
        println!();
        println!("删除成功：{} 项", style(success_count).green());

        if skipped_count > 0 {
            println!(
                "  {} 跳过：{} 项（文件不存在）",
                style("○").dim(),
                style(skipped_count).dim()
            );
        }

        if failed_count > 0 {
            println!("  删除失败：{} 项", style(failed_count).red());
        } else {
            println!();
            println!("✔  卸载完成！");
        }

        Ok(())
    }

    fn download_start(&self, filename: &str, total: Option<u64>) -> Result<usize> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let pb = total.map_or_else(
            || {
                let pb = ProgressBar::new_spinner();
                pb.set_message(format!("下载：{filename}"));
                pb
            },
            |size| {
                let pb = ProgressBar::new(size);
                let style = ProgressStyle::default_bar()
                    .template(
                        "{msg}\n[{bar:40.cyan/blue}] {bytes}/{total_bytes} {bytes_per_sec} ({eta})",
                    )
                    .map_or_else(
                        |_| ProgressStyle::default_bar(),
                        |style| style.progress_chars("#>-"),
                    );
                pb.set_style(style);
                pb.set_message(format!("下载：{filename}"));
                pb
            },
        );
        let pb = self.multi.add(pb);

        {
            let mut progress = self.progress.lock().unwrap_or_else(PoisonError::into_inner);
            progress.active.insert(id, (0, total));
            let summary = progress.summary();
            drop(progress);

            self.overall.set_message(summary);
        }

        let mut guard = self.bars.lock().unwrap_or_else(PoisonError::into_inner);
        guard.insert(id, pb);
        drop(guard);

        Ok(id)
    }

    fn download_update(&self, id: usize, downloaded: u64) -> Result<()> {
        let guard = self.bars.lock().unwrap_or_else(PoisonError::into_inner);

        if let Some(pb) = guard.get(&id) {
            pb.set_position(downloaded);
        }
        drop(guard);

        {
            let mut progress = self.progress.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(entry) = progress.active.get_mut(&id) {
                entry.0 = downloaded;
            }
            progress.sample();
            let summary = progress.summary();
            drop(progress);

            self.overall.set_message(summary);
        }

        Ok(())
    }

    fn download_finish(&self, id: usize, message: &str) -> Result<()> {
        let mut guard = self.bars.lock().unwrap_or_else(PoisonError::into_inner);

        if let Some(pb) = guard.remove(&id) {
            pb.finish_with_message(message.to_string());
        }
        drop(guard);

        {
            let mut progress = self.progress.lock().unwrap_or_else(PoisonError::into_inner);

            if let Some((downloaded, _)) = progress.active.remove(&id) {
                progress.finished_bytes += downloaded;
            }

            if progress.active.is_empty() {
                // 全部任务完成：清掉总进度条并重置汇总，供下次安装复用
                *progress = ProgressTotals::default();
                drop(progress);
                self.overall.finish_and_clear();
            } else {
                let summary = progress.summary();
                drop(progress);
                self.overall.set_message(summary);
            }
        }

        Ok(())
    }

    fn download_version_info_start(&self) -> Result<()> {
        println!("正在获取版本信息...");
        Ok(())
    }

    fn download_version_info_failed(&self, err: &str) -> Result<()> {
        println!("{}", style(format!("获取版本信息失败：{err}")).yellow());
        Ok(())
    }

    fn download_version_info_success(&self) -> Result<()> {
        println!("获取版本信息成功");
        Ok(())
    }

    fn download_version_info_parse_failed(&self, err: &str, snippet: &str) -> Result<()> {
        println!(
            "{}",
            style(format!(
                "版本信息解析失败：{err}，response snippet：{snippet}"
            ))
            .yellow()
        );
        Ok(())
    }

    fn download_share_code_start(&self) -> Result<()> {
        println!("正在获取下载链接...");
        Ok(())
    }

    fn download_share_code_failed(&self, err: &str) -> Result<()> {
        println!("{}", style(format!("获取下载链接失败：{err}")).yellow());
        Ok(())
    }

    fn download_share_code_success(&self) -> Result<()> {
        println!("获取下载链接成功");
        Ok(())
    }

    fn download_attempt_github_dll(&self) -> Result<()> {
        println!("尝试从 GitHub 下载 MetaMystia DLL...");
        Ok(())
    }

    fn download_found_github_asset(&self, name: &str) -> Result<()> {
        println!("找到文件：{name}");
        Ok(())
    }

    fn download_github_dll_not_found(&self) -> Result<()> {
        println!("{}", style("未找到 MetaMystia DLL 文件").yellow());
        Ok(())
    }

    fn download_display_github_release_notes(
        &self,
        tag: &str,
        name: &str,
        body: &str,
    ) -> Result<()> {
        println!();
        println!("{}", style(format!("发行说明：{name}（{tag}）")).cyan());
        println!("{}", "-".repeat(60));

        let trimmed = body.trim();
        if trimmed.is_empty() {
            println!("{}", style("（发行说明为空）").dim());
        } else {
            print_markdown(trimmed);
        }

        println!("{}", "-".repeat(60));

        Ok(())
    }

    fn download_ask_continue_after_release_notes(&self) -> Result<bool> {
        println!();

        Self::confirm_with_event(
            " 以上内容为发行说明，是否继续当前操作？",
            false,
            "UI.Download.GitHubReleaseNotes.Choice",
        )
    }

    fn download_switch_to_fallback(&self, reason: &str) -> Result<()> {
        println!();
        println!("{}", style(reason).yellow());
        Ok(())
    }

    fn download_try_fallback_metamystia(&self) -> Result<()> {
        println!("尝试从备用源下载 MetaMystia DLL...");
        Ok(())
    }

    fn download_bepinex_attempt_primary(&self) -> Result<()> {
        println!("尝试从 bepinex.dev 下载 BepInEx...");
        Ok(())
    }

    fn download_bepinex_primary_failed(&self, err: &str) -> Result<()> {
        println!("{}", style(err).yellow());
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
        println!(
            "{}",
            style(format!(
                "{op_desc}失败，{delay_secs} 秒后重试...（重试 {attempt}/{attempts}）"
            ))
            .yellow()
        );
        println!("{}", style(format!("错误：{err}")).yellow());
        println!(
            "{}",
            style("提醒：若重试次数耗尽后仍失败，将自动切换至备用源继续当前操作，请耐心等待。")
                .dim()
        );
        Ok(())
    }

    fn network_rate_limited(&self, secs: u64) -> Result<()> {
        println!(
            "{}",
            style(format!(
                "检测到限流，服务器指定 Retry-After={secs} 秒，将等待后重试..."
            ))
            .yellow()
        );
        Ok(())
    }

    fn manager_ask_self_update(&self, current_version: &str, latest_version: &str) -> Result<bool> {
        println!(
            "管理工具可以升级：{} -> {}",
            style(current_version).green(),
            style(latest_version).green()
        );
        println!();

        let choice = Self::confirm_with_event(" 是否立即升级？", true, "UI.SelfUpdate.Choice")?;

        println!();

        Ok(choice)
    }

    fn manager_update_starting(&self) -> Result<()> {
        println!();
        println!("正在启动升级脚本，请稍候...");
        println!();
        Ok(())
    }

    fn manager_update_failed(&self, err: &str) -> Result<()> {
        println!();
        println!("{}", style(format!("升级失败：{err}")).red());
        println!("请手动下载并升级管理工具。");
        println!();
        Ok(())
    }

    fn manager_prompt_manual_update(&self) -> Result<()> {
        println!();
        println!("无法向当前运行目录写入文件，请手动下载并升级管理工具。");
        println!();
        Ok(())
    }

    fn select_version_ask_select(&self, component: &str) -> Result<bool> {
        println!();

        Self::confirm_with_event(
            format!(" 是否需要安装旧版本的 {component}？"),
            false,
            &format!("UI.SelectHistoricalVersion.Choice.{component}"),
        )
    }

    fn select_version_from_list(&self, component: &str, versions: &[String]) -> Result<usize> {
        let page_size = 10;
        let total_pages = versions.len().div_ceil(page_size);
        let mut current_page = 0;

        loop {
            println!();
            println!(
                "{}",
                style(format!("可用的 {component} 版本：")).cyan().bold()
            );
            println!();

            let start = current_page * page_size;
            let end = min(start + page_size, versions.len());

            for (i, v) in versions[start..end].iter().enumerate() {
                let global_index = start + i;
                if global_index == 0 {
                    println!(
                        "  {} {}（最新版）",
                        style(format!("[{}]", i + 1)).green(),
                        v
                    );
                } else {
                    println!("  {} {}", style(format!("[{}]", i + 1)).green(), v);
                }
            }

            println!();

            if total_pages > 1 {
                let mut nav_hints = Vec::new();
                if current_page > 0 {
                    nav_hints.push(format!("{} 上一页", style("[P]").green()));
                }
                if current_page < total_pages - 1 {
                    nav_hints.push(format!("{} 下一页", style("[N]").green()));
                }
                if !nav_hints.is_empty() {
                    print!("  {}", nav_hints.join("  "));
                }
                println!(
                    "  {}",
                    style(format!("（第 {}/{} 页）", current_page + 1, total_pages)).dim()
                );
                println!();
            }

            let current_page_count = end - start;
            let input: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt(format!(
                    " 请选择版本编号（1-{}）{}",
                    current_page_count,
                    if total_pages > 1 {
                        "，或输入 P（上一页）/ N（下一页）翻页"
                    } else {
                        ""
                    }
                ))
                .interact_text()?;

            let trimmed = input.trim().to_lowercase();
            if trimmed == "n" || trimmed == "next" {
                current_page = (current_page + 1) % total_pages;
                continue;
            }
            if trimmed == "p" || trimmed == "prev" || trimmed == "previous" {
                current_page = if current_page == 0 {
                    total_pages - 1
                } else {
                    current_page - 1
                };
                continue;
            }

            match trimmed.parse::<usize>() {
                Ok(num) if num >= 1 && num <= current_page_count => {
                    let index = start + num - 1;
                    report_event(
                        "UI.SelectHistoricalVersion.Selected",
                        Some(&versions[index]),
                    );
                    return Ok(index);
                }
                _ => {
                    println!();
                    println!(
                        "{}",
                        style(format!(
                            "无效的输入，请输入 1 到 {} 之间的数字{}",
                            current_page_count,
                            if total_pages > 1 {
                                "，或输入 P（上一页）/ N（下一页）翻页"
                            } else {
                                ""
                            }
                        ))
                        .yellow()
                    );
                }
            }
        }
    }

    fn sso_ask_open_browser(&self) -> Result<bool> {
        Self::confirm_with_event(" 是否打开浏览器登录？", false, "UI.Sso.OpenBrowser.Confirm")
    }

    fn diagnostics_confirm_export(&self, entries: &[String]) -> Result<bool> {
        println!();
        println!(
            "{}",
            style("将收集以下内容，打包到管理器所在目录：")
                .cyan()
                .bold()
        );
        for entry in entries {
            println!("  · {entry}");
        }
        println!();

        Self::confirm_with_event(" 是否导出诊断包？", true, "UI.Diagnostics.ConfirmExport")
    }
}

fn print_markdown(text: &str) {
    let mut in_code_block = false;

    'outer: for line in text.lines() {
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }

        if in_code_block {
            println!("  {}", style(line).dim());
            continue;
        }

        let trimmed_line = line.trim();

        if matches!(
            trimmed_line,
            "---" | "***" | "___" | "- - -" | "* * *" | "_ _ _"
        ) {
            println!("{}", style("─".repeat(60)).dim());
            continue;
        }

        let heading_level = trimmed_line.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&heading_level)
            && let Some(rest) = trimmed_line[heading_level..].strip_prefix(' ')
        {
            let rendered = render_inline(rest);
            if heading_level <= 2 {
                println!("{}", style(rendered).bold().cyan());
            } else {
                println!("{}", style(rendered).bold());
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix('>') {
            let content = rest.strip_prefix(' ').unwrap_or(rest);
            println!(
                "  {} {}",
                style("│").dim(),
                style(render_inline(content)).dim()
            );
            continue;
        }

        for prefix in &["- ", "* ", "+ "] {
            if let Some(rest) = trimmed_line.strip_prefix(prefix) {
                println!("  {} {}", style("•").cyan(), render_inline(rest));
                continue 'outer;
            }
        }

        let digit_count = trimmed_line
            .chars()
            .take_while(char::is_ascii_digit)
            .count();
        if digit_count > 0
            && let Some(rest) = trimmed_line[digit_count..].strip_prefix(". ")
        {
            println!(
                "  {} {}",
                style(format!("{}.", &trimmed_line[..digit_count])).cyan(),
                render_inline(rest)
            );
            continue;
        }

        if trimmed_line.is_empty() {
            println!();
            continue;
        }

        println!("{}", render_inline(line));
    }
}

/// 渲染行内 Markdown：`` `代码` ``、`~~删除线~~`、`***粗斜体***`、`**粗体**`、`*斜体*`、`[文本](链接)`
#[allow(
    clippy::too_many_lines,
    reason = "按标记逐项线性解析，拆开反而难以对照"
)]
fn render_inline(text: &str) -> String {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut result = String::with_capacity(len + 32);
    let mut idx = 0;

    while idx < len {
        match bytes[idx] {
            b'`' => {
                let start = idx + 1;
                if let Some(offset) = bytes[start..].iter().position(|&c| c == b'`') {
                    let end = start + offset;
                    result.push_str(&style(&text[start..end]).yellow().to_string());
                    idx = end + 1;
                } else {
                    result.push('`');
                    idx += 1;
                }
            }
            b'~' if bytes.get(idx + 1) == Some(&b'~') => {
                let start = idx + 2;
                if let Some(offset) = bytes[start..].windows(2).position(|w| w == b"~~") {
                    let end = start + offset;
                    result.push_str(&style(&text[start..end]).strikethrough().to_string());
                    idx = end + 2;
                } else {
                    result.push_str("~~");
                    idx += 2;
                }
            }
            b'*' if bytes.get(idx + 1) == Some(&b'*') && bytes.get(idx + 2) == Some(&b'*') => {
                let start = idx + 3;
                if let Some(offset) = bytes[start..].windows(3).position(|w| w == b"***") {
                    let end = start + offset;
                    result.push_str(&style(&text[start..end]).bold().italic().to_string());
                    idx = end + 3;
                } else {
                    result.push_str("***");
                    idx += 3;
                }
            }
            b'*' if bytes.get(idx + 1) == Some(&b'*') => {
                let start = idx + 2;
                if let Some(offset) = bytes[start..].windows(2).position(|w| w == b"**") {
                    let end = start + offset;
                    result.push_str(&style(&text[start..end]).bold().to_string());
                    idx = end + 2;
                } else {
                    result.push_str("**");
                    idx += 2;
                }
            }
            b'_' if bytes.get(idx + 1) == Some(&b'_') => {
                let start = idx + 2;
                if let Some(offset) = bytes[start..].windows(2).position(|w| w == b"__") {
                    let end = start + offset;
                    result.push_str(&style(&text[start..end]).bold().to_string());
                    idx = end + 2;
                } else {
                    result.push_str("__");
                    idx += 2;
                }
            }
            b'*' => {
                let start = idx + 1;
                if let Some(offset) = bytes[start..].iter().position(|&c| c == b'*') {
                    let end = start + offset;
                    if end > start {
                        result.push_str(&style(&text[start..end]).italic().to_string());
                        idx = end + 1;
                    } else {
                        result.push('*');
                        idx += 1;
                    }
                } else {
                    result.push('*');
                    idx += 1;
                }
            }
            b'[' => {
                let text_start = idx + 1;
                if let Some(offset) = bytes[text_start..].iter().position(|&c| c == b']') {
                    let text_end = text_start + offset;
                    let after = text_end + 1;
                    if bytes.get(after) == Some(&b'(')
                        && let Some(url_offset) = bytes[after + 1..].iter().position(|&c| c == b')')
                    {
                        let url_start = after + 1;
                        let url_end = url_start + url_offset;
                        result.push('[');
                        result.push_str(&render_inline(&text[text_start..text_end]));
                        result.push_str("](");
                        result.push_str(&text[url_start..url_end]);
                        result.push(')');
                        idx = url_end + 1;
                        continue;
                    }
                }
                result.push('[');
                idx += 1;
            }
            byte if byte >= 0x80 => {
                if let Some(ch) = text[idx..].chars().next() {
                    result.push(ch);
                    idx += ch.len_utf8();
                } else {
                    break;
                }
            }
            byte => {
                result.push(byte as char);
                idx += 1;
            }
        }
    }

    result
}
