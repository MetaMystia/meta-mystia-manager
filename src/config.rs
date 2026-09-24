use std::time::Duration;

pub const GAME_EXECUTABLE: &str = "Touhou Mystia Izakaya.exe";
pub const GAME_PROCESS_NAME: &str = "Touhou Mystia Izakaya.exe";
pub const GAME_STEAM_APP_ID: u32 = 1_584_090;
pub const TEMP_DIR_NAME: &str = concat!(".", env!("CARGO_PKG_NAME"), "-temp");
pub const BEPINEX_VERSION_FILE: &str = "BepInEx/.mmm-bepinex-version";
pub const METAMYSTIA_PLUGIN_GLOB: &str = "BepInEx/plugins/MetaMystia-v*.dll";
pub const RESOURCEEX_ZIP_GLOB: &str = "ResourceEx/ResourceExample-v*.zip";
pub const METAMYSTIA_PLUGIN_OLD_GLOB: &str = "BepInEx/plugins/MetaMystia-v*.dll.old*";
pub const RESOURCEEX_ZIP_OLD_GLOB: &str = "ResourceEx/ResourceExample-v*.zip.old*";
pub const USER_AGENT: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/AnYiEE/",
    env!("CARGO_PKG_NAME"),
    ")"
);

/// 操作模式枚举
pub enum OperationMode {
    Install,
    Upgrade,
    Uninstall,
    Diagnostics,
}

/// 获取不到版本信息时的离线模式：只提供不依赖服务端的功能
pub enum OfflineMode {
    Uninstall,
    Diagnostics,
}

/// 卸载模式枚举
#[derive(Clone, Copy, Debug)]
pub enum UninstallMode {
    Light,
    Full,
}

impl UninstallMode {
    const LIGHT_TARGETS: &'static [(&'static str, bool)] = &[
        (METAMYSTIA_PLUGIN_GLOB, false),
        (RESOURCEEX_ZIP_GLOB, false),
    ];

    const FULL_TARGETS: &'static [(&'static str, bool)] = &[
        ("BepInEx", true),
        (".doorstop_version", false),
        ("changelog.txt", false),
        ("doorstop_config.ini", false),
        ("MinHook.x64.dll", false),
        ("winhttp.dll", false),
        ("ResourceEx", true),
    ];

    /// 获取卸载模式描述
    pub const fn description(&self) -> &str {
        match self {
            Self::Light => "仅移除 MetaMystia 相关文件（保留 BepInEx 框架和其他 Mod 相关文件）",
            Self::Full => "移除所有和 Mod 有关的文件（还原为原版游戏）",
        }
    }

    /// 获取卸载目标列表（模式字符串，是否为目录）
    pub const fn targets(self) -> &'static [(&'static str, bool)] {
        match self {
            Self::Light => Self::LIGHT_TARGETS,
            Self::Full => Self::FULL_TARGETS,
        }
    }
}

/// 通用重试配置
pub struct RetryConfig {
    /// 最大重试次数（至少 1）
    pub attempts: usize,
    /// 基础延迟（秒）
    pub base_delay_secs: u64,
    /// 指数倍数（例如 2.0 表示每次延迟翻倍）
    pub multiplier: f64,
    /// 最大延迟（秒）上限
    pub max_delay_secs: u64,
}

impl RetryConfig {
    /// 网络操作的默认重试配置
    pub const fn network() -> Self {
        Self {
            attempts: 3,
            base_delay_secs: 5,
            multiplier: 2.0,
            max_delay_secs: 15,
        }
    }

    /// GitHub Release Notes 的重试配置
    pub const fn github_release_note() -> Self {
        Self {
            attempts: 2,
            base_delay_secs: 5,
            multiplier: 1.0,
            max_delay_secs: 5,
        }
    }

    /// 卸载操作的默认重试配置
    pub const fn uninstall() -> Self {
        Self {
            attempts: 3,
            base_delay_secs: 10,
            multiplier: 2.0,
            max_delay_secs: 60,
        }
    }

    /// 第 `attempt` 次重试（从 0 开始）前的等待时长
    ///
    /// 即 `base_delay_secs * multiplier^attempt`，并以 `max_delay_secs` 为上限。
    #[allow(
        clippy::cast_precision_loss,
        reason = "退避时长最多几十秒，f64 足以精确表示"
    )]
    pub fn delay(&self, attempt: usize) -> Duration {
        let exponent = i32::try_from(attempt).unwrap_or(i32::MAX);
        let secs = (self.base_delay_secs as f64 * self.multiplier.powi(exponent))
            .min(self.max_delay_secs as f64)
            .ceil();

        Duration::from_secs_f64(secs)
    }
}
