//! 一次性密钥下载：申请、续传、校验与落盘。

use super::{
    DOWNLOAD_BUFFER_SIZE, Deserialize, Downloader, Instant, KEY_ATTEMPTS, ManagerError, Path,
    PathBuf, Read, RemoteConfig, Result, Write, check_response_status, fs, io, remote_config,
    report_event, sso,
};
use crate::downloader::transfer::{
    ChunkReader, STALL_TIMEOUT, sleep_for_rate_limit, wait_while_paused,
};
use crate::error::service_error_reason;
use crate::net::rate_limit_error;
use crate::preflight::format_bytes;
use crate::ui::{JobOutcome, UiEvent};

use std::result::Result as StdResult;

struct DownloadKey {
    md5: String,
    /// `None` 表示不限速
    rate_limit_bps: Option<usize>,
    size: u64,
    url: String,
}

/// 下载服务返回的密钥响应；失败时只有 `error`
#[derive(Deserialize)]
struct DownloadKeyEnvelope {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    md5: Option<String>,
    #[serde(default)]
    rate_limit_kb_per_second: Option<u64>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    url: Option<String>,
}

/// 一次性密钥下载的失败原因
enum KeyedDownloadError {
    Failed(ManagerError),
    /// 密钥已消费、过期或边缘签名失效，可重新申请
    KeyExpired,
    /// 断点范围无效，需要删除部分文件后重新下载
    Restart,
}

/// 部分下载文件：与目标同目录、同名追加 `.part`
fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".part");

    PathBuf::from(name)
}

/// 可续传的字节数；文件不存在、为空或超过目标大小时返回 0（重新下载）。
/// 等于目标大小时原样返回，由调用方直接校验而不再请求网络。
fn resume_offset(part: &Path, total: u64) -> u64 {
    fs::metadata(part).map_or(0, |meta| {
        let len = meta.len();

        if len > 0 && len <= total { len } else { 0 }
    })
}

fn hex_digest(context: md5::Context) -> String {
    format!("{:x}", context.finalize())
}

/// 校验文件 MD5；`context` 为 `None`（续传）时重新读取整文件计算
fn verify_digest(path: &Path, expected: &str, context: Option<md5::Context>) -> Result<()> {
    let actual = if let Some(context) = context {
        hex_digest(context)
    } else {
        let mut file = fs::File::open(path).map_err(|e| {
            ManagerError::from(io::Error::new(
                e.kind(),
                format!("读取文件 {} 失败：{}", path.display(), e),
            ))
        })?;
        let mut context = md5::Context::new();
        let mut buffer = vec![0; DOWNLOAD_BUFFER_SIZE];

        loop {
            let read = file.read(&mut buffer).map_err(|e| {
                ManagerError::from(io::Error::new(
                    e.kind(),
                    format!("读取文件 {} 失败：{}", path.display(), e),
                ))
            })?;
            if read == 0 {
                break;
            }

            context.consume(&buffer[..read]);
        }

        hex_digest(context)
    };

    if actual.eq_ignore_ascii_case(expected) {
        return Ok(());
    }

    let _ = fs::remove_file(path);
    report_event(
        "Download.Verify.Mismatch",
        Some(&format!("file={}", path.display())),
    );

    Err(ManagerError::NetworkError(
        "下载文件校验失败，请重试".to_string(),
    ))
}

