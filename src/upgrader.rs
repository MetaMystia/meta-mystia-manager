use crate::config::{
    BEPINEX_CORE_DLL, METAMYSTIA_PLUGIN_GLOB, METAMYSTIA_PLUGIN_OLD_GLOB, RESOURCEEX_ZIP_GLOB,
    RESOURCEEX_ZIP_OLD_GLOB,
};
use crate::downloader::{DownloadJob, Downloader};
use crate::error::{ManagerError, Result};
use crate::extractor::Extractor;
use crate::file_ops::{
    atomic_rename_or_copy, backup_to_path, cleanup_tmp_residue, glob_matches_by_filename,
    next_backup_path, remove_paths,
};
use crate::installer::update_bepinex_config;
use crate::metrics::report_event;
use crate::model::VersionInfo;
use crate::platform::{file_product_version, fs_dry_run};
use crate::preflight::check;
use crate::rollback::Rollback;
use crate::temp_dir::create_temp_dir_with_guard;
use crate::ui::{Ui, UiEvent};

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

struct InstalledAssetPattern<'a> {
    matcher: fn(&str) -> bool,
    pattern: &'a str,
    version_from_filename: fn(&str) -> Option<String>,
}

/// 读取已安装的 BepInEx 构建号（`BepInEx.Core.dll` 产品版本里的 `be.<n>`）
pub fn read_bepinex_version(game_root: &Path) -> Option<String> {
    file_product_version(&game_root.join(BEPINEX_CORE_DLL))
        .and_then(|product_version| parse_bepinex_build(&product_version))
}

