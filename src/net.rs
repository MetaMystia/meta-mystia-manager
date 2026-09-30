use crate::config::{RetryConfig, USER_AGENT};
use crate::error::{ManagerError, Result};
use crate::metrics::report_event;
use crate::ui::{Ui, UiEvent};

use serde::de::DeserializeOwned;
use std::{env, net::IpAddr, result::Result as StdResult, thread::sleep, time::Duration};
use ureq::{
    Body,
    http::Response,
    unversioned::{
        resolver::DefaultResolver,
        transport::{
            Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport, time,
        },
    },
};

const RECV_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const METADATA_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(70);

#[cfg(windows)]
use crate::win32::dword_len;
#[cfg(windows)]
use std::{
    collections::HashMap,
    ffi::OsString,
    mem::size_of,
    os::windows::ffi::OsStringExt,
    ptr::{null, null_mut},
    slice,
    sync::{Mutex, OnceLock},
};
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::GlobalFree,
    Networking::WinHttp::{
        WINHTTP_ACCESS_TYPE_NAMED_PROXY, WINHTTP_ACCESS_TYPE_NO_PROXY,
        WINHTTP_AUTOPROXY_CONFIG_URL, WINHTTP_AUTOPROXY_OPTIONS, WINHTTP_PROXY_INFO,
        WinHttpCloseHandle, WinHttpGetProxyForUrl, WinHttpOpen, WinHttpSetTimeouts,
    },
    System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD, REG_SZ, RegCloseKey, RegOpenKeyExW,
        RegQueryValueExW,
    },
};

#[cfg(windows)]
const PAC_TIMEOUT_MS: i32 = 5_000;

