//! 本地回环回调服务
//!
//! 只绑定本地回环 `127.0.0.1` 的临时端口，固定路径 `/sso/callback`，收到一次有效回调即结束。
//! 回调可能是授权成功（带 `ticket` 与原始 `state`），也可能是用户在授权页取消
//! （带 `error=access_denied` 与原始 `state`）。

use crate::error::{ManagerError, Result};

use percent_encoding::percent_decode_str;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

pub const CALLBACK_PATH: &str = "/sso/callback";

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(30);
const CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REQUEST_BYTES: usize = 8 * 1024;

pub enum CallbackOutcome {
    Authorized { ticket: String },
    Cancelled,
    TimedOut,
}

#[derive(Debug, PartialEq, Eq)]
enum CallbackRequest {
    /// 授权成功
    Authorized { ticket: String, state: String },
    /// 用户取消授权
    Cancelled,
    /// 不是回调路径（例如浏览器自动请求 favicon），忽略
    Ignored,
    /// 是回调路径但缺少必要参数
    Malformed,
}

pub struct CallbackServer {
    listener: TcpListener,
    port: u16,
}

impl CallbackServer {
    /// 绑定本地回环的临时端口
    pub fn bind() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| ManagerError::SsoLoginFailed(format!("无法监听本地回调端口：{e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| ManagerError::SsoLoginFailed(format!("无法读取本地回调端口：{e}")))?
            .port();
        listener
            .set_nonblocking(true)
            .map_err(|e| ManagerError::SsoLoginFailed(format!("无法设置本地回调监听属性：{e}")))?;

        Ok(Self { listener, port })
    }

    /// 本次登录使用的回环回调地址
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}{CALLBACK_PATH}", self.port)
    }

    /// 等待回调，直到拿到有效回调或超时
    pub fn wait_for_callback(
        &self,
        expected_state: &str,
        timeout: Duration,
    ) -> Result<CallbackOutcome> {
        let deadline = Instant::now() + timeout;

        loop {
            if Instant::now() >= deadline {
                return Ok(CallbackOutcome::TimedOut);
            }

            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(outcome) = handle_connection(stream, expected_state) {
                        return Ok(outcome);
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(ACCEPT_POLL_INTERVAL);
                }
                Err(e) => {
                    return Err(ManagerError::SsoLoginFailed(format!(
                        "本地回调监听失败：{e}"
                    )));
                }
            }
        }
    }
}

/// 处理一次连接；返回 `Some` 表示本次回调已经给出结论
fn handle_connection(mut stream: TcpStream, expected_state: &str) -> Option<CallbackOutcome> {
    let _ = stream.set_read_timeout(Some(CONNECTION_READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONNECTION_READ_TIMEOUT));

    let Some(request_target) = read_request_target(&mut stream) else {
        write_response(
            &mut stream,
            "400 Bad Request",
            "无法读取请求",
            "请返回管理器重试。",
        );
        return None;
    };

    match parse_request_target(&request_target) {
        CallbackRequest::Ignored => {
            write_response(&mut stream, "404 Not Found", "页面不存在", "");
            None
        }
        CallbackRequest::Malformed => {
            write_response(&mut stream, "400 Bad Request", "回调参数不完整", "");
            None
        }
        CallbackRequest::Cancelled => {
            write_response(
                &mut stream,
                "200 OK",
                "已取消登录",
                "可以关闭本页面并返回管理器。",
            );
            Some(CallbackOutcome::Cancelled)
        }
        CallbackRequest::Authorized { ticket, state } => {
            if state != expected_state {
                write_response(&mut stream, "400 Bad Request", "回调校验失败", "");
                return None;
            }

            write_response(
                &mut stream,
                "200 OK",
                "登录完成",
                "可以关闭本页面并返回管理器。",
            );
            Some(CallbackOutcome::Authorized { ticket })
        }
    }
}

/// 读取并解析请求行中的请求目标
fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];

    while buffer.len() < MAX_REQUEST_BYTES {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);

        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    let text = String::from_utf8_lossy(&buffer);
    let request_line = text.lines().next()?;
    let mut parts = request_line.split(' ');
    let method = parts.next()?;
    let target = parts.next()?;

    (method == "GET" && target.starts_with('/')).then(|| target.to_string())
}

fn parse_request_target(target: &str) -> CallbackRequest {
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target, ""),
    };
    if path != CALLBACK_PATH {
        return CallbackRequest::Ignored;
    }

    let mut error = None;
    let mut ticket = None;
    let mut state = None;

    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (raw_key, raw_value) = match pair.split_once('=') {
            Some((key, value)) => (key, value),
            None => (pair, ""),
        };
        let key = decode_query_component(raw_key);
        let value = decode_query_component(raw_value);

        match key.as_str() {
            "error" => error = Some(value),
            "ticket" => ticket = Some(value),
            "state" => state = Some(value),
            _ => {}
        }
    }

    if error.is_some() {
        return CallbackRequest::Cancelled;
    }

    match (ticket, state) {
        (Some(ticket), Some(state)) if !ticket.is_empty() && !state.is_empty() => {
            CallbackRequest::Authorized { ticket, state }
        }
        _ => CallbackRequest::Malformed,
    }
}

fn decode_query_component(value: &str) -> String {
    percent_decode_str(&value.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

/// 回写一个极简页面；浏览器可能提前断开，写失败不影响回调结论
fn write_response(stream: &mut TcpStream, status: &str, title: &str, hint: &str) {
    let body = format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\">\
         <title>{title}</title></head><body><h1>{title}</h1><p>{hint}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );

    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}
