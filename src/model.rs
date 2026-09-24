use crate::error::{ManagerError, Result};
use crate::metrics::report_event;

use serde::Deserialize;

use std::{
    collections::HashSet,
    fmt::{Display, Formatter},
};

#[derive(Clone, Deserialize)]
pub struct VersionInfo {
    #[serde(rename = "bepInEx")]
    pub bep_in_ex: String,
    /// 上游 BepInEx 文件名（如 `BepInEx-Unity.IL2CPP-win-x64-6.0.0-be.785+6abdba4.zip`）
    #[serde(rename = "bepInExFileName", default)]
    pub bep_in_ex_file_name: Option<String>,
    pub manager: String,
    /// 运行期配置地址
    #[serde(rename = "configUrl")]
    pub config_url: String,
    pub dlls: Vec<String>,
    pub paths: DownloadPaths,
    pub zips: Vec<String>,
}

#[derive(Clone, Deserialize)]
pub struct DownloadPaths {
    #[serde(rename = "bepInEx")]
    pub bep_in_ex: Option<String>,
    pub dll: Option<String>,
    pub zip: Option<String>,
}

impl VersionInfo {
    const META_MYSTIA_CANONICAL_PREFIX: &'static str = "metamystia";
    const RESOURCEEX_CANONICAL_PREFIX: &'static str = "resourceexample";

    pub fn normalize_version(version: &str) -> String {
        let trimmed = version.trim();
        trimmed
            .strip_prefix('v')
            .or_else(|| trimmed.strip_prefix('V'))
            .unwrap_or(trimmed)
            .trim()
            .to_string()
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

    pub fn normalize_versions(&mut self) {
        Self::normalize_version_list(&mut self.dlls);
        Self::normalize_version_list(&mut self.zips);
    }

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
        if self.zips.is_empty() {
            report_event("Model.VersionInfo.Invalid", Some("zips_empty"));
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

    /// 最新的 MetaMystia DLL 版本；列表为空时按无效版本信息处理
    pub fn latest_dll(&self) -> Result<&str> {
        self.dlls
            .first()
            .map(String::as_str)
            .ok_or_else(|| Self::empty_list_error("dlls_empty"))
    }

    /// 最新的 ResourceExample ZIP 版本；列表为空时按无效版本信息处理
    pub fn latest_resourceex(&self) -> Result<&str> {
        self.zips
            .first()
            .map(String::as_str)
            .ok_or_else(|| Self::empty_list_error("zips_empty"))
    }

    fn empty_list_error(reason: &str) -> ManagerError {
        report_event("Model.VersionInfo.Invalid", Some(reason));

        ManagerError::InvalidVersionInfo
    }

    pub fn metamystia_version_from_filename(filename: &str) -> Option<String> {
        Self::version_fragment(
            filename,
            Self::META_MYSTIA_CANONICAL_PREFIX,
            ".dll",
            Self::normalize_canonical_version,
        )
    }

    pub fn resourceex_version_from_filename(filename: &str) -> Option<String> {
        Self::version_fragment(
            filename,
            Self::RESOURCEEX_CANONICAL_PREFIX,
            ".zip",
            Self::normalize_canonical_version,
        )
    }

    pub fn is_metamystia_filename(filename: &str) -> bool {
        Self::metamystia_version_from_filename(filename).is_some()
    }

    pub fn is_resourceex_filename(filename: &str) -> bool {
        Self::resourceex_version_from_filename(filename).is_some()
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

    pub fn is_canonical_metamystia_backup_filename(filename: &str) -> bool {
        Self::matches_backup_filename(filename, ".dll", Self::is_metamystia_filename)
    }

    pub fn is_canonical_resourceex_backup_filename(filename: &str) -> bool {
        Self::matches_backup_filename(filename, ".zip", Self::is_resourceex_filename)
    }

    pub fn versions_match(left: &str, right: &str) -> bool {
        Self::normalize_version(left) == Self::normalize_version(right)
    }

    /// 上游 BepInEx 文件名（服务端通过 `bepInExFileName` 下发）
    pub fn bepinex_filename(&self) -> Result<&str> {
        self.bep_in_ex_file_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                report_event("Model.VersionInfo.Invalid", Some("bepinex_filename"));
                ManagerError::InvalidVersionInfo
            })
    }

    /// BepInEx 构建号（服务端只下发构建号，例如 `785`）
    pub fn bepinex_version(&self) -> Result<&str> {
        let version = self.bep_in_ex.trim();

        if version.is_empty() {
            report_event("Model.VersionInfo.Invalid", Some("bepinex_version"));
            return Err(ManagerError::InvalidVersionInfo);
        }

        Ok(version)
    }

    pub fn metamystia_filename(version: &str) -> String {
        let version = Self::normalize_canonical_version(version)
            .unwrap_or_else(|| Self::normalize_version(version));
        format!("MetaMystia-v{version}.dll")
    }

    pub fn resourceex_filename(version: &str) -> String {
        let version = Self::normalize_canonical_version(version)
            .unwrap_or_else(|| Self::normalize_version(version));
        format!("ResourceExample-v{version}.zip")
    }

    pub fn manager_filename(&self) -> String {
        format!("meta-mystia-manager-v{}.exe", self.manager.trim())
    }
}

impl Display for VersionInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "BepInEx: {}, dll: {}, zip: {}",
            self.bep_in_ex.trim(),
            self.dlls.first().map_or("", |s| s.trim()),
            self.zips.first().map_or("", |s| s.trim())
        )
    }
}
