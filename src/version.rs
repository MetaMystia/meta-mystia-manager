//! 远端版本信息与文件名解析。

use crate::config::BEPINEX_CORE_DLL;
use crate::error::{ManagerError, Result};
use crate::platform::file_product_version;
use crate::telemetry::report_event;

use serde::Deserialize;

use std::{
    collections::HashSet,
    fmt::{self, Display, Formatter},
    path::Path,
};

/// 读取已安装的 BepInEx 构建号（`BepInEx.Core.dll` 产品版本里的 `be.<n>`）。
pub fn read_bepinex_version(game_root: &Path) -> Option<String> {
    file_product_version(&game_root.join(BEPINEX_CORE_DLL))
        .and_then(|product_version| parse_bepinex_build(&product_version))
}

/// 从产品版本字符串里提取 `be.<构建号>`。
fn parse_bepinex_build(product_version: &str) -> Option<String> {
    let (_, build) = product_version.split_once("be.")?;
    let digits: String = build.chars().take_while(char::is_ascii_digit).collect();

    (!digits.is_empty()).then_some(digits)
}

/// 主站版本接口返回的版本与下载路径信息。
#[derive(Clone, Deserialize)]
pub struct VersionInfo {
    /// BepInEx 版本号
    #[serde(rename = "bepInEx", default)]
    pub bep_in_ex: Option<String>,
    /// 上游 BepInEx 文件名（如 `BepInEx-Unity.IL2CPP-win-x64-6.0.0-be.785+6abdba4.zip`）
    #[serde(rename = "bepInExFileName", default)]
    pub bep_in_ex_file_name: Option<String>,
    /// 运行期配置接口地址
    #[serde(rename = "configUrl")]
    pub config_url: String,
    /// MetaMystia DLL 的候选文件名列表
    #[serde(default)]
    pub dlls: Vec<String>,
    /// 最新管理工具版本
    #[serde(default)]
    pub manager: Option<String>,
    /// 各组件的下载路径
    #[serde(default)]
    pub paths: DownloadPaths,
    /// ResourceExample ZIP 的候选文件名列表
    #[serde(default)]
    pub zips: Vec<String>,
}

/// 各组件在下载服务上的路径。
#[derive(Clone, Default, Deserialize)]
pub struct DownloadPaths {
    /// BepInEx 包的路径
    #[serde(rename = "bepInEx")]
    pub bep_in_ex: Option<String>,
    /// MetaMystia DLL 的路径
    pub dll: Option<String>,
    /// ResourceExample ZIP 的路径
    pub zip: Option<String>,
}

impl VersionInfo {
    const META_MYSTIA_CANONICAL_PREFIX: &'static str = "metamystia";
    const RESOURCEEX_CANONICAL_PREFIX: &'static str = "resourceexample";

    /// 去掉版本号前的 `v`/`V` 与首尾空白。
    pub fn normalize_version(version: &str) -> String {
        let trimmed = version.trim();
        trimmed
            .strip_prefix('v')
            .or_else(|| trimmed.strip_prefix('V'))
            .unwrap_or(trimmed)
            .trim()
            .to_string()
    }

    /// 严格解析数字版本（`major[.minor[.patch]]`）；含非数字段时返回 `None`。
    pub(crate) fn strict_numeric_version_parts(version: &str) -> Option<Vec<u64>> {
        let mut parts = Vec::new();

        for segment in version.split('.') {
            if segment.is_empty() {
                return None;
            }

            match segment.parse::<u64>() {
                Ok(value) => parts.push(value),
                Err(_) => return None,
            }
        }

        if parts.is_empty() { None } else { Some(parts) }
    }

    /// 规范化 `dlls` 与 `zips` 里的全部版本号。
    pub fn normalize_versions(&mut self) {
        Self::normalize_version_list(&mut self.dlls);
        Self::normalize_version_list(&mut self.zips);
    }

    /// 校验版本信息完整性：列表非空且文件名安全。
    pub fn validate(&self) -> Result<()> {
        if self.dlls.is_empty() {
            report_event("Model.VersionInfo.Invalid", Some("dlls_empty"));
            return Err(ManagerError::InvalidVersionInfo);
        }
        if self
            .dlls
            .iter()
            .any(|version| Self::normalize_canonical_version(version).is_none())
        {
            report_event("Model.VersionInfo.Invalid", Some("dlls_invalid"));
            return Err(ManagerError::InvalidVersionInfo);
        }
        if self
            .zips
            .iter()
            .any(|version| Self::normalize_canonical_version(version).is_none())
        {
            report_event("Model.VersionInfo.Invalid", Some("zips_invalid"));
            return Err(ManagerError::InvalidVersionInfo);
        }
        Ok(())
    }

