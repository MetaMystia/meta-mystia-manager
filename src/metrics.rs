use crate::config::USER_AGENT;
use crate::error::Result;
use crate::net::read_system_proxy;
use crate::shutdown::SHUTDOWN_TIMEOUT;

use native_tls::TlsConnector;
use percent_encoding::{NON_ALPHANUMERIC, percent_encode};
use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::thread::{JoinHandle, spawn};
use std::time::Duration;

const ID_SITE: &str = "13";
const TRACKING_ENDPOINT: &str = "https://track.izakaya.cc/api.php";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

fn build_tracking_url(user_id: &str, params: &HashMap<&str, String>) -> String {
    let mut base = vec![
        ("idsite".to_string(), ID_SITE.to_string()),
        ("rec".to_string(), "1".to_string()),
        ("_id".to_string(), user_id.to_string()),
        ("uid".to_string(), user_id.to_string()),
    ];

    for (k, v) in params.iter() {
        base.push((k.to_string(), v.clone()));
    }

    let q: String = base
        .into_iter()
        .map(|(k, v)| format!("{}={}", k, percent_encode(v.as_bytes(), NON_ALPHANUMERIC)))
        .collect::<Vec<_>>()
        .join("&");

    format!("{}?{}", TRACKING_ENDPOINT, q)
}

fn read_machine_guid() -> Option<String> {
    let out = Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Cryptography",
            "/v",
            "MachineGuid",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }

    let s = String::from_utf8_lossy(&out.stdout);
    for line in s.lines() {
        let t = line.trim();
        if t.starts_with("MachineGuid") {
            let parts: Vec<&str> = t.split_whitespace().collect();
            if let Some(val) = parts.last() {
                return Some(val.to_string());
            }
        }
    }

    None
}

fn md5_hex(input: &str) -> String {
    format!("{:x}", md5::compute(input))
}

static CACHED_USER_ID: OnceLock<String> = OnceLock::new();

pub fn get_user_id() -> String {
    CACHED_USER_ID
        .get_or_init(|| {
            if let Some(guid) = read_machine_guid() {
                return md5_hex(&guid);
            }

            let hostname = std::env::var("COMPUTERNAME").unwrap_or_default();
            let username = std::env::var("USERNAME").unwrap_or_default();
            let combined = format!("{}|{}", hostname, username);

            md5_hex(&combined)
        })
        .clone()
}

static CACHED_AGENT: OnceLock<ureq::Agent> = OnceLock::new();

fn get_agent() -> &'static ureq::Agent {
    CACHED_AGENT.get_or_init(|| {
        let tls = TlsConnector::new().expect("TLS init failed");
        let mut builder = ureq::AgentBuilder::new()
            .tls_connector(Arc::new(tls))
            .timeout(DEFAULT_TIMEOUT)
            .user_agent(USER_AGENT);
        if let Some(proxy) = read_system_proxy()
            && let Ok(p) = ureq::Proxy::new(&proxy)
        {
            builder = builder.proxy(p);
        }
        builder.build()
    })
}

fn send_with_client(url: String) {
    let _ = get_agent().get(&url).call();
}

struct TrackingWorker {
    sender: Sender<String>,
    handle: JoinHandle<()>,
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
            send_with_client(url);
        }
    });
    let worker = TrackingWorker {
        sender: tx.clone(),
        handle,
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
        spawn(move || send_with_client(e.0));
    }
}

fn join_handle_with_timeout(h: JoinHandle<()>, timeout: Duration) -> bool {
    let (tx, rx) = channel::<()>();

    spawn(move || {
        let _ = h.join();
        let _ = tx.send(());
    });

    match rx.recv_timeout(timeout) {
        Ok(_) => true,
        Err(RecvTimeoutError::Timeout) => false,
        Err(_) => true,
    }
}

pub fn shutdown(timeout: Option<Duration>) -> Result<()> {
    let Some(m) = TRACKING_WORKER.get() else {
        return Ok(());
    };

    let to = timeout.unwrap_or(SHUTDOWN_TIMEOUT);
    let mut guard = match m.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };

    if let Some(worker) = guard.take() {
        drop(guard);
        let _ = join_handle_with_timeout(worker.handle, to);
    }

    Ok(())
}

pub fn report_event(action: &str, name: Option<&str>) {
    if cfg!(debug_assertions) {
        return;
    }

    let user_id = get_user_id();

    let mut params: HashMap<&str, String> = HashMap::new();
    params.insert("ca", "1".to_string());
    params.insert("e_c", "Manager".to_string());
    params.insert("e_a", action.to_string());
    if let Some(n) = name {
        params.insert("e_n", n.to_string());
    }

    let url = build_tracking_url(&user_id, &params);
    send_tracking_request(url);
}
