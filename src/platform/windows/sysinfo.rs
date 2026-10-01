//! Windows 系统信息采集：注册表与系统 API 只读。

use super::{
    read_system_proxy_settings,
    registry::{enum_subkeys, read_dword, read_string},
};
use crate::format::format_bytes;
use crate::platform::{SystemReport, is_elevated};

use std::{
    env,
    mem::{size_of, zeroed},
};
use windows_sys::Win32::System::{
    Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
    SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
};

// 系统信息标签
const CPU_LABEL: &str = "CPU：";

/// 采集 Windows 版本、硬件、代理与权限信息。
pub fn collect_system_report() -> SystemReport {
    let time_zone = read_time_zone();
    let utc_offset_seconds = time_zone.as_ref().map(|(_, offset)| *offset);
    let mut lines = Vec::new();

    if let Some(line) = windows_version_line() {
        lines.push(line);
    }
    lines.push(cpu_line().unwrap_or_else(|| format!("{CPU_LABEL}未知")));
    if let Some(line) = memory_line() {
        lines.push(line);
    }
    if let Some(line) = locale_line() {
        lines.push(line);
    }
    if let Some((name, offset)) = &time_zone {
        lines.push(format!("时区：{name}（{}）", format_utc_offset(*offset)));
    }
    lines.push(privilege_line());
    lines.push(proxy_line());
    lines.push(defender_line());
    lines.extend(display_adapters());

    SystemReport {
        lines,
        utc_offset_seconds,
    }
}

fn windows_version_line() -> Option<String> {
    const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

    let mut product = read_string(HKEY_LOCAL_MACHINE, CURRENT_VERSION, "ProductName")?;
    let build = read_string(HKEY_LOCAL_MACHINE, CURRENT_VERSION, "CurrentBuildNumber");
    let revision = read_dword(HKEY_LOCAL_MACHINE, CURRENT_VERSION, "UBR");
    let display_version = read_string(HKEY_LOCAL_MACHINE, CURRENT_VERSION, "DisplayVersion");

    // Windows 11 的 ProductName 仍写作 Windows 10，用 build 号纠正
    if build
        .as_deref()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .is_some_and(|build| build >= 22_000)
        && product.starts_with("Windows 10")
    {
        product = product.replacen("Windows 10", "Windows 11", 1);
    }

    let build = match (build, revision) {
        (Some(build), Some(revision)) => format!("build {build}.{revision}"),
        (Some(build), None) => format!("build {build}"),
        _ => "版本号未知".to_string(),
    };
    let display = display_version.map_or_else(String::new, |version| format!("，{version}"));

    Some(format!(
        "Windows：{product}{display}（{build}，{}）",
        env::consts::ARCH
    ))
}

fn cpu_line() -> Option<String> {
    read_string(
        HKEY_LOCAL_MACHINE,
        r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
        "ProcessorNameString",
    )
    .or_else(|| env::var("PROCESSOR_IDENTIFIER").ok())
    .map(|cpu| format!("{CPU_LABEL}{}", cpu.trim()))
}

fn memory_line() -> Option<String> {
    let mut status: MEMORYSTATUSEX = unsafe { zeroed() };
    status.dwLength = u32::try_from(size_of::<MEMORYSTATUSEX>()).unwrap_or(u32::MAX);

    if unsafe { GlobalMemoryStatusEx(&raw mut status) } == 0 {
        return None;
    }

    Some(format!(
        "内存：{}（可用 {}）",
        format_bytes(status.ullTotalPhys),
        format_bytes(status.ullAvailPhys)
    ))
}

fn locale_line() -> Option<String> {
    let locale = read_string(
        HKEY_CURRENT_USER,
        r"Control Panel\International",
        "LocaleName",
    )?;
    let country = read_string(
        HKEY_CURRENT_USER,
        r"Control Panel\International",
        "sCountry",
    );

    Some(country.map_or_else(
        || format!("区域/语言：{locale}"),
        |country| format!("区域/语言：{locale}（{country}）"),
    ))
}

fn read_time_zone() -> Option<(String, i64)> {
    const TIME_ZONE: &str = r"SYSTEM\CurrentControlSet\Control\TimeZoneInformation";

    let name = read_string(HKEY_LOCAL_MACHINE, TIME_ZONE, "TimeZoneKeyName")?;
    let bias = read_dword(HKEY_LOCAL_MACHINE, TIME_ZONE, "ActiveTimeBias")?;

    Some((name, -i64::from(bias.cast_signed()) * 60))
}

fn format_utc_offset(offset_seconds: i64) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let total_minutes = offset_seconds.unsigned_abs() / 60;

    format!(
        "UTC{sign}{:02}:{:02}",
        total_minutes / 60,
        total_minutes % 60
    )
}

fn privilege_line() -> String {
    const SYSTEM_POLICIES: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";

    let role = if is_elevated() {
        "管理员"
    } else {
        "标准用户"
    };
    let uac = match read_dword(HKEY_LOCAL_MACHINE, SYSTEM_POLICIES, "EnableLUA") {
        Some(0) => "已关闭",
        Some(_) => "已启用",
        None => "未知",
    };

    format!("权限：{role}（UAC {uac}）")
}

fn proxy_line() -> String {
    let settings = read_system_proxy_settings();

    if !settings.enabled && settings.server.is_none() && settings.auto_config_url.is_none() {
        return "代理：未启用（或未检测到）".to_string();
    }

    let mut parts = Vec::new();
    if let Some(server) = settings.server {
        parts.push(format!("服务器 {server}"));
    }
    if let Some(pac) = settings.auto_config_url {
        parts.push(format!("自动配置 {pac}"));
    }

    format!(
        "代理：{}{}",
        if settings.enabled {
            "已启用"
        } else {
            "未启用"
        },
        if parts.is_empty() {
            String::new()
        } else {
            format!("（{}）", parts.join("，"))
        }
    )
}

fn defender_line() -> String {
    let realtime = read_dword(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows Defender\Real-Time Protection",
        "DisableRealtimeMonitoring",
    );
    let policy = read_dword(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Policies\Microsoft\Windows Defender",
        "DisableAntiSpyware",
    );

    match (realtime, policy) {
        (Some(0), Some(0) | None) => "Windows Defender 实时保护：已开启".to_string(),
        (Some(_), _) | (_, Some(1)) => "Windows Defender 实时保护：已关闭".to_string(),
        (None, _) => "Windows Defender 实时保护：未知（无权限读取或未安装）".to_string(),
    }
}

fn display_adapters() -> Vec<String> {
    const DISPLAY_CLASS: &str =
        r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";

    let mut adapters = Vec::new();

    for key in enum_subkeys(HKEY_LOCAL_MACHINE, DISPLAY_CLASS) {
        let path = format!(r"{DISPLAY_CLASS}\{key}");
        let Some(desc) = read_string(HKEY_LOCAL_MACHINE, &path, "DriverDesc") else {
            continue;
        };

        let version = read_string(HKEY_LOCAL_MACHINE, &path, "DriverVersion");
        let date = read_string(HKEY_LOCAL_MACHINE, &path, "DriverDate");
        let detail = match (version, date) {
            (Some(version), Some(date)) => format!("（驱动 {version}，{date}）"),
            (Some(version), None) => format!("（驱动 {version}）"),
            _ => String::new(),
        };

        let line = format!("显示适配器：{desc}{detail}");
        if !adapters.contains(&line) {
            adapters.push(line);
        }
        if adapters.len() >= 4 {
            break;
        }
    }

    adapters
}
