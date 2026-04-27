use crate::config::{
    BEPINEX_VERSION_FILE, METAMYSTIA_PLUGIN_GLOB, METAMYSTIA_PLUGIN_OLD_GLOB, RESOURCEEX_ZIP_GLOB,
    RESOURCEEX_ZIP_OLD_GLOB,
};
use crate::downloader::Downloader;
use crate::error::{ManagerError, Result};
use crate::extractor::Extractor;
use crate::file_ops::{
    atomic_rename_or_copy, backup_paths_with_index, glob_matches_by_filename, remove_glob_files,
    write_bepinex_version_marker,
};
use crate::metrics::report_event;
use crate::model::VersionInfo;
use crate::temp_dir::create_temp_dir_with_guard;
use crate::ui::Ui;

use std::{
    cmp::Ordering,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedVersion {
    parts: Vec<u64>,
    display: String,
}

struct InstalledAssetPattern<'a> {
    pattern: &'a str,
    matcher: fn(&str) -> bool,
    version_from_filename: fn(&str) -> Option<String>,
    backup_suffix: &'a str,
}

impl Ord for ParsedVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let max_len = self.parts.len().max(other.parts.len());

        for index in 0..max_len {
            let left = *self.parts.get(index).unwrap_or(&0);
            let right = *other.parts.get(index).unwrap_or(&0);

            match left.cmp(&right) {
                Ordering::Equal => continue,
                non_eq => return non_eq,
            }
        }

        Ordering::Equal
    }
}

impl PartialOrd for ParsedVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// 升级管理器
pub struct Upgrader<'a> {
    game_root: PathBuf,
    downloader: Downloader<'a>,
    ui: &'a dyn Ui,
}

impl<'a> Upgrader<'a> {
    pub fn new(game_root: PathBuf, ui: &'a dyn Ui) -> Result<Self> {
        let downloader = Downloader::new(ui)?;
        Ok(Self {
            game_root,
            downloader,
            ui,
        })
    }

    fn report_backup_results(&self, results: Vec<Result<PathBuf>>) -> Result<()> {
        for res in results {
            if let Err(e) = res {
                self.ui.upgrade_backup_failed(&format!("{}", e))?;
            }
        }

        Ok(())
    }

    fn cleanup_old_files_by_pattern(
        &self,
        pattern: &Path,
        matcher: fn(&str) -> bool,
    ) -> Result<()> {
        for entry in glob_matches_by_filename(pattern, matcher) {
            let result = remove_glob_files(&entry);
            for removed in result.removed.iter() {
                self.ui.upgrade_deleted(removed)?;
            }
            for (path, err) in result.failed.into_iter() {
                self.ui.upgrade_delete_failed(&path, &format!("{}", err))?;
            }
        }

        Ok(())
    }

    fn parse_numeric_version(version: &str) -> Option<ParsedVersion> {
        let version = VersionInfo::normalize_version(version);
        let parts = VersionInfo::strict_numeric_version_parts(&version)?;

        Some(ParsedVersion {
            parts,
            display: version,
        })
    }

    fn versions_match(current: &str, latest: &str) -> bool {
        VersionInfo::versions_match(current, latest)
    }

    fn backup_existing_assets(
        &self,
        pattern: &Path,
        matcher: fn(&str) -> bool,
        current_filename: &str,
        backup_suffix: &str,
    ) -> Result<()> {
        let mut to_backup = Vec::new();

        for old_entry in glob_matches_by_filename(pattern, matcher) {
            if let Some(old_filename) = old_entry.file_name().and_then(|name| name.to_str())
                && (old_filename == current_filename || old_filename.ends_with(".old"))
            {
                continue;
            }

            to_backup.push(old_entry);
        }

        self.report_backup_results(backup_paths_with_index(&to_backup, backup_suffix))
    }

