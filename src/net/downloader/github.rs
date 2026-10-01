//! GitHub 发布信息：Release Notes 与 DLL 直链。

use super::{
    Downloader, JsonRequestError, ManagerError, Result, RetryConfig, VersionInfo,
    fetch_json_with_retry_stopping_on_status, report_event,
};
use crate::ui::UiEvent;

use std::{
    collections::HashMap,
    result::Result as StdResult,
    sync::{Mutex, OnceLock},
};

type ReleaseCacheKey = (String, String);

static RELEASE_CACHE: OnceLock<Mutex<HashMap<ReleaseCacheKey, serde_json::Value>>> =
    OnceLock::new();

fn release_cache() -> &'static Mutex<HashMap<ReleaseCacheKey, serde_json::Value>> {
    RELEASE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

impl Downloader<'_> {
    fn github_release_fallback_tag(version: &str) -> Option<String> {
        let normalized = VersionInfo::normalize_version(version);
        let parts: Vec<_> = normalized.split('.').collect();

        match parts.as_slice() {
            [major, minor, patch]
                if major.parse::<u64>().is_ok()
                    && minor.parse::<u64>().is_ok()
                    && patch.parse::<u64>().ok() == Some(0) =>
            {
                Some(format!("{major}.{minor}"))
            }
            _ => None,
        }
    }

    fn github_release_fallback_tag_for_status(version: &str, status_code: u16) -> Option<String> {
        if status_code == 404 {
            Self::github_release_fallback_tag(version)
        } else {
            None
        }
    }

    fn github_release_tag_matches_requested_version(requested: &str, tag: &str) -> bool {
        let requested = VersionInfo::normalize_version(requested);
        let tag = VersionInfo::normalize_version(tag);

        !tag.is_empty()
            && (requested == tag
                || Self::github_release_fallback_tag(&requested)
                    .as_deref()
                    .is_some_and(|fallback| fallback == tag))
    }

    fn github_release_api_url(base: &str, version: Option<&str>) -> String {
        let base = base.trim_end_matches('/');

        version.map_or_else(
            || format!("{base}/latest"),
            |version| format!("{base}/tags/v{version}"),
        )
    }

    fn github_release_not_found_error() -> ManagerError {
        ManagerError::NetworkError("请求 GitHub API 返回错误：HTTP 404".to_string())
    }

    fn fetch_github_release_json_from_url(
        &self,
        api_url: &str,
    ) -> StdResult<serde_json::Value, JsonRequestError> {
        let agent = self.metadata_agent(api_url);

        fetch_json_with_retry_stopping_on_status(
            &agent,
            self.ui,
            api_url,
            Some("application/vnd.github+json"),
            "请求 GitHub API ",
            Some(RetryConfig::github_release_note()),
            &[404],
        )
    }

    fn fetch_github_release_json(&self, version: Option<&str>) -> Result<serde_json::Value> {
        let config = self.remote_config()?;
        let Some(sources) = config.sources.as_ref() else {
            report_event("Download.GitHub.Disabled", None);
            return Err(ManagerError::NetworkError(
                "服务端未配置 GitHub 发布信息地址".to_string(),
            ));
        };
        let api_base = sources.github_release_api.trim();
        if api_base.is_empty() {
            report_event("Download.GitHub.Disabled", None);
            return Err(ManagerError::NetworkError(
                "服务端未配置 GitHub 发布信息地址".to_string(),
            ));
        }

        let cache_key = (
            api_base.to_ascii_lowercase(),
            version.unwrap_or("latest").to_string(),
        );
        if let Ok(guard) = release_cache().lock()
            && let Some(json) = guard.get(&cache_key)
        {
            return Ok(json.clone());
        }

        let api_url = Self::github_release_api_url(api_base, version);

        let json = match self.fetch_github_release_json_from_url(&api_url) {
            Ok(json) => json,
            Err(JsonRequestError::HttpStatus(404)) => {
                if let Some(v) = version
                    && let Some(fallback_tag) = Self::github_release_fallback_tag_for_status(v, 404)
                {
                    let fallback_url = Self::github_release_api_url(api_base, Some(&fallback_tag));
                    match self.fetch_github_release_json_from_url(&fallback_url) {
                        Ok(fallback_json) => {
                            if let Ok(mut guard) = release_cache().lock() {
                                guard.insert(cache_key, fallback_json.clone());
                            }
                            return Ok(fallback_json);
                        }
                        Err(JsonRequestError::HttpStatus(404)) => {
                            return Err(Self::github_release_not_found_error());
                        }
                        Err(JsonRequestError::Other(err)) => {
                            return Err(err);
                        }
                        Err(JsonRequestError::HttpStatus(status)) => {
                            return Err(ManagerError::NetworkError(format!(
                                "请求 GitHub API 返回错误：HTTP {status}"
                            )));
                        }
                    }
                }
                return Err(Self::github_release_not_found_error());
            }
            Err(JsonRequestError::Other(err)) => {
                return Err(err);
            }
            Err(JsonRequestError::HttpStatus(status)) => {
                return Err(ManagerError::NetworkError(format!(
                    "请求 GitHub API 返回错误：HTTP {status}"
                )));
            }
        };

        if let Ok(mut guard) = release_cache().lock() {
            guard.insert(cache_key, json.clone());
        }

        Ok(json)
    }

    fn github_metamystia_asset_candidates_for_tag(tag: &str) -> Vec<String> {
        let normalized = VersionInfo::normalize_version(tag);
        let parts: Vec<_> = normalized.split('.').collect();

        match parts.as_slice() {
            [major, minor, patch]
                if major.parse::<u64>().is_ok()
                    && minor.parse::<u64>().is_ok()
                    && patch.parse::<u64>().is_ok() =>
            {
                vec![format!("MetaMystia-v{}.{}.{}.dll", major, minor, patch)]
            }
            [major, minor] if major.parse::<u64>().is_ok() && minor.parse::<u64>().is_ok() => {
                vec![
                    format!("MetaMystia-v{}.{}.dll", major, minor),
                    format!("MetaMystia-v{}.{}.0.dll", major, minor),
                ]
            }
            _ => Vec::new(),
        }
    }

    /// 通过 GitHub Release 解析 DLL 的下载地址。
    pub(super) fn fetch_dll_download_url_from_github(&self, version: &str) -> Result<String> {
        self.ui.emit(UiEvent::DownloadAttemptGithubDll)?;

        let json = self.fetch_github_release_json(Some(version))?;
        let tag = json["tag_name"].as_str().unwrap_or("");

        if !Self::github_release_tag_matches_requested_version(version, tag) {
            report_event(
                "Download.GitHub.Dll.TagMismatch",
                Some(&format!("requested={version};tag={tag}")),
            );
            return Err(ManagerError::NetworkError(format!(
                "GitHub Release 标签与目标版本不一致：目标 {}，实际 {}",
                version,
                if tag.is_empty() { "<empty>" } else { tag }
            )));
        }

        let candidates = Self::github_metamystia_asset_candidates_for_tag(tag);

        if candidates.is_empty() {
            report_event(
                "Download.GitHub.Dll.InvalidTag",
                Some(&format!("requested={version};tag={tag}")),
            );
            return Err(ManagerError::NetworkError(format!(
                "无法从 GitHub Release 标签解析 MetaMystia 文件名：{}",
                if tag.is_empty() { "<empty>" } else { tag }
            )));
        }

        if let Some(assets) = json["assets"].as_array() {
            for asset in assets {
                if let (Some(name), Some(url)) = (
                    asset["name"].as_str(),
                    asset["browser_download_url"].as_str(),
                ) && candidates
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(name))
                {
                    self.ui.emit(UiEvent::DownloadFoundGithubAsset(name))?;
                    report_event(
                        "Download.GitHub.Dll.Found",
                        Some(&format!("requested={version};tag={tag};file={name}")),
                    );
                    return Ok(url.to_string());
                }
            }
        }

        self.ui.emit(UiEvent::DownloadGithubDllNotFound)?;
        report_event("Download.GitHub.Dll.NotFound", None);

        Err(ManagerError::NetworkError(
            "未找到 MetaMystia DLL 文件".to_string(),
        ))
    }

    /// 获取指定版本的 GitHub Release 说明（`None` 表示最新版本），返回 `(tag, name, body)`。
    pub fn fetch_github_release_notes(
        &self,
        version: Option<&str>,
    ) -> Result<Option<(String, String, String)>> {
        let json = self.fetch_github_release_json(version)?;

        let tag = json["tag_name"].as_str().unwrap_or("").to_string();
        let name = json["name"].as_str().unwrap_or("").to_string();
        let body = json["body"].as_str().unwrap_or("").to_string();

        if tag.is_empty() && name.is_empty() && body.trim().is_empty() {
            report_event("Download.GitHub.ReleaseNotes.Empty", version);
            Ok(None)
        } else {
            report_event("Download.GitHub.ReleaseNotes.Found", Some(&tag));
            Ok(Some((tag, name, body)))
        }
    }

    /// 获取并展示 GitHub Release 说明；失败或不存在时返回 `Ok(None)`。
    pub fn fetch_and_display_github_release_notes(
        &self,
        version: Option<&str>,
    ) -> Result<Option<(String, String, String)>> {
        match self.fetch_github_release_notes(version) {
            Ok(Some((tag, name, body))) => {
                self.ui
                    .emit(UiEvent::DownloadReleaseNotes(&tag, &name, &body))?;
                Ok(Some((tag, name, body)))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                report_event(
                    "Download.GitHub.ReleaseNotes.Failed",
                    Some(&format!("version={version:?};error={e}")),
                );
                Ok(None)
            }
        }
    }
}
