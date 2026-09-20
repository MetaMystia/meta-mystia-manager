use crate::metrics::report_event;

use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ManagerError {
    #[error("未在游戏根目录下运行")]
    GameNotFound,

    #[error("游戏正在运行，请关闭游戏后重试")]
    GameRunning,

    #[error("进程列表错误：{0}")]
    ProcessListError(String),

    #[error("权限不足：{0}")]
    PermissionDenied(String),

    #[error("文件被占用：{0}")]
    FileInUse(String),

    #[error("网络错误：{0}")]
    NetworkError(String),

    #[error("被限流：{0}")]
    RateLimited(String),

    #[error("下载速度过慢：{0}")]
    SlowDownload(String),

    #[error("解压失败：{0}")]
    ExtractFailed(String),

    #[error("版本信息无效或解析失败")]
    InvalidVersionInfo,

    #[error("IO 错误：{0}")]
    Io(#[source] io::Error),

    #[error("UI 错误：{0}")]
    Ui(String),

    #[error("其他错误：{0}")]
    Other(String),

    #[error("用户取消了操作")]
    UserCancelled,

    #[error("{0}")]
    SsoLoginFailed(String),
}

impl From<dialoguer::Error> for ManagerError {
    fn from(err: dialoguer::Error) -> Self {
        let s = err.to_string();
        report_event("Error.From.Ui", Some(&s));
        Self::Ui(s)
    }
}

impl From<ureq::Error> for ManagerError {
    /// 传输层错误；4xx/5xx 由 `net::check_response_status` 处理，不会走到这里
    fn from(err: ureq::Error) -> Self {
        Self::NetworkError(format!("请求失败：{err}"))
    }
}

impl From<io::Error> for ManagerError {
    fn from(err: io::Error) -> Self {
        let s = err.to_string();
        report_event("Error.From.Io", Some(&s));
        Self::Io(err)
    }
}

pub type Result<T> = std::result::Result<T, ManagerError>;
