//! 东方夜雀食堂小助手 SSO 登录门禁
//!
//! 只在交互式安装/升级前使用：先说明、再让用户确认，然后用系统浏览器完成授权，
//! 最后用回调中的一次性 ticket 换取账号资料。会话只保存在当前进程内存里，退出即失效。

mod browser;
mod crypto;
mod exchange;
mod loopback;
mod pkce;

use crate::error::Result;
use crate::metrics;
use crate::net::build_agent;
use crate::remote_config;
use crate::ui::Ui;
use crate::window;

use percent_encoding::{NON_ALPHANUMERIC, percent_encode};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

const SSO_CLIENT_ID: &str = "meta-mystia-manager";
const AGENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const AGENT_GLOBAL_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct AccountSession {
    pub download_token: String,
    pub user_id: String,
    pub username: String,
    pub nickname: Option<String>,
}

impl AccountSession {
    /// 展示用名称：优先昵称，昵称为空时退回用户名
    fn display_name(&self) -> String {
        match self.nickname.as_deref().map(str::trim) {
            Some(nickname) if !nickname.is_empty() => format!("{nickname}（{}）", self.username),
            _ => self.username.clone(),
        }
    }
}

static SESSION: OnceLock<Mutex<Option<AccountSession>>> = OnceLock::new();
static CACHED_AGENT: OnceLock<ureq::Agent> = OnceLock::new();

/// 确保当前进程已登录
///
/// - 已登录：直接复用当次会话并返回 `true`
/// - 用户拒绝确认、在浏览器中取消或等待超时：返回 `false`，调用方回到操作菜单
/// - 真正的失败（网络异常、客户端失效、账号不可用等）：返回 `Err`
pub fn ensure_logged_in(ui: &dyn Ui, config_url: &str) -> Result<bool> {
    if current_account().is_some() {
        return Ok(true);
    }

    login(ui, config_url)
}

pub fn current_account() -> Option<AccountSession> {
    let slot = SESSION
        .get()?
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    slot.clone()
}

/// 清除当前登录态；下载会话被服务端判定失效时调用，下次操作会重新登录
pub fn clear_session() {
    if let Some(slot) = SESSION.get() {
        *slot.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// 当前登录态对应的下载会话 token（用于申请一次性下载密钥）
pub fn current_download_token() -> Option<String> {
    current_account().map(|account| account.download_token)
}

fn login(ui: &dyn Ui, config_url: &str) -> Result<bool> {
    ui.blank_line()?;
    ui.message("需要登录东方夜雀食堂小助手账号才能继续（登录在浏览器中完成）。")?;

    let config = remote_config::get(ui, config_url)?;

    if !ui.sso_ask_open_browser()? {
        ui.message("已取消登录，未执行任何操作")?;
        ui.blank_line()?;
        return Ok(false);
    }

    let server = loopback::CallbackServer::bind()?;
    let pkce = pkce::create_pkce_pair()?;
    let state = pkce::create_state()?;
    let redirect_uri = server.redirect_uri();
    let authorize_url = create_authorize_url(
        &config.sso.authorize_origin,
        &redirect_uri,
        &state,
        &pkce.code_challenge,
    );

    if browser::open_url(&authorize_url).is_err() {
        ui.message(&format!(
            "如果浏览器没有自动打开，请手动访问：{authorize_url}"
        ))?;
    }
    ui.message("请在浏览器中完成登录并确认授权……")?;

    let ticket = match server.wait_for_callback(&state, LOGIN_TIMEOUT)? {
        loopback::CallbackOutcome::Authorized { ticket } => {
            window::focus_console();

            ticket
        }
        loopback::CallbackOutcome::Cancelled => {
            ui.message("已取消登录，未执行任何操作")?;
            ui.blank_line()?;
            return Ok(false);
        }
        loopback::CallbackOutcome::TimedOut => {
            ui.message("等待登录超时，未执行任何操作")?;
            ui.blank_line()?;
            return Ok(false);
        }
    };

    let session = exchange::create_session(
        get_agent(),
        &config.sso.session_url,
        SSO_CLIENT_ID,
        &ticket,
        &pkce.code_verifier,
    )?;
    let session = AccountSession {
        download_token: session.download_token,
        nickname: session.nickname,
        user_id: session.user_id,
        username: session.username,
    };

    store_session(session.clone());
    metrics::set_account_user_id(&session.user_id);
    ui.message(&format!("已登录：{}", session.display_name()))?;
    ui.blank_line()?;

    Ok(true)
}

fn create_authorize_url(
    authorize_origin: &str,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> String {
    let encoded_redirect_uri = percent_encode(redirect_uri.as_bytes(), NON_ALPHANUMERIC);
    let origin = authorize_origin.trim_end_matches('/');

    format!(
        "{origin}/api/v1/sso/authorize?client_id={SSO_CLIENT_ID}\
         &redirect_uri={encoded_redirect_uri}&state={state}&code_challenge={code_challenge}"
    )
}

fn store_session(session: AccountSession) {
    let slot = SESSION.get_or_init(|| Mutex::new(None));
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(session);
}

fn get_agent() -> &'static ureq::Agent {
    CACHED_AGENT
        .get_or_init(|| build_agent(Some(AGENT_CONNECT_TIMEOUT), Some(AGENT_GLOBAL_TIMEOUT)))
}
