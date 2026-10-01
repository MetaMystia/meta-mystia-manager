//! 运行期配置：来自主站的统一配置接口 `GET /config/meta-mystia-manager`。
//!
//! 只包含端点地址与限速等公开参数；`client secret` 不在其中，换票由服务端完成。

use crate::error::{ManagerError, Result};
use crate::http::build_metadata_agent;
use crate::net::check_response_status;
use crate::telemetry::report_event;
use crate::ui::Ui;

use serde::Deserialize;
use std::sync::{Mutex, OnceLock};

/// 主站下发的运行期配置。
#[derive(Clone, Deserialize)]
pub struct RemoteConfig {
    /// 下载相关配置
    pub download: DownloadConfig,
    /// 自更新端点（未启用时为 `None`）
    #[serde(default)]
    pub self_update: Option<SelfUpdateConfig>,
    /// 下载源（可选）
    #[serde(default)]
    pub sources: Option<SourcesConfig>,
    /// 登录端点
    pub sso: SsoConfig,
}

/// 登录端点。
#[derive(Clone, Deserialize)]
pub struct SsoConfig {
    /// 授权页面来源
    pub authorize_origin: String,
    /// 换票接口地址
    pub session_url: String,
}

/// 下载密钥端点与限速参数。
#[derive(Clone, Deserialize)]
pub struct DownloadConfig {
    /// 一次性下载密钥接口
    pub keys_url: String,
    /// 并发下载上限
    pub max_concurrent_downloads: u32,
    /// 单任务限速（KB/s，0 表示不限速）
    pub rate_limit_kb_per_second: u64,
}

/// 自更新端点。
#[derive(Clone, Deserialize)]
pub struct SelfUpdateConfig {
    /// 更新信息接口
    pub api_base: String,
    /// 更新包入口地址
    pub entry_url: String,
}

/// 下载源配置。
#[derive(Clone, Deserialize)]
pub struct SourcesConfig {
    /// BepInEx 主源
    pub bep_in_ex_primary: String,
    /// GitHub 发布信息接口
    pub github_release_api: String,
}

static REMOTE_CONFIG_CACHE: OnceLock<Mutex<Option<(String, RemoteConfig)>>> = OnceLock::new();

/// 获取运行期配置；进程内只请求一次。
pub fn get(ui: &dyn Ui, config_url: &str) -> Result<RemoteConfig> {
    require_https("版本配置地址", config_url)?;

    let cache = REMOTE_CONFIG_CACHE.get_or_init(|| Mutex::new(None));

    if let Ok(guard) = cache.lock()
        && let Some((cached_url, config)) = guard.as_ref()
        && cached_url == config_url
    {
        return Ok(config.clone());
    }

    let agent = build_metadata_agent(config_url);
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

    require_https("下载密钥地址", &config.download.keys_url)?;
    require_https("登录授权地址", &config.sso.authorize_origin)?;
    require_https("登录会话地址", &config.sso.session_url)?;
    if let Some(self_update) = &config.self_update {
        require_https("自更新下载地址", &self_update.api_base)?;
        require_https("自更新入口", &self_update.entry_url)?;
    }
    if let Some(sources) = &config.sources {
        require_https("BepInEx 主源", &sources.bep_in_ex_primary)?;
        require_https("GitHub 发布信息地址", &sources.github_release_api)?;
    }

    if let Ok(mut guard) = cache.lock() {
        *guard = Some((config_url.to_string(), config.clone()));
    }

    Ok(config)
}

/// 携带凭据或下载可执行文件的地址必须走 HTTPS。
fn require_https(label: &str, url: &str) -> Result<()> {
    let url = url.trim();
    let authority = url
        .get(..8)
        .filter(|scheme| scheme.eq_ignore_ascii_case("https://"))
        .map(|_| &url[8..])
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .map(|authority| authority.rsplit('@').next().unwrap_or(authority));
    let has_host = authority.is_some_and(|host| {
        let host = if host.starts_with('[') {
            host
        } else {
            host.split(':').next().unwrap_or(host)
        };

        !host.is_empty()
    });

    if has_host {
        return Ok(());
    }

    report_event(
        "RemoteConfig.InvalidScheme",
        Some(&format!("{label}={url}")),
    );

    Err(ManagerError::NetworkError(format!(
        "配置无效：{label}不是 HTTPS 地址"
    )))
}

/// KB/s 换算成字节/s（1 KB = 1024 字节）；0 表示不限速。
pub fn rate_limit_bytes_per_second(kb_per_second: u64) -> Option<usize> {
    if kb_per_second == 0 {
        return None;
    }

    Some(usize::try_from(kb_per_second.saturating_mul(1024)).unwrap_or(usize::MAX))
}
