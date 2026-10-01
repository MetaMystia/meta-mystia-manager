//! 全新安装流程。

use crate::config::{
    BEPINEX_CORE_DLL, METAMYSTIA_PLUGIN_GLOB, METAMYSTIA_PLUGIN_OLD_GLOB, RESOURCEEX_ZIP_GLOB,
    RESOURCEEX_ZIP_OLD_GLOB,
};
use crate::error::{ManagerError, Result};
use crate::fs::extractor::Extractor;
use crate::fs::file_ops::{
    atomic_rename_or_copy, cleanup_tmp_residue, count_results, glob_matches_by_filename,
    run_deletion,
};
use crate::fs::temp_dir::create_temp_dir_with_guard;
use crate::mode::UninstallMode;
use crate::net::downloader::{DownloadJob, Downloader};
use crate::ops::preflight::run;
use crate::ops::rollback::Rollback;
use crate::platform::is_fs_dry_run;
use crate::telemetry::report_event;
use crate::ui::{Ui, UiEvent};
use crate::version::VersionInfo;

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// 全新安装流程。
pub struct Installer<'a> {
    dll_version: Option<String>,
    downloader: Downloader<'a>,
    game_root: PathBuf,
    install_resourceex: bool,
    resourceex_version: Option<String>,
    show_bepinex_console: bool,
    ui: &'a dyn Ui,
}

impl<'a> Installer<'a> {
    /// 创建安装器。
    pub fn new(game_root: PathBuf, ui: &'a dyn Ui) -> Self {
        let downloader = Downloader::new(ui);
        Self {
            dll_version: None,
            downloader,
            game_root,
            install_resourceex: false,
            resourceex_version: None,
            show_bepinex_console: false,
            ui,
        }
    }

    /// 指定要安装的 MetaMystia DLL 版本（`None` 表示最新）。
    #[must_use]
    pub fn with_dll_version(mut self, version: Option<String>) -> Self {
        self.dll_version = version;
        self
    }

    /// 指定 ResourceExample ZIP 版本与是否安装。
    #[must_use]
    pub fn with_resourceex(mut self, version: Option<String>, install: bool) -> Self {
        self.install_resourceex = install;
        self.resourceex_version = version;
        self
    }

    /// 是否在游戏启动时显示 BepInEx 控制台。
    #[must_use]
    pub const fn with_console(mut self, show_console: bool) -> Self {
        self.show_bepinex_console = show_console;
        self
    }

    /// 游戏目录中是否已存在 MetaMystia 插件。
    pub fn is_metamystia_installed(&self) -> bool {
        let matches = glob_matches_by_filename(
            &self.game_root.join(METAMYSTIA_PLUGIN_GLOB),
            VersionInfo::is_metamystia_filename,
        );
        !matches.is_empty()
    }

    /// 是否存在 ResourceExample ZIP。
    pub fn is_resourceex_installed(&self) -> bool {
        let resourceex_dir = self.game_root.join("ResourceEx");
        resourceex_dir.exists() && resourceex_dir.is_dir() && {
            let matches = glob_matches_by_filename(
                &self.game_root.join(RESOURCEEX_ZIP_GLOB),
                VersionInfo::is_resourceex_filename,
            );
            !matches.is_empty()
        }
    }

    /// 是否已安装 BepInEx 框架。
    pub fn is_bepinex_installed(&self) -> bool {
        self.game_root.join(BEPINEX_CORE_DLL).is_file()
    }

