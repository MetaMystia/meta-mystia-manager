use crate::config::{
    BEPINEX_VERSION_FILE, METAMYSTIA_PLUGIN_GLOB, RESOURCEEX_ZIP_GLOB, UninstallMode,
};
use crate::downloader::{DownloadJob, Downloader};
use crate::error::{ManagerError, Result};
use crate::extractor::Extractor;
use crate::file_ops::{
    atomic_rename_or_copy, count_results, execute_deletion, glob_matches, glob_matches_by_filename,
    write_bepinex_version_marker,
};
use crate::metrics::report_event;
use crate::model::VersionInfo;
use crate::platform::fs_dry_run;
use crate::preflight::check;
use crate::rollback::Rollback;
use crate::temp_dir::create_temp_dir_with_guard;
use crate::ui::Ui;

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub struct Installer<'a> {
    downloader: Downloader<'a>,
    game_root: PathBuf,
    ui: &'a dyn Ui,
}

impl<'a> Installer<'a> {
    pub fn new(game_root: PathBuf, ui: &'a dyn Ui) -> Self {
        let downloader = Downloader::new(ui);
        Self {
            downloader,
            game_root,
            ui,
        }
    }

    pub fn check_metamystia_installed(&self) -> bool {
        let matches = glob_matches_by_filename(
            &self.game_root.join(METAMYSTIA_PLUGIN_GLOB),
            VersionInfo::is_metamystia_filename,
        );
        !matches.is_empty()
    }

    pub fn check_resourceex_installed(&self) -> bool {
        let resourceex_dir = self.game_root.join("ResourceEx");
        resourceex_dir.exists() && resourceex_dir.is_dir() && {
            let matches = glob_matches_by_filename(
                &self.game_root.join(RESOURCEEX_ZIP_GLOB),
                VersionInfo::is_resourceex_filename,
            );
            !matches.is_empty()
        }
    }

    pub fn check_bepinex_installed(&self) -> bool {
        let bepinex_dir = self.game_root.join("BepInEx");
        bepinex_dir.exists() && bepinex_dir.is_dir() && {
            let core_pattern = bepinex_dir.join("core").join("BepInEx.Core.dll");
            let matches = glob_matches(&core_pattern);
            !matches.is_empty()
        }
    }

