//! `manager-info.txt` 的内容组装与本地清单扫描。

use super::collect::unity_log_dir;
use super::{Collector, display_name, file_mtime, format_time};
use crate::config::{GAME_EXECUTABLE, GAME_STEAM_APP_ID, TEMP_DIR_NAME};
use crate::format::format_bytes;
use crate::fs::file_ops::glob_matches_by_filename;
use crate::net::downloader::cached_version_info;
use crate::net::sso;
use crate::platform::{
    SystemReport, file_product_version, free_space, is_game_running, sha256_file,
};
use crate::telemetry::{MAX_RECENT_EVENTS, recent_events, user_id};
use crate::version::{VersionInfo, read_bepinex_version};

use std::{
    env,
    fmt::Write as FmtWrite,
    fs,
    io::Read,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

// 清单、哈希与残留列出上限
const MAX_HASH_BYTES: u64 = 256 * 1024 * 1024;
const MAX_LISTED_FILES: usize = 300;
const MAX_LISTED_REMNANTS: usize = 40;
const MAX_WALK_FILES: usize = 2000;

/// 组装 `manager-info.txt` 的文本内容。
#[allow(clippy::too_many_lines, reason = "诊断信息按固定顺序逐段拼接")]
pub(super) fn build_manager_info(
    game_root: &Path,
    collector: &Collector,
    system: &SystemReport,
) -> String {
    let offset = collector.offset_secs;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let mut info = String::new();

    let _ = writeln!(
        info,
        "管理工具版本：{} - {}",
        env!("CARGO_PKG_VERSION"),
        user_id()
    );
    let _ = writeln!(
        info,
        "导出时间：{}（epoch {now}）",
        format_time(now, offset)
    );
    let _ = writeln!(info, "系统：{} {}", env::consts::OS, env::consts::ARCH);
    for line in &system.lines {
        let _ = writeln!(info, "{line}");
    }
    if offset.is_none() {
        let _ = writeln!(info, "时区：未知（以下时间按 UTC 显示）");
    }

    let game_path = game_root.display().to_string();
    let ascii = if game_path.is_ascii() {
        "纯 ASCII 路径"
    } else {
        "含非 ASCII 字符"
    };

    info.push_str("\n【游戏与 Steam】\n");
    let _ = writeln!(info, "游戏目录：{game_path}（{ascii}）");
    let _ = writeln!(info, "{}", game_running_line());
    for (label, path) in [
        ("游戏目录", game_root),
        ("临时目录", env::temp_dir().as_path()),
    ] {
        if let Some(free) = free_space(path) {
            let _ = writeln!(
                info,
                "{label}（{}）剩余空间：{}",
                path.display(),
                format_bytes(free)
            );
        }
    }
    for line in game_version_lines(game_root, offset) {
        let _ = writeln!(info, "{line}");
    }

    info.push_str("\n【部署状态】\n");
    let _ = writeln!(
        info,
        "BepInEx 构建号：{}",
        read_bepinex_version(game_root).unwrap_or_else(|| "未安装或无法识别".to_string())
    );
    for line in deployment_lines(game_root) {
        let _ = writeln!(info, "{line}");
    }
    info.push_str("BepInEx/core：\n");
    for line in inventory_lines(&game_root.join("BepInEx/core"), 1, offset) {
        let _ = writeln!(info, "{line}");
    }
    info.push_str("BepInEx/patchers：\n");
    for line in inventory_lines(&game_root.join("BepInEx/patchers"), 3, offset) {
        let _ = writeln!(info, "{line}");
    }

    info.push_str("\n【插件与资源】\n");
    info.push_str("BepInEx/plugins：\n");
    for line in inventory_lines(&game_root.join("BepInEx/plugins"), 4, offset) {
        let _ = writeln!(info, "{line}");
    }
    info.push_str("ResourceEx：\n");
    for line in inventory_lines(&game_root.join("ResourceEx"), 3, offset) {
        let _ = writeln!(info, "{line}");
    }
    info.push_str("关键文件校验：\n");
    let hashes = hash_lines(game_root);
    if hashes.is_empty() {
        info.push_str("  未找到可校验的 MetaMystia / winhttp.dll 文件\n");
    }
    for line in hashes {
        let _ = writeln!(info, "{line}");
    }

    info.push_str("\n【残留与临时文件】\n");
    for line in remnant_lines(game_root, offset) {
        let _ = writeln!(info, "{line}");
    }
    for line in temp_dir_lines(game_root, offset) {
        let _ = writeln!(info, "{line}");
    }

    info.push_str("\n【崩溃报告】\n");
    if collector.crash_lines.is_empty() {
        info.push_str("  未发现最近的崩溃转储或 WER 报告\n");
    } else {
        for line in &collector.crash_lines {
            let _ = writeln!(info, "{line}");
        }
    }

    info.push_str("\n【本次会话】\n");
    for line in remote_version_lines() {
        let _ = writeln!(info, "{line}");
    }
    let _ = writeln!(info, "{}", account_line());

    info.push_str("\n【诊断包内容】\n");
    if collector.notes.is_empty() {
        info.push_str("  没有需要说明的截断或跳过\n");
    } else {
        for note in &collector.notes {
            let _ = writeln!(info, "  {note}");
        }
    }
    let total_bytes: u64 = collector
        .entries
        .iter()
        .filter_map(|entry| fs::metadata(&entry.source).ok())
        .map(|meta| meta.len())
        .sum();
    let _ = writeln!(
        info,
        "  打包文件：{} 个，原始共 {}",
        collector.entries.len(),
        format_bytes(total_bytes)
    );
    for line in &collector.packed {
        let _ = writeln!(info, "    {line}");
    }

    info.push_str("\n【最近操作】\n");
    let _ = writeln!(
        info,
        "  仅本次运行会话（最多 {MAX_RECENT_EVENTS} 条，工具重启后不保留）："
    );
    let events = recent_events();
    if events.is_empty() {
        info.push_str("  （本次会话没有记录到操作）\n");
    }
    for event in events {
        let time = if event.at_epoch_secs == 0 {
            "时间未知".to_string()
        } else {
            format_time(event.at_epoch_secs, offset)
        };
        let _ = writeln!(info, "  [{time} / +{}s] {}", event.elapsed_secs, event.line);
    }

    info
}

fn game_running_line() -> String {
    match is_game_running() {
        Ok(true) => "游戏进程：运行中（日志可能仍在写入）".to_string(),
        Ok(false) => "游戏进程：未运行".to_string(),
        Err(e) => format!("游戏进程：检测失败（{e}）"),
    }
}

fn game_version_lines(game_root: &Path, offset: Option<i64>) -> Vec<String> {
    vec![
        format!(
            "  {GAME_EXECUTABLE}：{}",
            product_version_label(&game_root.join(GAME_EXECUTABLE))
        ),
        format!(
            "  GameAssembly.dll：{}",
            product_version_label(&game_root.join("GameAssembly.dll"))
        ),
        format!(
            "  Unity 版本：{}",
            unity_version().unwrap_or_else(|| "未知".to_string())
        ),
    ]
    .into_iter()
    .chain(steam_lines(game_root, offset))
    .collect()
}

fn product_version_label(path: &Path) -> String {
    if !path.is_file() {
        return "缺失".to_string();
    }

    file_product_version(path).map_or_else(
        || "版本未知".to_string(),
        |version| format!("版本 {version}"),
    )
}

fn unity_version() -> Option<String> {
    let path = unity_log_dir()?.join("Player.log");
    let mut file = fs::File::open(path).ok()?;
    let mut buffer = vec![0u8; 64 * 1024];
    let read = file.read(&mut buffer).ok()?;
    let text = String::from_utf8_lossy(&buffer[..read]);

    for line in text.lines().take(10) {
        if let Some((_, version)) = line.split_once("Initialize engine version:") {
            let version = version.trim();
            if !version.is_empty() {
                return Some(version.to_string());
            }
        }
        if let Some(rest) = line.strip_prefix("Unity ")
            && rest.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            return Some(rest.trim().to_string());
        }
    }

    None
}