    /// 执行安装；`cleanup_before_deploy` 表示先清理旧组件。
    #[allow(
        clippy::too_many_lines,
        reason = "安装流程按步骤线性推进，拆分步骤会让上下文参数来回传递"
    )]
    pub fn install(&self, cleanup_before_deploy: bool) -> Result<()> {
        report_event("Install.Start", None);

        self.ui.emit(UiEvent::InstallStep(1, "获取版本信息"))?;
        let version_info = self.downloader.fetch_version_info()?;
        self.ui.emit(UiEvent::InstallVersionInfo(&version_info))?;
        report_event("Install.VersionInfo", Some(&version_info.to_string()));

        let install_resourceex = self.install_resourceex && !version_info.zips.is_empty();
        let show_bepinex_console = self.show_bepinex_console;
        let dll_version = match VersionInfo::resolve_selected_version(
            &version_info.dlls,
            self.dll_version.as_deref(),
            "MetaMystia",
        )? {
            Some(version) => version,
            None => version_info.latest_dll()?.to_string(),
        };
        let resourceex_version = if install_resourceex {
            Some(
                match VersionInfo::resolve_selected_version(
                    &version_info.zips,
                    self.resourceex_version.as_deref(),
                    "ResourceExample",
                )? {
                    Some(version) => version,
                    None => version_info.latest_resourceex()?.to_string(),
                },
            )
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

        let _ = self
            .downloader
            .fetch_and_display_github_release_notes(Some(&dll_version));

        let (temp_dir, _temp_guard) = create_temp_dir_with_guard(&self.game_root).map_err(|e| {
            ManagerError::from(io::Error::new(e.kind(), format!("创建临时目录失败：{e}")))
        })?;

        self.ui.emit(UiEvent::InstallStep(2, "下载必要文件"))?;

        run(
            self.ui,
            &self.game_root,
            &temp_dir,
            &version_info.config_url,
        )?;

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
                Box::new(|slot| {
                    let from_primary =
                        self.downloader
                            .download_bepinex(&version_info, &bepinex_path, slot)?;
                    bepinex_from_primary.store(from_primary, Ordering::Relaxed);

                    Ok(())
                }),
            ),
            (
                "下载 MetaMystia DLL",
                Box::new(|slot| {
                    self.downloader.download_metamystia(
                        &dll_version,
                        &dll_path,
                        version_info.paths.dll.as_deref(),
                        try_github,
                        slot,
                    )
                }),
            ),
        ];

        if let (Some(version), Some(path)) = (&resourceex_version, &resourceex_path) {
            jobs.push((
                "下载 ResourceExample",
                Box::new(|slot| {
                    self.downloader.download_resourceex(
                        version,
                        path,
                        version_info.paths.zip.as_deref(),
                        slot,
                    )
                }),
            ));
        }

        self.downloader.download_files(&jobs)?;
        drop(jobs);
        let bepinex_from_primary = bepinex_from_primary.load(Ordering::Relaxed);

        self.ui.emit(UiEvent::InstallDownloadsCompleted)?;

        if !is_fs_dry_run() {
            let removed = cleanup_tmp_residue(&self.game_root);
            if removed > 0 {
                report_event("Install.ResidueCleaned", Some(&removed.to_string()));
                self.ui.emit(UiEvent::Message(&format!(
                    "已清理 {removed} 个上次运行残留的临时文件"
                )))?;
            }
        }

        let mut rollback = Rollback::new(&self.game_root, &temp_dir);

        if cleanup_before_deploy {
            self.ui.emit(UiEvent::InstallStartCleanup)?;
            let (success, failed) = self.run_install_cleanup(&mut rollback)?;
            // 清理失败只提示：后续部署会真实写入，写不进去时再走回滚
            self.ui
                .emit(UiEvent::InstallCleanupResult(success, failed))?;
            report_event(
                "Install.Cleanup",
                Some(&format!("success:{success};failed:{failed}")),
            );
        }

        self.ui.emit(UiEvent::InstallStep(3, "安装文件"))?;

        let bepinex_dir = self.game_root.join("BepInEx");
        let bepinex_exists = bepinex_dir.exists();
        let bepinex_cfg_path = bepinex_dir.join("config").join("BepInEx.cfg");
        let config_exists = bepinex_cfg_path.is_file();
        let mut exclusions: Vec<&str> = Vec::new();
        if bepinex_exists {
            exclusions.push("BepInEx/plugins");
        }
        if config_exists {
            // 保留已有配置（含其它插件的 .cfg），不覆盖为压缩包默认值
            exclusions.push("BepInEx/config");
        }
        let exclusions = exclusions.as_slice();
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

        // 干跑模式下不会真正写文件，无需备份
        if !is_fs_dry_run() {
            rollback.plan_zip(&bepinex_path, exclusions)?;
            rollback.plan(&bepinex_cfg_path)?;
            rollback.plan(&dll_destination)?;
            if let Some(destination) = &resourceex_destination {
                rollback.plan(destination)?;
            }

            rollback.arm()?;
        }

        let deploy = || -> Result<()> {
            Extractor::deploy_bepinex(&bepinex_path, &self.game_root, exclusions)?;

            let bepinex_config_dir = self.game_root.join("BepInEx").join("config");
            if !bepinex_config_dir.exists() && !is_fs_dry_run() {
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
            return match rollback.restore() {
                Ok(()) => {
                    self.ui
                        .emit(UiEvent::Message("安装失败，已回滚到操作前状态"))?;
                    report_event("Install.Failed.RolledBack", Some(&format!("{e}")));
                    Err(e)
                }
                Err(restore_err) => {
                    self.ui.emit(UiEvent::Warn(&format!(
                        "安装失败，且回滚未完全成功：{restore_err}"
                    )))?;
                    report_event(
                        "Install.Failed.RollbackIncomplete",
                        Some(&format!("{e};{restore_err}")),
                    );
                    Err(ManagerError::Other(format!(
                        "{e}；另外回滚未完全成功：{restore_err}"
                    )))
                }
            };
        }
        rollback.discard();

        self.ui
            .emit(UiEvent::InstallFinished(show_bepinex_console))?;
        report_event("Install.Finished", None);

        Ok(())
    }

    const PRESERVED_BEPINEX_DIRS: [&'static str; 5] =
        ["plugins", "config", "patchers", "cache", "interop"];

    /// 安装前清理 BepInEx 框架文件，保留用户目录。
    fn run_install_cleanup(&self, rollback: &mut Rollback) -> Result<(usize, usize)> {
        let mut targets = Vec::new();
        let mut seen = HashSet::new();
        let game_root = &self.game_root;

        let mut push = |p: PathBuf| {
            if seen.insert(p.clone()) {
                targets.push(p);
            }
        };

        let bepinex_dir = game_root.join("BepInEx");
        if bepinex_dir.exists() {
            for entry in fs::read_dir(&bepinex_dir).map_err(ManagerError::from)? {
                let entry = entry.map_err(ManagerError::from)?;
                let path = entry.path();
                let name = entry.file_name();

                let name = name.to_string_lossy();
                if Self::PRESERVED_BEPINEX_DIRS
                    .iter()
                    .any(|dir| name.eq_ignore_ascii_case(dir))
                {
                    continue;
                }

                push(path);
            }
        }

        let plugins_dir = bepinex_dir.join("plugins");
        if plugins_dir.exists() {
            for entry in glob_matches_by_filename(
                &game_root.join(METAMYSTIA_PLUGIN_GLOB),
                VersionInfo::is_metamystia_filename,
            ) {
                push(entry);
            }

            for entry in glob_matches_by_filename(
                &game_root.join(METAMYSTIA_PLUGIN_OLD_GLOB),
                VersionInfo::is_canonical_metamystia_backup_filename,
            ) {
                push(entry);
            }
        }

        let resourceex_dir = game_root.join("ResourceEx");
        if resourceex_dir.exists() {
            for entry in glob_matches_by_filename(
                &game_root.join(RESOURCEEX_ZIP_GLOB),
                VersionInfo::is_resourceex_filename,
            ) {
                push(entry);
            }

            for entry in glob_matches_by_filename(
                &game_root.join(RESOURCEEX_ZIP_OLD_GLOB),
                VersionInfo::is_canonical_resourceex_backup_filename,
            ) {
                push(entry);
            }
        }

        let full_targets = UninstallMode::Full.targets();
        for &(pattern, _) in full_targets {
            if pattern == "BepInEx" || pattern == "ResourceEx" {
                continue;
            }

            let target_path = game_root.join(pattern);
            if target_path.exists() {
                push(target_path);
            }
        }

        if !is_fs_dry_run() {
            for target in &targets {
                rollback.plan_removal(target)?;
            }

            rollback.arm()?;
        }

        let results = run_deletion(&targets, self.ui);
        let (success, failed, _skipped) = count_results(&results);

        Ok((success, failed))
    }
}