    /// 执行安装前的清理：全量卸载但保留 BepInEx/plugins（除了 MetaMystia DLL）
    fn execute_install_cleanup(game_root: &Path, ui: &dyn Ui) -> Result<(usize, usize)> {
        let mut targets = Vec::new();
        let mut seen = HashSet::new();

        let mut push = |p: PathBuf| {
            if seen.insert(p.clone()) {
                targets.push(p);
            }
        };

        // 1. 删除 BepInEx 目录下的所有项目（跳过 plugins）
        let bepinex_dir = game_root.join("BepInEx");
        if bepinex_dir.exists() {
            for entry in fs::read_dir(&bepinex_dir).map_err(ManagerError::from)? {
                let entry = entry.map_err(ManagerError::from)?;
                let path = entry.path();
                let name = entry.file_name();

                if name.to_string_lossy().eq_ignore_ascii_case("plugins") {
                    continue;
                }

                push(path);
            }
        }

        // 2. 删除 plugins 目录中的 MetaMystia DLL
        let plugins_dir = bepinex_dir.join("plugins");
        if plugins_dir.exists() {
            for entry in glob_matches_by_filename(
                &game_root.join(METAMYSTIA_PLUGIN_GLOB),
                VersionInfo::is_metamystia_filename,
            ) {
                push(entry);
            }
        }

        // 3. 删除 ResourceEx 目录中的 ResourceExample ZIP
        let resourceex_dir = game_root.join("ResourceEx");
        if resourceex_dir.exists() {
            for entry in glob_matches_by_filename(
                &game_root.join(RESOURCEEX_ZIP_GLOB),
                VersionInfo::is_resourceex_filename,
            ) {
                push(entry);
            }
        }

        // 4. 删除完全卸载模式中的其他文件
        let full_targets = UninstallMode::Full.targets();
        for &(pattern, is_dir) in full_targets {
            if pattern == "BepInEx" || pattern == "ResourceEx" {
                continue;
            }

            let target_path = game_root.join(pattern);

            if is_dir {
                if target_path.exists() {
                    push(target_path);
                }
            } else if pattern.contains('*') {
                for entry in glob_matches(&target_path) {
                    push(entry);
                }
            } else if target_path.exists() {
                push(target_path);
            }
        }

        let results = execute_deletion(&targets, ui);
        let (success, failed, _skipped) = count_results(&results);

        Ok((success, failed))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "安装流程按步骤线性推进，拆分步骤会让上下文参数来回传递"
    )]
    pub fn install(&self, cleanup_before_deploy: bool) -> Result<()> {
        report_event("Install.Start", None);

        // 1. 获取版本信息
        self.ui.install_display_step(1, "获取版本信息")?;
        let version_info = self.downloader.get_version_info()?;
        self.ui.install_display_version_info(&version_info)?;
        report_event("Install.VersionInfo", Some(&version_info.to_string()));

        // 2. 选择安装组件与版本
        // 2.1. 询问是否安装 ResourceEx
        let install_resourceex = if cleanup_before_deploy {
            let resourceex_exists = !glob_matches_by_filename(
                &self.game_root.join(RESOURCEEX_ZIP_GLOB),
                VersionInfo::is_resourceex_filename,
            )
            .is_empty();
            if resourceex_exists {
                true
            } else {
                self.ui.install_ask_install_resourceex()?
            }
        } else {
            self.ui.install_ask_install_resourceex()?
        };

        // 2.2. 询问是否在游戏启动时弹出 BepInEx 控制台窗口
        let show_bepinex_console = self.ui.install_ask_show_bepinex_console()?;

        // 2.3. 选择 DLL 版本
        let dll_version = if self.ui.select_version_ask_select("MetaMystia DLL")? {
            let idx = self
                .ui
                .select_version_from_list("MetaMystia DLL", &version_info.dlls)?;
            version_info
                .dlls
                .get(idx)
                .cloned()
                .ok_or(ManagerError::InvalidVersionInfo)?
        } else {
            version_info.latest_dll()?.to_string()
        };

        // 2.4. 选择 ResourceEx 版本（仅在安装时）
        let resourceex_version = if install_resourceex {
            if self.ui.select_version_ask_select("ResourceEx ZIP")? {
                let idx = self
                    .ui
                    .select_version_from_list("ResourceEx ZIP", &version_info.zips)?;
                Some(
                    version_info
                        .zips
                        .get(idx)
                        .cloned()
                        .ok_or(ManagerError::InvalidVersionInfo)?,
                )
            } else {
                Some(version_info.latest_resourceex()?.to_string())
            }
        } else {
            None
        };

        report_event(
            "Install.Version.Selected",
            Some(&format!(
                "dll={};resourceex={}",
                dll_version,
                resourceex_version.as_deref().unwrap_or("none")
            )),
        );

        if let Ok(Some(_)) = self
            .downloader
            .fetch_and_display_github_release_notes(Some(&dll_version))
            && !self.ui.download_ask_continue_after_release_notes()?
        {
            return Err(ManagerError::UserCancelled);
        }

        // 3. 创建临时下载目录
        let (temp_dir, _temp_guard) = create_temp_dir_with_guard(&self.game_root).map_err(|e| {
            ManagerError::from(io::Error::new(e.kind(), format!("创建临时目录失败：{e}")))
        })?;

        // 4. 下载文件
        self.ui.install_display_step(2, "下载必要文件")?;

        check(self.ui, &self.game_root, &temp_dir)?;

        let bepinex_path = temp_dir.join(version_info.bepinex_filename()?);
        let dll_path = temp_dir.join(VersionInfo::metamystia_filename(&dll_version));
        let resourceex_path = resourceex_version
            .as_ref()
            .map(|version| temp_dir.join(VersionInfo::resourceex_filename(version)));
        let try_github = VersionInfo::versions_match(&dll_version, version_info.latest_dll()?);
        let bepinex_from_primary = AtomicBool::new(false);

        let mut jobs: Vec<DownloadJob<'_>> = vec![
            (
                "下载 BepInEx",
                Box::new(|| {
                    let from_primary = self
                        .downloader
                        .download_bepinex(&version_info, &bepinex_path)?;
                    bepinex_from_primary.store(from_primary, Ordering::Relaxed);

                    Ok(())
                }),
            ),
            (
                "下载 MetaMystia DLL",
                Box::new(|| {
                    self.downloader.download_metamystia(
                        &dll_version,
                        &dll_path,
                        version_info.paths.dll.as_deref(),
                        try_github,
                    )
                }),
            ),
        ];

        if let (Some(version), Some(path)) = (&resourceex_version, &resourceex_path) {
            jobs.push((
                "下载 ResourceExample",
                Box::new(|| {
                    self.downloader.download_resourceex(
                        version,
                        path,
                        version_info.paths.zip.as_deref(),
                    )
                }),
            ));
        }

        self.downloader.download_files(&jobs)?;
        drop(jobs);
        let bepinex_from_primary = bepinex_from_primary.load(Ordering::Relaxed);

        self.ui.install_downloads_completed()?;

        // 5. 在安装前清理旧版本
        if cleanup_before_deploy {
            self.ui.install_start_cleanup()?;
            let (success, failed) = Self::execute_install_cleanup(&self.game_root, self.ui)?;
            // 清理失败只提示：后续部署会真实写入，写不进去时再走回滚
            self.ui.install_cleanup_result(success, failed)?;
            report_event(
                "Install.Cleanup",
                Some(&format!("success:{success};failed:{failed}")),
            );
        }

        // 6. 安装文件
        self.ui.install_display_step(3, "安装文件")?;

        // 检查 BepInEx 是否存在（用于决定是否跳过 plugins）
        let bepinex_dir = self.game_root.join("BepInEx");
        let bepinex_exists = bepinex_dir.exists();
        let exclusions: &[&str] = if bepinex_exists {
            &["BepInEx/plugins"]
        } else {
            &[]
        };
        let bepinex_cfg_path = bepinex_dir.join("config").join("BepInEx.cfg");
        let dll_destination = dll_path.file_name().map_or_else(
            || Err(ManagerError::Other("无效的 DLL 文件名".to_string())),
            |name| Ok(bepinex_dir.join("plugins").join(name)),
        )?;
        let resourceex_destination = resourceex_path
            .as_ref()
            .map(|path| {
                path.file_name().map_or_else(
                    || Err(ManagerError::Other("无效的 ZIP 文件名".to_string())),
                    |name| Ok(self.game_root.join("ResourceEx").join(name)),
                )
            })
            .transpose()?;

        // 部署前记录将被覆盖/新建的文件，失败时回滚
        let mut rollback = Rollback::new(&self.game_root, &temp_dir);
        // 干跑模式下不会真正写文件，无需备份
        if !fs_dry_run() {
            rollback.plan_zip(&bepinex_path, exclusions)?;
            rollback.plan(&self.game_root.join(BEPINEX_VERSION_FILE))?;
            rollback.plan(&bepinex_cfg_path)?;
            rollback.plan(&dll_destination)?;
            if let Some(destination) = &resourceex_destination {
                rollback.plan(destination)?;
            }
        }

        let deploy = || -> Result<()> {
            // 安装 BepInEx（如果之前存在则保留 plugins 目录）
            Extractor::deploy_bepinex(&bepinex_path, &self.game_root, exclusions)?;

            write_bepinex_version_marker(&self.game_root, &version_info)?;

            // 确保配置目录存在
            let bepinex_config_dir = self.game_root.join("BepInEx").join("config");
            if !bepinex_config_dir.exists() && !fs_dry_run() {
                fs::create_dir_all(&bepinex_config_dir).map_err(|e| {
                    ManagerError::from(io::Error::new(
                        e.kind(),
                        format!(
                            "创建 BepInEx 配置目录 {} 失败：{}",
                            bepinex_config_dir.display(),
                            e
                        ),
                    ))
                })?;
            }

            update_bepinex_config(&self.game_root, show_bepinex_console, !bepinex_from_primary)?;

            Extractor::deploy_metamystia(&dll_path, &self.game_root)?;

            if let Some(ref path) = resourceex_path {
                Extractor::deploy_resourceex(path, &self.game_root)?;
            }

            Ok(())
        };

        if let Err(e) = deploy() {
            let _ = rollback.restore();
            self.ui.message("安装失败，已回滚到操作前状态")?;
            report_event("Install.Failed.RolledBack", Some(&format!("{e}")));

            return Err(e);
        }
        rollback.discard();

        self.ui.install_finished(show_bepinex_console)?;
        report_event("Install.Finished", None);

        Ok(())
    }
}

