//! 进程退出清理与控制台事件处理。

use crate::platform::set_console_ctrl_handler;
use crate::telemetry::{report_event, shutdown};

use std::{
    mem::take,
    panic::{AssertUnwindSafe, catch_unwind},
    process,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::channel,
    },
    thread::spawn,
    time::{Duration, Instant},
};

type CleanupCallback = Box<dyn Fn() + Send + 'static>;

// 退出超时
/// 退出时等待清理与上报线程的总时长上限。
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

// 控制台事件类型
const CTRL_BREAK_EVENT: u32 = 1;
const CTRL_C_EVENT: u32 = 0;
const CTRL_CLOSE_EVENT: u32 = 2;
const CTRL_LOGOFF_EVENT: u32 = 5;
const CTRL_SHUTDOWN_EVENT: u32 = 6;

/// 注册控制台退出事件处理器，让中断/关机事件走统一清理流程。
pub fn install_console_handler() {
    unsafe extern "system" fn handler(ctrl_type: u32) -> i32 {
        if matches!(
            ctrl_type,
            CTRL_C_EVENT
                | CTRL_BREAK_EVENT
                | CTRL_CLOSE_EVENT
                | CTRL_LOGOFF_EVENT
                | CTRL_SHUTDOWN_EVENT
        ) {
            run_shutdown();
            process::exit(0);
        } else {
            0
        }
    }

    set_console_ctrl_handler(handler);
}

static CALLBACKS: OnceLock<Mutex<Vec<Option<CleanupCallback>>>> = OnceLock::new();
static SHUTDOWN_STARTED: AtomicBool = AtomicBool::new(false);

/// 注册清理回调；程序正常退出或收到中断事件时执行。
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

/// 并发执行所有清理回调，总耗时不超过 [`SHUTDOWN_TIMEOUT`]；重复调用只生效一次。
pub fn run_shutdown() {
    if SHUTDOWN_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    report_event("Shutdown", None);

    let to = SHUTDOWN_TIMEOUT;
    let callbacks: Vec<CleanupCallback> = CALLBACKS.get().map_or_else(Vec::new, |m| {
        let mut guard = match m.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        take(&mut *guard).into_iter().flatten().collect()
    });

    if callbacks.is_empty() {
        shutdown(Some(to));
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

    shutdown(Some(remaining));
}
