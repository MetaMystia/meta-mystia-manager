//! HTTP 响应校验、限速处理与重试。

use crate::error::{ManagerError, Result};
use crate::net::retry::RetryConfig;
use crate::telemetry::report_event;
use crate::ui::{Ui, UiEvent};

use serde::de::DeserializeOwned;
use std::{result::Result as StdResult, thread::sleep, time::Duration};
use ureq::{Body, http::Response};

/// JSON 请求在重试流程中的终态错误。
pub enum JsonRequestError {
    /// 服务端返回非 2xx 状态码
    HttpStatus(u16),
    /// 其他错误
    Other(ManagerError),
}

/// 按退避策略重试 `f`；等待期间响应用户取消，次数用尽后返回最后一次错误。
pub fn with_retry<F, T>(ui: &dyn Ui, op_desc: &str, cfg: Option<RetryConfig>, mut f: F) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    let cfg = cfg.unwrap_or_else(RetryConfig::network);

    if cfg.attempts == 0 {
        return Err(ManagerError::Other(
            "重试配置无效：attempts 必须至少为 1".to_string(),
        ));
    }

    for attempt in 0..cfg.attempts {
        if ui.is_download_cancelled() || ui.is_download_aborted() {
            return Err(ManagerError::UserCancelled);
        }

        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !e.is_retryable() {
                    return Err(e);
                }

                if attempt < cfg.attempts - 1 {
                    let delay = retry_delay(&e, &cfg, attempt);
                    let error = e.to_string();

                    ui.emit(UiEvent::NetworkRetrying(
                        op_desc,
                        delay.as_secs(),
                        attempt + 1,
                        cfg.attempts,
                        &error,
                    ))?;
                    report_event(
                        "Network.Retry",
                        Some(&format!(
                            "{};attempt={};delay={}",
                            op_desc,
                            attempt + 1,
                            delay.as_secs()
                        )),
                    );
                    if !sleep_with_cancel(ui, delay) {
                        return Err(ManagerError::UserCancelled);
                    }
                } else {
                    ui.emit(UiEvent::NetworkRetryFailed(
                        op_desc,
                        cfg.attempts,
                        &e.to_string(),
                    ))?;
                    report_event("Network.RetryFailed", Some(op_desc));
                    return Err(e);
                }
            }
        }
    }

    Err(ManagerError::Other("重试流程意外结束".to_string()))
}

/// 429 已在错误里带了服务端要求的等待时长，这里不再叠加客户端退避。
fn retry_delay(error: &ManagerError, cfg: &RetryConfig, attempt: usize) -> Duration {
    if let ManagerError::RateLimited(_, Some(secs)) = error {
        return Duration::from_secs((*secs).min(30));
    }

    cfg.delay(attempt)
}

/// 可取消的等待；返回 `false` 表示等待期间用户取消了操作。
fn sleep_with_cancel(ui: &dyn Ui, delay: Duration) -> bool {
    const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(100);
    let mut remaining = delay;

    while !remaining.is_zero() {
        if ui.is_download_cancelled() || ui.is_download_aborted() {
            return false;
        }

        let step = remaining.min(CANCEL_POLL_INTERVAL);
        sleep(step);
        remaining = remaining.saturating_sub(step);
    }

    !ui.is_download_cancelled() && !ui.is_download_aborted()
}

fn retry_after_secs(headers: &ureq::http::HeaderMap) -> Option<u64> {
    headers
        .get("Retry-After")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
}

/// 处理 429 Rate Limit：等待 `Retry-After` 指定的秒数（不超过 30 秒）。
pub fn rate_limit_error(resp: &Response<Body>, ui: &dyn Ui, op_desc: &str) -> ManagerError {
    let retry_after = retry_after_secs(resp.headers());

    if let Some(secs) = retry_after
        && secs <= 30
    {
        let _ = ui.emit(UiEvent::NetworkRateLimited(secs));
        report_event(
            "Network.RateLimited",
            Some(&format!("{op_desc};retry_after={secs}")),
        );
    } else {
        report_event("Network.RateLimited", Some(op_desc));
    }

    ManagerError::RateLimited(
        format!("{op_desc}失败：请求过于频繁，请稍后再试"),
        retry_after,
    )
}

