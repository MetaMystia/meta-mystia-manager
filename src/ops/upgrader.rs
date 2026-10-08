//! 升级与组件替换流程。

use crate::config::{
    BEPINEX_CORE_DLL, DISABLED_DIR_NAME, METAMYSTIA_PLUGIN_GLOB, METAMYSTIA_PLUGIN_OLD_GLOB,
    RESOURCEEX_ZIP_GLOB, RESOURCEEX_ZIP_OLD_GLOB,
};
use crate::error::{ManagerError, Result};
use crate::fs::extractor::Extractor;
use crate::fs::file_ops::{
    atomic_rename_or_copy, backup_to_path, cleanup_tmp_residue, delete_paths,
    glob_matches_by_filename, glob_matches_filtered, next_backup_path,
};
use crate::fs::temp_dir::create_temp_dir_with_guard;
use crate::net::downloader::{DownloadJob, Downloader};
use crate::ops::installer::update_bepinex_config;
use crate::ops::preflight::run;
use crate::ops::rollback::Rollback;
use crate::ops::toggle::{Component, component_files, set_doorstop_enabled};
use crate::platform::is_fs_dry_run;
use crate::telemetry::report_event;
use crate::ui::{Ui, UiEvent};
use crate::version::{VersionInfo, read_bepinex_version};

use std::{
    cmp::Ordering,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering as AtomicOrdering},
};

#[derive(Clone, Debug)]
struct ParsedVersion {
    display: String,
    parts: Vec<u64>,
}

struct AssetPattern<'a> {
    /// 是否禁用区里的文件
    disabled: bool,
    matcher: fn(&str) -> bool,
    pattern: &'a str,
    version_from_filename: fn(&str) -> Option<String>,
}

/// 待清理的备份模式（路径，文件名匹配）。
type BackupPattern = (PathBuf, fn(&str) -> bool);

/// 一个已安装组件的启用状态。
#[derive(Clone, Debug)]
pub struct EnabledState {
    /// 是否处于禁用状态（文件在禁用区）
    pub disabled: bool,
    /// 已安装版本
    pub version: String,
}

/// 两个组件的已安装版本。
pub struct InstalledVersions {
    /// MetaMystia DLL
    pub dll: Option<EnabledState>,
    /// ResourceExample ZIP
    pub resourceex: Option<EnabledState>,
}

impl Ord for ParsedVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let max_len = self.parts.len().max(other.parts.len());

        for index in 0..max_len {
            let left = *self.parts.get(index).unwrap_or(&0);
            let right = *other.parts.get(index).unwrap_or(&0);

            let ordering = left.cmp(&right);
            if ordering != Ordering::Equal {
                return ordering;
            }
        }

        Ordering::Equal
    }
}

impl PartialEq for ParsedVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for ParsedVersion {}

impl PartialOrd for ParsedVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// 升级流程。
#[allow(
    clippy::struct_excessive_bools,
    reason = "字段直接对应界面选项与流程分支"
)]
pub struct Upgrader<'a> {
    /// 界面上选定的版本；`None` 表示用最新版
    dll_version: Option<String>,
    downloader: Downloader<'a>,
    game_root: PathBuf,
    resourceex_version: Option<String>,
    show_bepinex_console: bool,
    /// 界面里被用户取消勾选的组件，升级时跳过
    skip_bepinex: bool,
    skip_dll: bool,
    skip_resourceex: bool,
    ui: &'a dyn Ui,
}

impl<'a> Upgrader<'a> {
    /// 创建升级器。
    pub fn new(game_root: PathBuf, ui: &'a dyn Ui) -> Self {
        let downloader = Downloader::new(ui);
        Self {
            dll_version: None,
            downloader,
            game_root,
            resourceex_version: None,
            show_bepinex_console: false,
            skip_bepinex: false,
            skip_dll: false,
            skip_resourceex: false,
            ui,
        }
    }

    /// 指定要升级到的 MetaMystia DLL 版本（`None` 表示最新）。
    #[must_use]
    pub fn with_dll_version(mut self, version: Option<String>) -> Self {
        self.dll_version = version;
        self
    }

    /// 指定要升级到的 ResourceExample ZIP 版本（`None` 表示最新）。
    #[must_use]
    pub fn with_resourceex_version(mut self, version: Option<String>) -> Self {
        self.resourceex_version = version;
        self
    }

