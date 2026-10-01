//! 安装/升级前的预检：磁盘剩余空间与关键站点可达性。

use crate::error::{ManagerError, Result};
use crate::format::format_bytes;
use crate::http::{build_agent_with_timeouts, host_key};
use crate::net::remote_config;
use crate::platform::free_space;
use crate::telemetry::report_event;
use crate::ui::{Ui, UiEvent};

use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};

// 预检阈值
/// 下载 + 解压需要的余量（BepInEx 压缩包约 34MB，解压后约 100MB+）。
const MIN_FREE_BYTES: u64 = 512 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// 安装/升级前的预检：磁盘空间与关键站点可达性。
pub fn run(ui: &dyn Ui, game_root: &Path, temp_dir: &Path, config_url: &str) -> Result<()> {
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
                        let started = Instant::now();
                        let ok = agent.get(&url).call().is_ok();
                        report_event(
                            "Preflight.Endpoint",
                            Some(&format!(
                                "{} {} {}ms",
                                host_key(&url),
                                if ok { "ok" } else { "failed" },
                                started.elapsed().as_millis()
                            )),
                        );

                        !ok
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
