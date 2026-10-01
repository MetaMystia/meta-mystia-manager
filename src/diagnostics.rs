//! 诊断包：把管理工具信息、系统信息、BepInEx 与 Unity 日志打包成 zip，
//! 只保存在本机，便于用户报障时提供；收集过程不额外写入任何日志文件。

mod archive;
mod collect;
mod info;

use crate::error::Result;
use crate::format::format_bytes;
use crate::platform::collect_system_report;
use crate::telemetry::report_event;
use crate::ui::{Ui, UiEvent};

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy)]
enum Body {
    Copy,
    /// 超限时只保留头尾，中间以标记代替
    TruncateText {
        head: u64,
        tail: u64,
    },
}

struct Entry {
    archive_name: String,
    source: PathBuf,
    body: Body,
}

struct Collector {
    entries: Vec<Entry>,
    descriptions: Vec<String>,
    notes: Vec<String>,
    packed: Vec<String>,
    crash_lines: Vec<String>,
    offset_secs: Option<i64>,
}

impl Collector {
    fn new(offset_secs: Option<i64>) -> Self {
        Self {
            entries: Vec::new(),
            descriptions: vec![
                "manager-info.txt（管理工具与系统信息、部署状态、已安装组件、最近操作）"
                    .to_string(),
            ],
            notes: Vec::new(),
            packed: Vec::new(),
            crash_lines: Vec::new(),
            offset_secs,
        }
    }

    fn add_file(&mut self, source: PathBuf, archive_name: String, label: &str, body: Body) {
        let size = fs::metadata(&source).ok().map(|meta| meta.len());
        let facts = size.map_or_else(
            || "无法读取大小".to_string(),
            |size| {
                format!(
                    "{}，修改于 {}",
                    format_bytes(size),
                    file_mtime(&source, self.offset_secs)
                )
            },
        );

        let (label, note) = collect::truncation_info(label, &archive_name, size, &body);
        self.descriptions.push(label);
        if let Some(note) = note {
            self.notes.push(note);
        }
        self.packed
            .push(format!("{archive_name} ← {}（{facts}）", source.display()));
        self.entries.push(Entry {
            archive_name,
            source,
            body,
        });
    }

    fn retain_readable(&mut self) {
        let mut readable = Vec::with_capacity(self.entries.len());

        for entry in self.entries.drain(..) {
            if fs::File::open(&entry.source).is_ok() {
                readable.push(entry);
                continue;
            }

            self.notes
                .push(format!("未打包（导出时无法读取）：{}", entry.archive_name));
            let prefix = format!("{} ← ", entry.archive_name);
            self.packed.retain(|line| !line.starts_with(&prefix));
        }

        self.entries = readable;
    }
}

/// 导出诊断包并返回 zip 路径；用户取消时返回空路径。
pub fn export(ui: &dyn Ui, game_root: &Path) -> Result<PathBuf> {
    let system = collect_system_report();
    let mut collector = collect::collect(game_root, system.utc_offset_seconds);

    if !ui
        .emit(UiEvent::DiagnosticsConfirmExport(&collector.descriptions))?
        .bool()?
    {
        ui.emit(UiEvent::Message("已取消导出诊断包"))?;
        return Ok(PathBuf::new());
    }

    collector.retain_readable();
    let info = info::build_manager_info(game_root, &collector, &system);
    let archive_path = archive::archive_path()?;

    if let Err(e) = archive::write_archive(&archive_path, &info, &collector.entries) {
        let _ = fs::remove_file(&archive_path);
        return Err(e);
    }

    report_event(
        "Diagnostics.Exported",
        archive_path.file_name().and_then(|name| name.to_str()),
    );

    Ok(archive_path)
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

fn file_mtime(path: &Path, offset_secs: Option<i64>) -> String {
    let modified = fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok());

    system_time_label(modified, offset_secs)
}

fn system_time_label(time: Option<SystemTime>, offset_secs: Option<i64>) -> String {
    time.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or_else(
            || "未知时间".to_string(),
            |elapsed| format_time(elapsed.as_secs(), offset_secs),
        )
}

/// epoch 秒 → `YYYYMMDD-HHMMSS`（UTC）。
fn format_timestamp(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let time = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);

    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

/// epoch 秒 → 当地 `YYYY-MM-DD HH:MM:SS`；时区未知时标注 UTC。
fn format_time(seconds: u64, offset_secs: Option<i64>) -> String {
    offset_secs.map_or_else(
        || format!("{} UTC", format_local_time(seconds, 0)),
        |offset| format_local_time(seconds, offset),
    )
}

fn format_local_time(seconds: u64, offset_secs: i64) -> String {
    let adjusted = i64::try_from(seconds)
        .unwrap_or(i64::MAX)
        .saturating_add(offset_secs);
    let days = adjusted.div_euclid(86_400);
    let time = adjusted.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);

    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

/// Howard Hinnant 的 `civil_from_days`（以 1970-01-01 为 0 天）。
fn civil_from_days(days: i64) -> (i64, u64, u64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };

    (
        if month <= 2 { year + 1 } else { year },
        u64::try_from(month).unwrap_or(1),
        u64::try_from(day).unwrap_or(1),
    )
}
