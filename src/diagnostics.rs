//! 诊断包：把管理器信息、最近操作、BepInEx 与 Unity 日志打包成 zip，
//! 生成在管理器所在目录，只保存在本机，便于用户报障时提供。

use crate::config::BEPINEX_VERSION_FILE;
use crate::error::{ManagerError, Result};
use crate::file_ops::glob_matches_by_filename;
use crate::metrics;
use crate::model::VersionInfo;
use crate::preflight::format_bytes;
use crate::ui::Ui;

use std::{
    env,
    fmt::Write as _,
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

/// Unity 日志最多收集几个文件 / 单文件上限
const MAX_UNITY_LOGS: usize = 5;
const MAX_UNITY_LOG_BYTES: u64 = 20 * 1024 * 1024;
const UNITY_LOG_NAMES: &[&str] = &["Player.log", "Player-prev.log", "crash.dmp"];
/// `BepInEx/config` 下最多收集多少个 `.cfg` / 单个文件上限
const MAX_CONFIG_FILES: usize = 30;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

struct Entry {
    archive_name: String,
    description: String,
    source: PathBuf,
}

/// 导出诊断包；返回生成的文件路径
pub fn export(ui: &dyn Ui, game_root: &Path) -> Result<PathBuf> {
    let mut entries = Vec::new();
    let mut descriptions = vec![
        "manager-info.txt（管理器版本、系统信息、已安装组件与插件列表、最近操作）".to_string(),
    ];

    for (relative, archive_name, description) in [
        (
            "BepInEx/LogOutput.log",
            "game/BepInEx-LogOutput.log",
            "BepInEx/LogOutput.log（Mod 日志，最关键）",
        ),
        (
            "doorstop_config.ini",
            "game/doorstop_config.ini",
            "doorstop_config.ini",
        ),
    ] {
        let source = game_root.join(relative);
        if source.is_file() {
            entries.push(Entry {
                archive_name: archive_name.to_string(),
                description: description.to_string(),
                source,
            });
            descriptions.push(description.to_string());
        }
    }

    for entry in collect_unity_logs() {
        descriptions.push(entry.description.clone());
        entries.push(entry);
    }

    for entry in collect_config_files(game_root) {
        descriptions.push(entry.description.clone());
        entries.push(entry);
    }

    if !ui.diagnostics_confirm_export(&descriptions)? {
        ui.message("已取消导出诊断包")?;
        return Ok(PathBuf::new());
    }

    let info = build_manager_info(game_root);
    let archive_path = archive_path()?;
    write_archive(&archive_path, &info, &entries)?;

    Ok(archive_path)
}

/// 收集 `BepInEx/config` 下的所有 `.cfg`（含插件自己生成的配置）
fn collect_config_files(game_root: &Path) -> Vec<Entry> {
    let config_dir = game_root.join("BepInEx/config");
    let mut found = Vec::new();

    walk_configs(&config_dir, &config_dir, 0, &mut found);

    found
}

fn walk_configs(root: &Path, dir: &Path, depth: usize, found: &mut Vec<Entry>) {
    if depth > 2 || found.len() >= MAX_CONFIG_FILES {
        return;
    }

    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };

    for entry in read_dir.flatten() {
        if found.len() >= MAX_CONFIG_FILES {
            return;
        }

        let path = entry.path();

        if path.is_dir() {
            walk_configs(root, &path, depth + 1, found);
            continue;
        }

        let is_config = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cfg"));
        if !is_config
            || entry
                .metadata()
                .map_or(true, |meta| meta.len() > MAX_CONFIG_BYTES)
        {
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        found.push(Entry {
            archive_name: format!("game/BepInEx-config/{relative}"),
            description: format!("BepInEx 配置：{relative}"),
            source: path,
        });
    }
}

/// 收集 `%USERPROFILE%\AppData\LocalLow` 下的 Unity 日志（深度 ≤ 3）
fn collect_unity_logs() -> Vec<Entry> {
    let Some(local_low) = env::var_os("USERPROFILE")
        .map(|profile| PathBuf::from(profile).join("AppData").join("LocalLow"))
    else {
        return Vec::new();
    };

    let mut found = Vec::new();
    walk_logs(&local_low, &local_low, 0, &mut found);

    found
}

fn walk_logs(root: &Path, dir: &Path, depth: usize, found: &mut Vec<Entry>) {
    if depth > 3 || found.len() >= MAX_UNITY_LOGS {
        return;
    }

    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };

    for entry in read_dir.flatten() {
        if found.len() >= MAX_UNITY_LOGS {
            return;
        }

        let path = entry.path();

        if path.is_dir() {
            walk_logs(root, &path, depth + 1, found);
            continue;
        }

        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !UNITY_LOG_NAMES.contains(&name) {
            continue;
        }
        if entry
            .metadata()
            .map_or(true, |meta| meta.len() > MAX_UNITY_LOG_BYTES)
        {
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        found.push(Entry {
            archive_name: format!("unity/{relative}"),
            description: format!("Unity 日志：{relative}"),
            source: path,
        });
    }
}

fn build_manager_info(game_root: &Path) -> String {
    let mut info = String::new();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());

    let _ = write!(
        info,
        "管理器版本：{}\n导出时间：{} UTC（epoch {now}）\n系统：{} {}\nCPU：{}\n",
        env!("CARGO_PKG_VERSION"),
        format_timestamp(now),
        env::consts::OS,
        env::consts::ARCH,
        env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "未知".to_string()),
    );
    let _ = writeln!(info, "游戏目录：{}", game_root.display());

    let version_marker = game_root.join(BEPINEX_VERSION_FILE);
    let _ = writeln!(
        info,
        "BepInEx 构建号：{}",
        fs::read_to_string(&version_marker).map_or_else(
            |_| "未安装或缺失".to_string(),
            |value| value.trim().to_string()
        )
    );

    let plugins = file_names(
        &game_root.join("BepInEx/plugins"),
        VersionInfo::is_metamystia_filename,
    );
    let resourceex = file_names(
        &game_root.join("ResourceEx"),
        VersionInfo::is_resourceex_filename,
    );
    let _ = writeln!(
        info,
        "MetaMystia DLL：{}\nResourceExample ZIP：{}",
        join_or_none(&plugins),
        join_or_none(&resourceex),
    );

    info.push_str("\n已安装插件（BepInEx/plugins）：\n");
    let installed = list_plugins(game_root);
    if installed.is_empty() {
        info.push_str("  未找到插件文件\n");
    } else {
        for line in &installed {
            let _ = writeln!(info, "  {line}");
        }
    }

    info.push_str("\n最近操作：\n");
    for event in metrics::recent_events() {
        let _ = writeln!(info, "  {event}");
    }

    info
}

