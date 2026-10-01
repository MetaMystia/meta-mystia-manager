//! Windows 系统代理读取与 PAC 解析。

use super::registry::{read_dword, read_string};
use crate::config::USER_AGENT;
use crate::platform::SystemProxySettings;

use std::{ffi::OsString, os::windows::ffi::OsStringExt, ptr::null, slice};
use windows_sys::Win32::{
    Foundation::GlobalFree,
    Networking::WinHttp::{
        WINHTTP_ACCESS_TYPE_NAMED_PROXY, WINHTTP_ACCESS_TYPE_NO_PROXY,
        WINHTTP_AUTOPROXY_CONFIG_URL, WINHTTP_AUTOPROXY_OPTIONS, WINHTTP_PROXY_INFO,
        WinHttpCloseHandle, WinHttpGetProxyForUrl, WinHttpOpen, WinHttpSetTimeouts,
    },
    System::Registry::HKEY_CURRENT_USER,
};

// 代理设置
const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const PAC_TIMEOUT_MS: i32 = 5_000;

/// 读取 `WinINET` 代理设置；注册表不可读时返回默认值。
pub fn read_system_proxy_settings() -> SystemProxySettings {
    SystemProxySettings {
        enabled: read_dword(HKEY_CURRENT_USER, INTERNET_SETTINGS, "ProxyEnable") == Some(1),
        auto_config_url: read_string(HKEY_CURRENT_USER, INTERNET_SETTINGS, "AutoConfigURL"),
        bypass: read_string(HKEY_CURRENT_USER, INTERNET_SETTINGS, "ProxyOverride"),
        server: read_string(HKEY_CURRENT_USER, INTERNET_SETTINGS, "ProxyServer")
            .and_then(|server| normalize_proxy_server(&server)),
    }
}

/// 归一化注册表里的代理服务器：`host:port` → `http://host:port`，`http=`/`https=` 列表取其一。
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

/// 调用 `WinHTTP` 执行 PAC 脚本，返回代理与 PAC 提供的绕过列表。
pub fn resolve_pac_proxy(target_url: &str, pac_url: &str) -> Option<(String, Option<String>)> {
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
        // PAC 可能返回多条代理（`proxy1:80;proxy2:80`），取第一条交给 ureq
        let first = proxy?
            .split([';', ' '])
            .find(|part| !part.is_empty())?
            .to_string();
        let proxy = if first.contains("://") {
            first
        } else {
            format!("http://{first}")
        };

        Some((proxy, bypass))
    }
}

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