impl Downloader<'_> {
    /// 用当前下载会话申请一次性密钥；`category` 为 `None` 表示扁平路径
    fn request_download_key(
        &self,
        config: &RemoteConfig,
        category: Option<&str>,
        filename: &str,
    ) -> Result<DownloadKey> {
        let token = sso::current_download_token().ok_or_else(|| {
            ManagerError::SsoLoginFailed("下载文件需要先登录，请重新登录后重试".to_string())
        })?;

        let request_body = serde_json::json!({
            "category": category,
            "filename": filename,
        });
        let request_body = serde_json::to_string(&request_body)
            .map_err(|e| ManagerError::Other(format!("构造下载请求失败：{e}")))?;

        let response = self
            .metadata_agent(&config.download.keys_url)
            .post(&config.download.keys_url)
            .header("Authorization", &format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .send(request_body.as_str())
            .map_err(|e| ManagerError::NetworkError(Self::convert_ureq_error(&e)))?;

        let status = response.status().as_u16();
        if status == 429 {
            return Err(rate_limit_error(&response, self.ui, "申请下载密钥"));
        }

        let text = response
            .into_body()
            .read_to_string()
            .map_err(|e| ManagerError::NetworkError(format!("读取下载密钥响应失败：{e}")))?;
        let envelope: DownloadKeyEnvelope = serde_json::from_str(&text).map_err(|e| {
            report_event(
                "Download.Key.ParseFailed",
                Some(&format!("category={category:?};status={status};err={e}")),
            );
            ManagerError::NetworkError(format!("申请下载密钥失败：HTTP {status}"))
        })?;

        if status != 200 {
            let code = envelope.error.unwrap_or_default();
            if code == "invalid-session" {
                sso::clear_session();
            }

            report_event(
                "Download.Key.Failed",
                Some(&format!(
                    "category={category:?};status={status};error={code}"
                )),
            );

            let reason = service_error_reason(&code, status);
            let retryable = status >= 500
                || code == "sso-unreachable"
                || code
                    .strip_prefix("http-")
                    .and_then(|value| value.parse::<u16>().ok())
                    .is_some_and(|status| (500..=599).contains(&status));

            return if retryable {
                Err(ManagerError::NetworkError(format!(
                    "申请下载密钥失败：{reason}"
                )))
            } else {
                Err(ManagerError::ServiceError(format!("下载失败：{reason}")))
            };
        }

        let (Some(url), Some(rate_limit_kb_per_second), Some(size)) = (
            envelope.url,
            envelope.rate_limit_kb_per_second,
            envelope.size,
        ) else {
            report_event(
                "Download.Key.InvalidResponse",
                Some(&format!("category={category:?};file={filename}")),
            );

            return Err(ManagerError::NetworkError(
                "下载服务响应异常，请升级管理工具后重试".to_string(),
            ));
        };
        let Some(md5) = envelope.md5 else {
            report_event(
                "Download.Key.InvalidResponse",
                Some(&format!("category={category:?};file={filename}")),
            );

            return Err(ManagerError::NetworkError(
                "下载服务响应异常，请升级管理工具后重试".to_string(),
            ));
        };
        let rate_limit_bps = remote_config::rate_limit_bytes_per_second(rate_limit_kb_per_second);

        report_event(
            "Download.Key.Success",
            Some(&format!("category={category:?};file={filename}")),
        );

        Ok(DownloadKey {
            md5,
            rate_limit_bps,
            size,
            url,
        })
    }

    /// 按一次性密钥下载文件（同一次运行内可续传 + MD5 校验）；
    /// 410 表示密钥已消费或过期
    fn try_download_keyed(
        &self,
        ticket: &DownloadKey,
        dest: &Path,
        slot: usize,
    ) -> StdResult<(), KeyedDownloadError> {
        if self.cancelled() {
            return Err(KeyedDownloadError::Failed(ManagerError::UserCancelled));
        }

        let part = part_path(dest);
        let resume_from = resume_offset(&part, ticket.size);
        let filename = dest.file_name().map_or_else(
            || dest.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );

        if ticket.size > 0 && resume_from == ticket.size {
            let id = self
                .ui
                .emit(UiEvent::DownloadStart(slot, &filename, Some(ticket.size)))
                .map_err(KeyedDownloadError::Failed)?
                .download_id()
                .map_err(KeyedDownloadError::Failed)?;

            return match verify_digest(&part, &ticket.md5, None) {
                Ok(()) => self
                    .finish_download(&part, dest, id)
                    .map_err(KeyedDownloadError::Failed),
                Err(e) => {
                    let message = format!("下载失败：{filename}");
                    let _ = self
                        .ui
                        .emit(UiEvent::DownloadFinish(id, &message, JobOutcome::Failed));

                    Err(KeyedDownloadError::Failed(e))
                }
            };
        }

        let mut request = self.agent_for(&ticket.url).get(&ticket.url);
        if resume_from > 0 {
            request = request.header("Range", &format!("bytes={resume_from}-"));
        }

        let response = request.call().map_err(|e| {
            KeyedDownloadError::Failed(ManagerError::NetworkError(Self::convert_ureq_error(&e)))
        })?;
        let status = response.status().as_u16();

        if matches!(status, 401 | 403 | 410) {
            return Err(KeyedDownloadError::KeyExpired);
        }
        if status == 416 {
            let _ = fs::remove_file(&part);
            report_event("Download.Range.Restart", Some(&dest.display().to_string()));
            return Err(KeyedDownloadError::Restart);
        }
        if let Some(err) = check_response_status(&response, self.ui, "下载文件") {
            return Err(KeyedDownloadError::Failed(err));
        }

        // 服务端忽略 Range 时会返回 200，此时从头写入（截断已有的部分文件）
        let append_from = if status == 206 { resume_from } else { 0 };
        // 续传时进度条只统计本次传输量，否则速度会瞬间跳到"已完成大小/0s"
        let label = if append_from > 0 {
            format!("{filename}（续传，已完成 {}）", format_bytes(append_from))
        } else {
            filename.clone()
        };
        let remaining = ticket.size.saturating_sub(append_from);
        let id = self
            .ui
            .emit(UiEvent::DownloadStart(slot, &label, Some(remaining)))
            .map_err(KeyedDownloadError::Failed)?
            .download_id()
            .map_err(KeyedDownloadError::Failed)?;

        let reader = response.into_body().into_reader();

        match self.write_keyed_response(reader, dest, id, ticket, append_from) {
            Ok(()) => Ok(()),
            Err(e) => {
                // 失败时收尾进度条，避免留下卡住的下载行
                let message = format!("下载失败：{filename}");
                let _ = self
                    .ui
                    .emit(UiEvent::DownloadFinish(id, &message, JobOutcome::Failed));

                Err(KeyedDownloadError::Failed(e))
            }
        }
    }

    /// 写入一次性密钥下载的内容：支持追加续传、流式校验，最后原子改名
    #[allow(
        clippy::cast_possible_truncation,
        reason = "这里的长度只用于进度显示，量级远小于 u64 上限"
    )]
    fn write_keyed_response<R: Read + Send + 'static>(
        &self,
        resp: R,
        dest: &Path,
        id: usize,
        ticket: &DownloadKey,
        append_from: u64,
    ) -> Result<()> {
        let part = part_path(dest);

        if let Some(parent) = part.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                ManagerError::from(io::Error::new(
                    e.kind(),
                    format!("创建目录 {} 失败：{}", parent.display(), e),
                ))
            })?;
        }

        let mut file = if append_from > 0 {
            fs::OpenOptions::new()
                .append(true)
                .open(&part)
                .map_err(|e| {
                    ManagerError::from(io::Error::new(
                        e.kind(),
                        format!("打开部分下载文件 {} 失败：{}", part.display(), e),
                    ))
                })?
        } else {
            fs::File::create(&part).map_err(|e| {
                ManagerError::from(io::Error::new(
                    e.kind(),
                    format!("创建部分下载文件 {} 失败：{}", part.display(), e),
                ))
            })?
        };

        // 续传时前半段没有经过哈希器，完成后按整文件重算
        let mut hasher = (append_from == 0).then(md5::Context::new);
        let chunks = ChunkReader::spawn(resp);
        let mut transferred = 0u64;
        let start = Instant::now();

        while let Some(chunk) = chunks.next(STALL_TIMEOUT)? {
            file.write_all(&chunk).map_err(|e| {
                ManagerError::from(io::Error::new(
                    e.kind(),
                    format!("写入部分下载文件 {} 失败：{}", part.display(), e),
                ))
            })?;

            if let Some(hasher) = hasher.as_mut() {
                hasher.consume(&chunk);
            }

            transferred += chunk.len() as u64;
            self.ui.emit(UiEvent::DownloadUpdate(id, transferred))?;

            if self.cancelled() {
                let _ = fs::remove_file(&part);
                return Err(ManagerError::UserCancelled);
            }

            if self.ui.download_paused() && wait_while_paused(self.ui).is_err() {
                let _ = fs::remove_file(&part);
                return Err(ManagerError::UserCancelled);
            }

            if let Some(rate_limit_bps) = ticket.rate_limit_bps {
                sleep_for_rate_limit(transferred, start.elapsed(), rate_limit_bps);
            }
        }

        file.flush().map_err(|e| {
            ManagerError::from(io::Error::new(
                e.kind(),
                format!("同步部分下载文件 {} 失败：{}", part.display(), e),
            ))
        })?;
        drop(file);

        verify_digest(&part, &ticket.md5, hasher)?;

        self.finish_download(&part, dest, id)
    }

    /// 申请一次性密钥并下载业务文件；密钥失效时重新申请（有限次）
    pub(super) fn download_asset_with_key(
        &self,
        category: Option<&str>,
        filename: &str,
        dest: &Path,
        op_desc: &str,
        slot: usize,
    ) -> Result<()> {
        let config = self.remote_config()?;

        if self.cancelled() {
            return Err(ManagerError::UserCancelled);
        }

        self.retry(op_desc, || {
            let mut last_err = ManagerError::NetworkError("下载密钥已失效，请重试".to_string());

            for _ in 0..KEY_ATTEMPTS {
                if self.cancelled() {
                    return Err(ManagerError::UserCancelled);
                }

                let ticket = self.request_download_key(&config, category, filename)?;

                match self.try_download_keyed(&ticket, dest, slot) {
                    Ok(()) => return Ok(()),
                    Err(KeyedDownloadError::KeyExpired) => {
                        report_event(
                            "Download.Key.Expired",
                            Some(&format!("category={category:?};file={filename}")),
                        );
                        last_err = ManagerError::NetworkError("下载密钥已失效，请重试".to_string());
                    }
                    Err(KeyedDownloadError::Restart) => {
                        last_err =
                            ManagerError::NetworkError("下载断点无效，重新下载失败".to_string());
                    }
                    Err(KeyedDownloadError::Failed(err)) => return Err(err),
                }
            }

            Err(last_err)
        })
    }
}
