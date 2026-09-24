//! GitHub 发布信息：Release Notes 与 DLL 直链。

use super::{
    Downloader, JsonRequestError, ManagerError, Result, RetryConfig, VersionInfo,
    get_json_with_retry_stopping_on_status, report_event,
};

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
    ) -> std::result::Result<serde_json::Value, JsonRequestError> {
        get_json_with_retry_stopping_on_status(
            &self.agent,
            self.ui,
            api_url,
            Some("application/vnd.github+json"),
            "请求 GitHub API ",
            Some(RetryConfig::github_release_note()),
            &[404],
        )
    }

    fn fetch_github_release_json(&self, version: Option<&str>) -> Result<serde_json::Value> {
        let cache_key = version.unwrap_or("latest").to_string();

        if let Ok(guard) = self.cached_github_releases.lock()
            && let Some(json) = guard.get(&cache_key)
        {
            return Ok(json.clone());
        }

        let config = self.remote_config()?;
        let api_base = &config.sources.github_release_api;
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
                            if let Ok(mut guard) = self.cached_github_releases.lock() {
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

        if let Ok(mut guard) = self.cached_github_releases.lock() {
            guard.insert(cache_key, json.clone());
        }

        Ok(json)
    }

    fn github_metamystia_asset_candidates_for_tag(tag: &str) -> Vec<String> {
        let normalized = VersionInfo::normalize_version(tag);
        let parts: Vec<_> = normalized.split('.').collect();

        match parts.as_slice() {
            [major, minor] if major.parse::<u64>().is_ok() && minor.parse::<u64>().is_ok() => {
                vec![
                    format!("MetaMystia-v{}.{}.dll", major, minor),
                    format!("MetaMystia-v{}.{}.0.dll", major, minor),
                ]
            }
            [major, minor, patch]
                if major.parse::<u64>().is_ok()
                    && minor.parse::<u64>().is_ok()
                    && patch.parse::<u64>().is_ok() =>
            {
                vec![format!("MetaMystia-v{}.{}.{}.dll", major, minor, patch)]
            }
            _ => Vec::new(),
        }
    }

    pub(super) fn get_dll_download_url_from_github(&self, version: &str) -> Result<String> {
        self.ui.download_attempt_github_dll()?;

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
                    self.ui.download_found_github_asset(name)?;
                    report_event(
                        "Download.GitHub.Dll.Found",
                        Some(&format!("requested={version};tag={tag};file={name}")),
                    );
                    return Ok(url.to_string());
                }
            }
        }

        self.ui.download_github_dll_not_found()?;
        report_event("Download.GitHub.Dll.NotFound", None);

        Err(ManagerError::NetworkError(
            "未找到 MetaMystia DLL 文件".to_string(),
        ))
    }

    fn get_github_release_notes(
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

    /// 获取并显示 GitHub Release Notes
    ///
    /// # 参数
    /// - `version`: 版本号（不含 'v' 前缀），例如 "1.0.0"。如果为 None，则获取最新版本的 notes
    pub fn fetch_and_display_github_release_notes(
        &self,
        version: Option<&str>,
    ) -> Result<Option<(String, String, String)>> {
        match self.get_github_release_notes(version) {
            Ok(Some((tag, name, body))) => {
                self.ui
                    .download_display_github_release_notes(&tag, &name, &body)?;
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
