use crate::config::RetryConfig;
use crate::error::{ManagerError, Result};
use crate::metrics::report_event;
use crate::ui::Ui;

use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderValue, RETRY_AFTER};
use serde::de::DeserializeOwned;
use std::thread::sleep;
use std::time::Duration;

/// 重试执行操作
///
/// # 参数
/// - `cfg`: 重试配置，`None` 表示使用默认的网络配置
pub fn with_retry<F, T>(ui: &dyn Ui, op_desc: &str, cfg: Option<RetryConfig>, mut f: F) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    let cfg = cfg.unwrap_or_else(RetryConfig::network);

    for attempt in 0..cfg.attempts {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                let raw = (cfg.base_delay_secs as f64) * cfg.multiplier.powi(attempt as i32);
                let delay_secs = raw.min(cfg.max_delay_secs as f64).ceil() as u64;

                ui.network_retrying(
                    op_desc,
                    delay_secs,
                    attempt + 1,
                    cfg.attempts,
                    &format!("{}", e),
                )?;
                report_event(
                    "Network.Retry",
                    Some(&format!(
                        "{};attempt={};delay={}",
                        op_desc,
                        attempt + 1,
                        delay_secs
                    )),
                );

                if attempt < cfg.attempts - 1 {
                    sleep(Duration::from_secs(delay_secs));
                } else {
                    report_event("Network.RetryFailed", Some(op_desc));
                    return Err(e);
                }
            }
        }
    }

    unreachable!()
}

fn parse_retry_after_seconds(hv: Option<&HeaderValue>) -> Option<u64> {
    hv.and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

fn check_response_status(resp: &Response, ui: &dyn Ui, op_desc: &str) -> Result<()> {
    if resp.status().is_success() {
        return Ok(());
    }

    if resp.status().as_u16() == 429 {
        if let Some(secs) = parse_retry_after_seconds(resp.headers().get(RETRY_AFTER))
            && secs <= 30
        {
            ui.network_rate_limited(secs)?;
            report_event(
                "Network.RateLimited",
                Some(&format!("{};retry_after={}", op_desc, secs)),
            );
            sleep(Duration::from_secs(secs));
        } else {
            report_event("Network.RateLimited", Some(op_desc));
        }
        return Err(ManagerError::RateLimited(op_desc.to_string()));
    }

    report_event(
        "Network.HttpError",
        Some(&format!("{};status={}", op_desc, resp.status())),
    );

    Err(ManagerError::NetworkError(format!(
        "{}返回错误：HTTP {}",
        op_desc,
        resp.status()
    )))
}

/// 使用重试机制获取并解析 JSON 数据
///
/// # 参数
/// - `cfg`: 重试配置，`None` 表示使用默认的网络配置
pub fn get_json_with_retry<T: DeserializeOwned>(
    client: &Client,
    ui: &dyn Ui,
    url: &str,
    accept_header: Option<&str>,
    op_desc: &str,
    cfg: Option<RetryConfig>,
) -> Result<T> {
    with_retry(ui, op_desc, cfg, || {
        let mut req = client.get(url);
        if let Some(h) = accept_header {
            req = req.header("Accept", h);
        }

        let resp = req
            .send()
            .map_err(|e| ManagerError::NetworkError(format!("请求失败：{}", e)))?;

        check_response_status(&resp, ui, op_desc)?;

        let text = resp.text().map_err(|e| {
            report_event(
                "Network.ReadFailed",
                Some(&format!("{};err={}", op_desc, e)),
            );
            ManagerError::NetworkError(format!("读取响应失败：{}", e))
        })?;

        serde_json::from_str(&text).map_err(|e| {
            report_event(
                "Network.JsonParseFailed",
                Some(&format!("{};err={}", op_desc, e)),
            );
            ManagerError::NetworkError(format!("解析 JSON 失败：{}", e))
        })
    })
}

/// 使用重试机制获取响应
///
/// # 参数
/// - `cfg`: 重试配置，`None` 表示使用默认的网络配置
pub fn get_response_with_retry(
    client: &Client,
    ui: &dyn Ui,
    url: &str,
    op_desc: &str,
    cfg: Option<RetryConfig>,
) -> Result<Response> {
    with_retry(ui, op_desc, cfg, || {
        let resp = client
            .get(url)
            .send()
            .map_err(|e| ManagerError::NetworkError(format!("请求失败：{}", e)))?;

        check_response_status(&resp, ui, op_desc)?;

        Ok(resp)
    })
}
