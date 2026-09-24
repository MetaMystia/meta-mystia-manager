use crate::config::{RetryConfig, USER_AGENT};
use crate::error::{ManagerError, Result};
use crate::metrics::report_event;
use crate::ui::Ui;

use serde::de::DeserializeOwned;
use std::{env, thread::sleep, time::Duration};
use ureq::{Body, http::Response};

#[cfg(windows)]
use crate::win32::dword_len;
#[cfg(windows)]
use std::{ffi::OsString, mem::size_of, os::windows::ffi::OsStringExt, ptr::null_mut};
#[cfg(windows)]
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD, REG_SZ, RegCloseKey, RegOpenKeyExW,
    RegQueryValueExW,
};

#[derive(Debug)]
pub enum JsonRequestError {
    HttpStatus(u16),
    Other(ManagerError),
}

/// 重试执行操作
///
/// # 参数
/// - `cfg`: 重试配置，`None` 表示使用默认的网络配置
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
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                if matches!(e, ManagerError::SlowDownload(_)) {
                    return Err(e);
                }

                let delay = cfg.delay(attempt);

                ui.network_retrying(
                    op_desc,
                    delay.as_secs(),
                    attempt + 1,
                    cfg.attempts,
                    &format!("{e}"),
                )?;
                report_event(
                    "Network.Retry",
                    Some(&format!(
                        "{};attempt={};delay={}",
                        op_desc,
                        attempt + 1,
                        delay.as_secs()
                    )),
                );

                if attempt < cfg.attempts - 1 {
                    sleep(delay);
                } else {
                    report_event("Network.RetryFailed", Some(op_desc));
                    return Err(e);
                }
            }
        }
    }

    Err(ManagerError::Other("重试流程意外结束".to_string()))
}

/// 解析 `Retry-After` 响应头（秒）
fn retry_after_secs(headers: &ureq::http::HeaderMap) -> Option<u64> {
    headers
        .get("Retry-After")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
}

/// 处理 429 Rate Limit：等待 `Retry-After` 指定的秒数（不超过 30 秒）
fn rate_limited_error(retry_after: Option<u64>, ui: &dyn Ui, op_desc: &str) -> ManagerError {
    if let Some(secs) = retry_after
        && secs <= 30
    {
        let _ = ui.network_rate_limited(secs);
        report_event(
            "Network.RateLimited",
            Some(&format!("{op_desc};retry_after={secs}")),
        );
        sleep(Duration::from_secs(secs));
    } else {
        report_event("Network.RateLimited", Some(op_desc));
    }
    ManagerError::RateLimited(op_desc.to_string())
}

/// 检查响应状态码，非成功状态（4xx/5xx）转换为 `ManagerError`，成功状态返回 `None`
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
        return Some(rate_limited_error(
            retry_after_secs(resp.headers()),
            ui,
            op_desc,
        ));
    }

    report_event(
        "Network.HttpError",
        Some(&format!("{op_desc};status={code}")),
    );
    Some(ManagerError::NetworkError(format!(
        "{op_desc}返回错误：HTTP {code}"
    )))
}

/// 使用重试机制获取 JSON 响应，并在遇到特定 HTTP 状态码时停止重试
///
/// # 参数
/// - `cfg`: 重试配置，`None` 表示使用默认的网络配置
/// - `stop_statuses`: 遇到这些 HTTP 状态码时停止重试，并返回 `JsonRequestError::HttpStatus`
pub fn get_json_with_retry_stopping_on_status<T: DeserializeOwned>(
    agent: &ureq::Agent,
    ui: &dyn Ui,
    url: &str,
    accept_header: Option<&str>,
    op_desc: &str,
    cfg: Option<RetryConfig>,
    stop_statuses: &[u16],
) -> std::result::Result<T, JsonRequestError> {
    let cfg = cfg.unwrap_or_else(RetryConfig::network);

    if cfg.attempts == 0 {
        return Err(JsonRequestError::Other(ManagerError::Other(
            "重试配置无效：attempts 必须至少为 1".to_string(),
        )));
    }

    for attempt in 0..cfg.attempts {
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

        let delay = cfg.delay(attempt);

        ui.network_retrying(
            op_desc,
            delay.as_secs(),
            attempt + 1,
            cfg.attempts,
            &err.to_string(),
        )
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

        if attempt < cfg.attempts - 1 {
            sleep(delay);
        } else {
            report_event("Network.RetryFailed", Some(op_desc));
            return Err(JsonRequestError::Other(err));
        }
    }

    Err(JsonRequestError::Other(ManagerError::Other(
        "重试流程意外结束".to_string(),
    )))
}

