//! 使用埋点上报与最近事件缓存。

use crate::http::{build_agent_with_timeouts, host_key};
use crate::platform::machine_id;
use crate::shutdown::SHUTDOWN_TIMEOUT;

use percent_encoding::{NON_ALPHANUMERIC, percent_encode};
use std::{
    collections::{HashMap, VecDeque},
    env,
    sync::{
        Mutex, OnceLock, PoisonError,
        mpsc::{RecvTimeoutError, Sender, channel},
    },
    thread::{JoinHandle, spawn},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// 上报
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REPORT_DETAIL_BYTES: usize = 1024;
const TRACKING_ENDPOINT: &str = "https://track.izakaya.cc/api.php";
const TRACKING_SITE_ID: &str = "13";

// 内存事件缓存
/// 内存中保留的最近事件条数上限（供诊断包导出）。
pub const MAX_RECENT_EVENTS: usize = 500;

static STARTED_AT: OnceLock<Instant> = OnceLock::new();
static RECENT_EVENTS: OnceLock<Mutex<VecDeque<RecentEvent>>> = OnceLock::new();

/// 一条内存中的操作记录，只在本次运行会话内保留。
#[derive(Clone)]
pub struct RecentEvent {
    /// 事件发生时刻（Unix 秒）
    pub at_epoch_secs: u64,
    /// 相对本次进程启动的秒数
    pub elapsed_secs: u64,
    /// `动作：明细`
    pub line: String,
}

fn build_tracking_url(visitor_id: &str, params: &HashMap<&str, String>) -> String {
    let mut base = vec![
        ("idsite".to_string(), TRACKING_SITE_ID.to_string()),
        ("rec".to_string(), "1".to_string()),
        ("_id".to_string(), visitor_id.to_string()),
        ("uid".to_string(), visitor_id.to_string()),
    ];

    for (k, v) in params {
        base.push((k.to_string(), v.clone()));
    }

    let q: String = base
        .into_iter()
        .map(|(k, v)| format!("{}={}", k, percent_encode(v.as_bytes(), NON_ALPHANUMERIC)))
        .collect::<Vec<_>>()
        .join("&");

    format!("{TRACKING_ENDPOINT}?{q}")
}

fn md5_hex(input: &str) -> String {
    format!("{:x}", md5::compute(input))
}

static CACHED_USER_ID: OnceLock<String> = OnceLock::new();

/// 稳定的匿名用户标识：机器标识的 MD5；取不到机器标识时回退为计算机名与用户名。
pub fn user_id() -> String {
    CACHED_USER_ID
        .get_or_init(|| {
            if let Some(machine_id) = machine_id() {
                return md5_hex(&machine_id);
            }

            let hostname = env::var("COMPUTERNAME").unwrap_or_default();
            let username = env::var("USERNAME").unwrap_or_default();
            let combined = format!("{hostname}|{username}");

            md5_hex(&combined)
        })
        .clone()
}

static AGENT_CACHE: OnceLock<Mutex<HashMap<String, ureq::Agent>>> = OnceLock::new();

fn send_with_client(url: &str) {
    // 埋点尽力而为，失败不影响主流程
    let agents = AGENT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = host_key(url);
    let agent = {
        let mut guard = agents.lock().unwrap_or_else(PoisonError::into_inner);

        guard
            .entry(key)
            .or_insert_with(|| {
                build_agent_with_timeouts(url, None, Some(DEFAULT_TIMEOUT), Some(DEFAULT_TIMEOUT))
            })
            .clone()
    };

    let _ = agent.get(url).call();
}

struct TrackingWorker {
    handle: JoinHandle<()>,
    sender: Sender<String>,
}

static TRACKING_WORKER: OnceLock<Mutex<Option<TrackingWorker>>> = OnceLock::new();

fn start_tracking_worker() -> Sender<String> {
    if let Some(m) = TRACKING_WORKER.get()
        && let Ok(guard) = m.lock()
        && let Some(w) = guard.as_ref()
    {
        return w.sender.clone();
    }

    let (tx, rx) = channel::<String>();

    let handle = spawn(move || {
        for url in rx {
            send_with_client(&url);
        }
    });
    let worker = TrackingWorker {
        handle,
        sender: tx.clone(),
    };

    let m = TRACKING_WORKER.get_or_init(|| Mutex::new(None));
    let mut guard = match m.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };

    if guard.is_none() {
        *guard = Some(worker);
    }

    guard.as_ref().map(|w| w.sender.clone()).unwrap_or(tx)
}

