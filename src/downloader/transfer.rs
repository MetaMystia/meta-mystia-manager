//! 通用下载：进度、限速、低速换源、临时文件与落盘。

use super::{
    DOWNLOAD_BUFFER_SIZE, Downloader, Duration, File, Instant, MAX_CONSECUTIVE_SLOW_OVERALL,
    MAX_CONSECUTIVE_SLOW_WINDOWS, ManagerError, OVERALL_CHECK_INTERVAL, Path, PathBuf, Read,
    Result, SPEED_CHECK_INTERVAL, TAIL_SKIP_MIN_REMAINING_CAP, TAIL_SKIP_RATIO, WARMUP_DURATION,
    Write, atomic_rename_or_copy, check_response_status, cmp, fs, io, report_event, sleep,
};
use crate::ui::{JobOutcome, Ui, UiEvent};

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, sync_channel},
    },
    thread,
};

const PAUSE_POLL_INTERVAL: Duration = Duration::from_millis(50);
pub(super) const STALL_TIMEOUT: Duration = Duration::from_secs(60);
const CHUNK_QUEUE_LEN: usize = 4;

/// 在独立线程里阻塞读取响应体，主线程按停顿超时消费数据块
pub(super) struct ChunkReader {
    rx: Receiver<io::Result<Vec<u8>>>,
    stop: Arc<AtomicBool>,
}

impl ChunkReader {
    pub(super) fn spawn<R: Read + Send + 'static>(mut source: R) -> Self {
        let (tx, rx) = sync_channel(CHUNK_QUEUE_LEN);
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);

        thread::spawn(move || {
            let mut buffer = vec![0u8; DOWNLOAD_BUFFER_SIZE];

            loop {
                if reader_stop.load(Ordering::Relaxed) {
                    break;
                }

                match source.read(&mut buffer) {
                    Ok(0) => {
                        // 空块表示正常结束，用于区分读取线程异常退出
                        let _ = tx.send(Ok(Vec::new()));
                        break;
                    }
                    Ok(n) => {
                        if tx.send(Ok(buffer[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });

        Self { rx, stop }
    }

    /// 读取下一块数据；`Ok(None)` 表示正常结束
    pub(super) fn next(&self, timeout: Duration) -> Result<Option<Vec<u8>>> {
        match self.rx.recv_timeout(timeout) {
            Ok(Ok(chunk)) if chunk.is_empty() => Ok(None),
            Ok(Ok(chunk)) => Ok(Some(chunk)),
            Ok(Err(e)) => Err(ManagerError::NetworkError(format!("读取响应失败：{e}"))),
            Err(RecvTimeoutError::Timeout) => {
                report_event(
                    "Download.Stalled",
                    Some(&format!("timeout={}", timeout.as_secs())),
                );
                Err(ManagerError::NetworkError(format!(
                    "下载停顿超过 {} 秒，连接可能已中断",
                    timeout.as_secs()
                )))
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err(ManagerError::NetworkError("读取线程异常结束".to_string()))
            }
        }
    }
}

impl Drop for ChunkReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// 等待暂停结束；期间被确认停止时返回取消错误
pub(super) fn wait_while_paused(ui: &dyn Ui) -> Result<()> {
    while ui.download_paused() {
        if ui.download_cancelled() {
            return Err(ManagerError::UserCancelled);
        }

        sleep(PAUSE_POLL_INTERVAL);
    }

    Ok(())
}

/// 平均速度低于阈值时返回 `(平均速度 KB/s, 阈值 KB/s)`
#[allow(
    clippy::cast_precision_loss,
    reason = "字节数与秒数都远小于 f64 的 2^53 精度上限"
)]
fn slow_speed(min_speed_bps: usize, bytes: u64, elapsed: Duration) -> Option<(f64, usize)> {
    let avg_speed = bytes as f64 / elapsed.as_secs_f64();

    if avg_speed < min_speed_bps as f64 {
        Some((avg_speed / 1024.0, min_speed_bps / 1024))
    } else {
        None
    }
}

/// 是否处于收尾豁免区间（剩余字节太少，不再判定低速）
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "仅按比例估算剩余字节，量级远小于 2^53"
)]
fn in_tail_skip(total_size: Option<u64>, downloaded: u64) -> bool {
    let Some(total) = total_size.filter(|total| *total > 0) else {
        return false;
    };

    let ratio_skip = (downloaded as f64 / total as f64) >= TAIL_SKIP_RATIO;
    let by_ratio_remaining = (total as f64 * (1.0 - TAIL_SKIP_RATIO)) as u64;
    let eff_tail_remaining = cmp::min(TAIL_SKIP_MIN_REMAINING_CAP, by_ratio_remaining);

    ratio_skip || total.saturating_sub(downloaded) <= eff_tail_remaining
}