const UNITY_LIBRARIES_MIRROR: &str = "https://url.izakaya.cc/unity-library";

/// 更新 BepInEx 配置：只写需要改的键，值没变就不动文件
pub fn update_bepinex_config(
    game_root: &Path,
    show_console: bool,
    mirror_unity_libraries: bool,
) -> Result<()> {
    if fs_dry_run() {
        return Ok(());
    }

    let config_path = game_root.join("BepInEx").join("config").join("BepInEx.cfg");
    let mut text = fs::read_to_string(&config_path).unwrap_or_default();
    let mut changed = false;

    // BepInEx 默认开启控制台，配置里没写这个键时按开启比较
    let console_now =
        read_ini_value(&text, "Logging.Console", "Enabled").unwrap_or_else(|| "true".to_string());
    let console_wanted = if show_console { "true" } else { "false" };

    if !console_now.eq_ignore_ascii_case(console_wanted) {
        text = set_ini_value(&text, "Logging.Console", "Enabled", console_wanted);
        changed = true;
    }

    if mirror_unity_libraries {
        let source_now =
            read_ini_value(&text, "IL2CPP", "UnityBaseLibrariesSource").unwrap_or_default();

        if !source_now.eq_ignore_ascii_case(UNITY_LIBRARIES_MIRROR) {
            text = set_ini_value(
                &text,
                "IL2CPP",
                "UnityBaseLibrariesSource",
                UNITY_LIBRARIES_MIRROR,
            );
            changed = true;
        }
    }

    if !changed {
        return Ok(());
    }

    if let Some(parent) = config_path.parent()
        && !parent.exists()
    {
        fs::create_dir_all(parent).map_err(|e| {
            ManagerError::from(io::Error::new(
                e.kind(),
                format!("创建 BepInEx 配置目录 {} 失败：{}", parent.display(), e),
            ))
        })?;
    }

    let tmp_path = config_path.with_extension("cfg.tmp");
    fs::write(&tmp_path, text.as_bytes()).map_err(|e| {
        ManagerError::from(io::Error::new(
            e.kind(),
            format!(
                "写入 BepInEx 临时配置文件 {} 失败：{}",
                tmp_path.display(),
                e
            ),
        ))
    })?;

    match atomic_rename_or_copy(&tmp_path, &config_path) {
        Ok(()) => {
            let _ = fs::remove_file(&tmp_path);
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp_path);
            Err(ManagerError::from(io::Error::other(format!(
                "写入 BepInEx 配置文件 {} 失败：{}",
                config_path.display(),
                e
            ))))
        }
    }
}

