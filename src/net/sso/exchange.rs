//! 换票：把浏览器回调拿到的一次性 ticket 交给下载服务，换回下载会话与账号资料。
//!
//! `client secret` 只保存在服务端，客户端不再直接调用主站的 validate 接口。

use crate::error::{ManagerError, Result, service_error};

use serde::Deserialize;

/// 换票成功后的账号资料与下载会话凭据。
pub struct DownloadSession {
    /// 换下载密钥用的会话令牌
    pub download_token: String,
    /// 昵称
    pub nickname: Option<String>,
    /// 服务端账号 ID
    pub user_id: String,
    /// 用户名
    pub username: String,
}

#[derive(Deserialize)]
struct SessionResponse {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    user: Option<SessionUser>,
}

#[derive(Deserialize)]
struct SessionUser {
    id: String,
    #[serde(default)]
    nickname: Option<String>,
    username: String,
}

/// 用一次性 ticket 与 PKCE verifier 换取账号会话。
pub fn create_session(
    agent: &ureq::Agent,
    session_url: &str,
    client_id: &str,
    ticket: &str,
    code_verifier: &str,
) -> Result<DownloadSession> {
    let request_body = serde_json::json!({
        "client_id": client_id,
        "ticket": ticket,
        "code_verifier": code_verifier,
    });
    let request_body = serde_json::to_string(&request_body)
        .map_err(|e| ManagerError::SsoLoginFailed(format!("登录失败：无法构造请求（{e}）")))?;

    // Agent 关闭了 http_status_as_error，因此 4xx/5xx 也会回到这里，由错误码决定提示
    let response = agent
        .post(session_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send(request_body.as_str())
        .map_err(transport_error)?;

    let http_status = response.status().as_u16();
    let text = response
        .into_body()
        .read_to_string()
        .map_err(|e| ManagerError::SsoLoginFailed(format!("登录失败：无法读取响应（{e}）")))?;

    let body: SessionResponse = serde_json::from_str(&text).map_err(|e| {
        ManagerError::SsoLoginFailed(format!("登录失败：无法解析登录服务响应（{e}）"))
    })?;

    let (Some(download_token), Some(user)) = (body.token, body.user) else {
        return Err(service_error(
            "登录",
            body.error.as_deref().unwrap_or_default(),
            http_status,
        ));
    };

    Ok(DownloadSession {
        download_token,
        nickname: user.nickname,
        user_id: user.id,
        username: user.username,
    })
}

fn transport_error(e: ureq::Error) -> ManagerError {
    match e {
        ureq::Error::Io(_)
        | ureq::Error::ConnectionFailed
        | ureq::Error::HostNotFound
        | ureq::Error::Timeout(_) => {
            ManagerError::SsoLoginFailed("登录需要联网：请检查网络后重试".to_string())
        }
        other => ManagerError::SsoLoginFailed(format!("登录失败：{other}")),
    }
}