fn send_tracking_request(url: String) {
    let sender = start_tracking_worker();
    if let Err(e) = sender.send(url) {
        spawn(move || send_with_client(&e.0));
    }
}

fn join_handle_with_timeout(h: JoinHandle<()>, timeout: Duration) -> bool {
    let (tx, rx) = channel::<()>();

    spawn(move || {
        let _ = h.join();
        let _ = tx.send(());
    });

    !matches!(rx.recv_timeout(timeout), Err(RecvTimeoutError::Timeout))
}

/// 等待上报线程收尾；`timeout` 为 `None` 时使用默认上限。
pub fn shutdown(timeout: Option<Duration>) {
    let Some(m) = TRACKING_WORKER.get() else {
        return;
    };

    let to = timeout.unwrap_or(SHUTDOWN_TIMEOUT);
    let mut guard = match m.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };

    if let Some(worker) = guard.take() {
        drop(guard);
        let TrackingWorker { handle, sender } = worker;
        drop(sender);
        let _ = join_handle_with_timeout(handle, to);
    }
}

/// 记录一次操作事件：写入内存事件缓存，并在 release 构建中异步上报。
pub fn report_event(action: &str, name: Option<&str>) {
    record_recent_event(action, name);

    if cfg!(debug_assertions) {
        return;
    }

    let visitor_id = user_id();

    let mut params: HashMap<&str, String> = HashMap::new();
    params.insert("ca", "1".to_string());
    params.insert("e_c", "Manager".to_string());
    params.insert("e_a", action.to_string());
    if let Some(n) = name {
        params.insert("e_n", truncate_for_report(n));
    }

    let url = build_tracking_url(&visitor_id, &params);
    send_tracking_request(url);
}

/// 最近事件（只留在内存里，供诊断包导出）。
pub fn recent_events() -> Vec<RecentEvent> {
    RECENT_EVENTS.get().map_or_else(Vec::new, |events| {
        events
            .lock()
            .map(|events| events.iter().cloned().collect())
            .unwrap_or_default()
    })
}

fn record_recent_event(action: &str, name: Option<&str>) {
    let events = RECENT_EVENTS.get_or_init(|| Mutex::new(VecDeque::new()));
    let Ok(mut events) = events.lock() else {
        return;
    };

    if events.len() >= MAX_RECENT_EVENTS {
        events.pop_front();
    }

    let elapsed = STARTED_AT.get_or_init(Instant::now).elapsed().as_secs();
    let at_epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let line = name.map_or_else(|| action.to_string(), |name| format!("{action}：{name}"));

    events.push_back(RecentEvent {
        at_epoch_secs,
        elapsed_secs: elapsed,
        line,
    });
}

fn truncate_for_report(detail: &str) -> String {
    let ellipsis = '…';
    let ellipsis_len = encoded_char_len(ellipsis);
    let mut used = 0;
    let mut truncated = String::new();

    for ch in detail.chars() {
        let len = encoded_char_len(ch);
        if used + len + ellipsis_len > MAX_REPORT_DETAIL_BYTES {
            truncated.push(ellipsis);
            return truncated;
        }

        used += len;
        truncated.push(ch);
    }

    truncated
}

fn encoded_char_len(ch: char) -> usize {
    let mut buffer = [0u8; 4];

    percent_encode(ch.encode_utf8(&mut buffer).as_bytes(), NON_ALPHANUMERIC)
        .map(str::len)
        .sum()
}
