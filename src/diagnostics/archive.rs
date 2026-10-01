//! 诊断 zip 的写出与文本日志的头尾截断。

use super::{Body, Entry, format_timestamp};
use crate::error::{ManagerError, Result};

use std::{
    env, fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const TRUNCATION_WINDOW_BYTES: usize = 64 * 1024;

/// 诊断包路径：优先管理工具所在目录，不可写时回落到临时目录。
pub(super) fn archive_path() -> Result<PathBuf> {
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
        let path = dir.join(&filename);

        if fs::File::create(&path).is_ok() {
            return Ok(path);
        }
    }

    Err(ManagerError::PermissionDenied(
        "无法在管理工具目录或临时目录创建诊断包".to_string(),
    ))
}

/// 把 `manager-info.txt` 与收集到的文件写成 zip。
pub(super) fn write_archive(path: &Path, info: &str, entries: &[Entry]) -> Result<()> {
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
        write_entry(&mut zip, &mut source, &entry.body)
            .map_err(|e| ManagerError::Other(format!("写入诊断包失败：{e}")))?;
    }

    zip.finish()
        .map_err(|e| ManagerError::Other(format!("完成诊断包失败：{e}")))?;

    Ok(())
}

fn write_entry(
    zip: &mut ZipWriter<fs::File>,
    source: &mut fs::File,
    body: &Body,
) -> io::Result<()> {
    let Body::TruncateText { head, tail } = body else {
        io::copy(source, zip)?;
        return Ok(());
    };

    let len = source.metadata().map_or(0, |meta| meta.len());
    if len <= head + tail {
        io::copy(source, zip)?;
        return Ok(());
    }

    let head_limit = usize::try_from(*head).unwrap_or(usize::MAX);
    let mut head_buffer = vec![0u8; head_limit.saturating_add(TRUNCATION_WINDOW_BYTES)];
    let read = read_up_to(source, &mut head_buffer)?;
    head_buffer.truncate(read);
    let cut = line_break_cut(&head_buffer, head_limit);

    let tail_start = len - tail;
    source.seek(SeekFrom::Start(tail_start))?;

    let mut probe = vec![0u8; TRUNCATION_WINDOW_BYTES];
    let read = read_up_to(source, &mut probe)?;
    probe.truncate(read);
    let skip = probe
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or_else(|| utf8_prefix_skip(&probe), |position| position + 1);
    let tail_from = tail_start + u64::try_from(skip).unwrap_or(u64::MAX);
    let omitted = tail_from.saturating_sub(u64::try_from(cut).unwrap_or(u64::MAX));

    zip.write_all(&head_buffer[..cut])?;
    write!(
        zip,
        "\n\n...[诊断包截断：已省略中间 {omitted} 字节，以下为文件末尾]...\n\n"
    )?;

    source.seek(SeekFrom::Start(tail_from))?;
    io::copy(source, zip)?;

    Ok(())
}

fn read_up_to(source: &mut fs::File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;

    while filled < buffer.len() {
        let read = source.read(&mut buffer[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }

    Ok(filled)
}

/// 优先在 `max` 之前的窗口内按换行截断，避免切断整行与 UTF-8 字符。
fn line_break_cut(buffer: &[u8], max: usize) -> usize {
    let limit = buffer.len().min(max);
    if limit == 0 {
        return 0;
    }

    let window_start = limit.saturating_sub(TRUNCATION_WINDOW_BYTES);
    if let Some(position) = buffer[window_start..limit]
        .iter()
        .rposition(|byte| *byte == b'\n')
    {
        return window_start + position + 1;
    }

    utf8_prefix_len(&buffer[..limit])
}

const fn utf8_prefix_len(bytes: &[u8]) -> usize {
    if let Err(error) = std::str::from_utf8(bytes)
        && bytes.len().saturating_sub(error.valid_up_to()) <= 3
    {
        return error.valid_up_to();
    }

    bytes.len()
}

fn utf8_prefix_skip(bytes: &[u8]) -> usize {
    let mut skip = 0;
    while skip < 3 && bytes.get(skip).is_some_and(|byte| byte & 0xC0 == 0x80) {
        skip += 1;
    }

    skip
}