#[derive(Debug)]
pub enum JsonRequestError {
    HttpStatus(u16),
    Other(ManagerError),
}

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
        if ui.download_cancelled() || ui.download_aborted() {
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

/// 429 已在错误里带了服务端要求的等待时长，这里不再叠加客户端退避
fn retry_delay(error: &ManagerError, cfg: &RetryConfig, attempt: usize) -> Duration {
    if let ManagerError::RateLimited(_, Some(secs)) = error {
        return Duration::from_secs((*secs).min(30));
    }

    cfg.delay(attempt)
}

/// 可取消的等待；返回 `false` 表示等待期间用户取消了操作
fn sleep_with_cancel(ui: &dyn Ui, delay: Duration) -> bool {
    const STEP: Duration = Duration::from_millis(100);
    let mut remaining = delay;

    while !remaining.is_zero() {
        if ui.download_cancelled() || ui.download_aborted() {
            return false;
        }

        let step = remaining.min(STEP);
        sleep(step);
        remaining = remaining.saturating_sub(step);
    }

    !ui.download_cancelled() && !ui.download_aborted()
}

/// 解析 `Retry-After` 响应头（秒）
fn retry_after_secs(headers: &ureq::http::HeaderMap) -> Option<u64> {
    headers
        .get("Retry-After")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
}

/// 处理 429 Rate Limit：等待 `Retry-After` 指定的秒数（不超过 30 秒）
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

/// 使用重试机制获取 JSON 响应，并在遇到特定 HTTP 状态码时停止重试
pub fn get_json_with_retry_stopping_on_status<T: DeserializeOwned>(
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
        if ui.download_cancelled() || ui.download_aborted() {
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

fn build_config(
    url: &str,
    connect_timeout: Option<Duration>,
    global_timeout: Option<Duration>,
    body_timeout: Option<Duration>,
) -> ureq::config::Config {
    // 只启用了 native-tls；不显式指定 provider 时会尝试使用未启用的 rustls。
    let tls_config = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();

    let mut builder = ureq::Agent::config_builder()
        .tls_config(tls_config)
        .timeout_connect(connect_timeout)
        .timeout_global(global_timeout)
        .timeout_recv_response(Some(RECV_RESPONSE_TIMEOUT))
        .timeout_recv_body(body_timeout)
        // 4xx/5xx 交由 check_response_status 判定，便于读取 Retry-After
        .http_status_as_error(false)
        .user_agent(USER_AGENT);

    if let Some(proxy) = read_system_proxy(url)
        && let Ok(p) = ureq::Proxy::new(&proxy)
    {
        builder = builder.proxy(Some(p));
    }

    builder.build()
}

pub fn build_agent_with_timeouts(
    url: &str,
    connect_timeout: Option<Duration>,
    global_timeout: Option<Duration>,
    body_timeout: Option<Duration>,
) -> ureq::Agent {
    ureq::Agent::new_with_config(build_config(
        url,
        connect_timeout,
        global_timeout,
        body_timeout,
    ))
}

pub fn build_metadata_agent(url: &str) -> ureq::Agent {
    build_agent_with_timeouts(
        url,
        Some(CONNECT_TIMEOUT),
        Some(METADATA_TIMEOUT),
        Some(METADATA_TIMEOUT),
    )
}

/// 只限制单次读写时长，不限制下载总时长。
pub fn build_download_agent(url: &str) -> ureq::Agent {
    let config = build_config(url, Some(CONNECT_TIMEOUT), None, None);

    ureq::Agent::with_parts(
        config,
        StallingConnector::new(DOWNLOAD_READ_TIMEOUT),
        DefaultResolver::default(),
    )
}

#[derive(Debug)]
struct StallingConnector {
    inner: DefaultConnector,
    timeout: Duration,
}

impl StallingConnector {
    fn new(timeout: Duration) -> Self {
        Self {
            inner: DefaultConnector::new(),
            timeout,
        }
    }
}

impl Connector<()> for StallingConnector {
    type Out = Box<dyn Transport>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<()>,
    ) -> StdResult<Option<Self::Out>, ureq::Error> {
        let Some(transport) = self.inner.connect(details, chained)? else {
            return Ok(None);
        };

        Ok(Some(Box::new(StallingTransport {
            inner: transport,
            timeout: self.timeout,
        })))
    }
}

#[derive(Debug)]
struct StallingTransport {
    inner: Box<dyn Transport>,
    timeout: Duration,
}

impl StallingTransport {
    fn cap(&self, timeout: NextTimeout) -> NextTimeout {
        let after = match timeout.after {
            time::Duration::Exact(after) => time::Duration::Exact(after.min(self.timeout)),
            time::Duration::NotHappening => time::Duration::Exact(self.timeout),
        };

        NextTimeout {
            after,
            reason: timeout.reason,
        }
    }
}

impl Transport for StallingTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(
        &mut self,
        amount: usize,
        timeout: NextTimeout,
    ) -> StdResult<(), ureq::Error> {
        self.inner.transmit_output(amount, self.cap(timeout))
    }

    fn await_input(&mut self, timeout: NextTimeout) -> StdResult<bool, ureq::Error> {
        self.inner.await_input(self.cap(timeout))
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

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
/// 所以这里先读环境变量，再回落到 PAC（自动配置脚本）与注册表里的静态代理。
/// 返回值形如 `http://host:port`，可直接传给 `ureq::Proxy::new`。
pub fn read_system_proxy(target_url: &str) -> Option<String> {
    let bypassed = env_proxy_bypassed(target_url);

    for var in &["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(val) = env::var(var)
            && !val.is_empty()
        {
            return (!bypassed).then_some(val);
        }
    }

    system_proxy_from_settings(target_url)
}

fn env_proxy_bypassed(target_url: &str) -> bool {
    ["NO_PROXY", "no_proxy"].iter().any(|var| {
        env::var(var)
            .is_ok_and(|value| !value.trim().is_empty() && proxy_bypasses(target_url, &value))
    })
}

fn proxy_bypasses(target_url: &str, patterns: &str) -> bool {
    let Some(host) = url_host(target_url) else {
        return false;
    };
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();

    for raw in patterns.split([';', ',']) {
        let pattern = raw.trim();
        if pattern.is_empty() {
            continue;
        }
        if pattern == "*" {
            return true;
        }

        let pattern = pattern.to_ascii_lowercase();
        if pattern == "<local>" {
            if !host.contains('.') {
                return true;
            }
            continue;
        }

        let pattern_host = pattern.strip_prefix('[').map_or_else(
            || {
                if pattern.matches(':').count() == 1 {
                    pattern.split(':').next().unwrap_or(&pattern)
                } else {
                    &pattern
                }
            },
            |rest| rest.split(']').next().unwrap_or(rest),
        );

        if host_pattern_matches(&host, pattern_host) {
            return true;
        }
    }

    false
}

/// 匹配单个代理绕过模式；无通配符的域名同时匹配其子域（与 `NO_PROXY` / `WinINET` 语义一致）
fn host_pattern_matches(host: &str, pattern: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return host == suffix || host.ends_with(&format!(".{suffix}"));
    }
    if let Some(suffix) = pattern.strip_prefix('.') {
        return host.ends_with(&format!(".{suffix}"));
    }
    if pattern.contains('*') {
        return wildcard_matches(host, pattern);
    }
    if host == pattern {
        return true;
    }
    if host.parse::<IpAddr>().is_ok() || pattern.parse::<IpAddr>().is_ok() {
        return false;
    }

    host.ends_with(&format!(".{pattern}"))
}

/// `*` 匹配任意字符（含 `.`）；调用方已把主机名与模式转成小写
fn wildcard_matches(text: &str, pattern: &str) -> bool {
    let mut rest = text;

    for (index, part) in pattern.split('*').enumerate() {
        if part.is_empty() {
            continue;
        }

        if index == 0 {
            let Some(stripped) = rest.strip_prefix(part) else {
                return false;
            };
            rest = stripped;
            continue;
        }

        let Some(position) = rest.find(part) else {
            return false;
        };
        rest = &rest[position + part.len()..];
    }

    pattern.ends_with('*') || rest.is_empty()
}

fn url_host(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = authority.strip_prefix('[').map_or_else(
        || authority.split(':').next().unwrap_or(authority),
        |rest| rest.split(']').next().unwrap_or(rest),
    );

    (!host.is_empty()).then_some(host)
}

/// 系统设置里的代理：PAC 优先，其次静态代理
#[cfg(windows)]
fn system_proxy_from_settings(target_url: &str) -> Option<String> {
    let settings = read_windows_proxy_settings();

    if let Some(pac_url) = settings.auto_config_url.as_deref() {
        return resolve_pac_proxy(target_url, pac_url);
    }

    if settings
        .bypass
        .as_deref()
        .is_some_and(|bypass| proxy_bypasses(target_url, bypass))
    {
        return None;
    }

    settings.server
}

#[cfg(not(windows))]
const fn system_proxy_from_settings(_target_url: &str) -> Option<String> {
    None
}

/// 从 URL 中提取用于缓存代理与连接的 host key（含显式端口）
pub fn host_key(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return url.to_ascii_lowercase();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority.rsplit('@').next().unwrap_or(authority);

    let (host, port) = split_authority(authority);

    let host = host.to_ascii_lowercase();

    match port {
        Some(port) => format!("{host}:{port}"),
        None => host,
    }
}

/// 拆分 authority 里的主机与显式端口，兼容 `[::1]:8080` 形式的 IPv6 字面量
fn split_authority(authority: &str) -> (&str, Option<&str>) {
    if let Some(rest) = authority.strip_prefix('[') {
        let Some((host, tail)) = rest.split_once(']') else {
            return (rest, None);
        };

        return (host, tail.strip_prefix(':').filter(|port| !port.is_empty()));
    }

    let Some((host, port)) = authority.split_once(':') else {
        return (authority, None);
    };

    (host, (!port.is_empty()).then_some(port))
}

#[cfg(windows)]
#[derive(Default)]
struct WindowsProxySettings {
    auto_config_url: Option<String>,
    bypass: Option<String>,
    server: Option<String>,
}

#[cfg(windows)]
fn read_windows_proxy_settings() -> WindowsProxySettings {
    let mut settings = WindowsProxySettings::default();

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
            return settings;
        }

        settings.auto_config_url = read_registry_string(hkey, "AutoConfigURL");
        settings.bypass = read_registry_string(hkey, "ProxyOverride");

        if read_registry_dword(hkey, "ProxyEnable") == Some(1)
            && let Some(server) = read_registry_string(hkey, "ProxyServer")
        {
            settings.server = normalize_proxy_server(&server);
        }

        RegCloseKey(hkey);
    }

    settings
}