fn steam_lines(game_root: &Path, offset: Option<i64>) -> Vec<String> {
    let Some(steamapps) = game_root.parent().and_then(Path::parent) else {
        return vec!["  Steam：未识别（游戏不在 steamapps/common 下）".to_string()];
    };
    if !steamapps
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("steamapps"))
    {
        return vec!["  Steam：未识别（游戏不在 steamapps/common 下）".to_string()];
    }

    let manifest = steamapps.join(format!("appmanifest_{GAME_STEAM_APP_ID}.acf"));
    let Ok(text) = fs::read_to_string(&manifest) else {
        return vec![format!("  Steam：未找到 {}", manifest.display())];
    };

    let mut lines = Vec::new();
    for key in ["buildid", "LastUpdated", "StateFlags"] {
        let Some(value) = acf_value(&text, key) else {
            continue;
        };
        if key == "LastUpdated"
            && let Ok(secs) = value.parse::<u64>()
        {
            lines.push(format!("  {key}={secs}（{}）", format_time(secs, offset)));
            continue;
        }

        lines.push(format!("  {key}={value}"));
    }

    lines
}

fn acf_value(text: &str, key: &str) -> Option<String> {
    let mut tokens = Vec::new();
    let mut chars = text.chars();

    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }

        let mut value = String::new();
        for c in chars.by_ref() {
            if c == '"' {
                break;
            }
            value.push(c);
        }
        tokens.push(value);
    }

    tokens
        .iter()
        .position(|token| token == key)
        .and_then(|index| tokens.get(index + 1).cloned())
}

