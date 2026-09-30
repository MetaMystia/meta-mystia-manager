//! 安装/升级前的预检：磁盘剩余空间与关键站点可达性。

use crate::error::{ManagerError, Result};
use crate::metrics::report_event;
use crate::net::build_agent_with_timeouts;
use crate::remote_config;
use crate::ui::{Ui, UiEvent};

use std::{path::Path, thread, time::Duration};

#[cfg(windows)]
use std::{os::windows::ffi::OsStrExt, ptr::null_mut};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

/// 下载 + 解压需要的余量（BepInEx 压缩包约 34MB，解压后约 100MB+）
const MIN_FREE_BYTES: u64 = 512 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

pub fn check(ui: &dyn Ui, game_root: &Path, temp_dir: &Path, config_url: &str) -> Result<()> {
    check_free_space("游戏目录", game_root)?;
    if !temp_dir.starts_with(game_root) {
        check_free_space("临时目录", temp_dir)?;
    }

    let config_url = config_url.trim();
    if !config_url.is_empty() {
        check_endpoints(ui, config_url)?;
    }

    Ok(())
}

fn check_free_space(label: &str, path: &Path) -> Result<()> {
    let Some(free) = free_space(path) else {
        return Ok(());
    };

    if free >= MIN_FREE_BYTES {
        return Ok(());
    }

    report_event(
        "Preflight.DiskSpace.Low",
        Some(&format!("{label};free={free}")),
    );

    Err(ManagerError::ServiceError(format!(
        "磁盘空间不足：{label}（{}）剩余 {}，至少需要 {}，请清理后重试",
        path.display(),
        format_bytes(free),
        format_bytes(MIN_FREE_BYTES)
    )))
}

fn check_endpoints(ui: &dyn Ui, config_url: &str) -> Result<()> {
    let config = remote_config::get(ui, config_url)?;
    let mut endpoints: Vec<(&str, &str)> = Vec::new();
    if let Some(self_update) = &config.self_update {
        endpoints.push(("文件服务", self_update.entry_url.as_str()));
    }
    if let Some(sources) = &config.sources {
        endpoints.push(("BepInEx 主源", sources.bep_in_ex_primary.as_str()));
        endpoints.push(("GitHub", sources.github_release_api.as_str()));
    }
    let unreachable = thread::scope(|scope| {
        endpoints
            .iter()
            .filter(|(_, url)| !url.trim().is_empty())
            .map(|(name, url)| {
                let name = *name;
                let url = (*url).to_string();

                (
                    name,
                    scope.spawn(move || {
                        let agent = build_agent_with_timeouts(
                            &url,
                            Some(PROBE_TIMEOUT),
                            Some(PROBE_TIMEOUT),
                            Some(PROBE_TIMEOUT),
                        );
                        agent.get(&url).call().is_err()
                    }),
                )
            })
            .filter_map(|(name, handle)| handle.join().ok().filter(|failed| *failed).map(|_| name))
            .collect::<Vec<_>>()
    });

    if !unreachable.is_empty() {
        ui.emit(UiEvent::Warn(&format!(
            "以下站点当前不可达：{}。下载可能变慢或回退到备用源。",
            unreachable.join("、")
        )))?;
    }

    Ok(())
}

#[allow(clippy::cast_precision_loss, reason = "仅用于展示，精度要求低")]
pub fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    if bytes >= GIB {
        format!("{:.1} GB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.0} MB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.0} KB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(windows)]
fn free_space(path: &Path) -> Option<u64> {
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<u16>>();
    let mut free: u64 = 0;

    // SAFETY: 传入的是合法的以 NUL 结尾的路径，其它参数按文档允许为 null
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &raw mut free, null_mut(), null_mut()) };

    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
const fn free_space(_path: &Path) -> Option<u64> {
    None
}
