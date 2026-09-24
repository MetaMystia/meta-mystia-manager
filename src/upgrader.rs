use crate::config::{
    BEPINEX_VERSION_FILE, METAMYSTIA_PLUGIN_GLOB, METAMYSTIA_PLUGIN_OLD_GLOB, RESOURCEEX_ZIP_GLOB,
    RESOURCEEX_ZIP_OLD_GLOB,
};
use crate::downloader::{DownloadJob, Downloader};
use crate::error::{ManagerError, Result};
use crate::extractor::Extractor;
use crate::file_ops::{
    atomic_rename_or_copy, backup_paths_with_index, glob_matches_by_filename, remove_glob_files,
    write_bepinex_version_marker,
};
use crate::metrics::report_event;
use crate::model::VersionInfo;
use crate::platform;
use crate::preflight;
use crate::rollback::Rollback;
use crate::temp_dir::create_temp_dir_with_guard;
use crate::ui::Ui;

use std::{
    cmp::Ordering,
    fs, io,
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

/// 文件名是否带有 `.old` 备份后缀（不区分大小写）
fn is_old_backup(filename: &str) -> bool {
    Path::new(filename)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("old"))
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

impl PartialOrd for ParsedVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub struct Upgrader<'a> {
    game_root: PathBuf,
    downloader: Downloader<'a>,
    ui: &'a dyn Ui,
}

impl<'a> Upgrader<'a> {
    pub fn new(game_root: PathBuf, ui: &'a dyn Ui) -> Self {
        let downloader = Downloader::new(ui);
        Self {
            game_root,
            downloader,
            ui,
        }
    }

    /// 备份待覆盖文件；任一失败即中止，避免在没有备份的情况下继续覆盖
    fn backup_paths(paths: &[PathBuf], suffix: &str) -> Result<()> {
        for result in backup_paths_with_index(paths, suffix) {
            result?;
        }

        Ok(())
    }

