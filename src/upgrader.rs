use crate::downloader::Downloader;
use crate::error::{ManagerError, Result};
use crate::file_ops::{
    atomic_rename_or_copy, backup_paths_with_index, glob_matches, remove_glob_files,
};
use crate::metrics::report_event;
use crate::model::VersionInfo;
use crate::temp_dir::create_temp_dir_with_guard;
use crate::ui::Ui;

use semver::Version;
use std::path::{Path, PathBuf};

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

    fn parse_version(name: &str, prefix: &str, suffix: &str) -> Option<Version> {
        if let Some(s) = name.strip_prefix(prefix)
            && let Some(ver_part) = s.strip_suffix(suffix)
            && let Ok(v) = Version::parse(ver_part)
        {
            return Some(v);
        }
        None
    }

    fn consolidate_installed_dlls(&self) -> Result<Option<(String, PathBuf)>> {
        let plugins_dir = self.game_root.join("BepInEx").join("plugins");
        self.consolidate_installed_by_pattern(
            &plugins_dir,
            "MetaMystia-*.dll",
            "MetaMystia-v",
            ".dll",
            "dll.old",
        )
    }

    fn consolidate_installed_resourceex(&self) -> Result<Option<(String, PathBuf)>> {
        let resourceex_dir = self.game_root.join("ResourceEx");
        self.consolidate_installed_by_pattern(
            &resourceex_dir,
            "ResourceExample-*.zip",
            "ResourceExample-v",
            ".zip",
            "zip.old",
        )
    }

    fn consolidate_installed_by_pattern(
        &self,
        dir: &Path,
        pattern: &str,
        prefix: &str,
        suffix: &str,
        backup_suffix: &str,
    ) -> Result<Option<(String, PathBuf)>> {
        if !dir.exists() {
            return Ok(None);
        }

        let mut parsed = Vec::new();
        let mut unparsed = Vec::new();

        for path in glob_matches(&dir.join(pattern)).into_iter() {
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(v) = Self::parse_version(filename, prefix, suffix) {
                    parsed.push((v, path.clone()));
                } else {
                    self.ui.upgrade_warn_unparse_version(filename)?;
                    unparsed.push(path.clone());
                }
            }
        }

        if parsed.is_empty() && unparsed.is_empty() {
            return Ok(None);
        }

        let latest: PathBuf;
        let latest_version_str: String;

        if !parsed.is_empty() {
            parsed.sort_by(|a, b| a.0.cmp(&b.0));

            let (v, p) = parsed.last().unwrap();
            latest = p.clone();
            latest_version_str = v.to_string();

            let to_backup: Vec<PathBuf> =
                parsed.into_iter().rev().skip(1).map(|(_, p)| p).collect();

            let results = backup_paths_with_index(&to_backup, backup_suffix);
            for res in results {
                match res {
                    Ok(_backup) => (),
                    Err(e) => self.ui.upgrade_backup_failed(&format!("{}", e))?,
                }
            }
        } else {
            if unparsed.is_empty() {
                return Ok(None);
            }

            unparsed.sort();

            latest = unparsed.last().unwrap().clone();
            latest_version_str = latest
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let to_backup: Vec<PathBuf> = unparsed.into_iter().rev().skip(1).collect();

            let results = backup_paths_with_index(&to_backup, backup_suffix);
            for res in results {
                match res {
                    Ok(_backup) => (),
                    Err(e) => self.ui.upgrade_backup_failed(&format!("{}", e))?,
                }
            }
        }

        Ok(Some((latest_version_str, latest)))
    }

    fn cleanup_old_files(&self) -> Result<()> {
        let plugins_dir = self.game_root.join("BepInEx").join("plugins");
        if plugins_dir.exists() {
            let pattern = plugins_dir.join("MetaMystia-*.dll.old*");
            let result = remove_glob_files(&pattern);
            for removed in result.removed.iter() {
                self.ui.upgrade_deleted(removed)?;
            }
            for (path, err) in result.failed.into_iter() {
                self.ui.upgrade_delete_failed(&path, &format!("{}", err))?;
            }
        }

        let resourceex_dir = self.game_root.join("ResourceEx");
        if resourceex_dir.exists() {
            let pattern = resourceex_dir.join("ResourceExample-*.zip.old*");
            let result = remove_glob_files(&pattern);
            for removed in result.removed.iter() {
                self.ui.upgrade_deleted(removed)?;
            }
            for (path, err) in result.failed.into_iter() {
                self.ui.upgrade_delete_failed(&path, &format!("{}", err))?;
            }
        }

        Ok(())
    }

    fn get_installed_versions(&self) -> Result<(Option<String>, Option<String>)> {
        let dll = self.consolidate_installed_dlls()?.map(|(v, _)| v);
        let res = self.consolidate_installed_resourceex()?.map(|(v, _)| v);

        Ok((dll, res))
    }

    /// 检查是否有可用升级
    pub fn has_updates(&self, version_info: &VersionInfo) -> Result<(bool, bool)> {
        let (dll_opt, res_opt) = self.get_installed_versions()?;

        let dll_needs = dll_opt
            .as_ref()
            .map(|cur| cur != version_info.latest_dll())
            .unwrap_or(false);
        let res_needs = res_opt
            .as_ref()
            .map(|cur| cur != version_info.latest_resourceex())
            .unwrap_or(false);

        Ok((dll_needs, res_needs))
    }

    /// 执行升级
    pub fn upgrade(&self) -> Result<()> {
        report_event("Upgrade.Start", None);

        // 1. 查找当前安装的版本
        self.ui.upgrade_checking_installed_version()?;

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
                "dll:{};resourceex:{}",
                current_dll_version, current_resourceex_version
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

        // 检查 MetaMystia DLL 是否需要升级
        let new_dll_version = version_info.latest_dll();
        let dll_needs_upgrade = current_dll_version != new_dll_version;
        self.ui
            .upgrade_display_current_and_latest_dll(&current_dll_version, new_dll_version)?;

        // 检查 ResourceExample ZIP 是否需要升级
        let new_resourceex_version = version_info.latest_resourceex();
        let resourceex_needs_upgrade =
            (current_resourceex_version != new_resourceex_version) && has_resourceex;
        if has_resourceex {
            self.ui.upgrade_display_current_and_latest_resourceex(
                &current_resourceex_version,
                new_resourceex_version,
            )?;
        }

        if !dll_needs_upgrade && !resourceex_needs_upgrade {
            self.ui.upgrade_no_update_needed()?;
            return Ok(());
        }

        // 显示升级信息
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
            self.ui.upgrade_resourceex_needs_upgrade()?;
        }
        if dll_needs_upgrade && !resourceex_needs_upgrade {
            self.ui.blank_line()?;
        }

        // 3. 获取分享码
        let share_code = self.downloader.get_share_code()?;

        // 4. 下载新版本

        if dll_needs_upgrade {
            self.ui.upgrade_downloading_dll()?;
        }

        let (temp_dir, _temp_guard) = create_temp_dir_with_guard(&self.game_root).map_err(|e| {
            ManagerError::from(std::io::Error::new(
                e.kind(),
                format!("创建临时目录失败：{}", e),
            ))
        })?;

        // 下载 DLL（仅当需要升级时）
        let temp_dll_path = if dll_needs_upgrade {
            let new_dll_filename = VersionInfo::metamystia_filename(new_dll_version);
            let path = temp_dir.join(&new_dll_filename);

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

        // 5. 安装新版本 MetaMystia DLL（仅当需要升级时）
        if let Some((temp_path, filename)) = temp_dll_path {
            let plugins_dir = self.game_root.join("BepInEx").join("plugins");
            let mut backup_paths = Vec::new();

            let old_dll_pattern = plugins_dir.join("MetaMystia-*.dll");
            let mut to_backup = Vec::new();
            for old_entry in glob_matches(&old_dll_pattern) {
                if let Some(old_filename) = old_entry.file_name().and_then(|n| n.to_str())
                    && (old_filename == filename || old_filename.ends_with(".old"))
                {
                    continue;
                }
                to_backup.push(old_entry);
            }

            for res in backup_paths_with_index(&to_backup, "dll.old") {
                match res {
                    Ok(backup_path) => backup_paths.push(backup_path),
                    Err(e) => self.ui.upgrade_backup_failed(&format!("{}", e))?,
                }
            }

            self.ui.upgrade_installing_dll()?;

            let new_dll_path = plugins_dir.join(&filename);

            if !plugins_dir.exists() {
                std::fs::create_dir_all(&plugins_dir).map_err(|e| {
                    ManagerError::from(std::io::Error::new(
                        e.kind(),
                        format!("创建 plugins 目录 {} 失败：{}", plugins_dir.display(), e),
                    ))
                })?;
            }

            let tmp_new = new_dll_path.with_extension("dll.tmp");
            std::fs::copy(&temp_path, &tmp_new).map_err(|e| {
                ManagerError::from(std::io::Error::new(
                    e.kind(),
                    format!("复制临时文件 {} 失败：{}", tmp_new.display(), e),
                ))
            })?;
            atomic_rename_or_copy(&tmp_new, &new_dll_path).map_err(|e| {
                ManagerError::from(std::io::Error::other(format!(
                    "安装新版本 {} 失败：{}",
                    new_dll_path.display(),
                    e
                )))
            })?;

            self.ui.upgrade_install_success(&new_dll_path)?;
            report_event("Upgrade.Installed.DLL", Some(&filename));

            if backup_paths.is_empty() {
                None
            } else {
                Some(backup_paths)
            }
        } else {
            None
        };

        // 6. 安装 ResourceExample ZIP（仅当需要升级时）
        if let Some((temp_path, filename)) = temp_resourceex_path {
            let resourceex_dir = self.game_root.join("ResourceEx");
            let old_resourceex_pattern = resourceex_dir.join("ResourceExample-*.zip");
            let mut to_backup = Vec::new();
            for old_entry in glob_matches(&old_resourceex_pattern) {
                if let Some(old_filename) = old_entry.file_name().and_then(|n| n.to_str())
                    && (old_filename == filename || old_filename.ends_with(".old"))
                {
                    continue;
                }
                to_backup.push(old_entry);
            }

            for res in backup_paths_with_index(&to_backup, "zip.old") {
                match res {
                    Ok(_) => (),
                    Err(e) => self.ui.upgrade_backup_failed(&format!("{}", e))?,
                }
            }

            if !dll_needs_upgrade {
                self.ui.blank_line()?;
                self.ui.blank_line()?;
            }
            self.ui.upgrade_installing_resourceex()?;

            let new_zip_path = resourceex_dir.join(&filename);
            let tmp_new = new_zip_path.with_extension("zip.tmp");
            std::fs::copy(&temp_path, &tmp_new).map_err(|e| {
                ManagerError::from(std::io::Error::new(
                    e.kind(),
                    format!("复制临时文件 {} 失败：{}", tmp_new.display(), e),
                ))
            })?;
            atomic_rename_or_copy(&tmp_new, &new_zip_path).map_err(|e| {
                ManagerError::from(std::io::Error::other(format!(
                    "安装新版本 {} 失败：{}",
                    new_zip_path.display(),
                    e
                )))
            })?;

            self.ui.upgrade_install_success(&new_zip_path)?;
            report_event("Upgrade.Installed.ResourceEx", Some(&filename));
        }

        // 7. 清理临时文件
        self.ui.upgrade_cleanup_start()?;
        self.cleanup_old_files()?;

        self.ui.upgrade_done()?;
        report_event("Upgrade.Finished", None);

        Ok(())
    }
}