    /// 最新的 MetaMystia DLL 版本；列表为空时按无效版本信息处理。
    pub fn latest_dll(&self) -> Result<&str> {
        self.dlls
            .first()
            .map(String::as_str)
            .ok_or_else(|| Self::empty_list_error("dlls_empty"))
    }

    /// 最新的 ResourceExample ZIP 版本；列表为空时按无效版本信息处理。
    pub fn latest_resourceex(&self) -> Result<&str> {
        self.zips
            .first()
            .map(String::as_str)
            .ok_or_else(|| Self::empty_list_error("zips_empty"))
    }

    /// 从 `MetaMystia-v<版本>.dll` 文件名提取版本号。
    pub fn metamystia_version_from_filename(filename: &str) -> Option<String> {
        Self::version_fragment(
            filename,
            Self::META_MYSTIA_CANONICAL_PREFIX,
            ".dll",
            Self::normalize_canonical_version,
        )
    }

    /// 从 `ResourceExample-v<版本>.zip` 文件名提取版本号。
    pub fn resourceex_version_from_filename(filename: &str) -> Option<String> {
        Self::version_fragment(
            filename,
            Self::RESOURCEEX_CANONICAL_PREFIX,
            ".zip",
            Self::normalize_canonical_version,
        )
    }

    /// 是否为规范的 MetaMystia DLL 文件名。
    pub fn is_metamystia_filename(filename: &str) -> bool {
        Self::metamystia_version_from_filename(filename).is_some()
    }

    /// 是否为规范的 ResourceExample ZIP 文件名。
    pub fn is_resourceex_filename(filename: &str) -> bool {
        Self::resourceex_version_from_filename(filename).is_some()
    }

    /// 是否为规范的 MetaMystia 备份文件名（`*.old*`）。
    pub fn is_canonical_metamystia_backup_filename(filename: &str) -> bool {
        Self::matches_backup_filename(filename, ".dll", Self::is_metamystia_filename)
    }

    /// 是否为规范的 ResourceExample 备份文件名（`*.old*`）。
    pub fn is_canonical_resourceex_backup_filename(filename: &str) -> bool {
        Self::matches_backup_filename(filename, ".zip", Self::is_resourceex_filename)
    }

    /// 是否为 MetaMystia 未完成下载文件名（`*.part`）。
    pub fn is_metamystia_part_filename(filename: &str) -> bool {
        Self::matches_part_filename(filename, ".dll", Self::is_metamystia_filename)
    }

    /// 是否为 ResourceExample 未完成下载文件名（`*.part`）。
    pub fn is_resourceex_part_filename(filename: &str) -> bool {
        Self::matches_part_filename(filename, ".zip", Self::is_resourceex_filename)
    }

    /// 规范化后比较两个版本号是否相等。
    pub fn versions_match(left: &str, right: &str) -> bool {
        Self::normalize_version(left) == Self::normalize_version(right)
    }

    /// 在候选列表里解析界面选择的版本；`None` 表示使用最新版，选择已失效时返回错误。
    pub fn resolve_selected_version(
        available: &[String],
        selected: Option<&str>,
        component: &str,
    ) -> Result<Option<String>> {
        let Some(selected) = selected else {
            return Ok(None);
        };

        available
            .iter()
            .find(|version| Self::versions_match(version, selected))
            .cloned()
            .map(Some)
            .ok_or_else(|| {
                report_event("Model.VersionInfo.Unavailable", Some(component));
                ManagerError::Other(format!(
                    "所选 {component} 版本已不在可用列表中，请返回上一步重新选择"
                ))
            })
    }