/// 构建统一配置的 `ureq::Agent`
///
/// ureq 3 TLS 配置需要显式指定 provider，否则会使用默认的 rustls；
/// 本程序编译时只启用了 native-tls 特性，所以必须设置为 `NativeTls` 并使用系统证书库。
///
/// # 参数
/// - `connect_timeout`: 连接超时（含 TLS 握手），`None` 表示不限制
/// - `global_timeout`: 整个请求的超时，`None` 表示不限制
pub fn build_agent(
    connect_timeout: Option<Duration>,
    global_timeout: Option<Duration>,
) -> ureq::Agent {
    let tls_config = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();

    let mut builder = ureq::Agent::config_builder()
        .tls_config(tls_config)
        .timeout_connect(connect_timeout)
        .timeout_global(global_timeout)
        // 4xx/5xx 交由 check_response_status 判定，便于读取 Retry-After
        .http_status_as_error(false)
        .user_agent(USER_AGENT);

    if let Some(proxy) = read_system_proxy()
        && let Ok(p) = ureq::Proxy::new(&proxy)
    {
        builder = builder.proxy(Some(p));
    }

    ureq::Agent::new_with_config(builder.build())
}

/// 使用重试机制获取响应
///
/// # 参数
/// - `cfg`: 重试配置，`None` 表示使用默认的网络配置
pub fn get_response_with_retry(
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

/// 读取系统代理设置，供构建 `ureq::Agent` 时使用。
///
/// ureq 自身只读环境变量（`HTTP_PROXY`、`HTTPS_PROXY` 等），不读 Windows 的系统代理设置，
/// 所以这里先读环境变量，再回落到注册表。返回值形如 `http://host:port`，可直接传给 `ureq::Proxy::new`。
pub fn read_system_proxy() -> Option<String> {
    for var in &["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(val) = env::var(var)
            && !val.is_empty()
        {
            return Some(val);
        }
    }

    read_windows_registry_proxy()
}

#[cfg(windows)]
fn read_windows_registry_proxy() -> Option<String> {
    unsafe {
        let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings\0"
            .encode_utf16()
            .collect();

        let mut hkey: HKEY = null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_READ,
            &raw mut hkey,
        ) != 0
        {
            return None;
        }

        let enable_name: Vec<u16> = "ProxyEnable\0".encode_utf16().collect();
        let mut enable: u32 = 0;
        let mut size = dword_len(size_of::<u32>());
        let mut kind: u32 = 0;
        RegQueryValueExW(
            hkey,
            enable_name.as_ptr(),
            null_mut(),
            &raw mut kind,
            (&raw mut enable).cast::<u8>(),
            &raw mut size,
        );

        if kind != REG_DWORD || enable == 0 {
            RegCloseKey(hkey);
            return None;
        }

        let server_name: Vec<u16> = "ProxyServer\0".encode_utf16().collect();
        let mut buf = vec![0u16; 512];
        let mut buf_size = dword_len(buf.len() * 2);
        kind = 0;
        let ret = RegQueryValueExW(
            hkey,
            server_name.as_ptr(),
            null_mut(),
            &raw mut kind,
            buf.as_mut_ptr().cast::<u8>(),
            &raw mut buf_size,
        );
        RegCloseKey(hkey);

        if ret != 0 || kind != REG_SZ {
            return None;
        }

        let len = buf_size as usize / 2;
        let s = OsString::from_wide(&buf[..len])
            .to_string_lossy()
            .trim_end_matches('\0')
            .to_string();

        if s.is_empty() {
            return None;
        }

        let proxy_addr = if s.contains('=') {
            let find = |prefix: &str| -> Option<String> {
                s.split(';').find_map(|part| {
                    let part = part.trim();
                    part.strip_prefix(prefix).map(ToString::to_string)
                })
            };
            find("https=")
                .or_else(|| find("http="))
                .unwrap_or_else(|| s.clone())
        } else {
            s
        };

        if proxy_addr.contains("://") {
            Some(proxy_addr)
        } else {
            Some(format!("http://{proxy_addr}"))
        }
    }
}

#[cfg(not(windows))]
const fn read_windows_registry_proxy() -> Option<String> {
    None
}
