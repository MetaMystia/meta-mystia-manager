//! 诊断包的文件收集。

use super::{Body, Collector, display_name, system_time_label};
use crate::config::{GAME_EXECUTABLE, TEMP_DIR_NAME};
use crate::format::format_bytes;
use crate::i18n::{t, tf, TextId};

use std::{
    cmp::Reverse,
    env, fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

// Unity 日志位置与文本截断
const MAX_UNITY_TEXT_LOG_BYTES: u64 = 20 * 1024 * 1024;
const UNITY_LOG_DIR: &str = "Epicomic/Touhou Mystia Izakaya";
const UNITY_LOG_NAMES: &[&str] = &["Player.log", "Player-prev.log", "crash.dmp"];
const UNITY_TEXT_LOG_HEAD_BYTES: u64 = 2 * 1024 * 1024;
const UNITY_TEXT_LOG_TAIL_BYTES: u64 = 18 * 1024 * 1024;
// BepInEx 日志文本截断
const BEPINEX_LOG_HEAD_BYTES: u64 = 4 * 1024 * 1024;
const BEPINEX_LOG_TAIL_BYTES: u64 = 28 * 1024 * 1024;

// BepInEx 配置收集
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_CONFIG_FILES: usize = 30;
const MAX_CONFIG_SCAN: usize = 2000;

// 崩溃报告收集
const MAX_LISTED_CRASH_DUMPS: usize = 10;
const MAX_LISTED_WER_REPORTS: usize = 10;
const MAX_WER_REPORT_BYTES: u64 = 256 * 1024;
const MAX_WER_REPORTS: usize = 3;

// 中断回滚记录
const MAX_ROLLBACK_JOURNAL_BYTES: u64 = 2 * 1024 * 1024;

/// 收集诊断包需要的文件并记录说明。
pub(super) fn collect(game_root: &Path, offset_secs: Option<i64>) -> Collector {
    let mut collector = Collector::new(offset_secs);

    collect_game_files(game_root, &mut collector);
    collect_output_logs(game_root, &mut collector);
    collect_unity_logs(&mut collector);
    collect_config_files(game_root, &mut collector);
    collect_rollback_journal(game_root, &mut collector);
    collect_crash_reports(&mut collector);

    collector
}

fn collect_game_files(game_root: &Path, collector: &mut Collector) {
    let bepinex_log = Body::TruncateText {
        head: BEPINEX_LOG_HEAD_BYTES,
        tail: BEPINEX_LOG_TAIL_BYTES,
    };

    for (relative, archive_name, label, body) in [
        (
            "BepInEx/LogOutput.log",
            "game/BepInEx-LogOutput.log",
            t(TextId::Diag_Item_BepInExLog),
            bepinex_log,
        ),
        (
            "BepInEx/ErrorLog.log",
            "game/BepInEx-ErrorLog.log",
            t(TextId::Diag_Item_BepInExErrorLog),
            bepinex_log,
        ),
        (
            "doorstop_config.ini",
            "game/doorstop_config.ini",
            "doorstop_config.ini",
            Body::Copy,
        ),
    ] {
        let source = game_root.join(relative);
        if source.is_file() {
            collector.add_file(source, archive_name.to_string(), label, body);
        }
    }
}

fn collect_output_logs(game_root: &Path, collector: &mut Collector) {
    let stem = Path::new(GAME_EXECUTABLE).file_stem().map_or_else(
        || "game".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let data_dir = format!("{stem}_Data");
    let body = Body::TruncateText {
        head: UNITY_TEXT_LOG_HEAD_BYTES,
        tail: UNITY_TEXT_LOG_TAIL_BYTES,
    };

    for (source, archive_name, label) in [
        (
            game_root.join("output_log.txt"),
            "game/output_log.txt".to_string(),
            t(TextId::Diag_Item_UnityOutputRoot).to_string(),
        ),
        (
            game_root.join(&data_dir).join("output_log.txt"),
            format!("game/{data_dir}-output_log.txt"),
            tf(TextId::Diag_Item_UnityOutputData, &[("dir", &data_dir)]),
        ),
    ] {
        if source.is_file() {
            collector.add_file(source, archive_name, &label, body);
        }
    }
}

fn collect_unity_logs(collector: &mut Collector) {
    let Some(dir) = unity_log_dir() else {
        return;
    };

    for name in UNITY_LOG_NAMES {
        let path = dir.join(name);
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }

        let archive_name = format!("unity/{name}");
        let label = tf(TextId::Diag_Item_UnityLog, &[("name", *name)]);

        if *name == "crash.dmp" {
            if meta.len() > MAX_UNITY_TEXT_LOG_BYTES {
                collector.notes.push(format!(
                    "未打包：{archive_name} 为 {}，超过 {}",
                    format_bytes(meta.len()),
                    format_bytes(MAX_UNITY_TEXT_LOG_BYTES)
                ));
                continue;
            }
            collector.add_file(path, archive_name, &label, Body::Copy);
        } else {
            collector.add_file(
                path,
                archive_name,
                &label,
                Body::TruncateText {
                    head: UNITY_TEXT_LOG_HEAD_BYTES,
                    tail: UNITY_TEXT_LOG_TAIL_BYTES,
                },
            );
        }
    }
}

/// Unity 日志目录（`%USERPROFILE%\AppData\LocalLow` 下）。
pub(super) fn unity_log_dir() -> Option<PathBuf> {
    let profile = env::var_os("USERPROFILE")?;

    Some(
        PathBuf::from(profile)
            .join("AppData/LocalLow")
            .join(UNITY_LOG_DIR),
    )
}

/// 根据文件大小与截断策略生成说明和备注。
pub(super) fn truncation_info(
    label: &str,
    archive_name: &str,
    size: Option<u64>,
    body: &Body,
) -> (String, Option<String>) {
    let Body::TruncateText { head, tail } = body else {
        return (label.to_string(), None);
    };
    let Some(size) = size.filter(|size| *size > head + tail) else {
        return (label.to_string(), None);
    };

    let label = tf(
        TextId::Diag_Item_Truncated,
        &[
            ("label", label),
            ("size", &format_bytes(size)),
            ("head", &format_bytes(*head)),
            ("tail", &format_bytes(*tail)),
        ],
    );
    let note = format!(
        "日志截断：{archive_name} 原始 {}，仅打包前 {} 与后 {}",
        format_bytes(size),
        format_bytes(*head),
        format_bytes(*tail)
    );

    (label, Some(note))
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum ConfigKind {
    Core,
    MetaMystia,
    Other,
}

struct ConfigFile {
    relative: String,
    source: PathBuf,
    kind: ConfigKind,
}

#[derive(Default)]
struct ConfigScan {
    files: Vec<ConfigFile>,
    packable: usize,
    skipped_large: usize,
    unscanned: usize,
}

fn collect_config_files(game_root: &Path, collector: &mut Collector) {
    let config_dir = game_root.join("BepInEx/config");
    let mut scan = ConfigScan::default();

    walk_configs(&config_dir, &config_dir, 0, &mut scan);
    scan.files
        .sort_by(|a, b| (a.kind, &a.relative).cmp(&(b.kind, &b.relative)));

    let kept = scan.files.len().min(MAX_CONFIG_FILES);
    for file in &scan.files[..kept] {
        collector.add_file(
            file.source.clone(),
            format!("game/BepInEx-config/{}", file.relative),
            &tf(TextId::Diag_Item_BepInExConfig, &[("name", &file.relative)]),
            Body::Copy,
        );
    }

    if let Some(note) = config_collection_note(&scan, kept) {
        collector.descriptions.push(note.clone());
        collector.notes.push(note);
    }
}

fn walk_configs(root: &Path, dir: &Path, depth: usize, scan: &mut ConfigScan) {
    if depth > 2 {
        return;
    }

    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };

    for entry in read_dir.flatten() {
        let path = entry.path();

        if path.is_dir() {
            walk_configs(root, &path, depth + 1, scan);
            continue;
        }

        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cfg"))
        {
            continue;
        }
        if entry
            .metadata()
            .map_or(true, |meta| meta.len() > MAX_CONFIG_BYTES)
        {
            scan.skipped_large += 1;
            continue;
        }

        scan.packable += 1;
        if scan.files.len() >= MAX_CONFIG_SCAN {
            scan.unscanned += 1;
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let file_name = relative.rsplit('/').next().unwrap_or(relative.as_str());
        let kind = if file_name.eq_ignore_ascii_case("BepInEx.cfg") {
            ConfigKind::Core
        } else if file_name.to_ascii_lowercase().starts_with("metamystia") {
            ConfigKind::MetaMystia
        } else {
            ConfigKind::Other
        };

        scan.files.push(ConfigFile {
            relative,
            source: path,
            kind,
        });
    }
}

fn config_collection_note(scan: &ConfigScan, kept: usize) -> Option<String> {
    let total = scan.packable + scan.skipped_large;
    let truncated = scan.files.len().saturating_sub(kept);
    let mut reasons = Vec::new();

    if truncated > 0 {
        reasons.push(tf(
            TextId::Diag_Config_OverCount,
            &[
                ("count", &truncated.to_string()),
                ("max", &MAX_CONFIG_FILES.to_string()),
            ],
        ));
    }
    if scan.skipped_large > 0 {
        reasons.push(tf(
            TextId::Diag_Config_TooLarge,
            &[
                ("count", &scan.skipped_large.to_string()),
                ("size", &format_bytes(MAX_CONFIG_BYTES)),
            ],
        ));
    }
    if scan.unscanned > 0 {
        reasons.push(tf(
            TextId::Diag_Config_OverScan,
            &[
                ("count", &scan.unscanned.to_string()),
                ("max", &MAX_CONFIG_SCAN.to_string()),
            ],
        ));
    }

    if reasons.is_empty() {
        return None;
    }

    Some(tf(
        TextId::Diag_Config_Summary,
        &[
            ("total", &total.to_string()),
            ("kept", &kept.to_string()),
            ("reasons", &reasons.join(t(TextId::ListSeparator))),
        ],
    ))
}

fn collect_rollback_journal(game_root: &Path, collector: &mut Collector) {
    let journal = game_root.join(TEMP_DIR_NAME).join("rollback/journal.json");
    if !journal.is_file() {
        return;
    }

    let size = fs::metadata(&journal).map_or(0, |meta| meta.len());
    if size > MAX_ROLLBACK_JOURNAL_BYTES {
        collector.notes.push(format!(
            "未打包：回滚记录 journal.json 为 {}，超过 {}",
            format_bytes(size),
            format_bytes(MAX_ROLLBACK_JOURNAL_BYTES)
        ));
        return;
    }

    collector.add_file(
        journal,
        "manager/rollback-journal.json".to_string(),
        t(TextId::Diag_Item_RollbackJournal),
        Body::Copy,
    );
}

fn collect_crash_reports(collector: &mut Collector) {
    let Some(local) = env::var_os("LOCALAPPDATA") else {
        return;
    };
    let local = PathBuf::from(local);

    collect_crash_dumps(&local, collector);
    collect_wer_reports(&local, collector);
}

fn collect_crash_dumps(local: &Path, collector: &mut Collector) {
    let dir = local.join("CrashDumps");
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };

    let mut dumps: Vec<(PathBuf, u64, Option<SystemTime>)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let name = path.file_name()?.to_str()?.to_ascii_lowercase();
            if !name.starts_with("touhou mystia izakaya") {
                return None;
            }
            let meta = entry.metadata().ok()?;

            Some((path, meta.len(), meta.modified().ok()))
        })
        .collect();

    if dumps.is_empty() {
        return;
    }

    dumps.sort_by_key(|dump| Reverse(dump.2));
    collector.crash_lines.push(format!(
        "崩溃转储（体积大，未打包；需要时请单独提供）：{}",
        dir.display()
    ));
    for (path, size, modified) in dumps.iter().take(MAX_LISTED_CRASH_DUMPS) {
        collector.crash_lines.push(format!(
            "  {}（{}，{}）",
            display_name(path),
            format_bytes(*size),
            system_time_label(*modified, collector.offset_secs)
        ));
    }
    if dumps.len() > MAX_LISTED_CRASH_DUMPS {
        collector.crash_lines.push(format!(
            "  …另有 {} 个",
            dumps.len() - MAX_LISTED_CRASH_DUMPS
        ));
    }
}

