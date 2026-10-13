//! 统一错误类型与服务端错误文案。

use crate::i18n::{t, tf, TextId};

use std::io;
use thiserror::Error;

/// 管理工具的统一错误类型。
#[derive(Debug, Error)]
pub enum ManagerError {
    /// 解压失败，附原因
    #[error("解压失败：{0}")]
    ExtractFailed(String),

    /// 目标文件被占用，附路径
    #[error("文件被占用：{0}")]
    FileInUse(String),

    /// 未在游戏根目录下运行
    #[error("未在游戏根目录下运行")]
    GameNotFound,

    /// 游戏正在运行，操作被拒绝
    #[error("游戏正在运行，请关闭游戏后重试")]
    GameRunning,

    /// HTTP 4xx（429 除外）：请求本身有问题，重试无意义
    #[error("{0}")]
    HttpError(String),

    /// 版本信息缺失或无法解析
    #[error("版本信息无效或解析失败")]
    InvalidVersionInfo,

    /// IO 错误，附底层错误信息
    #[error("IO 错误：{0}")]
    Io(#[source] io::Error),

    /// 网络错误，附原因
    #[error("网络错误：{0}")]
    NetworkError(String),

    /// 未归类的其他错误
    #[error("其他错误：{0}")]
    Other(String),

    /// 权限不足，附路径或原因
    #[error("权限不足：{0}")]
    PermissionDenied(String),

    /// 枚举系统进程失败，附原因
    #[error("进程列表错误：{0}")]
    #[cfg(windows)]
    ProcessListError(String),

    /// 服务端限流；第二个字段是建议等待秒数
    #[error("{0}")]
    RateLimited(String, Option<u64>),

    /// 服务端返回的错误，文案可直接展示给用户
    #[error("{0}")]
    ServiceError(String),

    /// 下载速度低于阈值被中止
    #[error("下载速度过慢：{0}")]
    SlowDownload(String),

    /// SSO 登录失败，附原因
    #[error("{0}")]
    SsoLoginFailed(String),

    /// 卸载过程未完成，附原因
    #[error("卸载未完成：{0}")]
    UninstallIncomplete(String),

    /// 用户主动取消了操作
    #[error("用户取消了操作")]
    UserCancelled,
}

impl ManagerError {
    /// 面向界面的本地化文案；服务端下发的自由文本按原文返回。
    #[must_use]
    pub fn localized(&self) -> String {
        match self {
            Self::ExtractFailed(detail) => tf(TextId::Err_ExtractFailed, &[("error", detail)]),
            Self::FileInUse(detail) => tf(TextId::Err_FileInUse, &[("error", detail)]),
            Self::GameNotFound => t(TextId::Err_NotInGameRoot).to_string(),
            Self::GameRunning => t(TextId::Err_GameRunning).to_string(),
            Self::HttpError(message)
            | Self::RateLimited(message, _)
            | Self::ServiceError(message)
            | Self::SsoLoginFailed(message) => message.clone(),
            Self::InvalidVersionInfo => t(TextId::Err_InvalidVersion).to_string(),
            Self::Io(error) => tf(TextId::Err_Io, &[("error", &error.to_string())]),
            Self::NetworkError(detail) => tf(TextId::Err_Network, &[("error", detail)]),
            Self::Other(detail) => tf(TextId::Err_Other, &[("error", detail)]),
            Self::PermissionDenied(detail) => tf(TextId::Err_Permission, &[("error", detail)]),
            #[cfg(windows)]
            Self::ProcessListError(detail) => {
                tf(TextId::Err_ProcessList, &[("error", detail)])
            }
            Self::SlowDownload(detail) => tf(TextId::Err_SlowDownload, &[("error", detail)]),
            Self::UninstallIncomplete(detail) => {
                tf(TextId::Err_UninstallIncomplete, &[("error", detail)])
            }
            Self::UserCancelled => t(TextId::Err_UserCancelled).to_string(),
        }
    }

    /// 网络/IO 类问题可通过重试恢复；服务端语义错误、用户取消等重试无意义。
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

/// 由服务端错误码与 HTTP 状态构造用户可读错误。
pub fn service_error(scope: &str, code: &str, status: u16) -> ManagerError {
    ManagerError::ServiceError(tf(
        TextId::Err_ScopeFailed,
        &[("scope", scope), ("reason", &service_error_reason(code, status))],
    ))
}

/// 把服务端错误码翻译成用户可读文案。
pub fn service_error_reason(code: &str, status: u16) -> String {
    let code = code.trim();
    let http_status = code
        .strip_prefix("http-")
        .and_then(|value| value.parse::<u16>().ok());

    match code {
        "" if status >= 500 => {
            tf(TextId::Err_ServerUnavailable, &[("status", &status.to_string())])
        }
        "" => tf(TextId::Err_ServerHttp, &[("status", &status.to_string())]),
        "client-disabled" => t(TextId::Err_ClientDisabled).to_string(),
        "feature-disabled" => t(TextId::Err_FeatureDisabled).to_string(),
        "internal-error" => t(TextId::Err_ServerInternal).to_string(),
        "invalid-client" | "unknown-client" => t(TextId::Err_ClientInvalid).to_string(),
        "invalid-request" => t(TextId::Err_RequestInvalid).to_string(),
        "invalid-ticket" | "invalid-session" => t(TextId::Err_TicketInvalid).to_string(),
        "not-found" => t(TextId::Err_FileNotFound).to_string(),
        "sso-unreachable" => t(TextId::Err_SsoUnreachable).to_string(),
        "too-many-keys" => t(TextId::Err_TooManyKeys).to_string(),
        "too-many-requests" => t(TextId::Err_TooManyRequests).to_string(),
        "user-blocked" => t(TextId::Err_UserBlocked).to_string(),
        "user-deleted" => t(TextId::Err_UserDeleted).to_string(),
        "user-disabled" | "disabled" => t(TextId::Err_UserDisabled).to_string(),
        "user-not-found" => t(TextId::Err_UserNotFound).to_string(),
        _ if http_status.is_some_and(|status| (500..=599).contains(&status)) => {
            t(TextId::Err_ServerTemporary).to_string()
        }
        _ if http_status.is_some_and(|status| status == 401 || status == 403) => {
            t(TextId::Err_LoginInvalid).to_string()
        }
        _ if http_status.is_some() => {
            tf(
                TextId::Err_ServerHttp,
                &[("status", &http_status.unwrap_or_default().to_string())],
            )
        }
        _ if code.contains(char::is_whitespace) || !code.is_ascii() => code.to_string(),
        other => tf(TextId::Err_ServerOther, &[("other", other)]),
    }
}

impl From<ureq::Error> for ManagerError {
    /// 传输层错误；4xx/5xx 由 `net::check_response_status` 处理，不会走到这里。
    fn from(err: ureq::Error) -> Self {
        Self::NetworkError(tf(TextId::Err_RequestFailed, &[("error", &err.to_string())]))
    }
}

impl From<io::Error> for ManagerError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// 统一 `Result` 别名。
pub type Result<T> = std::result::Result<T, ManagerError>;
