//! glob 匹配工具。

use glob::{MatchOptions, glob_with};
use std::path::{Path, PathBuf};

const fn case_insensitive_match_options() -> MatchOptions {
    MatchOptions {
        case_sensitive: false,
        require_literal_leading_dot: false,
        require_literal_separator: false,
    }
}

fn glob_pattern_string(pattern: &Path) -> String {
    let file_name = pattern
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let Some(parent) = pattern
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return file_name;
    };

    let parent = glob::Pattern::escape(&parent.to_string_lossy().replace('\\', "/"));
    format!("{parent}/{file_name}")
}

/// 按 glob 模式匹配路径，并用额外谓词过滤。
pub fn glob_matches_filtered<F>(pattern: &Path, matcher: F) -> Vec<PathBuf>
where
    F: Fn(&Path) -> bool,
{
    let mut matched_paths = Vec::new();
    let s = glob_pattern_string(pattern);

    if let Ok(entries) = glob_with(&s, case_insensitive_match_options()) {
        for entry in entries.flatten() {
            if entry.exists() && matcher(&entry) {
                matched_paths.push(entry);
            }
        }
    }

    matched_paths
}

/// 根据 glob 模式获取匹配的路径列表，并通过 matcher 进行额外过滤（仅对文件名部分进行过滤）。
pub fn glob_matches_by_filename(pattern: &Path, matcher: fn(&str) -> bool) -> Vec<PathBuf> {
    glob_matches_filtered(pattern, |path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(matcher)
    })
}
