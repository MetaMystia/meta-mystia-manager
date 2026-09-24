//! 安装/升级前的预检：磁盘剩余空间与关键站点可达性。
//!
//! 磁盘空间不足会让用户在下载/解压中途失败，这里提前拦下；
//! 站点不可达只提示不阻断（BepInEx 有上游主源与备用源两条路）。

use crate::error::{ManagerError, Result};
use crate::metrics::report_event;
use crate::net::build_agent;
use crate::ui::Ui;

use std::{path::Path, ptr::null_mut, time::Duration};

/// 下载 + 解压需要的余量（BepInEx 压缩包约 34MB，解压后约 100MB+）
const MIN_FREE_BYTES: u64 = 512 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

const PROBE_ENDPOINTS: &[(&str, &str)] = &[
    ("文件服务", "https://file.izakaya.cc/"),
    (
        "BepInEx 主源",
        "https://builds.bepinex.dev/projects/bepinex_be",
    ),
    (
        "GitHub",
        "https://api.github.com/repos/MetaMikuAI/MetaMystia/releases/latest",
    ),
];

/// 组合预检；任何一项失败都不阻断，只提示
pub fn check(ui: &dyn Ui, game_root: &Path, temp_dir: &Path) -> Result<()> {
    check_free_space("游戏目录", game_root)?;
    check_free_space("临时目录", temp_dir)?;
    check_endpoints(ui)
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

fn check_endpoints(ui: &dyn Ui) -> Result<()> {
    let agent = build_agent(Some(PROBE_TIMEOUT), Some(PROBE_TIMEOUT));

    let unreachable = std::thread::scope(|scope| {
        PROBE_ENDPOINTS
            .iter()
            .map(|(name, url)| {
                let agent = agent.clone();

                (*name, scope.spawn(move || agent.get(*url).call().is_err()))
            })
            .filter_map(|(name, handle)| handle.join().ok().filter(|failed| *failed).map(|_| name))
            .collect::<Vec<_>>()
    });

    if !unreachable.is_empty() {
        ui.warn(&format!(
            "以下站点当前不可达：{}。下载可能变慢或回退到备用源。",
            unreachable.join("、")
        ))?;
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
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

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
fn free_space(_path: &Path) -> Option<u64> {
    None
}