fn collect_wer_reports(local: &Path, collector: &mut Collector) {
    let dir = local.join("Microsoft/Windows/WER/ReportArchive");
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };

    let mut reports: Vec<(PathBuf, Option<SystemTime>)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let name = path.file_name()?.to_str()?.to_ascii_lowercase();
            if !name.contains("touhou") {
                return None;
            }

            Some((
                path,
                entry.metadata().ok().and_then(|meta| meta.modified().ok()),
            ))
        })
        .collect();

    if reports.is_empty() {
        return;
    }

    reports.sort_by_key(|report| Reverse(report.1));
    collector
        .crash_lines
        .push(format!("Windows 错误报告（WER）：{}", dir.display()));
    for (path, modified) in reports.iter().take(MAX_LISTED_WER_REPORTS) {
        collector.crash_lines.push(format!(
            "  {}（{}）",
            display_name(path),
            system_time_label(*modified, collector.offset_secs)
        ));
    }
    if reports.len() > MAX_LISTED_WER_REPORTS {
        collector.crash_lines.push(format!(
            "  …另有 {} 份",
            reports.len() - MAX_LISTED_WER_REPORTS
        ));
    }
    if reports.len() > MAX_WER_REPORTS {
        collector.crash_lines.push(format!(
            "  仅打包最近 {MAX_WER_REPORTS} 份 Report.wer，其余只列出"
        ));
    }

    for (path, _) in reports.iter().take(MAX_WER_REPORTS) {
        let report = path.join("Report.wer");
        let Ok(meta) = fs::metadata(&report) else {
            continue;
        };

        let archive_name = format!("wer/{}/Report.wer", display_name(path));
        if meta.len() > MAX_WER_REPORT_BYTES {
            collector.notes.push(format!(
                "未打包：{archive_name} 为 {}，超过 {}",
                format_bytes(meta.len()),
                format_bytes(MAX_WER_REPORT_BYTES)
            ));
            continue;
        }

        collector.add_file(
            report,
            archive_name,
            &tf(TextId::Diag_Item_WerReport, &[("name", &display_name(path))]),
            Body::Copy,
        );
    }
}
