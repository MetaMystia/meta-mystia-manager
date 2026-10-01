//! 跨模块共享的全局约定：程序标识与目标游戏的目录、文件命名。
//!
//! 各模块自己的策略与阈值（端点、并发、超时、重试、上限等）定义在该模块内。

// 应用标识
/// 所有 HTTP 请求使用的 User-Agent。
pub const USER_AGENT: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/MetaMystia/meta-mystia-manager)"
);

// 游戏路径约定
/// BepInEx 核心 DLL 相对游戏根目录的路径。
pub const BEPINEX_CORE_DLL: &str = "BepInEx/core/BepInEx.Core.dll";

/// 游戏主程序文件名。
pub const GAME_EXECUTABLE: &str = "Touhou Mystia Izakaya.exe";

/// 游戏进程名。
pub const GAME_PROCESS_NAME: &str = GAME_EXECUTABLE;

/// 游戏的 Steam App ID。
pub const GAME_STEAM_APP_ID: u32 = 1_584_090;

/// MetaMystia 插件 DLL 的匹配模式。
pub const METAMYSTIA_PLUGIN_GLOB: &str = "BepInEx/plugins/MetaMystia-v*.dll";

/// MetaMystia 插件改名备份（`.old*`）的匹配模式。
pub const METAMYSTIA_PLUGIN_OLD_GLOB: &str = "BepInEx/plugins/MetaMystia-v*.dll.old*";

/// MetaMystia 插件未完成下载（`.part`）的匹配模式。
pub const METAMYSTIA_PLUGIN_PART_GLOB: &str = "BepInEx/plugins/MetaMystia-v*.dll.part";

/// ResourceExample ZIP 的匹配模式。
pub const RESOURCEEX_ZIP_GLOB: &str = "ResourceEx/ResourceExample-v*.zip";

/// ResourceExample ZIP 改名备份（`.old*`）的匹配模式。
pub const RESOURCEEX_ZIP_OLD_GLOB: &str = "ResourceEx/ResourceExample-v*.zip.old*";

/// ResourceExample ZIP 未完成下载（`.part`）的匹配模式。
pub const RESOURCEEX_ZIP_PART_GLOB: &str = "ResourceEx/ResourceExample-v*.zip.part";

/// 管理工具在游戏目录下使用的临时目录名。
pub const TEMP_DIR_NAME: &str = concat!(".", env!("CARGO_PKG_NAME"), "-temp");
