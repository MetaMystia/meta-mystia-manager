//! 换票：把浏览器回调拿到的一次性 ticket 换成账号资料

use crate::error::{ManagerError, Result};
use crate::sso::{SSO_CLIENT_ID, SSO_CLIENT_SECRET};

use serde::Deserialize;

/// 换票 API 的入口短链：先解析它的重定向目标得到 API origin，再拼接 `VALIDATE_PATH`。
/// 这样客户端不写死具体域名，服务端换域名时无需重新发版。
const API_ORIGIN_URL: &str = "https://url.izakaya.cc/assistant-bff";
/// 换票接口路径
const VALIDATE_PATH: &str = "/api/v1/sso/validate";

/// 换票成功后的账号资料
pub struct AccountProfile {
    pub user_id: String,
    pub username: String,
    pub nickname: Option<String>,
}

#[derive(Deserialize)]
struct ValidateEnvelope {
    status: String,
    #[serde(default)]
    data: Option<ValidateData>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
struct ValidateData {
    user: ValidateUser,
}

#[derive(Deserialize)]
struct ValidateUser {
    id: String,
    username: String,
    #[serde(default)]
    nickname: Option<String>,
}

/// 用 `ticket` 与 `code_verifier` 换取账号资料
pub fn validate_ticket(
    agent: &ureq::Agent,
    ticket: &str,
    code_verifier: &str,
) -> Result<AccountProfile> {
    let validate_url = resolve_validate_url(agent)?;
    let request_body = serde_json::json!({
        "client_id": SSO_CLIENT_ID,
        "client_secret": SSO_CLIENT_SECRET,
        "ticket": ticket,
        "code_verifier": code_verifier,
    });
    let request_body = serde_json::to_string(&request_body)
        .map_err(|e| ManagerError::SsoLoginFailed(format!("登录失败：无法构造请求（{e}）")))?;

    // Agent 关闭了 http_status_as_error，因此 4xx/5xx 也会回到这里，由错误码决定提示
    let response = agent
        .post(validate_url.as_str())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send(request_body.as_str())
        .map_err(transport_error)?;

    let http_status = response.status().as_u16();
    let text = response
        .into_body()
        .read_to_string()
        .map_err(|e| ManagerError::SsoLoginFailed(format!("登录失败：无法读取响应（{e}）")))?;

    let envelope: ValidateEnvelope = serde_json::from_str(&text).map_err(|e| {
        ManagerError::SsoLoginFailed(format!("登录失败：无法解析登录服务响应（{e}）"))
    })?;

    if envelope.status == "ok" {
        let data = envelope.data.ok_or_else(|| {
            ManagerError::SsoLoginFailed("登录失败：登录服务响应缺少账号资料".to_string())
        })?;

        return Ok(AccountProfile {
            nickname: data.user.nickname,
            user_id: data.user.id,
            username: data.user.username,
        });
    }

    Err(login_failure_message(
        envelope.message.as_deref().unwrap_or_default(),
        http_status,
    ))
}

/// 把服务端错误码映射为面向用户的提示
fn login_failure_message(code: &str, http_status: u16) -> ManagerError {
    let message = match code {
        "invalid-object-structure" => "登录失败：请求格式异常，请升级管理器后重试".to_string(),
        "invalid-client" => "登录失败：管理器登录配置无效，请升级管理器后重试".to_string(),
        "invalid-ticket" => "登录失败：授权已过期，请重新尝试".to_string(),
        "client-disabled" => "登录失败：该管理器的登录已被停用，请联系管理员".to_string(),
        "user-disabled" => "登录失败：当前账号已被禁用".to_string(),
        "user-deleted" => "登录失败：当前账号已被删除".to_string(),
        "feature-disabled" => "登录失败：服务端未启用登录功能".to_string(),
        "too-many-requests" => "登录失败：请求过于频繁，请稍后再试".to_string(),
        "" => format!("登录失败：登录服务返回 HTTP {http_status}"),
        other => format!("登录失败：登录服务返回 {other}"),
    };

    ManagerError::SsoLoginFailed(message)
}

/// 解析 API 入口短链，返回换票接口的完整地址
fn resolve_validate_url(agent: &ureq::Agent) -> Result<String> {
    let response = agent
        .get(API_ORIGIN_URL)
        .config()
        .max_redirects(0)
        .max_redirects_will_error(false)
        .build()
        .call()
        .map_err(transport_error)?;

    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ManagerError::SsoLoginFailed("登录失败：无法解析登录服务地址".to_string())
        })?;
    let uri: ureq::http::Uri = location
        .parse()
        .map_err(|_| ManagerError::SsoLoginFailed("登录失败：登录服务地址格式无效".to_string()))?;

    // secret 以明文放在请求体里，解析出的地址必须是 HTTPS
    let scheme = uri.scheme_str().unwrap_or_default();
    if scheme != "https" {
        return Err(ManagerError::SsoLoginFailed(
            "登录失败：登录服务地址不是 HTTPS".to_string(),
        ));
    }
    let authority = uri.authority().ok_or_else(|| {
        ManagerError::SsoLoginFailed("登录失败：登录服务地址缺少主机名".to_string())
    })?;

    Ok(format!("{scheme}://{authority}{VALIDATE_PATH}"))
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