fn file_names(dir: &Path, matcher: fn(&str) -> bool) -> Vec<String> {
    glob_matches_by_filename(&dir.join("*"), matcher)
        .iter()
        .filter_map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect()
}

fn join_or_none(names: &[String]) -> String {
    if names.is_empty() {
        "未安装".to_string()
    } else {
        names.join("、")
    }
}

/// 列出 `BepInEx/plugins` 下的所有文件（含第三方插件），保留相对路径
fn list_plugins(game_root: &Path) -> Vec<String> {
    let root = game_root.join("BepInEx/plugins");
    let mut found = Vec::new();

    walk_plugin_files(&root, &root, 0, &mut found);
    found.sort();

    found
}

fn walk_plugin_files(root: &Path, dir: &Path, depth: usize, found: &mut Vec<String>) {
    if depth > 4 {
        return;
    }

    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };

    for entry in read_dir.flatten() {
        let path = entry.path();

        if path.is_dir() {
            walk_plugin_files(root, &path, depth + 1, found);
            continue;
        }

        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or_else(
                || "未知".to_string(),
                |elapsed| format_timestamp(elapsed.as_secs()),
            );

        found.push(format!(
            "{relative}（{}，{modified}）",
            format_bytes(meta.len())
        ));
    }
}

/// 管理器所在目录；不可写时回落到临时目录
fn archive_path() -> Result<PathBuf> {
    let stamp = format_timestamp(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs()),
    );
    let filename = format!("meta-mystia-manager-diagnostics-{stamp}.zip");

    let dirs = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .into_iter()
        .chain([env::temp_dir()]);

    for dir in dirs {
        let candidate = dir.join(&filename);
        if fs::File::create(&candidate).is_ok() {
            return Ok(candidate);
        }
    }

    Err(ManagerError::PermissionDenied(
        "无法在管理器目录或临时目录创建诊断包".to_string(),
    ))
}

fn write_archive(path: &Path, info: &str, entries: &[Entry]) -> Result<()> {
    let file = fs::File::create(path).map_err(|e| {
        ManagerError::from(io::Error::new(
            e.kind(),
            format!("创建诊断包 {} 失败：{}", path.display(), e),
        ))
    })?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    zip.start_file("manager-info.txt", options)
        .map_err(|e| ManagerError::Other(format!("写入诊断包失败：{e}")))?;
    zip.write_all(info.as_bytes())
        .map_err(|e| ManagerError::Other(format!("写入诊断包失败：{e}")))?;

    for entry in entries {
        let Ok(mut source) = fs::File::open(&entry.source) else {
            continue;
        };

        zip.start_file(&entry.archive_name, options)
            .map_err(|e| ManagerError::Other(format!("写入诊断包失败：{e}")))?;
        io::copy(&mut source, &mut zip)
            .map_err(|e| ManagerError::Other(format!("写入诊断包失败：{e}")))?;
    }

    zip.finish()
        .map_err(|e| ManagerError::Other(format!("完成诊断包失败：{e}")))?;

    Ok(())
}

/// epoch 秒 → `YYYYMMDD-HHMMSS`（UTC）
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

/// Howard Hinnant 的 `civil_from_days`（以 1970-01-01 为 0 天）
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
