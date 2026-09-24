//! 运行期配置：来自主站的统一配置接口 `GET /config/meta-mystia-manager`。
//!
//! 只包含端点地址与限速等公开参数；client secret 不在其中，换票由服务端完成。

use crate::error::{ManagerError, Result};
use crate::net::{build_agent, check_response_status};
use crate::ui::Ui;

use serde::Deserialize;
use std::{
    sync::{Mutex, OnceLock},
    time::Duration,
};

const CONFIG_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Deserialize)]
pub struct RemoteConfig {
    pub download: DownloadConfig,
    pub self_update: SelfUpdateConfig,
    pub sources: SourcesConfig,
    pub sso: SsoConfig,
}

#[derive(Clone, Deserialize)]
pub struct SsoConfig {
    pub authorize_origin: String,
    pub session_url: String,
}

#[derive(Clone, Deserialize)]
pub struct DownloadConfig {
    pub keys_url: String,
    pub max_concurrent_downloads: u32,
    pub rate_limit_kb_per_second: u64,
}

#[derive(Clone, Deserialize)]
pub struct SelfUpdateConfig {
    pub entry_url: String,
    pub api_base: String,
}

#[derive(Clone, Deserialize)]
pub struct SourcesConfig {
    pub bep_in_ex_primary: String,
    pub github_release_api: String,
}

static CACHE: OnceLock<Mutex<Option<(String, RemoteConfig)>>> = OnceLock::new();

/// 获取运行期配置；进程内只请求一次
pub fn get(ui: &dyn Ui, config_url: &str) -> Result<RemoteConfig> {
    let cache = CACHE.get_or_init(|| Mutex::new(None));

    if let Ok(guard) = cache.lock()
        && let Some((cached_url, config)) = guard.as_ref()
        && cached_url == config_url
    {
        return Ok(config.clone());
    }

    let agent = build_agent(Some(CONFIG_TIMEOUT), None);
    let response = agent
        .get(config_url)
        .call()
        .map_err(|e| ManagerError::NetworkError(format!("获取下载配置失败：{e}")))?;

    if let Some(err) = check_response_status(&response, ui, "获取下载配置") {
        return Err(err);
    }

    let text = response
        .into_body()
        .read_to_string()
        .map_err(|e| ManagerError::NetworkError(format!("读取下载配置失败：{e}")))?;
    let config: RemoteConfig = serde_json::from_str(&text)
        .map_err(|e| ManagerError::NetworkError(format!("解析下载配置失败：{e}")))?;

    if let Ok(mut guard) = cache.lock() {
        *guard = Some((config_url.to_string(), config.clone()));
    }

    Ok(config)
}

/// KB/s 换算成字节/s（1 KB = 1024 字节）；0 表示不限速
pub fn rate_limit_bytes_per_second(kb_per_second: u64) -> Option<usize> {
    if kb_per_second == 0 {
        return None;
    }

    Some(usize::try_from(kb_per_second.saturating_mul(1024)).unwrap_or(usize::MAX))
}