    /// 远端 BepInEx 包文件名；缺失时返回错误。
    pub fn bepinex_filename(&self) -> Result<&str> {
        let name = self
            .bep_in_ex_file_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                report_event("Model.VersionInfo.Invalid", Some("bepinex_filename"));
                ManagerError::InvalidVersionInfo
            })?;

        if !Self::is_safe_file_name(name) {
            report_event("Model.VersionInfo.Invalid", Some("bepinex_filename_unsafe"));
            return Err(ManagerError::InvalidVersionInfo);
        }

        Ok(name)
    }

    /// BepInEx 构建号（服务端只下发构建号，例如 `785`）。
    pub fn bepinex_version(&self) -> Result<&str> {
        let version = self
            .bep_in_ex
            .as_deref()
            .map(str::trim)
            .filter(|version| !version.is_empty())
            .ok_or_else(|| {
                report_event("Model.VersionInfo.Invalid", Some("bepinex_version"));
                ManagerError::InvalidVersionInfo
            })?;

        if !version.chars().all(|ch| ch.is_ascii_digit()) {
            report_event("Model.VersionInfo.Invalid", Some("bepinex_version"));
            return Err(ManagerError::InvalidVersionInfo);
        }

        Ok(version)
    }

    /// 最新管理工具版本；服务端未下发时返回 `None`。
    pub fn manager_version(&self) -> Option<&str> {
        self.manager
            .as_deref()
            .map(str::trim)
            .filter(|version| !version.is_empty())
            .map(|version| version.strip_prefix(['v', 'V']).unwrap_or(version))
    }

    /// 生成指定版本的 MetaMystia DLL 文件名。
    pub fn metamystia_filename(version: &str) -> String {
        let version = Self::normalize_canonical_version(version)
            .unwrap_or_else(|| Self::normalize_version(version));
        format!("MetaMystia-v{version}.dll")
    }

    /// 生成指定版本的 ResourceExample ZIP 文件名。
    pub fn resourceex_filename(version: &str) -> String {
        let version = Self::normalize_canonical_version(version)
            .unwrap_or_else(|| Self::normalize_version(version));
        format!("ResourceExample-v{version}.zip")
    }

    /// 管理工具更新包的文件名；版本缺失或不安全时返回错误。
    pub fn manager_filename(&self) -> Result<String> {
        let version = self.manager_version().ok_or_else(|| {
            report_event("Model.VersionInfo.Invalid", Some("manager_version"));
            ManagerError::InvalidVersionInfo
        })?;

        if !Self::is_safe_file_name(version) {
            report_event("Model.VersionInfo.Invalid", Some("manager_version_unsafe"));
            return Err(ManagerError::InvalidVersionInfo);
        }

        Ok(format!("meta-mystia-manager-v{version}.exe"))
    }
    fn is_safe_file_name(name: &str) -> bool {
        let name = name.trim();

        !name.is_empty()
            && !name.ends_with(['.', ' '])
            && !name.contains(['/', '\\'])
            && !name.chars().any(|ch| {
                ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '#' | '%')
            })
    }

    fn normalize_version_list(versions: &mut Vec<String>) {
        let mut normalized = Vec::with_capacity(versions.len());
        let mut seen = HashSet::with_capacity(versions.len());

        for version in versions.drain(..) {
            let version = Self::normalize_canonical_version(&version)
                .unwrap_or_else(|| Self::normalize_version(&version));
            if !version.is_empty() && seen.insert(version.clone()) {
                normalized.push(version);
            }
        }

        *versions = normalized;
    }
    fn normalize_canonical_version(version: &str) -> Option<String> {
        let version = Self::normalize_version(version);
        let parts = Self::strict_numeric_version_parts(&version)?;

        match parts.as_slice() {
            [major, minor, patch] => Some(format!("{major}.{minor}.{patch}")),
            _ => None,
        }
    }

    fn version_fragment(
        filename: &str,
        prefix: &str,
        suffix: &str,
        normalizer: fn(&str) -> Option<String>,
    ) -> Option<String> {
        let lower = filename.trim().to_ascii_lowercase();
        let stem = lower.strip_suffix(suffix)?;

        let version_part = stem
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix("-v"))?;

        normalizer(version_part)
    }
    fn empty_list_error(reason: &str) -> ManagerError {
        report_event("Model.VersionInfo.Invalid", Some(reason));

        ManagerError::InvalidVersionInfo
    }
    fn matches_backup_filename(filename: &str, suffix: &str, matcher: fn(&str) -> bool) -> bool {
        let lower = filename.trim().to_ascii_lowercase();
        let marker = format!("{suffix}.old");
        let Some((base, tail)) = lower.split_once(&marker) else {
            return false;
        };

        if !tail.is_empty() {
            let Some(index) = tail.strip_prefix('.') else {
                return false;
            };

            if index.is_empty() || !index.chars().all(|ch| ch.is_ascii_digit()) {
                return false;
            }
        }

        let original = format!("{base}{suffix}");
        matcher(&original)
    }
    fn matches_part_filename(filename: &str, suffix: &str, matcher: fn(&str) -> bool) -> bool {
        let lower = filename.trim().to_ascii_lowercase();
        let Some(base) = lower.strip_suffix(&format!("{suffix}.part")) else {
            return false;
        };

        matcher(&format!("{base}{suffix}"))
    }
}

impl Display for VersionInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BepInEx: {}, dll: {}, zip: {}",
            self.bep_in_ex.as_deref().map_or("", str::trim),
            self.dlls.first().map_or("", |s| s.trim()),
            self.zips.first().map_or("", |s| s.trim())
        )
    }
}
