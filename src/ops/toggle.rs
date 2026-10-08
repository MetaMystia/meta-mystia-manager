//! 组件启用 / 禁用：把已安装的组件在原路径与禁用区之间移动。

use crate::config::{
    DISABLED_DIR_NAME, DISABLED_METAMYSTIA_PLUGIN_GLOB, DISABLED_RESOURCEEX_ZIP_GLOB,
    METAMYSTIA_PLUGIN_GLOB, RESOURCEEX_ZIP_GLOB,
};
use crate::error::{ManagerError, Result};
use crate::fs::file_ops::{atomic_rename_or_copy, glob_matches_by_filename};
use crate::ops::installer::{read_ini_value, set_ini_value};
use crate::version::VersionInfo;

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// 组件在游戏目录中的文件布局。
pub struct AssetSpec {
    /// 禁用区内的 glob 模式，相对禁用区
    pub disabled_pattern: &'static str,
    /// 原始（启用状态）glob 模式，相对游戏根目录
    pub enabled_pattern: &'static str,
    /// 文件名匹配
    pub matcher: fn(&str) -> bool,
    /// 组件显示名
    pub name: &'static str,
}

/// 参与启用 / 禁用的组件。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Component {
    /// MetaMystia Mod 本体
    MetaMystia,
    /// ResourceExample 内容包
    ResourceEx,
}

/// 单个组件在两类位置上的现有文件。
pub struct ComponentFiles {
    /// 禁用区里的文件
    pub disabled: Vec<PathBuf>,
    /// 原路径（启用状态）下的文件
    pub enabled: Vec<PathBuf>,
}

impl Component {
    /// 组件的文件布局。
    pub const fn spec(self) -> AssetSpec {
        match self {
            Self::MetaMystia => AssetSpec {
                disabled_pattern: DISABLED_METAMYSTIA_PLUGIN_GLOB,
                enabled_pattern: METAMYSTIA_PLUGIN_GLOB,
                matcher: VersionInfo::is_metamystia_filename,
                name: "MetaMystia",
            },
            Self::ResourceEx => AssetSpec {
                disabled_pattern: DISABLED_RESOURCEEX_ZIP_GLOB,
                enabled_pattern: RESOURCEEX_ZIP_GLOB,
                matcher: VersionInfo::is_resourceex_filename,
                name: "ResourceExample",
            },
        }
    }
}

/// 扫描一个组件在原路径与禁用区里的文件。
pub fn component_files(game_root: &Path, component: Component) -> ComponentFiles {
    let files = component.spec();
    let scan = |pattern: &str| glob_matches_by_filename(&game_root.join(pattern), files.matcher);

    ComponentFiles {
        disabled: scan(&format!(
            "{DISABLED_DIR_NAME}/{pattern}",
            pattern = files.disabled_pattern
        )),
        enabled: scan(files.enabled_pattern),
    }
}

/// 禁用区根目录。
pub fn disabled_root(game_root: &Path) -> PathBuf {
    game_root.join(DISABLED_DIR_NAME)
}

/// 读取 `doorstop_config.ini` 的 `[General] enabled`；文件或键缺失时按启用处理。
pub fn doorstop_enabled(game_root: &Path) -> Result<bool> {
    let path = game_root.join("doorstop_config.ini");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(e) => {
            return Err(ManagerError::from(io::Error::new(
                e.kind(),
                format!("读取 {} 失败：{}", path.display(), e),
            )));
        }
    };

    Ok(read_ini_value(&text, "General", "enabled")
        .is_none_or(|value| !value.eq_ignore_ascii_case("false")))
}

/// 禁用或启用一个组件；返回是否真的移动了文件。
pub fn set_component_enabled(
    game_root: &Path,
    component: Component,
    enabled: bool,
) -> Result<bool> {
    let files = component_files(game_root, component);
    let root = disabled_root(game_root);
    let spec = component.spec();

    if enabled {
        let Some(source) = files.disabled.first() else {
            return Ok(false);
        };

        let relative = source
            .strip_prefix(&root)
            .map_err(|_| ManagerError::Other(format!("无效的禁用区路径：{}", source.display())))?;
        let destination = game_root.join(relative);

        if destination.exists() {
            return Err(ManagerError::Other(format!(
                "{} 已存在同名文件，请先删除或还原后再试：{}",
                spec.name,
                destination.display()
            )));
        }

        atomic_rename_or_copy(source, &destination)?;
        remove_empty_parents(source.parent(), &root);

        return Ok(true);
    }

    let Some(source) = files.enabled.first() else {
        return Ok(false);
    };

    let relative = source
        .strip_prefix(game_root)
        .map_err(|_| ManagerError::Other(format!("无效的组件路径：{}", source.display())))?;
    let destination = root.join(relative);

    if destination.exists() {
        return Err(ManagerError::Other(format!(
            "{} 的禁用区已存在同名文件（可能是旧版本备份），请先还原或删除后再试：{}",
            spec.name,
            destination.display()
        )));
    }

    atomic_rename_or_copy(source, &destination)?;

    Ok(true)
}

/// 写入 `doorstop_config.ini` 的 `[General] enabled`；返回是否真的改写了文件。
pub fn set_doorstop_enabled(game_root: &Path, enabled: bool) -> Result<bool> {
    let path = game_root.join("doorstop_config.ini");
    let Ok(text) = fs::read_to_string(&path) else {
        return Err(ManagerError::Other(format!(
            "未找到 {}，请先安装 BepInEx",
            path.display()
        )));
    };
    let wanted = if enabled { "true" } else { "false" };

    if read_ini_value(&text, "General", "enabled")
        .is_some_and(|value| value.eq_ignore_ascii_case(wanted))
    {
        return Ok(false);
    }

    let updated = set_ini_value(&text, "General", "enabled", wanted);
    fs::write(&path, updated.as_bytes()).map_err(|e| {
        ManagerError::from(io::Error::new(
            e.kind(),
            format!("写入 {} 失败：{}", path.display(), e),
        ))
    })?;

    Ok(true)
}

/// 自底向上删除禁用区里已经空掉的目录；根目录也空了就一并删除。
fn remove_empty_parents(mut dir: Option<&Path>, root: &Path) {
    while let Some(current) = dir {
        if !current.starts_with(root) {
            break;
        }
        if fs::remove_dir(current).is_err() {
            break;
        }
        dir = current.parent();
    }
}
