use crate::config::RetryConfig;
use crate::error::{ManagerError, Result};
use crate::metrics::report_event;
use crate::ui::Ui;

use serde::de::DeserializeOwned;
use std::{
    ffi::OsString, mem::size_of, os::windows::ffi::OsStringExt, ptr::null_mut, thread::sleep,
    time::Duration,
};
use ureq::Response;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD, REG_SZ, RegCloseKey, RegOpenKeyExW,
    RegQueryValueExW,
};

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
                if matches!(e, ManagerError::SlowDownload(_)) {
                    return Err(e);
                }

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

/// 将 ureq::Error 转换为 ManagerError，同时处理 429 Rate Limit
pub fn handle_ureq_error(e: ureq::Error, ui: &dyn Ui, op_desc: &str) -> ManagerError {
    match e {
        ureq::Error::Status(429, ref resp) => {
            let retry_after = resp
                .header("Retry-After")
                .and_then(|v| v.parse::<u64>().ok());

            if let Some(secs) = retry_after
                && secs <= 30
            {
                let _ = ui.network_rate_limited(secs);
                report_event(
                    "Network.RateLimited",
                    Some(&format!("{};retry_after={}", op_desc, secs)),
                );
                sleep(Duration::from_secs(secs));
            } else {
                report_event("Network.RateLimited", Some(op_desc));
            }
            ManagerError::RateLimited(op_desc.to_string())
        }
        ureq::Error::Status(code, _) => {
            report_event(
                "Network.HttpError",
                Some(&format!("{};status={}", op_desc, code)),
            );
            ManagerError::NetworkError(format!("{}返回错误：HTTP {}", op_desc, code))
        }
        ureq::Error::Transport(t) => ManagerError::NetworkError(format!("请求失败：{}", t)),
    }
}

/// 使用重试机制获取并解析 JSON 数据
///
/// # 参数
/// - `cfg`: 重试配置，`None` 表示使用默认的网络配置
pub fn get_json_with_retry<T: DeserializeOwned>(
    agent: &ureq::Agent,
    ui: &dyn Ui,
    url: &str,
    accept_header: Option<&str>,
    op_desc: &str,
    cfg: Option<RetryConfig>,
) -> Result<T> {
    with_retry(ui, op_desc, cfg, || {
        let mut req = agent.get(url);
        if let Some(h) = accept_header {
            req = req.set("Accept", h);
        }

        let resp = req.call().map_err(|e| handle_ureq_error(e, ui, op_desc))?;

        let text = resp.into_string().map_err(|e| {
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
    agent: &ureq::Agent,
    ui: &dyn Ui,
    url: &str,
    op_desc: &str,
    cfg: Option<RetryConfig>,
) -> Result<Response> {
    with_retry(ui, op_desc, cfg, || {
        let resp = agent
            .get(url)
            .call()
            .map_err(|e| handle_ureq_error(e, ui, op_desc))?;

        Ok(resp)
    })
}

/// 读取系统代理设置，供构建 ureq::Agent 时使用。
/// ureq 自身仅读取环境变量（HTTP_PROXY 等），不读取 Windows 注册表系统代理，
/// 此函数优先读取环境变量，再回落到注册表。
/// 返回形如 `"http://host:port"` 的字符串，可直接传入 `ureq::Proxy::new`
pub fn read_system_proxy() -> Option<String> {
    for var in &["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(val) = std::env::var(var)
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
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
            return None;
        }

        let enable_name: Vec<u16> = "ProxyEnable\0".encode_utf16().collect();
        let mut enable: u32 = 0;
        let mut size = size_of::<u32>() as u32;
        let mut kind: u32 = 0;
        RegQueryValueExW(
            hkey,
            enable_name.as_ptr(),
            null_mut(),
            &mut kind,
            &mut enable as *mut u32 as *mut u8,
            &mut size,
        );

        if kind != REG_DWORD || enable == 0 {
            RegCloseKey(hkey);
            return None;
        }

        let server_name: Vec<u16> = "ProxyServer\0".encode_utf16().collect();
        let mut buf = vec![0u16; 512];
        let mut buf_size = (buf.len() * 2) as u32;
        kind = 0;
        let ret = RegQueryValueExW(
            hkey,
            server_name.as_ptr(),
            null_mut(),
            &mut kind,
            buf.as_mut_ptr() as *mut u8,
            &mut buf_size,
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
                    part.strip_prefix(prefix).map(|v| v.to_string())
                })
            };
            find("https=")
                .or_else(|| find("http="))
                .unwrap_or(s.clone())
        } else {
            s
        };

        if proxy_addr.contains("://") {
            Some(proxy_addr)
        } else {
            Some(format!("http://{}", proxy_addr))
        }
    }
}

#[cfg(not(windows))]
fn read_windows_registry_proxy() -> Option<String> {
    None
}
