//! 系统代理与环境变量代理探测。

use super::url::{host_key, url_host};
use crate::platform::{read_system_proxy_settings, resolve_pac_proxy};
use std::{
    collections::HashMap,
    env,
    net::IpAddr,
    sync::{Mutex, OnceLock},
};

/// 读取系统代理设置，供构建 `ureq::Agent` 时使用。
///
/// ureq 自身只读环境变量（`HTTP_PROXY`、`HTTPS_PROXY` 等），不读 Windows 的系统代理设置，
/// 所以这里先读环境变量，再回落到 PAC（自动配置脚本）与注册表里的静态代理。
/// 返回值形如 `http://host:port`，可直接传给 `ureq::Proxy::new`。
pub(super) fn read_system_proxy(target_url: &str) -> Option<String> {
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

/// 匹配单个代理绕过模式；无通配符的域名同时匹配其子域（与 `NO_PROXY` / `WinINET` 语义一致）。
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

/// `*` 匹配任意字符（含 `.`）；调用方已把主机名与模式转成小写。
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

fn system_proxy_from_settings(target_url: &str) -> Option<String> {
    let settings = read_system_proxy_settings();

    if let Some(pac_url) = settings.auto_config_url.as_deref() {
        return resolve_pac_proxy_cached(target_url, pac_url);
    }
    if !settings.enabled {
        return None;
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

/// 用 PAC 脚本解析目标地址的代理；按 host 缓存结果。
fn resolve_pac_proxy_cached(target_url: &str, pac_url: &str) -> Option<String> {
    static PAC_PROXY_CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();

    let key = host_key(target_url);
    let cache = PAC_PROXY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Ok(guard) = cache.lock()
        && let Some(value) = guard.get(&key)
    {
        return value.clone();
    }

    let resolved = resolve_pac_proxy(target_url, pac_url).and_then(|(proxy, bypass)| {
        if bypass
            .as_deref()
            .is_some_and(|bypass| proxy_bypasses(target_url, bypass))
        {
            None
        } else {
            Some(proxy)
        }
    });

    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, resolved.clone());
    }

    resolved
}
