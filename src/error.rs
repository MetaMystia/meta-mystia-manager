use crate::metrics::report_event;

use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ManagerError {
    #[error("解压失败：{0}")]
    ExtractFailed(String),

    #[error("文件被占用：{0}")]
    FileInUse(String),

    #[error("未在游戏根目录下运行")]
    GameNotFound,

    #[error("游戏正在运行，请关闭游戏后重试")]
    GameRunning,

    /// HTTP 4xx（429 除外）：请求本身有问题，重试无意义
    #[error("{0}")]
    HttpError(String),

    #[error("版本信息无效或解析失败")]
    InvalidVersionInfo,

    #[error("IO 错误：{0}")]
    Io(#[source] io::Error),

    #[error("网络错误：{0}")]
    NetworkError(String),

    #[error("其他错误：{0}")]
    Other(String),

    #[error("权限不足：{0}")]
    PermissionDenied(String),

    #[error("进程列表错误：{0}")]
    #[cfg(windows)]
    ProcessListError(String),

    #[error("{0}")]
    RateLimited(String, Option<u64>),

    /// 服务端返回的错误，文案可直接展示给用户
    #[error("{0}")]
    ServiceError(String),

    #[error("下载速度过慢：{0}")]
    SlowDownload(String),

    #[error("{0}")]
    SsoLoginFailed(String),

    #[error("卸载未完成：{0}")]
    UninstallIncomplete(String),

    #[error("用户取消了操作")]
    UserCancelled,
}

impl ManagerError {
    /// 网络/IO 类问题可通过重试恢复；服务端语义错误、用户取消等重试无意义
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::NetworkError(_) | Self::RateLimited(..) => true,
            Self::Io(err) => matches!(
                err.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::Interrupted
                    | io::ErrorKind::TimedOut
                    | io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::WouldBlock
            ),
            _ => false,
        }
    }
}

pub fn service_error(scope: &str, code: &str, status: u16) -> ManagerError {
    ManagerError::ServiceError(format!(
        "{scope}失败：{}",
        service_error_reason(code, status)
    ))
}

pub fn service_error_reason(code: &str, status: u16) -> String {
    let code = code.trim();
    let http_status = code
        .strip_prefix("http-")
        .and_then(|value| value.parse::<u16>().ok());

    match code {
        "" if status >= 500 => format!("服务端暂时不可用（HTTP {status}），请稍后再试"),
        "" => format!("服务端返回异常（HTTP {status}）"),
        "client-disabled" => "该管理工具已被停用，请联系管理员".to_string(),
        "feature-disabled" => "服务端未启用该功能，请联系管理员".to_string(),
        "internal-error" => "服务端出错，请稍后再试".to_string(),
        "invalid-client" | "unknown-client" => "管理工具配置无效，请升级管理工具后重试".to_string(),
        "invalid-request" => "请求格式异常，请升级管理工具后重试".to_string(),
        "invalid-ticket" | "invalid-session" => "授权已过期，请重新登录后重试".to_string(),
        "not-found" => {
            "服务器上没有该文件（可能已下架或未同步），请更换版本或升级管理工具".to_string()
        }
        "sso-unreachable" => "登录服务暂时不可用，请稍后再试".to_string(),
        "too-many-keys" => "同时下载的文件过多，请稍后再试".to_string(),
        "too-many-requests" => "请求过于频繁，请稍后再试".to_string(),
        "user-blocked" => "当前账号已被限制下载，请联系管理员".to_string(),
        "user-deleted" => "当前账号已被删除".to_string(),
        "user-disabled" | "disabled" => "当前账号已被禁用".to_string(),
        "user-not-found" => "当前账号不可用，请联系管理员".to_string(),
        _ if http_status.is_some_and(|status| (500..=599).contains(&status)) => {
            "服务端暂时不可用，请稍后再试".to_string()
        }
        _ if http_status.is_some_and(|status| status == 401 || status == 403) => {
            "登录状态无效或没有访问权限，请重新登录后重试".to_string()
        }
        _ if http_status.is_some() => {
            format!("服务端返回异常（HTTP {}）", http_status.unwrap_or_default())
        }
        _ if code.contains(char::is_whitespace) || !code.is_ascii() => code.to_string(),
        other => format!("服务端返回异常（{other}）"),
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