const UNITY_LIBRARIES_MIRROR: &str = "https://url.izakaya.cc/unity-library";

/// 更新 BepInEx 配置：只写需要改的键，值没变就不动文件。
pub fn update_bepinex_config(
    game_root: &Path,
    show_console: bool,
    mirror_unity_libraries: bool,
) -> Result<()> {
    if is_fs_dry_run() {
        return Ok(());
    }

    let config_path = game_root.join("BepInEx").join("config").join("BepInEx.cfg");
    let mut text = match fs::read_to_string(&config_path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(ManagerError::from(io::Error::new(
                e.kind(),
                format!(
                    "读取 BepInEx 配置文件 {} 失败：{}",
                    config_path.display(),
                    e
                ),
            )));
        }
    };
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

/// 读取 INI 文本里 `[section]` 下的 `key` 值；不存在时返回 `None`。
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
            return Some(strip_ini_inline_comment(value).to_string());
        }
    }

    None
}

/// 去掉值后面的 ` # 注释` / ` ; 注释`（仅当注释符前面是空白时才算注释）。
fn strip_ini_inline_comment(value: &str) -> &str {
    let trimmed = value.trim();
    let bytes = trimmed.as_bytes();

    for (index, &byte) in bytes.iter().enumerate() {
        if (byte == b'#' || byte == b';') && index > 0 && bytes[index - 1].is_ascii_whitespace() {
            return trimmed[..index].trim_end();
        }
    }

    trimmed
}

/// BepInEx 是否配置为显示日志控制台；配置缺失或没有该键时按不勾选处理。
pub fn is_bepinex_console_enabled(game_root: &Path) -> bool {
    let path = game_root.join("BepInEx").join("config").join("BepInEx.cfg");
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };

    read_ini_value(&text, "Logging.Console", "Enabled")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

/// 设置 INI 文本里 `[section]` 下的 `key = value`：存在则替换，不存在则追加，保留其它内容。
fn set_ini_value(text: &str, section: &str, key: &str, value: &str) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = text.is_empty() || text.ends_with('\n');
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

        return join_ini_lines(&lines, newline, trailing_newline);
    }

    if lines.last().is_some_and(|line| !line.trim().is_empty()) {
        lines.push(String::new());
    }
    lines.push(header);
    lines.push(entry);

    join_ini_lines(&lines, newline, trailing_newline)
}

fn join_ini_lines(lines: &[String], newline: &str, trailing_newline: bool) -> String {
    let mut text = lines.join(newline);

    if trailing_newline {
        text.push_str(newline);
    }

    text
}
