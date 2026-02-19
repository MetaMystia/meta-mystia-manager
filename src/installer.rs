use crate::cli::InstallConfig;
use crate::config::UninstallMode;
use crate::downloader::Downloader;
use crate::error::{ManagerError, Result};
use crate::extractor::Extractor;
use crate::file_ops::{atomic_rename_or_copy, count_results, execute_deletion, glob_matches};
use crate::metrics::report_event;
use crate::model::VersionInfo;
use crate::temp_dir::create_temp_dir_with_guard;
use crate::ui::Ui;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

/// 安装管理器
pub struct Installer<'a> {
    game_root: PathBuf,
    downloader: Downloader<'a>,
    ui: &'a dyn Ui,
}

impl<'a> Installer<'a> {
    pub fn new(game_root: PathBuf, ui: &'a dyn Ui) -> Result<Self> {
        let downloader = Downloader::new(ui)?;
        Ok(Self {
            game_root,
            downloader,
            ui,
        })
    }

    /// 检查是否已安装 MetaMystia DLL
    pub fn check_metamystia_installed(&self) -> bool {
        let metamystia_pattern = self
            .game_root
            .join("BepInEx")
            .join("plugins")
            .join("MetaMystia-*.dll");

        let matches = glob_matches(&metamystia_pattern);
        !matches.is_empty()
    }

    /// 检查是否已安装 ResourceExample ZIP
    pub fn check_resourceex_installed(&self) -> bool {
        let resourceex_dir = self.game_root.join("ResourceEx");
        resourceex_dir.exists() && resourceex_dir.is_dir() && {
            let resourceex_pattern = resourceex_dir.join("ResourceExample-*.zip");
            let matches = glob_matches(&resourceex_pattern);
            !matches.is_empty()
        }
    }

    /// 检查是否已安装 BepInEx
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

        // 添加路径到删除列表
        let mut push = |p: PathBuf| {
            if seen.insert(p.clone()) {
                targets.push(p);
            }
        };

