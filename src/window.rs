//! 窗口辅助：需要用户在浏览器里操作（登录授权）之后，把管理工具窗口带回前台。

use std::sync::atomic::{AtomicIsize, Ordering};

#[cfg(windows)]
use std::{
    ffi::c_void,
    ptr::{null, null_mut},
};
#[cfg(windows)]
use windows_sys::Win32::{
    System::Threading::{AttachThreadInput, GetCurrentThreadId},
    UI::WindowsAndMessaging::{
        FLASHW_ALL, FLASHW_TIMERNOFG, FLASHWINFO, FindWindowW, FlashWindowEx, GetForegroundWindow,
        GetWindowThreadProcessId, IsWindow, SW_RESTORE, SetForegroundWindow, ShowWindow,
    },
};

/// 主窗口类名；单实例检测也用它找到已经运行的窗口
pub const MAIN_WINDOW_CLASS: &str = "MetaMystiaManager";

static MAIN_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// 界面创建主窗口后登记句柄，供登录回跳时切回前台
pub fn set_main_window(hwnd: isize) {
    MAIN_WINDOW.store(hwnd, Ordering::Relaxed);
}

/// 把管理工具窗口还原并切到前台；抢不到前台时闪烁任务栏提醒
#[cfg(windows)]
pub fn focus_manager_window() {
    let window = MAIN_WINDOW.load(Ordering::Relaxed) as *mut c_void;
    if window.is_null() || unsafe { IsWindow(window) } == 0 {
        return;
    }

    focus_window(window);
}

#[cfg(windows)]
pub fn focus_existing_manager_window() {
    let class: Vec<u16> = MAIN_WINDOW_CLASS.encode_utf16().chain([0]).collect();
    let window = unsafe { FindWindowW(class.as_ptr(), null()) };

    if !window.is_null() {
        focus_window(window);
    }
}

#[cfg(windows)]
fn focus_window(window: *mut c_void) {
    // 直接 SetForegroundWindow 会被前台锁定拦下，先把自己挂到当前前台线程上
    let focused = unsafe {
        let foreground = GetForegroundWindow();
        let foreground_thread = GetWindowThreadProcessId(foreground, null_mut());
        let current_thread = GetCurrentThreadId();
        let attached = foreground_thread != 0
            && foreground_thread != current_thread
            && AttachThreadInput(current_thread, foreground_thread, 1) != 0;

        ShowWindow(window, SW_RESTORE);
        let focused = SetForegroundWindow(window) != 0;

        if attached {
            AttachThreadInput(current_thread, foreground_thread, 0);
        }

        focused
    };

    if !focused {
        flash_window(window);
    }
}

#[cfg(windows)]
fn flash_window(window: *mut c_void) {
    let info = FLASHWINFO {
        cbSize: u32::try_from(size_of::<FLASHWINFO>()).unwrap_or(u32::MAX),
        dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
        dwTimeout: 0,
        hwnd: window,
        uCount: 3,
    };

    unsafe {
        FlashWindowEx(&raw const info);
    }
}

#[cfg(not(windows))]
pub const fn focus_manager_window() {}

#[cfg(not(windows))]
pub const fn focus_existing_manager_window() {}
