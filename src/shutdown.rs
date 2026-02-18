use crate::metrics;

use std::mem::take;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Mutex, Once, OnceLock};
use std::thread::spawn;
use std::time::{Duration, Instant};

pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

type CleanupCallback = Box<dyn Fn() + Send + 'static>;
static CALLBACKS: OnceLock<Mutex<Vec<Option<CleanupCallback>>>> = OnceLock::new();
static SET_HANDLER: Once = Once::new();
static SHUTDOWN_STARTED: AtomicBool = AtomicBool::new(false);

const CTRL_C_EVENT: u32 = 0;
const CTRL_BREAK_EVENT: u32 = 1;
const CTRL_CLOSE_EVENT: u32 = 2;
const CTRL_LOGOFF_EVENT: u32 = 5;
const CTRL_SHUTDOWN_EVENT: u32 = 6;

unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> i32 {
    if matches!(
        ctrl_type,
        CTRL_C_EVENT
            | CTRL_BREAK_EVENT
            | CTRL_CLOSE_EVENT
            | CTRL_LOGOFF_EVENT
            | CTRL_SHUTDOWN_EVENT
    ) {
        run_shutdown();
        std::process::exit(0);
    } else {
        0
    }
}

unsafe extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}

fn ensure_handlers() {
    SET_HANDLER.call_once(|| unsafe {
        let _ = SetConsoleCtrlHandler(Some(console_ctrl_handler), 1);
    });
}

/// 注册一个清理回调函数
pub fn register_cleanup<F>(f: F) -> usize
where
    F: Fn() + Send + 'static,
{
    ensure_handlers();

    let m = CALLBACKS.get_or_init(|| Mutex::new(Vec::new()));
    let mut guard = match m.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    guard.push(Some(Box::new(f)));

    guard.len() - 1
}

/// 执行所有注册的清理回调函数
pub fn run_shutdown() {
    if SHUTDOWN_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    metrics::report_event("Shutdown", None);

    let to = SHUTDOWN_TIMEOUT;
    let callbacks: Vec<CleanupCallback> = if let Some(m) = CALLBACKS.get() {
        let mut guard = match m.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        take(&mut *guard).into_iter().flatten().collect()
    } else {
        Vec::new()
    };

    if callbacks.is_empty() {
        let _ = metrics::shutdown(Some(to));
        return;
    }

    let (tx, rx) = channel::<usize>();

    let total = callbacks.len();
    let start = Instant::now();
    let deadline = start + to;

    for (i, cb) in callbacks.into_iter().enumerate() {
        let tx = tx.clone();
        spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(cb));
            let _ = tx.send(i);
        });
    }

    drop(tx);

    let mut completed_flags = vec![false; total];
    let mut completed = 0;

    while completed < total {
        let now = Instant::now();
        if now >= deadline {
            break;
        }

        let remaining = deadline - now;
        match rx.recv_timeout(remaining) {
            Ok(idx) => {
                if !completed_flags[idx] {
                    completed_flags[idx] = true;
                    completed += 1;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                break;
            }
            Err(_) => {
                break;
            }
        }
    }

    let elapsed = start.elapsed();
    let remaining = if elapsed >= to {
        Duration::from_secs(0)
    } else {
        to - elapsed
    };

    let _ = metrics::shutdown(Some(remaining));
}