        // 1. 删除 BepInEx 目录下的所有项目（跳过 plugins）
        let bepinex_dir = game_root.join("BepInEx");
        if bepinex_dir.exists() {
            for entry in std::fs::read_dir(&bepinex_dir).map_err(ManagerError::from)? {
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
            let metamystia_pattern = plugins_dir.join("MetaMystia-*.dll");
            for entry in glob_matches(&metamystia_pattern) {
                push(entry);
            }
        }

        // 3. 删除 ResourceEx 目录中的 ResourceExample ZIP
        let resourceex_dir = game_root.join("ResourceEx");
        if resourceex_dir.exists() {
            let resourceex_pattern = resourceex_dir.join("ResourceExample-*.zip");
            for entry in glob_matches(&resourceex_pattern) {
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

    /// 执行安装流程
    pub fn install(
        &self,
        cleanup_before_deploy: bool,
        config: Option<&InstallConfig>,
    ) -> Result<()> {
        report_event("Install.Start", None);

        // 1. 获取版本信息
        self.ui.install_display_step(1, "获取版本信息")?;
        let version_info = self.downloader.get_version_info()?;
        self.ui.install_display_version_info(&version_info)?;
        report_event("Install.VersionInfo", Some(&version_info.to_string()));

        // 2. 获取分享码
        self.ui.install_display_step(2, "获取下载链接")?;
        let share_code = self.downloader.get_share_code()?;
        report_event("Install.ShareCode", Some(&share_code));

        // 2.1. 询问是否安装 ResourceEx（如果 config 存在则使用，否则询问用户）
        let install_resourceex = if let Some(cfg) = config {
            cfg.install_resourceex
        } else if cleanup_before_deploy {
            let resourceex_pattern = self
                .game_root
                .join("ResourceEx")
                .join("ResourceExample-*.zip");
            let resourceex_exists = !glob_matches(&resourceex_pattern).is_empty();
            if resourceex_exists {
                true
            } else {
                self.ui.install_ask_install_resourceex()?
            }
        } else {
            self.ui.install_ask_install_resourceex()?
        };

        // 2.2. 询问是否在游戏启动时弹出 BepInEx 控制台窗口（如果 config 存在则使用，否则询问用户）
        let show_bepinex_console = if let Some(cfg) = config {
            cfg.show_bepinex_console
        } else {
            self.ui.install_ask_show_bepinex_console()?
        };

        // 2.3. 选择 DLL 版本
        let dll_version = if let Some(cfg) = config
            && let Some(ref v) = cfg.dll_version
        {
            if !version_info.dlls.contains(v) {
                self.ui
                    .select_version_not_available("MetaMystia DLL", v, &version_info.dlls)?;
                return Err(ManagerError::Other(format!(
                    "Specified MetaMystia DLL version \"{}\" is not available",
                    v
                )));
            }
            v.clone()
        } else if self.ui.select_version_ask_select("MetaMystia DLL")? {
            let idx = self
                .ui
                .select_version_from_list("MetaMystia DLL", &version_info.dlls)?;
            version_info.dlls[idx].clone()
        } else {
            version_info.latest_dll().to_string()
        };

        // 2.4. 选择 ResourceEx 版本（仅在安装时）
        let resourceex_version = if install_resourceex {
            if let Some(cfg) = config
                && let Some(ref v) = cfg.resourceex_version
            {
                if !version_info.zips.contains(v) {
                    self.ui.select_version_not_available(
                        "ResourceExample ZIP",
                        v,
                        &version_info.zips,
                    )?;
                    return Err(ManagerError::Other(format!(
                        "Specified ResourceExample ZIP version \"{}\" is not available",
                        v
                    )));
                }
                Some(v.clone())
            } else if self.ui.select_version_ask_select("ResourceExample ZIP")? {
                let idx = self
                    .ui
                    .select_version_from_list("ResourceExample ZIP", &version_info.zips)?;
                Some(version_info.zips[idx].clone())
            } else {
                Some(version_info.latest_resourceex().to_string())
            }
        } else {
            None
        };

        report_event(
            "Install.Version.Selected",
            Some(&format!(
                "dll={};resourceex={}",
                dll_version,
                resourceex_version.as_ref().unwrap_or(&"none".to_string())
            )),
        );

        // 显示 GitHub Release Notes（获取所选版本的发行说明）
        match self
            .downloader
            .fetch_and_display_github_release_notes(Some(&dll_version))
        {
            Ok(Some(_)) => {
                if !self.ui.download_ask_continue_after_release_notes()? {
                    return Err(ManagerError::UserCancelled);
                }
            }
            Ok(None) => {}
            Err(_) => {}
        }

        // 3. 创建临时下载目录
        let (temp_dir, _temp_guard) = create_temp_dir_with_guard(&self.game_root).map_err(|e| {
            ManagerError::from(std::io::Error::new(
                e.kind(),
                format!("创建临时目录失败：{}", e),
            ))
        })?;

        // 4. 下载文件
        self.ui.install_display_step(3, "下载必要文件")?;

        // 下载 BepInEx
        let bepinex_path = temp_dir.join(version_info.bepinex_filename()?);
        let bepinex_from_primary = self
            .downloader
            .download_bepinex(&version_info, &bepinex_path)?;

        // 下载 MetaMystia DLL
        let dll_path = temp_dir.join(VersionInfo::metamystia_filename(&dll_version));
        let try_github = dll_version == version_info.latest_dll();
        self.downloader
            .download_metamystia(&share_code, &dll_version, &dll_path, try_github)?;

        // 下载 ResourceExample ZIP
        let resourceex_path = if let Some(ref version) = resourceex_version {
            let path = temp_dir.join(VersionInfo::resourceex_filename(version));
            self.downloader
                .download_resourceex(&share_code, version, &path)?;
            Some(path)
        } else {
            None
        };

        self.ui.install_downloads_completed()?;

        // 5. 在安装前清理旧版本
        if cleanup_before_deploy {
            self.ui.install_start_cleanup()?;
            let (success, failed) = Self::execute_install_cleanup(&self.game_root, self.ui)?;
            self.ui.install_cleanup_result(success, failed)?;
            report_event(
                "Install.Cleanup",
                Some(&format!("success:{};failed:{}", success, failed)),
            );
        }

        // 6. 安装文件
        self.ui.install_display_step(4, "安装文件")?;

        // 检查 BepInEx 是否存在（用于决定是否跳过 plugins）
        let bepinex_dir = self.game_root.join("BepInEx");
        let bepinex_exists = bepinex_dir.exists();

        // 安装 BepInEx（如果之前存在则保留 plugins 目录）
        Extractor::deploy_bepinex(&bepinex_path, &self.game_root, bepinex_exists)?;

        // 写入默认配置（如果不存在）
        let bepinex_config_dir = self.game_root.join("BepInEx").join("config");
        if !bepinex_config_dir.exists() {
            std::fs::create_dir_all(&bepinex_config_dir).map_err(|e| {
                ManagerError::from(std::io::Error::new(
                    e.kind(),
                    format!(
                        "创建 BepInEx 配置目录 {} 失败：{}",
                        bepinex_config_dir.display(),
                        e
                    ),
                ))
            })?;
        }

        let bepinex_cfg_path = bepinex_config_dir.join("BepInEx.cfg");
        let bepinex_cfg_logging = r"[Logging.Console]

## Enables showing a console for log output.
# Setting type: Boolean
# Default value: true
Enabled = false
";
        let bepinex_cfg_il2cpp = r"[IL2CPP]

## URL to a ZIP file with managed Unity base libraries. They are used by Il2CppInterop to generate interop assemblies.
## The URL can include {VERSION} template which will be replaced with the game's Unity engine version.
## If a .zip file with the same filename as the URL (after template replacement) already exists in unity-libs, it will be used instead of downloading a new copy.
## If you want to ensure BepInEx doesn't try to connect to the internet, set this to only the .zip filename (without a URL) and manually place the file in the unity-libs directory.
##
# Setting type: String
# Default value: https://unity.bepinex.dev/libraries/{VERSION}.zip
UnityBaseLibrariesSource = https://url.izakaya.cc/unity-library
";

        let mut bepinex_cfg = String::new();
        if !show_bepinex_console {
            bepinex_cfg.push_str(bepinex_cfg_logging);
        }
        if !bepinex_from_primary {
            if !bepinex_cfg.is_empty() {
                bepinex_cfg.push('\n');
            }
            bepinex_cfg.push_str(bepinex_cfg_il2cpp);
        }
        if !bepinex_cfg.is_empty() {
            let bepinex_tmp_cfg = bepinex_cfg_path.with_extension("cfg.tmp");

            std::fs::write(&bepinex_tmp_cfg, bepinex_cfg.as_bytes()).map_err(|e| {
                ManagerError::from(std::io::Error::new(
                    e.kind(),
                    format!(
                        "写入 BepInEx 临时配置文件 {} 失败：{}",
                        bepinex_tmp_cfg.display(),
                        e
                    ),
                ))
            })?;

            match atomic_rename_or_copy(&bepinex_tmp_cfg, &bepinex_cfg_path) {
                Ok(_) => {
                    let _ = std::fs::remove_file(&bepinex_tmp_cfg);
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&bepinex_tmp_cfg);
                    return Err(ManagerError::from(std::io::Error::other(format!(
                        "写入 BepInEx 配置文件 {} 失败：{}",
                        bepinex_cfg_path.display(),
                        e
                    ))));
                }
            }
        }

        // 安装 MetaMystia DLL
        Extractor::deploy_metamystia(&dll_path, &self.game_root)?;

        // 安装 ResourceExample ZIP
        if let Some(ref path) = resourceex_path {
            Extractor::deploy_resourceex(path, &self.game_root)?;
        }

        self.ui.install_finished(show_bepinex_console)?;
        report_event("Install.Finished", None);

        Ok(())
    }
}