/// 限速：按已下载字节数补齐应有的耗时
#[allow(
    clippy::cast_precision_loss,
    reason = "限速时长以 f64 近似表示，量级远小于 2^53"
)]
pub(super) fn sleep_for_rate_limit(downloaded: u64, elapsed: Duration, rate_limit_bps: usize) {
    let expected_secs = downloaded as f64 / rate_limit_bps as f64;
    let elapsed_secs = elapsed.as_secs_f64();

    if expected_secs <= elapsed_secs {
        return;
    }

    sleep(Duration::from_secs_f64(
        (expected_secs - elapsed_secs).max(0.001),
    ));
}

fn create_download_temp_file(dest: &Path) -> Result<(PathBuf, File)> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            ManagerError::from(io::Error::new(
                e.kind(),
                format!("创建目录 {} 失败：{}", parent.display(), e),
            ))
        })?;
    }

    let mut tmp_path = dest.with_extension("dl.tmp");
    let mut tmp_idx = 0;
    while tmp_path.exists() {
        tmp_idx += 1;
        tmp_path = dest.with_extension(format!("dl.tmp{tmp_idx}"));
    }

    let tmp_file = fs::File::create(&tmp_path).map_err(|e| {
        ManagerError::from(io::Error::new(
            e.kind(),
            format!("创建临时文件 {} 失败：{}", tmp_path.display(), e),
        ))
    })?;

    Ok((tmp_path, tmp_file))
}

fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

