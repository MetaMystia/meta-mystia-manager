//! 通用下载：进度、限速、低速换源、临时文件与落盘。

use super::{
    DOWNLOAD_BUFFER_SIZE, Downloader, Duration, File, Instant, MAX_CONSECUTIVE_SLOW_OVERALL,
    MAX_CONSECUTIVE_SLOW_WINDOWS, ManagerError, OVERALL_CHECK_INTERVAL, Path, PathBuf, Read,
    Result, SPEED_CHECK_INTERVAL, TAIL_SKIP_MIN_REMAINING_CAP, TAIL_SKIP_RATIO, WARMUP_DURATION,
    Write, atomic_rename_or_copy, check_response_status, cmp, fs, io, report_event, sleep,
};

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

    let sleep_dur = if cfg!(test) {
        Duration::from_millis(1)
    } else {
        Duration::from_secs_f64((expected_secs - elapsed_secs).max(0.001))
    };

    sleep(sleep_dur);
}

/// 创建下载用的临时文件，返回临时路径与文件句柄
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

impl Downloader<'_> {
    pub(super) fn download_file_with_progress(
        &self,
        url: &str,
        dest: &Path,
        file_size: Option<u64>,
        rate_limit_bps: Option<usize>,
    ) -> Result<()> {
        self.download_file_with_progress_and_speed_check(url, dest, file_size, rate_limit_bps, None)
    }

    pub(super) fn download_file_with_progress_and_speed_check(
        &self,
        url: &str,
        dest: &Path,
        file_size: Option<u64>,
        rate_limit_bps: Option<usize>,
        min_speed_bps: Option<usize>,
    ) -> Result<()> {
        self.retry("下载文件", || {
            self.try_download(url, dest, file_size, rate_limit_bps, min_speed_bps)
        })
    }

    fn try_download(
        &self,
        url: &str,
        dest: &Path,
        file_size: Option<u64>,
        rate_limit_bps: Option<usize>,
        min_speed_bps: Option<usize>,
    ) -> Result<()> {
        let response = self
            .agent
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

        let id = self.ui.download_start(&filename, total_size)?;

        let mut reader = response.into_body().into_reader();
        self.write_response_to_file(
            &mut reader,
            dest,
            id,
            total_size,
            rate_limit_bps,
            min_speed_bps,
        )
    }

    pub(super) fn write_response_to_file<R: Read>(
        &self,
        resp: &mut R,
        dest: &Path,
        id: usize,
        total_size: Option<u64>,
        rate_limit_bps: Option<usize>,
        min_speed_bps: Option<usize>,
    ) -> Result<()> {
        let result = self.write_response_to_file_inner(
            resp,
            dest,
            id,
            total_size,
            rate_limit_bps,
            min_speed_bps,
        );

        // 中途失败（含换源重试）时收尾进度条，避免留下卡住的下载行
        if result.is_err() {
            let _ = self.ui.download_finish(id, "下载失败");
        }

        result
    }

    fn write_response_to_file_inner<R: Read>(
        &self,
        resp: &mut R,
        dest: &Path,
        id: usize,
        total_size: Option<u64>,
        rate_limit_bps: Option<usize>,
        min_speed_bps: Option<usize>,
    ) -> Result<()> {
        // 0 表示不限速
        let rate_limit_bps = rate_limit_bps.filter(|limit| *limit > 0);
        let (tmp_path, mut tmp_file) = create_download_temp_file(dest)?;

        let mut buffer = vec![0; DOWNLOAD_BUFFER_SIZE];

        let mut downloaded = 0u64;
        let start = Instant::now();

        let mut window_start = Instant::now();
        let mut window_bytes = 0u64;
        let mut slow_window_count: u32 = 0;
        let mut last_overall_check = Instant::now();
        let mut slow_overall_count: u32 = 0;

        loop {
            let n = resp
                .read(&mut buffer)
                .map_err(|e| ManagerError::NetworkError(e.to_string()))?;
            if n == 0 {
                break;
            }

            tmp_file.write_all(&buffer[..n]).map_err(|e| {
                ManagerError::from(io::Error::new(
                    e.kind(),
                    format!("写入临时文件 {} 失败：{}", tmp_path.display(), e),
                ))
            })?;
            downloaded += n as u64;
            window_bytes += n as u64;

            self.ui.download_update(id, downloaded)?;

            if let Some(min_speed) = min_speed_bps {
                let elapsed = start.elapsed();

                // 启动期豁免
                if elapsed >= WARMUP_DURATION {
                    // 收尾豁免（同时作用于两条检测路径）
                    if !in_tail_skip(total_size, downloaded) {
                        // 路径 A：滑动窗口
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

                        // 路径 B：整体均速
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
            }

            if let Some(rate_limit_bps) = rate_limit_bps {
                sleep_for_rate_limit(downloaded, start.elapsed(), rate_limit_bps);
            }
        }

        tmp_file.flush().map_err(|e| {
            ManagerError::from(io::Error::new(
                e.kind(),
                format!("同步临时文件 {} 失败：{}", tmp_path.display(), e),
            ))
        })?;

        self.finish_download(&tmp_path, dest, id)
    }

    /// 将临时文件落到目标路径，并报告下载完成
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
        let filename = dest.file_name().map_or_else(
            || dest.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );

        self.ui
            .download_finish(id, &format!("下载完成：{filename}"))
    }
}