    fn install_asset_from_temp(
        &self,
        temp_path: &Path,
        destination: &Path,
        temp_extension: &str,
    ) -> Result<()> {
        if let Some(parent) = destination.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                ManagerError::from(std::io::Error::new(
                    e.kind(),
                    format!("创建目录 {} 失败：{}", parent.display(), e),
                ))
            })?;
        }

        let tmp_new = destination.with_extension(temp_extension);
        std::fs::copy(temp_path, &tmp_new).map_err(|e| {
            ManagerError::from(std::io::Error::new(
                e.kind(),
                format!("复制临时文件 {} 失败：{}", tmp_new.display(), e),
            ))
        })?;

        atomic_rename_or_copy(&tmp_new, destination).map_err(|e| {
            ManagerError::from(std::io::Error::other(format!(
                "安装新版本 {} 失败：{}",
                destination.display(),
                e
            )))
        })
    }

    fn consolidate_installed_dlls(&self) -> Result<Option<(String, PathBuf)>> {
        self.consolidate_installed_by_pattern(InstalledAssetPattern {
            pattern: METAMYSTIA_PLUGIN_GLOB,
            matcher: VersionInfo::is_metamystia_filename,
            version_from_filename: VersionInfo::metamystia_version_from_filename,
            backup_suffix: "dll.old",
        })
    }

    fn consolidate_installed_resourceex(&self) -> Result<Option<(String, PathBuf)>> {
        self.consolidate_installed_by_pattern(InstalledAssetPattern {
            pattern: RESOURCEEX_ZIP_GLOB,
            matcher: VersionInfo::is_resourceex_filename,
            version_from_filename: VersionInfo::resourceex_version_from_filename,
            backup_suffix: "zip.old",
        })
    }

    fn consolidate_installed_by_pattern(
        &self,
        asset_pattern: InstalledAssetPattern<'_>,
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
                        "升级扫描失败：无法从文件名解析版本：{}",
                        filename
                    )));
                };

                let Some(parsed_version) = Self::parse_numeric_version(&version) else {
                    return Err(ManagerError::Other(format!(
                        "升级扫描失败：无法解析版本号：{}",
                        filename
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

        let to_backup: Vec<PathBuf> = parsed
            .into_iter()
            .rev()
            .skip(1)
            .map(|(_, path)| path)
            .collect();

        self.report_backup_results(backup_paths_with_index(
            &to_backup,
            asset_pattern.backup_suffix,
        ))?;

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

    fn get_installed_versions(&self) -> Result<(Option<String>, Option<String>)> {
        let dll = self.consolidate_installed_dlls()?.map(|(v, _)| v);
        let res = self.consolidate_installed_resourceex()?.map(|(v, _)| v);

        Ok((dll, res))
    }

    fn read_bepinex_version(&self) -> Option<String> {
        let version_file = self.game_root.join(BEPINEX_VERSION_FILE);
        std::fs::read_to_string(&version_file)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// 检查是否有可用升级
    pub fn has_updates(&self, version_info: &VersionInfo) -> Result<(bool, bool, bool)> {
        let bep_needs = version_info
            .bepinex_version()
            .ok()
            .map(|latest| {
                self.read_bepinex_version()
                    .map(|cur| cur != latest)
                    .unwrap_or(true)
            })
            .unwrap_or(false);

        let (dll_opt, res_opt) = self.get_installed_versions()?;

        let dll_needs = dll_opt
            .as_ref()
            .map(|cur| !Self::versions_match(cur, version_info.latest_dll()))
            .unwrap_or(false);
        let res_needs = res_opt
            .as_ref()
            .map(|cur| !Self::versions_match(cur, version_info.latest_resourceex()))
            .unwrap_or(false);

        Ok((bep_needs, dll_needs, res_needs))
    }

    /// 执行升级
    pub fn upgrade(&self) -> Result<()> {
        report_event("Upgrade.Start", None);

        // 1. 查找当前安装的版本
        self.ui.upgrade_checking_installed_version()?;

        let current_bepinex_version = self.read_bepinex_version();
        let (dll_opt, res_opt) = self.get_installed_versions()?;
        let current_dll_version = match dll_opt {
            Some(v) => v,
            None => {
                return Err(ManagerError::Other(
                    "未找到已安装的 MetaMystia Mod，请先使用安装功能。".to_string(),
                ));
            }
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

        // 检查是否已安装 ResourceExample ZIP
        let has_resourceex = !current_resourceex_version.is_empty();
        if has_resourceex {
            self.ui.upgrade_detected_resourceex()?;
        }

        // 2. 获取最新版本信息
        self.ui.blank_line()?;
        let version_info = self.downloader.get_version_info()?;
        report_event("Upgrade.VersionInfo", Some(&version_info.to_string()));

        // 检查 BepInEx 是否需要升级
        let new_bepinex_version = version_info.bepinex_version().ok().map(|s| s.to_string());
        let bepinex_needs_upgrade = new_bepinex_version
            .as_ref()
            .map(|new_ver| {
                current_bepinex_version
                    .as_ref()
                    .map(|cur| cur != new_ver)
                    .unwrap_or(true)
            })
            .unwrap_or(false);
        if let Some(ref new_ver) = new_bepinex_version {
            self.ui.upgrade_display_current_and_latest_bepinex(
                current_bepinex_version.as_deref().unwrap_or("未知"),
                new_ver,
            )?;
        }

        // 检查 MetaMystia DLL 是否需要升级
        let new_dll_version = version_info.latest_dll();
        let dll_needs_upgrade = !Self::versions_match(&current_dll_version, new_dll_version);
        self.ui
            .upgrade_display_current_and_latest_dll(&current_dll_version, new_dll_version)?;

        // 检查 ResourceExample ZIP 是否需要升级
        let new_resourceex_version = version_info.latest_resourceex();
        let resourceex_needs_upgrade =
            !Self::versions_match(&current_resourceex_version, new_resourceex_version)
                && has_resourceex;
        if has_resourceex {
            self.ui.upgrade_display_current_and_latest_resourceex(
                &current_resourceex_version,
                new_resourceex_version,
            )?;
        }

        if !bepinex_needs_upgrade && !dll_needs_upgrade && !resourceex_needs_upgrade {
            self.ui.upgrade_no_update_needed()?;
            return Ok(());
        }

        // 显示升级信息
        if bepinex_needs_upgrade {
            self.ui.upgrade_bepinex_needs_upgrade()?;
        } else if new_bepinex_version.is_some() {
            self.ui.upgrade_bepinex_already_latest()?;
        }
        if dll_needs_upgrade {
            self.ui
                .upgrade_detected_new_dll(&current_dll_version, new_dll_version)?;

            // 显示 GitHub Release Notes（获取新版本的发行说明）
            match self
                .downloader
                .fetch_and_display_github_release_notes(Some(new_dll_version))
            {
                Ok(Some(_)) => {
                    if !self.ui.download_ask_continue_after_release_notes()? {
                        return Err(ManagerError::UserCancelled);
                    }
                }
                Ok(None) => {}
                Err(_) => {}
            }
        } else {
            self.ui.upgrade_dll_already_latest()?;
        }
        if resourceex_needs_upgrade {
            if dll_needs_upgrade {
                self.ui.blank_line()?;
            }
            self.ui.upgrade_resourceex_needs_upgrade()?;
        }
        if bepinex_needs_upgrade && !dll_needs_upgrade && !resourceex_needs_upgrade {
            self.ui.blank_line()?;
        }
        if dll_needs_upgrade && !resourceex_needs_upgrade {
            self.ui.blank_line()?;
        }

        // 3. 获取分享码
        let share_code = self.downloader.get_share_code()?;

        // 4. 下载新版本

        let (temp_dir, _temp_guard) = create_temp_dir_with_guard(&self.game_root).map_err(|e| {
            ManagerError::from(std::io::Error::new(
                e.kind(),
                format!("创建临时目录失败：{}", e),
            ))
        })?;

        // 下载 BepInEx（仅当需要升级时）
        let temp_bepinex_path = if bepinex_needs_upgrade {
            let bepinex_filename = version_info.bepinex_filename()?;
            let path = temp_dir.join(bepinex_filename);

            self.ui.upgrade_downloading_bepinex()?;

            self.downloader.download_bepinex(&version_info, &path)?;

            Some(path)
        } else {
            None
        };

        // 下载 DLL（仅当需要升级时）
        let temp_dll_path = if dll_needs_upgrade {
            let new_dll_filename = VersionInfo::metamystia_filename(new_dll_version);
            let path = temp_dir.join(&new_dll_filename);

            self.ui.upgrade_downloading_dll()?;

            self.downloader
                .download_metamystia(&share_code, new_dll_version, &path, true)?;

            Some((path, new_dll_filename))
        } else {
            None
        };

        // 下载 ResourceExample ZIP（仅当已安装且需要升级时）
        let temp_resourceex_path = if has_resourceex && resourceex_needs_upgrade {
            let resourceex_filename = VersionInfo::resourceex_filename(new_resourceex_version);
            let path = temp_dir.join(&resourceex_filename);

            self.ui.upgrade_downloading_resourceex()?;

            self.downloader
                .download_resourceex(&share_code, new_resourceex_version, &path)?;

            Some((path, resourceex_filename))
        } else {
            None
        };

        // 5. 安装 BepInEx（仅当需要升级时）
        if let Some(bepinex_path) = temp_bepinex_path {
            self.ui.upgrade_installing_bepinex()?;

            // 升级时保留 plugins 和 config 目录
            Extractor::deploy_bepinex(
                &bepinex_path,
                &self.game_root,
                &["BepInEx/config", "BepInEx/plugins"],
            )?;

            // 更新版本标记文件
            write_bepinex_version_marker(&self.game_root, &version_info);

            self.ui
                .upgrade_install_success(&self.game_root.join("BepInEx"))?;
            report_event("Upgrade.Installed.BepInEx", new_bepinex_version.as_deref());
        }

        // 6. 安装新版本 MetaMystia DLL（仅当需要升级时）
        if let Some((temp_path, filename)) = temp_dll_path {
            let plugins_dir = self.game_root.join("BepInEx").join("plugins");

            self.backup_existing_assets(
                &self.game_root.join(METAMYSTIA_PLUGIN_GLOB),
                VersionInfo::is_metamystia_filename,
                &filename,
                "dll.old",
            )?;

            if !bepinex_needs_upgrade {
                self.ui.blank_line()?;
                self.ui.blank_line()?;
            }
            self.ui.upgrade_installing_dll()?;

            let new_dll_path = plugins_dir.join(&filename);
            self.install_asset_from_temp(&temp_path, &new_dll_path, "dll.tmp")?;

            self.ui.upgrade_install_success(&new_dll_path)?;
            report_event("Upgrade.Installed.DLL", Some(&filename));
        }

        // 7. 安装 ResourceExample ZIP（仅当需要升级时）
        if let Some((temp_path, filename)) = temp_resourceex_path {
            let resourceex_dir = self.game_root.join("ResourceEx");
            self.backup_existing_assets(
                &self.game_root.join(RESOURCEEX_ZIP_GLOB),
                VersionInfo::is_resourceex_filename,
                &filename,
                "zip.old",
            )?;

            if !bepinex_needs_upgrade && !dll_needs_upgrade {
                self.ui.blank_line()?;
                self.ui.blank_line()?;
            }
            self.ui.upgrade_installing_resourceex()?;

            let new_zip_path = resourceex_dir.join(&filename);
            self.install_asset_from_temp(&temp_path, &new_zip_path, "zip.tmp")?;

            self.ui.upgrade_install_success(&new_zip_path)?;
            report_event("Upgrade.Installed.ResourceEx", Some(&filename));
        }

        // 8. 清理临时文件
        self.ui.upgrade_cleanup_start()?;
        self.cleanup_old_files()?;

        self.ui.upgrade_done()?;
        report_event("Upgrade.Finished", None);

        Ok(())
    }
}