impl Downloader<'_> {
    pub(super) fn download_file_with_progress(
        &self,
        url: &str,
        dest: &Path,
        file_size: Option<u64>,
        rate_limit_bps: Option<usize>,
        slot: usize,
    ) -> Result<()> {
        self.download_file_with_progress_and_speed_check(
            url,
            dest,
            file_size,
            rate_limit_bps,
            None,
            slot,
        )
    }

    pub(super) fn download_file_with_progress_and_speed_check(
        &self,
        url: &str,
        dest: &Path,
        file_size: Option<u64>,
        rate_limit_bps: Option<usize>,
        min_speed_bps: Option<usize>,
        slot: usize,
    ) -> Result<()> {
        self.retry("下载文件", || {
            self.try_download(url, dest, file_size, rate_limit_bps, min_speed_bps, slot)
        })
    }

    fn try_download(
        &self,
        url: &str,
        dest: &Path,
        file_size: Option<u64>,
        rate_limit_bps: Option<usize>,
        min_speed_bps: Option<usize>,
        slot: usize,
    ) -> Result<()> {
        let agent = self.agent_for(url);
        let response = agent
            .get(url)
            .call()
            .map_err(|e| ManagerError::NetworkError(Self::convert_ureq_error(&e)))?;

        if let Some(err) = check_response_status(&response, self.ui, "下载文件") {
            return Err(err);
        }

        let total_size = file_size.or_else(|| Self::content_length(&response));
        let filename = dest.file_name().map_or_else(
            || dest.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );

        let id = self
            .ui
            .emit(UiEvent::DownloadStart(slot, &filename, total_size))?
            .download_id()?;

        let reader = response.into_body().into_reader();
        self.write_response_to_file(reader, dest, id, total_size, rate_limit_bps, min_speed_bps)
    }

    pub(super) fn write_response_to_file<R: Read + Send + 'static>(
        &self,
        resp: R,
        dest: &Path,
        id: usize,
        total_size: Option<u64>,
        rate_limit_bps: Option<usize>,
        min_speed_bps: Option<usize>,
    ) -> Result<()> {
        let chunks = ChunkReader::spawn(resp);
        let result = self.write_response_to_file_inner(
            &chunks,
            dest,
            id,
            total_size,
            rate_limit_bps,
            min_speed_bps,
        );

        // 中途失败（含换源重试）时收尾进度条，避免留下卡住的下载行
        if result.is_err() {
            let message = format!("下载失败：{}", display_name(dest));
            let _ = self
                .ui
                .emit(UiEvent::DownloadFinish(id, &message, JobOutcome::Failed));
        }

        result
    }

    #[allow(
        clippy::too_many_lines,
        reason = "下载循环里进度、取消、暂停、低速检测与限速依次处理，拆开反而看不清顺序"
    )]
    fn write_response_to_file_inner(
        &self,
        resp: &ChunkReader,
        dest: &Path,
        id: usize,
        total_size: Option<u64>,
        rate_limit_bps: Option<usize>,
        min_speed_bps: Option<usize>,
    ) -> Result<()> {
        // 0 表示不限速
        let rate_limit_bps = rate_limit_bps.filter(|limit| *limit > 0);

        if self.cancelled() {
            return Err(ManagerError::UserCancelled);
        }

        let (tmp_path, mut tmp_file) = create_download_temp_file(dest)?;

        let mut downloaded = 0u64;
        let start = Instant::now();
        let mut paused = Duration::ZERO;

        let mut window_start = Instant::now();
        let mut window_bytes = 0u64;
        let mut slow_window_count: u32 = 0;
        let mut last_overall_check = Instant::now();
        let mut slow_overall_count: u32 = 0;

        loop {
            let chunk = match resp.next(STALL_TIMEOUT) {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(e) => {
                    let _ = fs::remove_file(&tmp_path);
                    return Err(e);
                }
            };

            if let Err(e) = tmp_file.write_all(&chunk) {
                drop(tmp_file);
                let _ = fs::remove_file(&tmp_path);

                return Err(ManagerError::from(io::Error::new(
                    e.kind(),
                    format!("写入临时文件 {} 失败：{}", tmp_path.display(), e),
                )));
            }
            downloaded += chunk.len() as u64;
            window_bytes += chunk.len() as u64;

            self.ui.emit(UiEvent::DownloadUpdate(id, downloaded))?;

            if self.cancelled() {
                let _ = fs::remove_file(&tmp_path);
                return Err(ManagerError::UserCancelled);
            }

            if self.ui.download_paused() {
                let paused_at = Instant::now();

                if wait_while_paused(self.ui).is_err() {
                    let _ = fs::remove_file(&tmp_path);
                    return Err(ManagerError::UserCancelled);
                }

                paused += paused_at.elapsed();
                window_start = Instant::now();
                window_bytes = 0;
                slow_window_count = 0;
                last_overall_check = Instant::now();
                slow_overall_count = 0;
            }

            if let Some(min_speed) = min_speed_bps {
                let elapsed = start.elapsed().saturating_sub(paused);

                if elapsed >= WARMUP_DURATION && !in_tail_skip(total_size, downloaded) {
                    let window_elapsed = window_start.elapsed();
                    if window_elapsed >= SPEED_CHECK_INTERVAL {
                        if let Some((speed_kbs, threshold_kbs)) =
                            slow_speed(min_speed, window_bytes, window_elapsed)
                        {
                            slow_window_count += 1;
                            if slow_window_count >= MAX_CONSECUTIVE_SLOW_WINDOWS {
                                let _ = fs::remove_file(&tmp_path);
                                report_event(
                                    "Download.SlowSpeed.Triggered.Window",
                                    Some(&format!("{speed_kbs:.1}KB/s<{threshold_kbs}KB/s")),
                                );
                                return Err(ManagerError::SlowDownload(format!(
                                    "{speed_kbs:.1} KB/s < {threshold_kbs} KB/s"
                                )));
                            }
                        } else {
                            slow_window_count = 0;
                        }
                        window_start = Instant::now();
                        window_bytes = 0;
                    }

                    if last_overall_check.elapsed() >= OVERALL_CHECK_INTERVAL {
                        if let Some((speed_kbs, threshold_kbs)) =
                            slow_speed(min_speed, downloaded, elapsed)
                        {
                            slow_overall_count += 1;
                            if slow_overall_count >= MAX_CONSECUTIVE_SLOW_OVERALL {
                                let _ = fs::remove_file(&tmp_path);
                                report_event(
                                    "Download.SlowSpeed.Triggered.Overall",
                                    Some(&format!("{speed_kbs:.1}KB/s<{threshold_kbs}KB/s")),
                                );
                                return Err(ManagerError::SlowDownload(format!(
                                    "整体均速 {speed_kbs:.1} KB/s < {threshold_kbs} KB/s"
                                )));
                            }
                        } else {
                            slow_overall_count = 0;
                        }
                        last_overall_check = Instant::now();
                    }
                }
            }

            if let Some(rate_limit_bps) = rate_limit_bps {
                sleep_for_rate_limit(downloaded, start.elapsed(), rate_limit_bps);
            }
        }

        if let Err(e) = tmp_file.flush() {
            drop(tmp_file);
            let _ = fs::remove_file(&tmp_path);

            return Err(ManagerError::from(io::Error::new(
                e.kind(),
                format!("同步临时文件 {} 失败：{}", tmp_path.display(), e),
            )));
        }

        self.finish_download(&tmp_path, dest, id)
    }

    pub(super) fn finish_download(&self, tmp_path: &Path, dest: &Path, id: usize) -> Result<()> {
        if let Err(e) = atomic_rename_or_copy(tmp_path, dest) {
            let _ = fs::remove_file(tmp_path);
            return Err(ManagerError::from(io::Error::other(format!(
                "重命名或复制临时文件 {} 失败：{}",
                tmp_path.display(),
                e
            ))));
        }

        let _ = fs::remove_file(tmp_path);
        let message = format!("下载完成：{}", display_name(dest));
        self.ui
            .emit(UiEvent::DownloadFinish(id, &message, JobOutcome::Completed))?;

        Ok(())
    }
}