fn deployment_lines(game_root: &Path) -> Vec<String> {
    let mut lines = Vec::new();

    let winhttp = game_root.join("winhttp.dll");
    lines.push(if winhttp.is_file() {
        file_product_version(&winhttp).map_or_else(
            || "  winhttp.dll：存在（版本未知）".to_string(),
            |version| format!("  winhttp.dll：存在（版本 {version}）"),
        )
    } else {
        "  winhttp.dll：缺失（Doorstop 未部署或被安全软件删除）".to_string()
    });

    let doorstop_version = game_root.join(".doorstop_version");
    if doorstop_version.is_file() {
        let content = fs::read_to_string(&doorstop_version)
            .map_or_else(|_| "无法读取".to_string(), |text| text.trim().to_string());
        lines.push(format!(
            "  .doorstop_version：{}",
            if content.is_empty() {
                "（空）".to_string()
            } else {
                content
            }
        ));
    } else {
        lines.push("  .doorstop_version：缺失".to_string());
    }

    for name in ["doorstop_config.ini", "MinHook.x64.dll"] {
        lines.push(format!("  {name}：{}", exists_label(&game_root.join(name))));
    }

    let dotnet = game_root.join("dotnet");
    if dotnet.is_dir() {
        let mut files = Vec::new();
        let capped = walk_files(&dotnet, &dotnet, 0, 2, &mut files);
        lines.push(if capped {
            format!("  dotnet/：存在（超过 {MAX_WALK_FILES} 个文件，未完整统计）")
        } else {
            format!("  dotnet/：存在（{} 个文件）", files.len())
        });
    } else {
        lines.push("  dotnet/：不存在".to_string());
    }

    lines
}

fn exists_label(path: &Path) -> &'static str {
    if path.is_file() { "存在" } else { "缺失" }
}

fn inventory_lines(root: &Path, max_depth: usize, offset: Option<i64>) -> Vec<String> {
    let mut files = Vec::new();
    let capped = walk_files(root, root, 0, max_depth, &mut files);
    files.sort();

    if files.is_empty() {
        return vec!["  （无）".to_string()];
    }

    let total = files.len();
    let mut lines: Vec<String> = files
        .iter()
        .take(MAX_LISTED_FILES)
        .map(|relative| listing_line(relative, &root.join(relative), offset))
        .collect();

    if total > MAX_LISTED_FILES {
        lines.push(format!("  …另有 {} 个文件", total - MAX_LISTED_FILES));
    }
    if capped {
        lines.push(format!("  …文件数量过多，仅扫描前 {MAX_WALK_FILES} 个"));
    }

    lines
}

fn listing_line(relative: &str, path: &Path, offset: Option<i64>) -> String {
    let size =
        fs::metadata(path).map_or_else(|_| "大小未知".to_string(), |meta| format_bytes(meta.len()));
    let version = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
        .then(|| file_product_version(path))
        .flatten()
        .map_or_else(String::new, |version| format!("，版本 {version}"));

    format!(
        "  {relative}（{size}{version}，{}）",
        file_mtime(path, offset)
    )
}

fn hash_lines(game_root: &Path) -> Vec<String> {
    let mut targets = Vec::new();

    for path in glob_matches_by_filename(
        &game_root.join("BepInEx/plugins/*"),
        VersionInfo::is_metamystia_filename,
    ) {
        targets.push(("MetaMystia DLL", path));
    }
    for path in glob_matches_by_filename(
        &game_root.join("ResourceEx/*"),
        VersionInfo::is_resourceex_filename,
    ) {
        targets.push(("ResourceEx ZIP", path));
    }
    targets.push(("winhttp.dll", game_root.join("winhttp.dll")));
    targets.push((
        "BepInEx.Core.dll",
        game_root.join("BepInEx/core/BepInEx.Core.dll"),
    ));

    let mut lines = Vec::new();
    for (label, path) in targets {
        if !path.is_file() {
            continue;
        }

        let size = fs::metadata(&path).map_or(0, |meta| meta.len());
        let name = display_name(&path);

        if size > MAX_HASH_BYTES {
            lines.push(format!(
                "  {label} {name}（{}）：超过 {}，未计算哈希",
                format_bytes(size),
                format_bytes(MAX_HASH_BYTES)
            ));
            continue;
        }

        match file_sha256(&path) {
            Some(hash) => lines.push(format!(
                "  {label} {name}（{}）：SHA-256 {hash}",
                format_bytes(size)
            )),
            None => lines.push(format!("  {label} {name}：读取失败，未计算哈希")),
        }
    }

    lines
}