    /// 删除遗留的旧版本/备份文件；失败只提示，不阻断升级
    fn cleanup_old_files_by_pattern(
        &self,
        pattern: &Path,
        matcher: fn(&str) -> bool,
    ) -> Result<()> {
        for entry in glob_matches_by_filename(pattern, matcher) {
            let result = remove_glob_files(&entry);
            for removed in &result.removed {
                self.ui.upgrade_deleted(removed)?;
            }
            for (path, err) in result.failed {
                self.ui.upgrade_delete_failed(&path, &format!("{err}"))?;
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
        pattern: &Path,
        matcher: fn(&str) -> bool,
        current_filename: &str,
        backup_suffix: &str,
    ) -> Result<()> {
        let mut to_backup = Vec::new();

        for old_entry in glob_matches_by_filename(pattern, matcher) {
            if let Some(old_filename) = old_entry.file_name().and_then(|name| name.to_str())
                && (old_filename == current_filename || is_old_backup(old_filename))
            {
                continue;
            }

            to_backup.push(old_entry);
        }

        Self::backup_paths(&to_backup, backup_suffix)
    }

    fn install_asset_from_temp(
        temp_path: &Path,
        destination: &Path,
        temp_extension: &str,
    ) -> Result<()> {
        // 开发模拟模式的干跑：只打印将要执行的动作
        if platform::fs_dry_run() {
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

        atomic_rename_or_copy(&tmp_new, destination).map_err(|e| {
            ManagerError::from(io::Error::other(format!(
                "安装新版本 {} 失败：{}",
                destination.display(),
                e
            )))
        })
    }

    fn consolidate_installed_dlls(&self) -> Result<Option<(String, PathBuf)>> {
        self.consolidate_installed_by_pattern(&InstalledAssetPattern {
            pattern: METAMYSTIA_PLUGIN_GLOB,
            matcher: VersionInfo::is_metamystia_filename,
            version_from_filename: VersionInfo::metamystia_version_from_filename,
            backup_suffix: "dll.old",
        })
    }

    fn consolidate_installed_resourceex(&self) -> Result<Option<(String, PathBuf)>> {
        self.consolidate_installed_by_pattern(&InstalledAssetPattern {
            pattern: RESOURCEEX_ZIP_GLOB,
            matcher: VersionInfo::is_resourceex_filename,
            version_from_filename: VersionInfo::resourceex_version_from_filename,
            backup_suffix: "zip.old",
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

        let to_backup: Vec<PathBuf> = parsed
            .into_iter()
            .rev()
            .skip(1)
            .map(|(_, path)| path)
            .collect();

        Self::backup_paths(&to_backup, asset_pattern.backup_suffix)?;

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
        fs::read_to_string(&version_file)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// 检查是否有可用升级，返回（BepInEx、MetaMystia DLL、ResourceExample ZIP）是否需更新
    pub fn has_updates(&self, version_info: &VersionInfo) -> Result<(bool, bool, bool)> {
        let bep_needs = version_info
            .bepinex_version()
            .is_ok_and(|latest| self.read_bepinex_version().is_none_or(|cur| cur != latest));

        let (dll_opt, res_opt) = self.get_installed_versions()?;

        let dll_needs = dll_opt.as_ref().is_some_and(|cur| {
            version_info
                .latest_dll()
                .is_ok_and(|latest| !Self::versions_match(cur, latest))
        });
        let res_needs = res_opt.as_ref().is_some_and(|cur| {
            version_info
                .latest_resourceex()
                .is_ok_and(|latest| !Self::versions_match(cur, latest))
        });

        Ok((bep_needs, dll_needs, res_needs))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "升级流程按步骤线性推进，拆分步骤会让上下文参数来回传递"
    )]
    pub fn upgrade(&self) -> Result<()> {
        report_event("Upgrade.Start", None);

        // 1. 查找当前安装的版本
        self.ui.upgrade_checking_installed_version()?;

        let current_bepinex_version = self.read_bepinex_version();
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
        let new_bepinex_version = version_info.bepinex_version().ok().map(ToString::to_string);
        let bepinex_needs_upgrade = new_bepinex_version
            .as_ref()
            .is_some_and(|new_ver| current_bepinex_version.as_ref() != Some(new_ver));
        if let Some(ref new_ver) = new_bepinex_version {
            self.ui.upgrade_display_current_and_latest_bepinex(
                current_bepinex_version.as_deref().unwrap_or("未知"),
                new_ver,
            )?;
        }

        // 检查 MetaMystia DLL 是否需要升级
        let new_dll_version = version_info.latest_dll()?;
        let dll_needs_upgrade = !Self::versions_match(&current_dll_version, new_dll_version);
        self.ui
            .upgrade_display_current_and_latest_dll(&current_dll_version, new_dll_version)?;

        // 检查 ResourceExample ZIP 是否需要升级
        let new_resourceex_version = version_info.latest_resourceex()?;
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
            if let Ok(Some(_)) = self
                .downloader
                .fetch_and_display_github_release_notes(Some(new_dll_version))
                && !self.ui.download_ask_continue_after_release_notes()?
            {
                return Err(ManagerError::UserCancelled);
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

        // 3. 下载新版本
        let (temp_dir, _temp_guard) = create_temp_dir_with_guard(&self.game_root).map_err(|e| {
            ManagerError::from(io::Error::new(e.kind(), format!("创建临时目录失败：{e}")))
        })?;

        preflight::check(self.ui, &self.game_root, &temp_dir)?;

        let temp_bepinex_path = if bepinex_needs_upgrade {
            Some(temp_dir.join(version_info.bepinex_filename()?))
        } else {
            None
        };

        let temp_dll_path = if dll_needs_upgrade {
            let new_dll_filename = VersionInfo::metamystia_filename(new_dll_version);
            let path = temp_dir.join(&new_dll_filename);

            Some((path, new_dll_filename))
        } else {
            None
        };

        let temp_resourceex_path = if has_resourceex && resourceex_needs_upgrade {
            let resourceex_filename = VersionInfo::resourceex_filename(new_resourceex_version);
            let path = temp_dir.join(&resourceex_filename);

            Some((path, resourceex_filename))
        } else {
            None
        };

        let mut jobs: Vec<DownloadJob<'_>> = Vec::new();

        if let Some(path) = &temp_bepinex_path {
            jobs.push((
                "下载 BepInEx",
                Box::new(|| {
                    self.ui.upgrade_downloading_bepinex()?;
                    self.downloader
                        .download_bepinex(&version_info, path)
                        .map(|_| ())
                }),
            ));
        }
        if let Some((path, _)) = &temp_dll_path {
            jobs.push((
                "下载 MetaMystia DLL",
                Box::new(|| {
                    self.ui.upgrade_downloading_dll()?;
                    self.downloader.download_metamystia(
                        new_dll_version,
                        path,
                        version_info.paths.dll.as_deref(),
                        true,
                    )
                }),
            ));
        }
        if let Some((path, _)) = &temp_resourceex_path {
            jobs.push((
                "下载 ResourceExample",
                Box::new(|| {
                    self.ui.upgrade_downloading_resourceex()?;
                    self.downloader.download_resourceex(
                        new_resourceex_version,
                        path,
                        version_info.paths.zip.as_deref(),
                    )
                }),
            ));
        }

        self.downloader.download_files(&jobs)?;
        drop(jobs);

        // 部署前记录将被覆盖/新建的文件，失败时回滚
        let mut rollback = Rollback::new(&self.game_root, &temp_dir);
        // 干跑模式下不会真正写文件，无需备份
        if !platform::fs_dry_run() {
            if let Some(path) = &temp_bepinex_path {
                rollback.plan_zip(path, &["BepInEx/config", "BepInEx/plugins"])?;
                rollback.plan(&self.game_root.join(BEPINEX_VERSION_FILE))?;
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
        }

        let deploy = || -> Result<()> {
            // 4. 安装 BepInEx（仅当需要升级时；升级时保留 plugins 和 config 目录）
            if let Some(bepinex_path) = &temp_bepinex_path {
                self.ui.upgrade_installing_bepinex()?;

                Extractor::deploy_bepinex(
                    bepinex_path,
                    &self.game_root,
                    &["BepInEx/config", "BepInEx/plugins"],
                )?;

                // 更新版本标记文件
                write_bepinex_version_marker(&self.game_root, &version_info)?;

                self.ui
                    .upgrade_install_success(&self.game_root.join("BepInEx"))?;
                report_event("Upgrade.Installed.BepInEx", new_bepinex_version.as_deref());
            }

            // 5. 安装新版本 MetaMystia DLL（仅当需要升级时）
            if let Some((temp_path, filename)) = &temp_dll_path {
                let plugins_dir = self.game_root.join("BepInEx").join("plugins");

                Self::backup_existing_assets(
                    &self.game_root.join(METAMYSTIA_PLUGIN_GLOB),
                    VersionInfo::is_metamystia_filename,
                    filename,
                    "dll.old",
                )?;

                if !bepinex_needs_upgrade {
                    self.ui.blank_line()?;
                    self.ui.blank_line()?;
                }
                self.ui.upgrade_installing_dll()?;

                let new_dll_path = plugins_dir.join(filename);
                Self::install_asset_from_temp(temp_path, &new_dll_path, "dll.tmp")?;

                self.ui.upgrade_install_success(&new_dll_path)?;
                report_event("Upgrade.Installed.DLL", Some(filename));
            }

            // 6. 安装 ResourceExample ZIP（仅当需要升级时）
            if let Some((temp_path, filename)) = &temp_resourceex_path {
                let resourceex_dir = self.game_root.join("ResourceEx");
                Self::backup_existing_assets(
                    &self.game_root.join(RESOURCEEX_ZIP_GLOB),
                    VersionInfo::is_resourceex_filename,
                    filename,
                    "zip.old",
                )?;

                if !bepinex_needs_upgrade && !dll_needs_upgrade {
                    self.ui.blank_line()?;
                    self.ui.blank_line()?;
                }
                self.ui.upgrade_installing_resourceex()?;

                let new_zip_path = resourceex_dir.join(filename);
                Self::install_asset_from_temp(temp_path, &new_zip_path, "zip.tmp")?;

                self.ui.upgrade_install_success(&new_zip_path)?;
                report_event("Upgrade.Installed.ResourceEx", Some(filename));
            }

            Ok(())
        };

        if let Err(e) = deploy() {
            let _ = rollback.restore();
            self.ui.message("升级失败，已回滚到操作前状态")?;
            report_event("Upgrade.Failed.RolledBack", Some(&format!("{e}")));

            return Err(e);
        }
        rollback.discard();

        // 7. 清理临时文件
        self.ui.upgrade_cleanup_start()?;
        self.cleanup_old_files()?;

        self.ui.upgrade_done()?;
        report_event("Upgrade.Finished", None);

        Ok(())
    }
}