    /// 是否在游戏启动时显示 BepInEx 控制台。
    #[must_use]
    pub const fn with_console(mut self, show_console: bool) -> Self {
        self.show_bepinex_console = show_console;
        self
    }

    /// 跳过某个组件的升级。
    #[must_use]
    pub const fn with_skips(
        mut self,
        skip_bepinex: bool,
        skip_dll: bool,
        skip_resourceex: bool,
    ) -> Self {
        self.skip_bepinex = skip_bepinex;
        self.skip_dll = skip_dll;
        self.skip_resourceex = skip_resourceex;
        self
    }

    /// 已安装组件及其启用状态；只读，不改动文件。
    pub fn installed_versions(&self) -> Result<InstalledVersions> {
        Ok(InstalledVersions {
            dll: self.installed_state(Component::MetaMystia)?,
            resourceex: self.installed_state(Component::ResourceEx)?,
        })
    }

    /// 读取当前已安装的 BepInEx 构建号。
    pub fn read_bepinex_version(&self) -> Option<String> {
        read_bepinex_version(&self.game_root)
    }

    /// 执行升级。
    #[allow(
        clippy::too_many_lines,
        reason = "升级流程按步骤线性推进，拆分步骤会让上下文参数来回传递"
    )]
    pub fn upgrade(&self) -> Result<()> {
        report_event("Upgrade.Start", None);

        self.ui.emit(UiEvent::UpgradeCheckingInstalledVersion)?;

        let current_bepinex_version = self.read_bepinex_version();
        let bepinex_installed = self.is_bepinex_installed();
        let installed = self.installed_versions()?;
        let Some(dll_state) = installed.dll else {
            return Err(ManagerError::Other(
                "未找到已安装的 MetaMystia Mod，请先使用安装功能。".to_string(),
            ));
        };
        let current_dll_version = dll_state.version;
        let resourceex_state = installed.resourceex;
        let current_resourceex_version = resourceex_state
            .as_ref()
            .map_or_else(String::new, |state| state.version.clone());

        report_event(
            "Upgrade.Detected",
            Some(&format!(
                "bepinex:{};dll:{};resourceex:{}",
                current_bepinex_version.as_deref().unwrap_or("unknown"),
                current_dll_version,
                current_resourceex_version
            )),
        );

        let has_resourceex = !current_resourceex_version.is_empty();
        if has_resourceex {
            self.ui.emit(UiEvent::UpgradeDetectedResourceex)?;
        }

        let version_info = self.downloader.fetch_version_info()?;
        report_event("Upgrade.VersionInfo", Some(&version_info.to_string()));

        let new_bepinex_version = version_info.bepinex_version().ok().map(ToString::to_string);
        let bepinex_needs_upgrade = !self.skip_bepinex
            && new_bepinex_version
                .as_ref()
                .is_some_and(|new_ver| current_bepinex_version.as_ref() != Some(new_ver));
        if bepinex_needs_upgrade {
            self.ui.emit(UiEvent::UpgradeBepinexVersions(
                current_bepinex_version.as_deref().unwrap_or("未知"),
                new_bepinex_version.as_deref().unwrap_or("未知"),
            ))?;
        }

        let new_dll_version = VersionInfo::resolve_selected_version(
            &version_info.dlls,
            self.dll_version.as_deref(),
            "MetaMystia",
        )?
        .unwrap_or_else(|| version_info.latest_dll().unwrap_or_default().to_string());
        let dll_needs_upgrade = !self.skip_dll
            && !new_dll_version.is_empty()
            && !Self::versions_match(&current_dll_version, &new_dll_version);
        if dll_needs_upgrade {
            self.ui.emit(UiEvent::UpgradeDllVersions(
                &current_dll_version,
                &new_dll_version,
            ))?;
        }

        let new_resourceex_version = VersionInfo::resolve_selected_version(
            &version_info.zips,
            self.resourceex_version.as_deref(),
            "ResourceExample",
        )?
        .unwrap_or_else(|| {
            version_info
                .latest_resourceex()
                .unwrap_or_default()
                .to_string()
        });
        let resourceex_needs_upgrade = !self.skip_resourceex
            && !new_resourceex_version.is_empty()
            && (!has_resourceex
                || !Self::versions_match(&current_resourceex_version, &new_resourceex_version));
        if resourceex_needs_upgrade && has_resourceex {
            self.ui.emit(UiEvent::UpgradeResourceexVersions(
                &current_resourceex_version,
                &new_resourceex_version,
            ))?;
        }

        let show_bepinex_console = self.show_bepinex_console;

        if !bepinex_needs_upgrade && !dll_needs_upgrade && !resourceex_needs_upgrade {
            update_bepinex_config(&self.game_root, show_bepinex_console, false)?;
            self.ui.emit(UiEvent::UpgradeNoUpdateNeeded)?;
            return Ok(());
        }

        if bepinex_needs_upgrade {
            self.ui
                .emit(UiEvent::UpgradeBepinexNeedsUpgrade(bepinex_installed))?;
        } else if !self.skip_bepinex && new_bepinex_version.is_some() {
            self.ui.emit(UiEvent::UpgradeBepinexAlreadyLatest)?;
        }
        if dll_needs_upgrade {
            self.ui.emit(UiEvent::UpgradeDllDetected(
                &current_dll_version,
                &new_dll_version,
            ))?;

            let _ = self
                .downloader
                .fetch_and_display_github_release_notes(Some(&new_dll_version));
        } else if !self.skip_dll {
            self.ui.emit(UiEvent::UpgradeDllAlreadyLatest)?;
        }
        if resourceex_needs_upgrade {
            self.ui
                .emit(UiEvent::UpgradeResourceexNeedsUpgrade(has_resourceex))?;
        }

        let (temp_dir, _temp_guard) = create_temp_dir_with_guard(&self.game_root).map_err(|e| {
            ManagerError::from(io::Error::new(e.kind(), format!("创建临时目录失败：{e}")))
        })?;

        run(
            self.ui,
            &self.game_root,
            &temp_dir,
            &version_info.config_url,
        )?;

        let temp_bepinex_path = if bepinex_needs_upgrade {
            Some(temp_dir.join(version_info.bepinex_filename()?))
        } else {
            None
        };

        let temp_dll_path = if dll_needs_upgrade {
            let new_dll_filename = VersionInfo::metamystia_filename(&new_dll_version);
            let path = temp_dir.join(&new_dll_filename);

            Some((path, new_dll_filename))
        } else {
            None
        };

        let temp_resourceex_path = if resourceex_needs_upgrade {
            let resourceex_filename = VersionInfo::resourceex_filename(&new_resourceex_version);
            let path = temp_dir.join(&resourceex_filename);

            Some((path, resourceex_filename))
        } else {
            None
        };

        let bepinex_from_primary = AtomicBool::new(true);
        let mut jobs: Vec<DownloadJob<'_>> = Vec::new();

        if let Some(path) = &temp_bepinex_path {
            jobs.push((
                "下载 BepInEx",
                Box::new(|slot| {
                    self.ui.emit(UiEvent::UpgradeDownloadingBepinex)?;
                    let from_primary =
                        self.downloader
                            .download_bepinex(&version_info, path, slot)?;
                    bepinex_from_primary.store(from_primary, AtomicOrdering::Relaxed);

                    Ok(())
                }),
            ));
        }
        if let Some((path, _)) = &temp_dll_path {
            jobs.push((
                "下载 MetaMystia DLL",
                Box::new(|slot| {
                    self.ui.emit(UiEvent::UpgradeDownloadingDll)?;
                    self.downloader.download_metamystia(
                        &new_dll_version,
                        path,
                        version_info.paths.dll.as_deref(),
                        true,
                        slot,
                    )
                }),
            ));
        }
        if let Some((path, _)) = &temp_resourceex_path {
            jobs.push((
                "下载 ResourceExample",
                Box::new(|slot| {
                    self.ui.emit(UiEvent::UpgradeDownloadingResourceex)?;
                    self.downloader.download_resourceex(
                        &new_resourceex_version,
                        path,
                        version_info.paths.zip.as_deref(),
                        slot,
                    )
                }),
            ));
        }

        self.downloader.download_files(&jobs)?;
        drop(jobs);

        if !is_fs_dry_run() {
            let removed = cleanup_tmp_residue(&self.game_root);
            if removed > 0 {
                report_event("Upgrade.ResidueCleaned", Some(&removed.to_string()));
                self.ui.emit(UiEvent::Message(&format!(
                    "已清理 {removed} 个上次运行残留的临时文件"
                )))?;
            }
        }

        let dll_destination = temp_dll_path
            .as_ref()
            .map(|(_, filename)| self.asset_destination(METAMYSTIA_PLUGIN_GLOB, filename));
        let resourceex_destination = temp_resourceex_path
            .as_ref()
            .map(|(_, filename)| self.asset_destination(RESOURCEEX_ZIP_GLOB, filename));

        let mut rollback = Rollback::new(&self.game_root, &temp_dir);

        if !is_fs_dry_run() {
            rollback.plan(&self.bepinex_config_path())?;

            if let Some(path) = &temp_bepinex_path {
                rollback.plan_zip(path, &["BepInEx/config", "BepInEx/plugins"])?;
            }
            if let Some(destination) = &dll_destination {
                rollback.plan(destination)?;
            }
            if let Some(destination) = &resourceex_destination {
                rollback.plan(destination)?;
            }

            rollback.arm()?;
        }

        let mut deploy = || -> Result<()> {
            update_bepinex_config(
                &self.game_root,
                show_bepinex_console,
                !bepinex_from_primary.load(AtomicOrdering::Relaxed),
            )?;

            if let Some(bepinex_path) = &temp_bepinex_path {
                self.ui.emit(UiEvent::UpgradeInstallingBepinex)?;

                Extractor::deploy_bepinex(
                    bepinex_path,
                    &self.game_root,
                    &["BepInEx/config", "BepInEx/plugins"],
                )?;
                let _ = set_doorstop_enabled(&self.game_root, true)?;

                self.ui.emit(UiEvent::UpgradeInstallSuccess(
                    &self.game_root.join("BepInEx"),
                ))?;
                report_event("Upgrade.Installed.BepInEx", new_bepinex_version.as_deref());
            }

            if let Some((temp_path, filename)) = &temp_dll_path {
                Self::backup_existing_assets(
                    &mut rollback,
                    &self.asset_pattern(METAMYSTIA_PLUGIN_GLOB),
                    VersionInfo::is_metamystia_filename,
                    filename,
                    "dll.old",
                )?;

                self.ui.emit(UiEvent::UpgradeInstallingDll)?;

                let new_dll_path = self.asset_destination(METAMYSTIA_PLUGIN_GLOB, filename);
                Self::install_asset_from_temp(temp_path, &new_dll_path, "dll.tmp")?;
                self.remove_disabled_copy(Component::MetaMystia)?;

                self.ui
                    .emit(UiEvent::UpgradeInstallSuccess(&new_dll_path))?;
                report_event("Upgrade.Installed.DLL", Some(filename));
            } else if !self.skip_dll {
                Self::backup_existing_assets(
                    &mut rollback,
                    &self.asset_pattern(METAMYSTIA_PLUGIN_GLOB),
                    VersionInfo::is_metamystia_filename,
                    &VersionInfo::metamystia_filename(&current_dll_version),
                    "dll.old",
                )?;
                self.remove_disabled_copy(Component::MetaMystia)?;
            }

            if let Some((temp_path, filename)) = &temp_resourceex_path {
                Self::backup_existing_assets(
                    &mut rollback,
                    &self.asset_pattern(RESOURCEEX_ZIP_GLOB),
                    VersionInfo::is_resourceex_filename,
                    filename,
                    "zip.old",
                )?;

                self.ui.emit(UiEvent::UpgradeInstallingResourceex)?;

                let new_zip_path = self.asset_destination(RESOURCEEX_ZIP_GLOB, filename);
                Self::install_asset_from_temp(temp_path, &new_zip_path, "zip.tmp")?;
                self.remove_disabled_copy(Component::ResourceEx)?;

                self.ui
                    .emit(UiEvent::UpgradeInstallSuccess(&new_zip_path))?;
                report_event("Upgrade.Installed.ResourceEx", Some(filename));
            } else if !self.skip_resourceex && has_resourceex {
                Self::backup_existing_assets(
                    &mut rollback,
                    &self.asset_pattern(RESOURCEEX_ZIP_GLOB),
                    VersionInfo::is_resourceex_filename,
                    &VersionInfo::resourceex_filename(&current_resourceex_version),
                    "zip.old",
                )?;
                self.remove_disabled_copy(Component::ResourceEx)?;
            }

            Ok(())
        };

        if let Err(e) = deploy() {
            return match rollback.restore() {
                Ok(()) => {
                    self.ui
                        .emit(UiEvent::Message("升级失败，已回滚到操作前状态"))?;
                    report_event("Upgrade.Failed.RolledBack", Some(&format!("{e}")));
                    Err(e)
                }
                Err(restore_err) => {
                    self.ui.emit(UiEvent::Warn(&format!(
                        "升级失败，且回滚未完全成功：{restore_err}"
                    )))?;
                    report_event(
                        "Upgrade.Failed.RollbackIncomplete",
                        Some(&format!("{e};{restore_err}")),
                    );
                    Err(ManagerError::Other(format!(
                        "{e}；另外回滚未完全成功：{restore_err}"
                    )))
                }
            };
        }
        rollback.discard();

        self.ui.emit(UiEvent::UpgradeCleanupStart)?;
        self.cleanup_old_files()?;

        self.ui.emit(UiEvent::UpgradeDone)?;
        report_event("Upgrade.Finished", None);

        Ok(())
    }

    /// 删除遗留的旧版本/备份文件；失败只提示，不阻断升级。
    fn cleanup_old_files_by_pattern(
        &self,
        pattern: &Path,
        matcher: fn(&str) -> bool,
    ) -> Result<()> {
        let entries = glob_matches_by_filename(pattern, matcher);
        let result = delete_paths(&entries);

        for deleted in &result.deleted {
            self.ui.emit(UiEvent::UpgradeDeleted(deleted))?;
        }
        for (path, err) in result.failed {
            self.ui
                .emit(UiEvent::UpgradeDeleteFailed(&path, &format!("{err}")))?;
        }

        Ok(())
    }

    fn parse_numeric_version(version: &str) -> Option<ParsedVersion> {
        let version = VersionInfo::normalize_version(version);
        let parts = VersionInfo::strict_numeric_version_parts(&version)?;

        Some(ParsedVersion {
            display: version,
            parts,
        })
    }

    fn versions_match(current: &str, latest: &str) -> bool {
        VersionInfo::versions_match(current, latest)
    }

    fn backup_existing_assets(
        rollback: &mut Rollback,
        pattern: &Path,
        matcher: fn(&str) -> bool,
        current_filename: &str,
        backup_suffix: &str,
    ) -> Result<()> {
        for old_entry in glob_matches_by_filename(pattern, matcher) {
            if let Some(old_filename) = old_entry.file_name().and_then(|name| name.to_str())
                && old_filename == current_filename
            {
                continue;
            }

            let backup = next_backup_path(&old_entry, backup_suffix);
            rollback.plan_rename(&old_entry, &backup);
            rollback.arm()?;
            backup_to_path(&old_entry, &backup)?;
        }

        Ok(())
    }

    fn install_asset_from_temp(
        temp_path: &Path,
        destination: &Path,
        temp_extension: &str,
    ) -> Result<()> {
        if is_fs_dry_run() {
            eprintln!("[dev] 跳过文件部署（模拟）：{}", destination.display());
            return Ok(());
        }

        if let Some(parent) = destination.parent()
            && !parent.exists()
        {
            fs::create_dir_all(parent).map_err(|e| {
                ManagerError::from(io::Error::new(
                    e.kind(),
                    format!("创建目录 {} 失败：{}", parent.display(), e),
                ))
            })?;
        }

        let tmp_new = destination.with_extension(temp_extension);
        fs::copy(temp_path, &tmp_new).map_err(|e| {
            ManagerError::from(io::Error::new(
                e.kind(),
                format!("复制临时文件 {} 失败：{}", tmp_new.display(), e),
            ))
        })?;

        if let Err(e) = atomic_rename_or_copy(&tmp_new, destination) {
            let _ = fs::remove_file(&tmp_new);

            return Err(ManagerError::from(io::Error::other(format!(
                "安装新版本 {} 失败：{}",
                destination.display(),
                e
            ))));
        }

        Ok(())
    }

    fn installed_state(&self, component: Component) -> Result<Option<EnabledState>> {
        let spec = component.spec();
        let matcher = spec.matcher;
        let mut parsed = Vec::new();

        for is_disabled in [false, true] {
            let pattern = if is_disabled {
                spec.disabled_pattern
            } else {
                spec.enabled_pattern
            };
            let Some(entry) = self.consolidate_installed_by_pattern(&AssetPattern {
                disabled: is_disabled,
                matcher,
                pattern,
                version_from_filename: match component {
                    Component::MetaMystia => VersionInfo::metamystia_version_from_filename,
                    Component::ResourceEx => VersionInfo::resourceex_version_from_filename,
                },
            })?
            else {
                continue;
            };

            parsed.push((entry, is_disabled));
        }

        parsed.sort_by_key(|(_, is_disabled)| *is_disabled);

        let Some(((version, _), disabled)) = parsed.pop() else {
            return Ok(None);
        };

        Ok(Some(EnabledState { disabled, version }))
    }

    fn consolidate_installed_by_pattern(
        &self,
        asset_pattern: &AssetPattern<'_>,
    ) -> Result<Option<(String, PathBuf)>> {
        let root = if asset_pattern.disabled {
            self.game_root.join(DISABLED_DIR_NAME)
        } else {
            self.game_root.clone()
        };
        let pattern = root.join(asset_pattern.pattern);

        let mut parsed = Vec::new();

        for path in glob_matches_filtered(&pattern, |path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(asset_pattern.matcher)
        }) {
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                let Some(version) = (asset_pattern.version_from_filename)(filename) else {
                    return Err(ManagerError::Other(format!(
                        "升级扫描失败：无法从文件名解析版本：{filename}"
                    )));
                };

                let Some(parsed_version) = Self::parse_numeric_version(&version) else {
                    return Err(ManagerError::Other(format!(
                        "升级扫描失败：无法解析版本号：{filename}"
                    )));
                };

                parsed.push((parsed_version, path));
            }
        }

        if parsed.is_empty() {
            return Ok(None);
        }

        parsed.sort_by(|a, b| a.0.cmp(&b.0));

        let Some((latest_version, latest_path)) = parsed.last().cloned() else {
            return Err(ManagerError::Other(
                "升级扫描失败：未找到可用的已解析版本".to_string(),
            ));
        };

        // 只读；旧版本改名由升级流程在回滚保护下完成
        Ok(Some((latest_version.display, latest_path)))
    }

    fn cleanup_old_files(&self) -> Result<()> {
        let mut targets: Vec<BackupPattern> = vec![
            (
                self.game_root.join(METAMYSTIA_PLUGIN_OLD_GLOB),
                VersionInfo::is_canonical_metamystia_backup_filename,
            ),
            (
                self.game_root.join(RESOURCEEX_ZIP_OLD_GLOB),
                VersionInfo::is_canonical_resourceex_backup_filename,
            ),
        ];
        let disabled_root = self.game_root.join(DISABLED_DIR_NAME);
        targets.push((
            disabled_root.join(METAMYSTIA_PLUGIN_OLD_GLOB),
            VersionInfo::is_canonical_metamystia_backup_filename,
        ));
        targets.push((
            disabled_root.join(RESOURCEEX_ZIP_OLD_GLOB),
            VersionInfo::is_canonical_resourceex_backup_filename,
        ));

        for (pattern, matcher) in targets {
            self.cleanup_old_files_by_pattern(&pattern, matcher)?;
        }

        Ok(())
    }

    fn asset_pattern(&self, pattern: &str) -> PathBuf {
        self.game_root.join(pattern)
    }

    fn asset_destination(&self, pattern: &str, filename: &str) -> PathBuf {
        let parent = Path::new(pattern).parent().unwrap_or_else(|| Path::new(""));

        self.game_root.join(parent).join(filename)
    }

    fn remove_disabled_copy(&self, component: Component) -> Result<()> {
        let files = component_files(&self.game_root, component);
        let (Some(source), true) = (files.disabled.first(), files.enabled.is_empty()) else {
            return Ok(());
        };
        let root = self.game_root.join(DISABLED_DIR_NAME);

        fs::remove_file(source).map_err(|e| {
            ManagerError::from(io::Error::new(
                e.kind(),
                format!("删除禁用区旧副本 {} 失败：{}", source.display(), e),
            ))
        })?;

        let mut dir = source.parent();
        while let Some(current) = dir {
            if !current.starts_with(&root) {
                break;
            }
            if fs::remove_dir(current).is_err() {
                break;
            }
            dir = current.parent();
        }

        Ok(())
    }

    fn is_bepinex_installed(&self) -> bool {
        self.game_root.join(BEPINEX_CORE_DLL).is_file()
    }

    fn bepinex_config_path(&self) -> PathBuf {
        self.game_root
            .join("BepInEx")
            .join("config")
            .join("BepInEx.cfg")
    }
}
