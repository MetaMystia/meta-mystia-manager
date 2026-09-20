//! Win32 API 辅助函数

/// Win32 API 以字节为单位的 DWORD 长度参数
///
/// 调用方传入的缓冲区都远小于 4 GiB，超出 u32 时按上限截断，由 API 自行报错。
pub fn dword_len(bytes: usize) -> u32 {
    u32::try_from(bytes).unwrap_or(u32::MAX)
}