/// 读取一个 `REG_SZ` 值；不存在或类型不符时返回 `None`
#[cfg(windows)]
unsafe fn read_registry_string(hkey: HKEY, name: &str) -> Option<String> {
    let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let mut kind: u32 = 0;
    let mut buffer = vec![0u16; 1024];
    let mut size = dword_len(buffer.len() * size_of::<u16>());

    let ret = unsafe {
        RegQueryValueExW(
            hkey,
            name.as_ptr(),
            null_mut(),
            &raw mut kind,
            buffer.as_mut_ptr().cast::<u8>(),
            &raw mut size,
        )
    };

    if ret != 0 || kind != REG_SZ {
        return None;
    }

    let len = (size as usize / size_of::<u16>()).min(buffer.len());
    let value = OsString::from_wide(&buffer[..len])
        .to_string_lossy()
        .trim_end_matches('\0')
        .trim()
        .to_string();

    (!value.is_empty()).then_some(value)
}

/// 读取一个 `REG_DWORD` 值；不存在或类型不符时返回 `None`
#[cfg(windows)]
unsafe fn read_registry_dword(hkey: HKEY, name: &str) -> Option<u32> {
    let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let mut kind: u32 = 0;
    let mut value: u32 = 0;
    let mut size = dword_len(size_of::<u32>());

    let ret = unsafe {
        RegQueryValueExW(
            hkey,
            name.as_ptr(),
            null_mut(),
            &raw mut kind,
            (&raw mut value).cast::<u8>(),
            &raw mut size,
        )
    };

    (ret == 0 && kind == REG_DWORD).then_some(value)
}

