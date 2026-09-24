//! 窗口辅助：需要用户在浏览器里操作（登录授权）之后，把管理器窗口带回前台。

/// 把管理器控制台窗口还原并切到前台；抢不到前台时闪烁任务栏提醒
#[cfg(windows)]
pub fn focus_console() {
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId, IsWindowVisible, SW_RESTORE,
        SetForegroundWindow, ShowWindow,
    };

    let window = unsafe { GetConsoleWindow() };
    if window.is_null() || unsafe { IsWindowVisible(window) } == 0 {
        return;
    }

    // 直接 SetForegroundWindow 会被前台锁定拦下，先把自己挂到当前前台线程上
    let focused = unsafe {
        let foreground = GetForegroundWindow();
        let foreground_thread = GetWindowThreadProcessId(foreground, std::ptr::null_mut());
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
fn flash_window(window: *mut core::ffi::c_void) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FLASHW_ALL, FLASHW_TIMERNOFG, FLASHWINFO, FlashWindowEx,
    };

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
pub fn focus_console() {}
