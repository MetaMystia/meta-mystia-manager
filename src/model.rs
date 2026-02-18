use crate::error::{ManagerError, Result};
use crate::metrics::report_event;

use serde::Deserialize;

#[derive(Clone, Deserialize)]
pub struct VersionInfo {
    #[serde(rename = "bepInEx")]
    pub bep_in_ex: String,
    pub manager: String,
    pub dlls: Vec<String>,
    pub zips: Vec<String>,
}

impl VersionInfo {
    /// 验证版本信息
    pub fn validate(&self) -> Result<()> {
        if self.dlls.is_empty() {
            report_event("Model.VersionInfo.Invalid", Some("dlls_empty"));
            return Err(ManagerError::InvalidVersionInfo);
        }
        if self.zips.is_empty() {
            report_event("Model.VersionInfo.Invalid", Some("zips_empty"));
            return Err(ManagerError::InvalidVersionInfo);
        }
        Ok(())
    }

    /// 获取最新的 DLL 版本
    pub fn latest_dll(&self) -> &str {
        &self.dlls[0]
    }

    /// 获取最新的 ResourceEx 版本
    pub fn latest_resourceex(&self) -> &str {
        &self.zips[0]
    }

    /// 解析 BepInEx 的文件名
    pub fn bepinex_filename(&self) -> Result<&str> {
        self.bep_in_ex
            .split('#')
            .nth(1)
            .map(|s| s.trim())
            .ok_or_else(|| {
                report_event("Model.VersionInfo.Invalid", Some("bepinex_filename"));
                ManagerError::InvalidVersionInfo
            })
    }

    /// 解析 BepInEx 的版本号
    pub fn bepinex_version(&self) -> Result<&str> {
        self.bep_in_ex
            .split('#')
            .next()
            .map(|s| s.trim())
            .ok_or_else(|| {
                report_event("Model.VersionInfo.Invalid", Some("bepinex_version"));
                ManagerError::InvalidVersionInfo
            })
    }

    /// MetaMystia DLL 文件名
    pub fn metamystia_filename(version: &str) -> String {
        format!("MetaMystia-v{}.dll", version.trim())
    }

    /// ResourceExample ZIP 文件名
    pub fn resourceex_filename(version: &str) -> String {
        format!("ResourceExample-v{}.zip", version.trim())
    }

    /// MetaMystia Manager 可执行文件名
    pub fn manager_filename(&self) -> String {
        format!("meta-mystia-manager-v{}.exe", self.manager.trim())
    }
}

impl std::fmt::Display for VersionInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "BepInEx: {}, dll: {}, zip: {}",
            self.bep_in_ex.trim(),
            self.dlls.first().map(|s| s.trim()).unwrap_or(""),
            self.zips.first().map(|s| s.trim()).unwrap_or("")
        )
    }
}
