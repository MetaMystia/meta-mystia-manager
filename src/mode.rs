//! 操作类型与卸载范围。

use crate::config::{
    DISABLED_METAMYSTIA_PLUGIN_GLOB, DISABLED_RESOURCEEX_ZIP_GLOB, METAMYSTIA_PLUGIN_GLOB,
    METAMYSTIA_PLUGIN_OLD_GLOB, METAMYSTIA_PLUGIN_PART_GLOB, RESOURCEEX_ZIP_GLOB,
    RESOURCEEX_ZIP_OLD_GLOB, RESOURCEEX_ZIP_PART_GLOB,
};

/// 用户可选择的操作类型。
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum OperationMode {
    /// 导出诊断包
    Diagnostics,
    /// 全新安装
    Install,
    /// 启用 / 禁用已安装的组件
    Manage,
    /// 卸载已有安装
    Uninstall,
    /// 升级到新版本
    Upgrade,
}

impl OperationMode {
    /// 操作类型的小写名称（用于埋点与日志）。
    pub const fn name(self) -> &'static str {
        match self {
            Self::Diagnostics => "diagnostics",
            Self::Install => "install",
            Self::Manage => "manage",
            Self::Uninstall => "uninstall",
            Self::Upgrade => "upgrade",
        }
    }
}

/// 卸载范围。
#[derive(Clone, Copy, Debug)]
pub enum UninstallMode {
    /// 移除所有与 Mod 有关的文件，还原为原版游戏
    Full,
    /// 仅移除 MetaMystia 相关文件，保留 BepInEx 框架与其他 Mod
    Light,
}

impl UninstallMode {
    const DISABLED_LIGHT_TARGETS: &'static [&'static str] = &[
        DISABLED_METAMYSTIA_PLUGIN_GLOB,
        DISABLED_RESOURCEEX_ZIP_GLOB,
    ];

    const FULL_TARGETS: &'static [(&'static str, bool)] = &[
        ("BepInEx", true),
        (".doorstop_version", false),
        ("changelog.txt", false),
        ("doorstop_config.ini", false),
        ("dotnet", true),
        ("MinHook.x64.dll", false),
        ("winhttp.dll", false),
        ("ResourceEx", true),
    ];

    const LIGHT_TARGETS: &'static [(&'static str, bool)] = &[
        (METAMYSTIA_PLUGIN_GLOB, false),
        (METAMYSTIA_PLUGIN_OLD_GLOB, false),
        (METAMYSTIA_PLUGIN_PART_GLOB, false),
        (RESOURCEEX_ZIP_GLOB, false),
        (RESOURCEEX_ZIP_OLD_GLOB, false),
        (RESOURCEEX_ZIP_PART_GLOB, false),
    ];

    /// 供界面展示的模式说明。
    pub const fn description(&self) -> &str {
        match self {
            Self::Full => "移除所有和 Mod 有关的文件（还原为原版游戏）",
            Self::Light => "仅移除 MetaMystia 相关文件（保留 BepInEx 框架和其他 Mod 相关文件）",
        }
    }

    /// 禁用区里需要逐个清理的 glob 模式；完全卸载走整目录删除，不经过这里。
    pub const fn disabled_targets(self) -> &'static [&'static str] {
        match self {
            Self::Full => &[],
            Self::Light => Self::DISABLED_LIGHT_TARGETS,
        }
    }

    /// 卸载目标列表（每项为 glob 模式与是否为目录）。
    pub const fn targets(self) -> &'static [(&'static str, bool)] {
        match self {
            Self::Full => Self::FULL_TARGETS,
            Self::Light => Self::LIGHT_TARGETS,
        }
    }
}