/// 归一化注册表里的代理服务器：`host:port` → `http://host:port`，`http=`/`https=` 列表取其一
#[cfg(windows)]
fn normalize_proxy_server(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let proxy = if value.contains('=') {
        let find = |prefix: &str| {
            value
                .split(';')
                .find_map(|part| part.trim().strip_prefix(prefix).map(ToString::to_string))
        };

        find("https=").or_else(|| find("http="))?
    } else {
        value.to_string()
    };

    let proxy = proxy.trim();
    if proxy.is_empty() {
        return None;
    }

    if proxy.contains("://") {
        Some(proxy.to_string())
    } else {
        Some(format!("http://{proxy}"))
    }
}

/// 用 PAC 脚本解析目标地址应使用的代理；按 host 缓存解析结果
#[cfg(windows)]
fn resolve_pac_proxy(target_url: &str, pac_url: &str) -> Option<String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();

    let key = host_key(target_url);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Ok(guard) = cache.lock()
        && let Some(value) = guard.get(&key)
    {
        return value.clone();
    }

    let resolved = resolve_pac_proxy_uncached(target_url, pac_url);

    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, resolved.clone());
    }

    resolved
}

/// 调 `WinHTTP` 执行 PAC 脚本；解析失败时按“没有代理”处理
#[cfg(windows)]
fn resolve_pac_proxy_uncached(target_url: &str, pac_url: &str) -> Option<String> {
    unsafe {
        let agent: Vec<u16> = USER_AGENT.encode_utf16().chain([0]).collect();
        let session = WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            null(),
            null(),
            0,
        );
        if session.is_null() {
            return None;
        }

        // PAC 脚本本身要联网拉取，给个上限避免拖住启动
        WinHttpSetTimeouts(
            session,
            PAC_TIMEOUT_MS,
            PAC_TIMEOUT_MS,
            PAC_TIMEOUT_MS,
            PAC_TIMEOUT_MS,
        );

        let target: Vec<u16> = target_url.encode_utf16().chain([0]).collect();
        let script: Vec<u16> = pac_url.encode_utf16().chain([0]).collect();
        let mut options = WINHTTP_AUTOPROXY_OPTIONS {
            dwFlags: WINHTTP_AUTOPROXY_CONFIG_URL,
            lpszAutoConfigUrl: script.as_ptr(),
            ..WINHTTP_AUTOPROXY_OPTIONS::default()
        };
        let mut info = WINHTTP_PROXY_INFO::default();

        let resolved =
            WinHttpGetProxyForUrl(session, target.as_ptr(), &raw mut options, &raw mut info);

        let proxy = wide_ptr_to_string(info.lpszProxy);
        let bypass = wide_ptr_to_string(info.lpszProxyBypass);
        if !info.lpszProxy.is_null() {
            GlobalFree(info.lpszProxy.cast());
        }
        if !info.lpszProxyBypass.is_null() {
            GlobalFree(info.lpszProxyBypass.cast());
        }
        WinHttpCloseHandle(session);

        if resolved == 0 || info.dwAccessType != WINHTTP_ACCESS_TYPE_NAMED_PROXY {
            return None;
        }
        if bypass
            .as_deref()
            .is_some_and(|bypass| proxy_bypasses(target_url, bypass))
        {
            return None;
        }

        // PAC 可能返回多条代理（`proxy1:80;proxy2:80`），取第一条交给 ureq
        let first = proxy?
            .split([';', ' '])
            .find(|part| !part.is_empty())?
            .to_string();

        Some(if first.contains("://") {
            first
        } else {
            format!("http://{first}")
        })
    }
}

#[cfg(windows)]
unsafe fn wide_ptr_to_string(pointer: *const u16) -> Option<String> {
    if pointer.is_null() {
        return None;
    }

    let mut len = 0usize;
    while len < 4096 && unsafe { *pointer.add(len) } != 0 {
        len += 1;
    }

    Some(
        OsString::from_wide(unsafe { slice::from_raw_parts(pointer, len) })
            .to_string_lossy()
            .into_owned(),
    )
}