/// 读取 INI 文本里 `[section]` 下的 `key` 值；不存在时返回 `None`
fn read_ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let header = format!("[{section}]");
    let mut in_section = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed.eq_ignore_ascii_case(&header);
            continue;
        }

        if in_section
            && let Some((name, value)) = trimmed.split_once('=')
            && name.trim().eq_ignore_ascii_case(key)
        {
            return Some(value.trim().to_string());
        }
    }

    None
}

/// BepInEx 是否配置为显示日志控制台；配置缺失或没有该键时按不勾选处理
pub fn bepinex_console_enabled(game_root: &Path) -> bool {
    let path = game_root.join("BepInEx").join("config").join("BepInEx.cfg");
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };

    read_ini_value(&text, "Logging.Console", "Enabled")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

/// 设置 INI 文本里 `[section]` 下的 `key = value`：存在则替换，不存在则追加，保留其它内容
fn set_ini_value(text: &str, section: &str, key: &str, value: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(ToString::to_string).collect();
    let header = format!("[{section}]");
    let mut section_start = None;
    let mut section_end = lines.len();

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if !(trimmed.starts_with('[') && trimmed.ends_with(']')) {
            continue;
        }

        if section_start.is_some() {
            section_end = index;
            break;
        }

        if trimmed.eq_ignore_ascii_case(&header) {
            section_start = Some(index);
        }
    }

    let entry = format!("{key} = {value}");

    if let Some(start) = section_start {
        let existing = (start + 1..section_end).find(|index| {
            lines[*index]
                .trim()
                .split_once('=')
                .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case(key))
        });

        if let Some(index) = existing {
            lines[index] = entry;
        } else {
            lines.insert(start + 1, entry);
        }

        return with_trailing_newline(&lines);
    }

    if lines.last().is_some_and(|line| !line.trim().is_empty()) {
        lines.push(String::new());
    }
    lines.push(header);
    lines.push(entry);

    with_trailing_newline(&lines)
}

fn with_trailing_newline(lines: &[String]) -> String {
    let mut text = lines.join("\n");
    text.push('\n');
    text
}