fn file_sha256(path: &Path) -> Option<String> {
    let digest = sha256_file(path).ok()?;

    let mut hash = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hash, "{byte:02x}");
    }

    Some(hash)
}

#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "文件名已统一转小写后再比较"
)]
fn remnant_lines(game_root: &Path, offset: Option<i64>) -> Vec<String> {
    let mut lines = Vec::new();

    for (dir, label) in [
        (game_root.join("BepInEx/plugins"), "BepInEx/plugins"),
        (game_root.join("ResourceEx"), "ResourceEx"),
    ] {
        let found = glob_matches_by_filename(&dir.join("*"), |name| {
            let lower = name.to_ascii_lowercase();
            lower.ends_with(".part") || lower.contains(".old") || lower.ends_with(".tmp")
        });

        for path in found {
            lines.push(format!(
                "  {label}/{}（{}，{}）",
                display_name(&path),
                file_size(&path),
                file_mtime(&path, offset)
            ));
        }
    }

    if lines.is_empty() {
        lines.push("  未发现 .part / .old / .tmp 残留".to_string());
    }

    lines
}

fn temp_dir_lines(game_root: &Path, offset: Option<i64>) -> Vec<String> {
    let temp_dir = game_root.join(TEMP_DIR_NAME);
    if !temp_dir.is_dir() {
        return vec!["  未完成的临时目录：无残留".to_string()];
    }

    let mut files = Vec::new();
    let capped = walk_files(&temp_dir, &temp_dir, 0, 3, &mut files);
    files.sort();

    let mut lines = vec![format!(
        "  未完成的临时目录：{}（{} 个文件{}）",
        temp_dir.display(),
        files.len(),
        if capped { "，未完整统计" } else { "" }
    )];
    for relative in files.iter().take(MAX_LISTED_REMNANTS) {
        let path = temp_dir.join(relative);
        lines.push(format!(
            "    {relative}（{}，{}）",
            file_size(&path),
            file_mtime(&path, offset)
        ));
    }
    if files.len() > MAX_LISTED_REMNANTS {
        lines.push(format!(
            "    …另有 {} 个文件",
            files.len() - MAX_LISTED_REMNANTS
        ));
    }
    if capped {
        lines.push(format!("    …文件数量过多，仅扫描前 {MAX_WALK_FILES} 个"));
    }

    lines
}

fn remote_version_lines() -> Vec<String> {
    let Some(version_info) = cached_version_info() else {
        return vec!["  远端版本：本次会话未获取（诊断导出不联网）".to_string()];
    };

    vec![
        format!(
            "  远端版本：管理工具 {}；BepInEx {}；DLL {}；ResourceEx {}",
            version_info.manager_version().unwrap_or("未知"),
            version_info.bep_in_ex.as_deref().unwrap_or("未知"),
            summarize(&version_info.dlls),
            summarize(&version_info.zips),
        ),
        format!("  配置地址：{}", version_info.config_url),
    ]
}

fn summarize(names: &[String]) -> String {
    match names.split_first() {
        Some((first, [])) => first.clone(),
        Some((first, rest)) => format!("{first} 等 {} 个", rest.len() + 1),
        None => "无".to_string(),
    }
}

fn account_line() -> String {
    sso::current_account().map_or_else(
        || "登录状态：未登录（或本次会话未登录）".to_string(),
        |account| format!("登录状态：已登录（账号 ID {}）", account.user_id),
    )
}

fn walk_files(
    root: &Path,
    dir: &Path,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<String>,
) -> bool {
    if out.len() >= MAX_WALK_FILES {
        return true;
    }
    if depth > max_depth {
        return false;
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };

    for entry in entries.flatten() {
        if out.len() >= MAX_WALK_FILES {
            return true;
        }

        let path = entry.path();
        if path.is_dir() {
            if walk_files(root, &path, depth + 1, max_depth, out) {
                return true;
            }
            continue;
        }

        out.push(
            path.strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/"),
        );
    }

    false
}

fn file_size(path: &Path) -> String {
    fs::metadata(path).map_or_else(|_| "大小未知".to_string(), |meta| format_bytes(meta.len()))
}
