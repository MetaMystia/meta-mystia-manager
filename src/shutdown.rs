use crate::metrics;

use std::{
    mem::take,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::channel,
    },
    thread::spawn,
    time::{Duration, Instant},
};

pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

type CleanupCallback = Box<dyn Fn() + Send + 'static>;
static CALLBACKS: OnceLock<Mutex<Vec<Option<CleanupCallback>>>> = OnceLock::new();
static SHUTDOWN_STARTED: AtomicBool = AtomicBool::new(false);

/// 注册清理回调；程序正常退出或收到中断事件时执行
pub fn register_cleanup<F>(f: F) -> usize
where
    F: Fn() + Send + 'static,
{
    let m = CALLBACKS.get_or_init(|| Mutex::new(Vec::new()));
    let mut guard = match m.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    guard.push(Some(Box::new(f)));

    guard.len() - 1
}

/// 并发执行所有清理回调，总耗时不超过 [`SHUTDOWN_TIMEOUT`]；重复调用只生效一次
pub fn run_shutdown() {
    if SHUTDOWN_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    metrics::report_event("Shutdown", None);

    let to = SHUTDOWN_TIMEOUT;
    let callbacks: Vec<CleanupCallback> = CALLBACKS.get().map_or_else(Vec::new, |m| {
        let mut guard = match m.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        take(&mut *guard).into_iter().flatten().collect()
    });

    if callbacks.is_empty() {
        metrics::shutdown(Some(to));
        return;
    }

    let (tx, rx) = channel::<usize>();

    let total = callbacks.len();
    let start = Instant::now();
    let deadline = start + to;

    for (i, cb) in callbacks.into_iter().enumerate() {
        let tx = tx.clone();
        spawn(move || {
            let _ = catch_unwind(AssertUnwindSafe(cb));
            let _ = tx.send(i);
        });
    }

    drop(tx);

    let mut completed = 0;

    while completed < total {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };

        match rx.recv_timeout(remaining) {
            Ok(_idx) => completed += 1,
            Err(_) => break,
        }
    }

    let elapsed = start.elapsed();
    let remaining = to.checked_sub(elapsed).unwrap_or(Duration::ZERO);

    metrics::shutdown(Some(remaining));
}