/// 从产品版本字符串里提取 `be.<构建号>`
fn parse_bepinex_build(product_version: &str) -> Option<String> {
    let (_, build) = product_version.split_once("be.")?;
    let digits: String = build.chars().take_while(char::is_ascii_digit).collect();

    (!digits.is_empty()).then_some(digits)
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

    /// 指定要升级到的 MetaMystia DLL 版本（`None` 表示最新）
    #[must_use]
    pub fn with_dll_version(mut self, version: Option<String>) -> Self {
        self.dll_version = version;
        self
    }

    /// 指定要升级到的 ResourceExample ZIP 版本（`None` 表示最新）
    #[must_use]
    pub fn with_resourceex_version(mut self, version: Option<String>) -> Self {
        self.resourceex_version = version;
        self
    }

    /// 是否在游戏启动时显示 BepInEx 控制台
    #[must_use]
    pub const fn with_console(mut self, show_console: bool) -> Self {
        self.show_bepinex_console = show_console;
        self
    }

    /// 跳过某个组件的升级
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

    /// 删除遗留的旧版本/备份文件；失败只提示，不阻断升级
    fn cleanup_old_files_by_pattern(
        &self,
        pattern: &Path,
        matcher: fn(&str) -> bool,
    ) -> Result<()> {
        let entries = glob_matches_by_filename(pattern, matcher);
        let result = remove_paths(&entries);

        for removed in &result.removed {
            self.ui.emit(UiEvent::UpgradeDeleted(removed))?;
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
        if fs_dry_run() {
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

    fn consolidate_installed_dlls(&self) -> Result<Option<(String, PathBuf)>> {
        self.consolidate_installed_by_pattern(&InstalledAssetPattern {
            matcher: VersionInfo::is_metamystia_filename,
            pattern: METAMYSTIA_PLUGIN_GLOB,
            version_from_filename: VersionInfo::metamystia_version_from_filename,
        })
    }

    fn consolidate_installed_resourceex(&self) -> Result<Option<(String, PathBuf)>> {
        self.consolidate_installed_by_pattern(&InstalledAssetPattern {
            matcher: VersionInfo::is_resourceex_filename,
            pattern: RESOURCEEX_ZIP_GLOB,
            version_from_filename: VersionInfo::resourceex_version_from_filename,
        })
    }

    fn consolidate_installed_by_pattern(
        &self,
        asset_pattern: &InstalledAssetPattern<'_>,
    ) -> Result<Option<(String, PathBuf)>> {
        let pattern = self.game_root.join(asset_pattern.pattern);
        let Some(dir) = pattern.parent() else {
            return Ok(None);
        };

        if !dir.exists() {
            return Ok(None);
        }

        let mut parsed = Vec::new();

        for path in glob_matches_by_filename(&pattern, asset_pattern.matcher) {
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
        self.cleanup_old_files_by_pattern(
            &self.game_root.join(METAMYSTIA_PLUGIN_OLD_GLOB),
            VersionInfo::is_canonical_metamystia_backup_filename,
        )?;
        self.cleanup_old_files_by_pattern(
            &self.game_root.join(RESOURCEEX_ZIP_OLD_GLOB),
            VersionInfo::is_canonical_resourceex_backup_filename,
        )?;

        Ok(())
    }

    /// 已安装的 (MetaMystia DLL, ResourceExample ZIP) 版本；只读，不改动文件
    pub fn get_installed_versions(&self) -> Result<(Option<String>, Option<String>)> {
        let dll = self.consolidate_installed_dlls()?.map(|(v, _)| v);
        let res = self.consolidate_installed_resourceex()?.map(|(v, _)| v);

        Ok((dll, res))
    }

    pub fn read_bepinex_version(&self) -> Option<String> {
        read_bepinex_version(&self.game_root)
    }

    fn bepinex_installed(&self) -> bool {
        self.game_root.join(BEPINEX_CORE_DLL).is_file()
    }

    fn bepinex_config_path(&self) -> PathBuf {
        self.game_root
            .join("BepInEx")
            .join("config")
            .join("BepInEx.cfg")
    }

    #[allow(
        clippy::too_many_lines,
        reason = "升级流程按步骤线性推进，拆分步骤会让上下文参数来回传递"
    )]
    pub fn upgrade(&self) -> Result<()> {
        report_event("Upgrade.Start", None);

        self.ui.emit(UiEvent::UpgradeCheckingInstalledVersion)?;

        let current_bepinex_version = self.read_bepinex_version();
        let bepinex_installed = self.bepinex_installed();
        let (dll_opt, res_opt) = self.get_installed_versions()?;
        let Some(current_dll_version) = dll_opt else {
            return Err(ManagerError::Other(
                "未找到已安装的 MetaMystia Mod，请先使用安装功能。".to_string(),
            ));
        };
        let current_resourceex_version = res_opt.unwrap_or_default();

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

        let version_info = self.downloader.get_version_info()?;
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

        check(
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

        if !fs_dry_run() {
            let removed = cleanup_tmp_residue(&self.game_root);
            if removed > 0 {
                report_event("Upgrade.ResidueCleaned", Some(&removed.to_string()));
                self.ui.emit(UiEvent::Message(&format!(
                    "已清理 {removed} 个上次运行残留的临时文件"
                )))?;
            }
        }

        let mut rollback = Rollback::new(&self.game_root, &temp_dir);
        // 干跑模式下不会真正写文件，无需备份
        if !fs_dry_run() {
            rollback.plan(&self.bepinex_config_path())?;

            if let Some(path) = &temp_bepinex_path {
                rollback.plan_zip(path, &["BepInEx/config", "BepInEx/plugins"])?;
            }
            if let Some((_, filename)) = &temp_dll_path {
                rollback.plan(
                    &self
                        .game_root
                        .join("BepInEx")
                        .join("plugins")
                        .join(filename),
                )?;
            }
            if let Some((_, filename)) = &temp_resourceex_path {
                rollback.plan(&self.game_root.join("ResourceEx").join(filename))?;
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

                self.ui.emit(UiEvent::UpgradeInstallSuccess(
                    &self.game_root.join("BepInEx"),
                ))?;
                report_event("Upgrade.Installed.BepInEx", new_bepinex_version.as_deref());
            }

            if let Some((temp_path, filename)) = &temp_dll_path {
                let plugins_dir = self.game_root.join("BepInEx").join("plugins");

                Self::backup_existing_assets(
                    &mut rollback,
                    &self.game_root.join(METAMYSTIA_PLUGIN_GLOB),
                    VersionInfo::is_metamystia_filename,
                    filename,
                    "dll.old",
                )?;

                self.ui.emit(UiEvent::UpgradeInstallingDll)?;

                let new_dll_path = plugins_dir.join(filename);
                Self::install_asset_from_temp(temp_path, &new_dll_path, "dll.tmp")?;

                self.ui
                    .emit(UiEvent::UpgradeInstallSuccess(&new_dll_path))?;
                report_event("Upgrade.Installed.DLL", Some(filename));
            } else if !self.skip_dll {
                // 没有新版本要装时，也把残留的其它版本改名备份
                Self::backup_existing_assets(
                    &mut rollback,
                    &self.game_root.join(METAMYSTIA_PLUGIN_GLOB),
                    VersionInfo::is_metamystia_filename,
                    &VersionInfo::metamystia_filename(&current_dll_version),
                    "dll.old",
                )?;
            }

            if let Some((temp_path, filename)) = &temp_resourceex_path {
                let resourceex_dir = self.game_root.join("ResourceEx");
                Self::backup_existing_assets(
                    &mut rollback,
                    &self.game_root.join(RESOURCEEX_ZIP_GLOB),
                    VersionInfo::is_resourceex_filename,
                    filename,
                    "zip.old",
                )?;

                self.ui.emit(UiEvent::UpgradeInstallingResourceex)?;

                let new_zip_path = resourceex_dir.join(filename);
                Self::install_asset_from_temp(temp_path, &new_zip_path, "zip.tmp")?;

                self.ui
                    .emit(UiEvent::UpgradeInstallSuccess(&new_zip_path))?;
                report_event("Upgrade.Installed.ResourceEx", Some(filename));
            } else if !self.skip_resourceex && has_resourceex {
                Self::backup_existing_assets(
                    &mut rollback,
                    &self.game_root.join(RESOURCEEX_ZIP_GLOB),
                    VersionInfo::is_resourceex_filename,
                    &VersionInfo::resourceex_filename(&current_resourceex_version),
                    "zip.old",
                )?;
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
}