/// 检查响应状态码，非成功状态（4xx/5xx）转换为 `ManagerError`，成功状态返回 `None`。
///
/// ureq 3 默认会把 4xx/5xx 直接转换为 [`ureq::Error::StatusCode`]，那样会丢失响应头，
/// 因此本项目在构建 Agent 时关闭了该行为，统一在此处判定，以便处理 429 的 `Retry-After`。
pub fn check_response_status(
    resp: &Response<Body>,
    ui: &dyn Ui,
    op_desc: &str,
) -> Option<ManagerError> {
    let status = resp.status();
    if !status.is_client_error() && !status.is_server_error() {
        return None;
    }

    let code = status.as_u16();
    if code == 429 {
        return Some(rate_limit_error(resp, ui, op_desc));
    }

    report_event(
        "Network.HttpError",
        Some(&format!("{op_desc};status={code}")),
    );

    let message = format!("{op_desc}返回错误：HTTP {code}");

    // 4xx（429 除外）重试无意义；5xx 视为暂时故障
    if status.is_client_error() {
        Some(ManagerError::HttpError(message))
    } else {
        Some(ManagerError::NetworkError(message))
    }
}

/// 使用重试机制获取 JSON 响应，并在遇到特定 HTTP 状态码时停止重试。
pub fn fetch_json_with_retry_stopping_on_status<T: DeserializeOwned>(
    agent: &ureq::Agent,
    ui: &dyn Ui,
    url: &str,
    accept_header: Option<&str>,
    op_desc: &str,
    cfg: Option<RetryConfig>,
    stop_statuses: &[u16],
) -> StdResult<T, JsonRequestError> {
    let cfg = cfg.unwrap_or_else(RetryConfig::network);

    if cfg.attempts == 0 {
        return Err(JsonRequestError::Other(ManagerError::Other(
            "重试配置无效：attempts 必须至少为 1".to_string(),
        )));
    }

    for attempt in 0..cfg.attempts {
        if ui.is_download_cancelled() || ui.is_download_aborted() {
            return Err(JsonRequestError::Other(ManagerError::UserCancelled));
        }

        let mut req = agent.get(url);
        if let Some(h) = accept_header {
            req = req.header("Accept", h);
        }

        let err = match req.call() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if stop_statuses.contains(&status) {
                    return Err(JsonRequestError::HttpStatus(status));
                }

                if let Some(err) = check_response_status(&resp, ui, op_desc) {
                    err
                } else {
                    let text = resp.into_body().read_to_string().map_err(|e| {
                        report_event("Network.ReadFailed", Some(&format!("{op_desc};err={e}")));
                        JsonRequestError::Other(ManagerError::NetworkError(format!(
                            "读取响应失败：{e}"
                        )))
                    })?;

                    return serde_json::from_str(&text).map_err(|e| {
                        report_event(
                            "Network.JsonParseFailed",
                            Some(&format!("{op_desc};err={e}")),
                        );
                        JsonRequestError::Other(ManagerError::NetworkError(format!(
                            "解析 JSON 失败：{e}"
                        )))
                    });
                }
            }
            Err(err) => ManagerError::from(err),
        };

        if !err.is_retryable() {
            return Err(JsonRequestError::Other(err));
        }

        if attempt < cfg.attempts - 1 {
            let delay = retry_delay(&err, &cfg, attempt);

            ui.emit(UiEvent::NetworkRetrying(
                op_desc,
                delay.as_secs(),
                attempt + 1,
                cfg.attempts,
                &err.to_string(),
            ))
            .map_err(JsonRequestError::Other)?;
            report_event(
                "Network.Retry",
                Some(&format!(
                    "{};attempt={};delay={}",
                    op_desc,
                    attempt + 1,
                    delay.as_secs()
                )),
            );
            if !sleep_with_cancel(ui, delay) {
                return Err(JsonRequestError::Other(ManagerError::UserCancelled));
            }
        } else {
            ui.emit(UiEvent::NetworkRetryFailed(
                op_desc,
                cfg.attempts,
                &err.to_string(),
            ))
            .map_err(JsonRequestError::Other)?;
            report_event("Network.RetryFailed", Some(op_desc));
            return Err(JsonRequestError::Other(err));
        }
    }

    Err(JsonRequestError::Other(ManagerError::Other(
        "重试流程意外结束".to_string(),
    )))
}

/// 带重试地发起 GET 请求并校验响应状态。
pub fn fetch_response_with_retry(
    agent: &ureq::Agent,
    ui: &dyn Ui,
    url: &str,
    op_desc: &str,
    cfg: Option<RetryConfig>,
) -> Result<Response<Body>> {
    with_retry(ui, op_desc, cfg, || {
        let resp = agent.get(url).call().map_err(ManagerError::from)?;

        if let Some(err) = check_response_status(&resp, ui, op_desc) {
            return Err(err);
        }

        Ok(resp)
    })
}
