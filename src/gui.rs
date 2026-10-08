//! 主界面：安装向导 + Win32 原生控件 + comctl32 v6（清单见 app.manifest）。
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    unsafe_op_in_unsafe_fn
)]

mod bridge;

use self::bridge::{Choices, Event, GuiUi, JOB_SLOTS, LocalInfo, RemoteInfo, Stage, WM_UI_EVENT};
use crate::config::GAME_EXECUTABLE;
use crate::env::{check_game_running, check_game_running_cached};
use crate::error::ManagerError;
use crate::mode::OperationMode;
use crate::ops::flow::{Input, run as run_flow};
use crate::ops::self_update::remove_replaced_exe;
use crate::platform::acquire_single_instance;
use crate::platform::{MAIN_WINDOW_CLASS, focus_existing_manager_window, set_main_window};
use crate::shutdown::{install_console_handler, run_shutdown};
use crate::telemetry::{report_event, user_id};
use crate::ui::{JobOutcome, Ui};
use crate::version::VersionInfo;

use std::{
    any::Any,
    ffi::c_void,
    mem, panic,
    path::{Path, PathBuf},
    process, ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM},
    Graphics::Gdi::{
        COLOR_BTNFACE, COLOR_GRAYTEXT, CreateFontIndirectW, DEFAULT_GUI_FONT, DeleteObject,
        FW_SEMIBOLD, GetDC, GetDeviceCaps, GetStockObject, GetSysColor, GetSysColorBrush,
        GetTextExtentPoint32W, HFONT, HGDIOBJ, LOGFONTW, LOGPIXELSY, RDW_ALLCHILDREN,
        RDW_INVALIDATE, RDW_UPDATENOW, RedrawWindow, ReleaseDC, SelectObject, SetBkMode,
        SetTextColor, TRANSPARENT, UpdateWindow,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{
            EM_REPLACESEL, EM_SETSEL, ICC_LINK_CLASS, ICC_PROGRESS_CLASS, ICC_STANDARD_CLASSES,
            INITCOMMONCONTROLSEX, InitCommonControlsEx, LoadIconWithScaleDown, NM_CLICK, NM_RETURN,
            NMHDR, PBM_SETPOS, PBM_SETRANGE32, SetWindowTheme,
        },
        Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled},
        Shell::{
            BFFM_INITIALIZED, BFFM_SETSELECTIONW, BIF_RETURNONLYFSDIRS, BROWSEINFOW, ILFree,
            SHBrowseForFolderW, SHGetPathFromIDListW, ShellExecuteW,
        },
        WindowsAndMessaging::{
            AdjustWindowRectEx, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_AUTORADIOBUTTON,
            BS_DEFPUSHBUTTON, BS_GROUPBOX, BS_PUSHBUTTON, CREATESTRUCTW, CreateWindowExW,
            DefWindowProcW, DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL, ES_AUTOVSCROLL,
            ES_MULTILINE, ES_READONLY, EnableMenuItem, GWLP_USERDATA, GetClientRect, GetMessageW,
            GetSystemMenu, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
            GetWindowTextLengthW, HICON, ICON_BIG, ICON_SMALL, IDC_ARROW, IDCANCEL, IDOK,
            IsDialogMessageW, IsWindow, LoadCursorW, LoadIconW, MB_ICONERROR, MB_ICONINFORMATION,
            MB_OK, MF_BYCOMMAND, MF_ENABLED, MF_GRAYED, MSG, MessageBoxW, MoveWindow,
            NONCLIENTMETRICSW, PostMessageW, PostQuitMessage, RegisterClassW, SC_CLOSE, SM_CXICON,
            SM_CXSCREEN, SM_CXSMICON, SM_CYICON, SM_CYSCREEN, SM_CYSMICON, SPI_GETNONCLIENTMETRICS,
            STM_SETICON, SW_HIDE, SW_SHOW, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOZORDER,
            SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, SetWindowTextW,
            ShowWindow, SystemParametersInfoW, TranslateMessage, WM_CLOSE, WM_COMMAND, WM_CREATE,
            WM_CTLCOLORSTATIC, WM_DESTROY, WM_NOTIFY, WM_SETFONT, WM_SETICON, WNDCLASSW, WS_BORDER,
            WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT, WS_EX_DLGMODALFRAME,
            WS_MINIMIZEBOX, WS_OVERLAPPED, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
            WS_VSCROLL,
        },
    },
};

// 窗口布局
const MARGIN: i32 = 20;
const NAV_BUTTON_HEIGHT: i32 = 28;
const STATUS_BAR_HEIGHT: i32 = 24;
const WINDOW_HEIGHT: i32 = 420;
const WINDOW_WIDTH: i32 = 620;

// 控件样式与颜色
const ERROR_COLOR: u32 = 0x0030_30C0;
const LB_ADDSTRING: u32 = 0x0000_0180;
const LB_GETCURSEL: u32 = 0x0000_0188;
const LB_SETCURSEL: u32 = 0x0000_0186;
const LBN_DBLCLK: u32 = 2;
const LBS_NOTIFY: u32 = 0x0000_0001;
const PBM_DEFAULT_BAR_COLOR: usize = 0xFF00_0000;
const PBM_SETBARCOLOR: u32 = 0x0409;
const SS_CENTERIMAGE: u32 = 0x0000_0200;
const SS_ENDELLIPSIS: u32 = 0x0000_4000;
const SS_ETCHEDHORZ: u32 = 0x0000_0010;
const SS_ICON: u32 = 0x0000_0003;
const SS_NOTIFY: u32 = 0x0000_0100;
const SS_RIGHT_CENTER: u32 = 0x0000_0002 | SS_CENTERIMAGE;

// 网络状态
const NET_FAILED: usize = 2;
const NET_LOADING: usize = 0;
const NET_OK: usize = 1;

// 窗口样式
const WINDOW_STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;

// 向导页面
const KIND_OPERATION: usize = 0;
const KIND_DIRECTORY: usize = 1;
const KIND_COMPONENTS: usize = 2;
const KIND_UNINSTALL: usize = 3;
const KIND_PROGRESS: usize = 4;
const KIND_FINISH: usize = 5;
const KIND_NOTES: usize = 6;
const KIND_MANAGE: usize = 7;

const fn kind_name(kind: usize, op: usize, upgrade: bool) -> &'static str {
    match kind {
        KIND_COMPONENTS => {
            if upgrade {
                "选择更新内容"
            } else {
                "选择安装内容"
            }
        }
        KIND_DIRECTORY => "选择游戏目录",
        KIND_MANAGE => "启用/禁用组件",
        KIND_NOTES => "发行说明",
        KIND_OPERATION => "选择操作",
        KIND_PROGRESS => match op {
            OP_DIAGNOSTICS => "导出诊断包",
            OP_MANAGE => "应用更改",
            OP_SELF_UPDATE => "更新管理工具",
            OP_UNINSTALL => "卸载",
            _ if upgrade => "下载并更新",
            _ => "下载并安装",
        },
        KIND_UNINSTALL => "选择卸载方式",
        _ => "完成",
    }
}

// 操作类型
const OP_DIAGNOSTICS: usize = 2;
const OP_INSTALL: usize = 0;
const OP_MANAGE: usize = 4;
const OP_SELF_UPDATE: usize = 3;
const OP_UNINSTALL: usize = 1;

// 控件 ID
const ID_BACK: usize = 1000;
const ID_NEXT: usize = 1001;
const ID_CANCEL: usize = 1002;

/// 确认框按钮 ID：避开 `IsDialogMessageW` 为 Enter/Esc 生成的 `IDOK`/`IDCANCEL`（1/2）。
const ID_DIALOG_CONFIRM: usize = 100;
const ID_DIALOG_CANCEL: usize = 101;

const ID_OP_INSTALL: usize = 1100;
const ID_OP_UNINSTALL: usize = 1101;
const ID_OP_DIAGNOSTICS: usize = 1102;
const ID_OP_MANAGE: usize = 1103;
const ID_UNINSTALL_LIGHT: usize = 1110;
const ID_UNINSTALL_FULL: usize = 1111;
const ID_MANAGE_BEPINEX: usize = 1120;
const ID_MANAGE_DLL: usize = 1121;
const ID_MANAGE_RES: usize = 1122;
const ID_MANAGE_NOTE: usize = 1123;
const ID_MANAGE_RECHECK: usize = 1124;
const ID_MANAGE_TOGGLE_BEPINEX: usize = 1130;
const ID_MANAGE_TOGGLE_DLL: usize = 1131;
const ID_MANAGE_TOGGLE_RES: usize = 1132;
const ID_STEP_TEXT: usize = 1003;
const ID_SEPARATOR: usize = 1004;
const ID_TITLE: usize = 1005;
const ID_HEADER_LOGO: usize = 1006;
const ID_TRACE_TEXT: usize = 1007;
const ID_SITE_LINK: usize = 1008;
const ID_NET_BANNER: usize = 1070;
const ID_NET_RETRY: usize = 1071;
const ID_GAME_RUNNING_HINT: usize = 1072;
const ID_RECHECK_GAME: usize = 1073;
const ID_VERSION_LIST: usize = 3;

const ID_BROWSE: usize = 1010;
const ID_PATH_EDIT: usize = 1013;
const ID_PAGE0_LABEL: usize = 1016;

const ID_CHECK_BEPINEX: usize = 1021;
const ID_CHECK_DLL: usize = 1022;
const ID_CHECK_RES: usize = 1023;
const ID_VERSION_BEPINEX: usize = 1024;
const ID_HISTORY_BEPINEX: usize = 1027;
const ID_CHECK_CONSOLE: usize = 1030;

const ID_INSTALL_HINT: usize = 1040;
const ID_BAR: usize = 1041;
const ID_BAR_LABEL: usize = 1044;
const ID_BAR_STATUS: usize = 1047;
const ID_CHECK_DETAILS: usize = 1050;
const ID_LOG: usize = 1051;
const ID_BAR_SPEED: usize = 1054;

const ID_FINISH_TITLE: usize = 1060;
const ID_FINISH_NOTE: usize = 1062;
const ID_OPEN_DIAGNOSTICS: usize = 1065;
const ID_NOTES_TITLE: usize = 1066;
const ID_NOTES_EDIT: usize = 1067;

// 操作页单选项：顺序即界面从上到下的顺序
const OPTION_IDS: [usize; 4] = [
    ID_OP_INSTALL,
    ID_OP_UNINSTALL,
    ID_OP_MANAGE,
    ID_OP_DIAGNOSTICS,
];

const OPTION_OPERATIONS: [usize; 4] = [OP_INSTALL, OP_UNINSTALL, OP_MANAGE, OP_DIAGNOSTICS];

/// 按控件 ID 找操作页单选项对应的操作类型。
fn option_by_id(id: usize) -> Option<usize> {
    OPTION_IDS
        .iter()
        .position(|candidate| *candidate == id)
        .map(option_operation)
}

/// 操作页第 `index` 个单选项对应的操作类型。
const fn option_operation(index: usize) -> usize {
    OPTION_OPERATIONS[index]
}

// 文案与展示
/// 进度条文件名列的参考文本（只用于测量列宽）。
const JOB_NAME_SAMPLES: [&str; 3] = [
    "BepInEx-Unity.IL2CPP-win-x64-0.0.0-be.000+0000000.zip",
    "MetaMystia-v00.00.00.dll",
    "ResourceExample-v00.00.00.zip",
];
const NOTES_PLACEHOLDER: &str = "正在获取发行说明…";
const PRODUCT_NAME: &str = "MetaMystia Mod 管理工具";
const SITE_LABEL: &str = "官方网站：";
const SITE_URL: &str = "https://meta-mystia.izakaya.cc";

fn window_caption() -> String {
    format!("{PRODUCT_NAME} v{}", env!("CARGO_PKG_VERSION"))
}

// 界面状态
static CONFIRM_CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);
static DPI: AtomicI32 = AtomicI32::new(96);
static MODAL_DEPTH: AtomicUsize = AtomicUsize::new(0);
static VERSION_CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

struct ModalScope {
    owner: HWND,
}

impl ModalScope {
    unsafe fn enter(owner: HWND) -> Self {
        MODAL_DEPTH.fetch_add(1, Ordering::Relaxed);

        Self { owner }
    }
}

impl Drop for ModalScope {
    fn drop(&mut self) {
        MODAL_DEPTH.fetch_sub(1, Ordering::Relaxed);
        unsafe {
            PostMessageW(self.owner, WM_UI_EVENT, 0, 0);
        }
    }
}

fn s(value: i32) -> i32 {
    value * DPI.load(Ordering::Relaxed) / 96
}

fn window_height() -> i32 {
    s(WINDOW_HEIGHT + STATUS_BAR_HEIGHT)
}

fn nav_top() -> i32 {
    window_height() - s(STATUS_BAR_HEIGHT) - s(MARGIN) - s(NAV_BUTTON_HEIGHT)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// 确认框正文高度：按实际宽度折算自动换行后的行数，避免多行内容被截断。
unsafe fn confirm_content_height(font: HFONT, content: &str, dialog_width: i32) -> i32 {
    let available = (dialog_width - s(MARGIN * 2) - s(16)).max(1);
    let lines: i32 = content
        .lines()
        .map(|line| 1 + text_width(font, line) / available)
        .sum();

    s(18) * lines.max(1) + s(8)
}

/// 后台线程 panic 时给界面一个可读的错误，而不是让流程无声卡住。
fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "未知错误".to_string())
}

fn open_url(url: &str) -> bool {
    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            wide("open").as_ptr(),
            wide(url).as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };

    result as isize > 32
}

/// 载入 build.rs 嵌入的应用图标（资源 ID 1）并按目标尺寸缩放；无资源时返回空句柄。
fn load_app_icon(instance: HINSTANCE, cx: i32, cy: i32) -> HICON {
    let resource = ptr::without_provenance::<u16>(1);
    let mut icon: HICON = ptr::null_mut();
    let result = unsafe { LoadIconWithScaleDown(instance, resource, cx, cy, &raw mut icon) };

    if result < 0 || icon.is_null() {
        unsafe { LoadIconW(instance, resource) }
    } else {
        icon
    }
}

fn human_speed(bytes_per_second: f64) -> String {
    if bytes_per_second <= 0.0 {
        return String::new();
    }

    format!("{}/s", human_short(bytes_per_second as f32))
}

fn human_short(bytes: f32) -> String {
    const UNITS: [&str; 3] = ["KiB", "MiB", "GiB"];
    let mut value = bytes / 1024.0;
    let mut unit = 0;

    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }

    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "三个开关各自独立，拆结构体反而更难读"
)]
struct State {
    back: HWND,
    back_width: i32,
    bepinex_gate: bool,
    bar_labels: [HWND; 3],
    bar_percent: [HWND; 3],
    bar_speed: [HWND; 3],
    bars: [HWND; 3],
    busy: bool,
    cancel: HWND,
    cancel_width: i32,
    choices: Choices,
    component_checks: [HWND; 3],
    component_version_controls: [HWND; 3],
    console_check: HWND,
    dialog_font: HFONT,
    dll_versions: Vec<String>,
    failed: bool,
    finish_group: HWND,
    finish_note: HWND,
    finish_open: HWND,
    finish_path: HWND,
    finish_rows: Vec<HWND>,
    finish_title: HWND,
    font: HFONT,
    game_hint: HWND,
    game_recheck: HWND,
    install_hint: HWND,
    installed_line: HWND,
    job_bar_width: i32,
    job_bytes: [u64; JOB_SLOTS],
    job_name_width: i32,
    job_percent_width: i32,
    job_speed: [f64; JOB_SLOTS],
    job_speed_width: i32,
    job_total: [Option<u64>; JOB_SLOTS],
    job_visible: [bool; JOB_SLOTS],
    local: LocalInfo,
    log: HWND,
    log_visible: bool,
    manage_buttons: [HWND; 3],
    manage_applied: [bool; 3],
    manage_hint: HWND,
    manage_note: HWND,
    manage_pending: [bool; 3],
    manage_recheck: HWND,
    manage_statuses: [HWND; 3],
    manage_versions: [HWND; 3],
    net_attempt: usize,
    net_banner: HWND,
    net_error: Option<String>,
    net_phase: usize,
    net_retry: HWND,
    next: HWND,
    next_width: i32,
    no_update: bool,
    notes_edit: HWND,
    notes_title: HWND,
    notes_version: Option<String>,
    op: usize,
    op_before_self_update: usize,
    option_notes: [HWND; 4],
    option_labels: [HWND; 4],
    option_radios: [HWND; 4],
    page0: Vec<HWND>,
    page1: Vec<HWND>,
    page2: Vec<HWND>,
    page3: Vec<HWND>,
    page_manage: Vec<HWND>,
    page_notes: Vec<HWND>,
    page_op: Vec<HWND>,
    page_uninstall: Vec<HWND>,
    path_edit: HWND,
    pending_manager_update: Option<String>,
    phase: usize,
    plan: Vec<usize>,
    prefetched: Option<RemoteInfo>,
    progress_shown_at: Option<Instant>,
    resource_note: HWND,
    resourceex_versions: Vec<String>,
    secondary: Vec<HWND>,
    stage: Option<Stage>,
    step: usize,
    step_text: HWND,
    title_font: HFONT,
    ui: Arc<GuiUi>,
    uninstall_full: bool,
    uninstall_radios: [HWND; 2],
    update_line: HWND,
    width: i32,
}

/// 程序入口：初始化窗口、控件与消息循环。
#[allow(
    clippy::too_many_lines,
    reason = "界面入口：初始化、创建窗口与消息循环集中在一处"
)]
pub fn run() {
    unsafe {
        if !acquire_single_instance() {
            focus_existing_manager_window();
            return;
        }

        install_console_handler();
        report_event("Run", Some(env!("CARGO_PKG_VERSION")));

        panic::set_hook(Box::new(|info| {
            let text = format!("{info}");
            MessageBoxW(
                ptr::null_mut(),
                wide(&text).as_ptr(),
                wide("MetaMystia Mod 管理工具 - 错误").as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }));

        DPI.store(system_dpi(), Ordering::Relaxed);

        let controls = INITCOMMONCONTROLSEX {
            dwICC: ICC_STANDARD_CLASSES | ICC_PROGRESS_CLASS | ICC_LINK_CLASS,
            dwSize: mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        };
        InitCommonControlsEx(&raw const controls);

        let instance = GetModuleHandleW(ptr::null());
        let icon_big = load_app_icon(
            instance,
            GetSystemMetrics(SM_CXICON),
            GetSystemMetrics(SM_CYICON),
        );
        let icon_small = load_app_icon(
            instance,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
        );
        let class_name = wide(MAIN_WINDOW_CLASS);
        let window_class = WNDCLASSW {
            cbClsExtra: 0,
            cbWndExtra: 0,
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            hIcon: icon_big,
            hInstance: instance,
            hbrBackground: (COLOR_BTNFACE + 1) as usize as *mut c_void,
            lpfnWndProc: Some(window_proc),
            lpszClassName: class_name.as_ptr(),
            lpszMenuName: ptr::null(),
            style: 0,
        };
        RegisterClassW(&raw const window_class);

        let mut frame = RECT {
            bottom: window_height(),
            left: 0,
            right: s(WINDOW_WIDTH),
            top: 0,
        };
        AdjustWindowRectEx(&raw mut frame, WINDOW_STYLE, 0, WS_EX_CONTROLPARENT);

        let frame_width = frame.right - frame.left;
        let frame_height = frame.bottom - frame.top;
        let screen_width = GetSystemMetrics(SM_CXSCREEN);
        let screen_height = GetSystemMetrics(SM_CYSCREEN);

        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class_name.as_ptr(),
            wide(&window_caption()).as_ptr(),
            WINDOW_STYLE,
            (screen_width - frame_width) / 2,
            (screen_height - frame_height) / 2,
            frame_width,
            frame_height,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        );

        if hwnd.is_null() {
            return;
        }

        SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, icon_big as isize);
        SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, icon_small as isize);

        let state = Box::into_raw(build_children(hwnd));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        show_page(&mut *state, 0);
        (*state).ui.attach(hwnd);
        set_main_window(hwnd as isize);
        update_net_ui(&mut *state);

        let ui = Arc::clone(&(*state).ui);
        start_prefetch(ui);

        let mut frame = RECT {
            bottom: window_height(),
            left: 0,
            right: (*state).width,
            top: 0,
        };
        AdjustWindowRectEx(&raw mut frame, WINDOW_STYLE, 0, WS_EX_CONTROLPARENT);
        let real_width = frame.right - frame.left;
        let real_height = frame.bottom - frame.top;
        if real_width != frame_width || real_height != frame_height {
            SetWindowPos(
                hwnd,
                ptr::null_mut(),
                (screen_width - real_width) / 2,
                (screen_height - real_height) / 2,
                real_width,
                real_height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }

        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        remove_replaced_exe();

        let mut message: MSG = mem::zeroed();
        while GetMessageW(&raw mut message, ptr::null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &raw const message) == 0 {
                TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
        }

        run_shutdown();
    }
}

fn start_prefetch(ui: Arc<GuiUi>) {
    thread::spawn(move || {
        let task_ui = Arc::clone(&ui);
        let local = panic::catch_unwind(panic::AssertUnwindSafe(move || {
            bridge::prefetch_local(&task_ui)
        }))
        .unwrap_or_default();

        ui.push_event(Event::Local(local));

        let remote_ui = Arc::clone(&ui);
        thread::spawn(move || {
            let task_ui = Arc::clone(&remote_ui);
            let remote = panic::catch_unwind(panic::AssertUnwindSafe(move || {
                bridge::fetch_remote(&task_ui)
            }))
            .unwrap_or_else(|payload| {
                Err(ManagerError::Other(format!(
                    "内部错误：{}",
                    panic_message(&*payload)
                )))
            });

            remote_ui.push_event(Event::Remote(remote));
        });
    });
}

unsafe fn pick_folder(owner: HWND, initial: &Path) -> Option<PathBuf> {
    let title = wide("选择游戏根目录（包含 Touhou Mystia Izakaya.exe）");
    let initial_text = wide(&initial.display().to_string());
    let mut display: [u16; 260] = [0; 260];

    let browse = BROWSEINFOW {
        hwndOwner: owner,
        iImage: 0,
        lParam: initial_text.as_ptr() as isize,
        lpfn: Some(browse_callback),
        lpszTitle: title.as_ptr(),
        pidlRoot: ptr::null_mut(),
        pszDisplayName: display.as_mut_ptr(),
        ulFlags: BIF_RETURNONLYFSDIRS,
    };

    let pidl = SHBrowseForFolderW(&raw const browse);
    if pidl.is_null() {
        return None;
    }

    let mut buffer = [0u16; 260];
    let ok = SHGetPathFromIDListW(pidl, buffer.as_mut_ptr());

    ILFree(pidl);

    if ok == 0 {
        return None;
    }

    let len = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    let path = String::from_utf16_lossy(&buffer[..len]);
    let path = PathBuf::from(path);

    if path.join(GAME_EXECUTABLE).is_file() {
        Some(path)
    } else {
        MessageBoxW(
            owner,
            wide("所选目录里没有找到游戏主程序，请重新选择。").as_ptr(),
            wide(&window_caption()).as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
        None
    }
}

unsafe extern "system" fn browse_callback(
    hwnd: HWND,
    message: u32,
    _lparam: LPARAM,
    data: LPARAM,
) -> i32 {
    if message == BFFM_INITIALIZED {
        SendMessageW(hwnd, BFFM_SETSELECTIONW, 1, data);
    }

    0
}

unsafe fn system_dpi() -> i32 {
    let dc = GetDC(ptr::null_mut());
    let dpi = GetDeviceCaps(dc, LOGPIXELSY as i32);
    ReleaseDC(ptr::null_mut(), dc);
    if dpi <= 0 { 96 } else { dpi }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Win32 控件创建参数，本来就这么长"
)]
unsafe fn create_child(
    parent: HWND,
    class: &str,
    text: &str,
    style: u32,
    ex_style: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    id: usize,
    font: HFONT,
) -> HWND {
    let hwnd = CreateWindowExW(
        ex_style,
        wide(class).as_ptr(),
        wide(text).as_ptr(),
        style,
        x,
        y,
        width,
        height,
        parent,
        id as *mut c_void,
        GetModuleHandleW(ptr::null()),
        ptr::null(),
    );

    if !hwnd.is_null() && !font.is_null() {
        SendMessageW(hwnd, WM_SETFONT, font as usize, 1);
    }

    hwnd
}

unsafe fn create_fonts() -> (HFONT, HFONT, HFONT) {
    let mut metrics: NONCLIENTMETRICSW = mem::zeroed();
    metrics.cbSize = mem::size_of::<NONCLIENTMETRICSW>() as u32;
    SystemParametersInfoW(
        SPI_GETNONCLIENTMETRICS,
        metrics.cbSize,
        ptr::addr_of_mut!(metrics).cast::<c_void>(),
        0,
    );

    let body = metrics.lfMessageFont;
    let font = CreateFontIndirectW(&raw const body);

    let mut title: LOGFONTW = body;
    title.lfHeight = (title.lfHeight as f32 * 1.35) as i32;
    title.lfWeight = FW_SEMIBOLD as i32;
    let title_font = CreateFontIndirectW(&raw const title);

    let mut dialog: LOGFONTW = body;
    dialog.lfHeight = (dialog.lfHeight as f32 * 1.6) as i32;
    dialog.lfWeight = FW_SEMIBOLD as i32;
    let dialog_font = CreateFontIndirectW(&raw const dialog);

    (
        if font.is_null() {
            GetStockObject(DEFAULT_GUI_FONT)
        } else {
            font
        },
        title_font,
        dialog_font,
    )
}

unsafe fn window_state(hwnd: HWND) -> *mut State {
    GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State
}

unsafe fn is_error_text(state: &State, control: HWND) -> bool {
    (state.failed && control == state.install_hint)
        || control == state.game_hint
        || (control == state.manage_note
            && state.bepinex_gate
            && state.manage_pending[0]
            && state.local.bepinex_disabled)
        || (control == state.net_banner && state.net_phase == NET_FAILED)
        || (control == state.resource_note && IsWindowEnabled(state.component_checks[2]) != 0)
}

fn is_blocked_install(state: &State, control: HWND) -> bool {
    state.net_phase != NET_OK
        && (control == state.option_labels[0]
            || control == state.option_radios[0]
            || control == state.option_notes[0])
}

const fn manage_target_disabled(state: &State, index: usize) -> Option<bool> {
    match index {
        0 if state.local.bepinex_installed => Some(state.local.bepinex_disabled),
        1 if state.local.dll_version.is_some() => Some(state.local.dll_disabled),
        2 if state.local.resourceex_version.is_some() => Some(state.local.resourceex_disabled),
        _ => None,
    }
}

/// 组件版本文字：`disabled` 为自身状态，`bepinex_disabled` / `mystia_disabled` 为被依赖项挡住的情况。
fn manage_version_label(
    version: Option<&str>,
    disabled: bool,
    bepinex_disabled: bool,
    mystia_disabled: bool,
) -> String {
    let mut label = version.map_or_else(|| "版本未知".to_string(), str::to_string);

    if disabled {
        label.push_str("（已禁用）");
    }

    if bepinex_disabled {
        label.push_str("（框架已禁用，暂不生效）");
    } else if mystia_disabled {
        label.push_str("（本体已禁用，暂不生效）");
    }

    label
}

const fn manage_status_label(
    installed: bool,
    applied_disabled: bool,
    pending: bool,
) -> &'static str {
    match (installed, applied_disabled, pending) {
        (false, _, _) => "未安装",
        (true, _, true) if applied_disabled => "待启用",
        (true, _, true) => "待禁用",
        (true, true, false) => "已禁用",
        (true, false, false) => "已启用",
    }
}

unsafe fn update_manage_state(state: &mut State) {
    let running = check_game_running_cached().unwrap_or(false);
    let pending = state.manage_pending;
    state.bepinex_gate = state.local.bepinex_installed && !state.manage_applied[0];
    let has_path = state
        .local
        .game_root
        .as_deref()
        .is_some_and(|root| !root.as_os_str().is_empty())
        && !state.local.detect_failed;

    for (index, is_pending) in pending.iter().enumerate().take(3) {
        let disabled = manage_target_disabled(state, index);

        SetWindowTextW(
            state.manage_statuses[index],
            wide(manage_status_label(
                disabled.is_some(),
                state.manage_applied[index],
                *is_pending,
            ))
            .as_ptr(),
        );
        SetWindowTextW(
            state.manage_buttons[index],
            wide(if disabled.unwrap_or(false) {
                "启用"
            } else {
                "禁用"
            })
            .as_ptr(),
        );
        EnableWindow(
            state.manage_buttons[index],
            i32::from(has_path && disabled.is_some() && !running),
        );
    }

    for index in 1..3 {
        if !state.bepinex_gate && manage_target_disabled(state, index).is_some() {
            EnableWindow(state.manage_buttons[index], 0);
        }
    }

    if state.local.dll_disabled && manage_target_disabled(state, 2).is_some() {
        EnableWindow(state.manage_buttons[2], 0);
    }

    let hint = if !has_path {
        Some("未找到游戏目录，请返回上一步选择游戏目录。")
    } else if running {
        Some("检测到游戏正在运行，请先退出游戏。")
    } else {
        None
    };

    if let Some(text) = hint {
        SetWindowTextW(state.manage_hint, wide(text).as_ptr());
    }
    ShowWindow(
        state.manage_hint,
        if hint.is_some() { SW_SHOW } else { SW_HIDE },
    );
    ShowWindow(
        state.manage_recheck,
        if running { SW_SHOW } else { SW_HIDE },
    );

    if state.plan.get(state.step) == Some(&KIND_MANAGE) && (running || !has_path) {
        EnableWindow(state.next, 0);
    }
}

unsafe fn refresh_manage_ui(state: &mut State) {
    let bepinex_disabled = state.local.bepinex_installed && state.manage_applied[0];
    let blocked_by = |index: usize, dependency: bool| {
        dependency && !state.manage_applied[index] && !state.manage_pending[index]
    };
    let versions = [
        manage_version_label(
            state.local.bepinex_version.as_deref(),
            state.manage_applied[0],
            false,
            false,
        ),
        manage_version_label(
            state.local.dll_version.as_deref(),
            state.manage_applied[1],
            blocked_by(1, bepinex_disabled),
            false,
        ),
        manage_version_label(
            state.local.resourceex_version.as_deref(),
            state.manage_applied[2],
            blocked_by(2, bepinex_disabled),
            blocked_by(2, state.manage_applied[1]),
        ),
    ];

    for (control, text) in state.manage_versions.iter().zip(versions) {
        SetWindowTextW(*control, wide(&text).as_ptr());
    }

    update_manage_state(state);
    RedrawWindow(
        state.manage_note,
        ptr::null(),
        ptr::null_mut(),
        RDW_INVALIDATE | RDW_UPDATENOW,
    );
}

#[derive(Default)]
struct VersionDialog {
    font: HFONT,
    list: HWND,
    selected: Option<usize>,
}

/// 进度页最短可见时间，避免瞬时操作把“正在应用”一闪而过。
const PROGRESS_MIN_VISIBLE: Duration = Duration::from_millis(1200);

struct ConfirmDialog {
    cancel_label: String,
    confirm_label: String,
    confirmed: bool,
    content: String,
    dialog_font: HFONT,
    font: HFONT,
    /// 只有一个按钮时不给取消（强制继续，例如必须完成的版本升级）
    has_cancel: bool,
    instruction: String,
}

#[allow(
    clippy::too_many_lines,
    reason = "对话框创建分支（单按钮/双按钮）集中在一处，便于对照布局"
)]
unsafe extern "system" fn confirm_dialog_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_COMMAND => {
            let shared = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ConfirmDialog;

            if (wparam >> 16) & 0xFFFF != 0 {
                return 0;
            }

            match wparam & 0xFFFF {
                ID_DIALOG_CANCEL => {
                    DestroyWindow(hwnd);
                }
                ID_DIALOG_CONFIRM => {
                    if !shared.is_null() {
                        (*shared).confirmed = true;
                    }
                    DestroyWindow(hwnd);
                }
                // Esc 一律按取消处理；Enter 走对话框的默认按钮（有取消按钮时默认取消）
                id if id == IDCANCEL as usize => {
                    DestroyWindow(hwnd);
                }
                id if id == IDOK as usize => {
                    if !shared.is_null() && !(*shared).has_cancel {
                        (*shared).confirmed = true;
                    }
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_CREATE => {
            let create = lparam as *const CREATESTRUCTW;
            let shared = (*create).lpCreateParams.cast::<ConfirmDialog>();
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, shared as isize);

            let mut rect: RECT = mem::zeroed();
            GetClientRect(hwnd, &raw mut rect);
            let client_width = rect.right - rect.left;
            let client_height = rect.bottom - rect.top;
            let font = (*shared).font;

            create_child(
                hwnd,
                "STATIC",
                &(*shared).instruction,
                WS_CHILD | WS_VISIBLE,
                0,
                s(MARGIN),
                s(18),
                client_width - s(MARGIN * 2),
                s(34),
                0,
                (*shared).dialog_font,
            );
            create_child(
                hwnd,
                "STATIC",
                &(*shared).content,
                WS_CHILD | WS_VISIBLE,
                0,
                s(MARGIN),
                s(56),
                client_width - s(MARGIN * 2),
                (client_height - s(56) - s(60)).max(s(30)),
                0,
                font,
            );

            let confirm_width = button_width(font, &[(*shared).confirm_label.as_str()]);
            let button_y = client_height - s(MARGIN) - s(28);

            if (*shared).has_cancel {
                let cancel_width = button_width(font, &[(*shared).cancel_label.as_str()]);
                create_child(
                    hwnd,
                    "BUTTON",
                    &(*shared).cancel_label,
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
                    0,
                    client_width - s(MARGIN) - confirm_width - s(8) - cancel_width,
                    button_y,
                    cancel_width,
                    s(28),
                    ID_DIALOG_CANCEL,
                    font,
                );
            }
            create_child(
                hwnd,
                "BUTTON",
                &(*shared).confirm_label,
                WS_CHILD
                    | WS_VISIBLE
                    | WS_TABSTOP
                    | if (*shared).has_cancel {
                        BS_PUSHBUTTON as u32
                    } else {
                        BS_DEFPUSHBUTTON as u32
                    },
                0,
                client_width - s(MARGIN) - confirm_width,
                button_y,
                confirm_width,
                s(28),
                ID_DIALOG_CONFIRM,
                font,
            );
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

/// 模态确认框，返回 `true` 表示用户选择了确认按钮。
#[allow(
    clippy::too_many_arguments,
    reason = "确认框文案/按钮/字体都由调用方给，拆参数反而绕"
)]
unsafe fn confirm_dialog(
    owner: HWND,
    title: &str,
    instruction: &str,
    content: &str,
    confirm_label: &str,
    cancel_label: &str,
    font: HFONT,
    dialog_font: HFONT,
) -> bool {
    show_confirm_dialog(
        owner,
        title,
        instruction,
        content,
        confirm_label,
        Some(cancel_label),
        font,
        dialog_font,
    )
}

/// 只有一个“继续”按钮的提示框：用于必须完成的升级。
/// 返回 `true` 表示点了继续；关掉窗口（X）返回 `false`，由调用方决定退出。
#[allow(clippy::too_many_arguments, reason = "和确认框共用一套参数")]
unsafe fn notice_dialog(
    owner: HWND,
    title: &str,
    instruction: &str,
    content: &str,
    confirm_label: &str,
    font: HFONT,
    dialog_font: HFONT,
) -> bool {
    show_confirm_dialog(
        owner,
        title,
        instruction,
        content,
        confirm_label,
        None,
        font,
        dialog_font,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "确认框文案/按钮/字体都由调用方给，拆参数反而绕"
)]
unsafe fn show_confirm_dialog(
    owner: HWND,
    title: &str,
    instruction: &str,
    content: &str,
    confirm_label: &str,
    cancel_label: Option<&str>,
    font: HFONT,
    dialog_font: HFONT,
) -> bool {
    let instance = GetModuleHandleW(ptr::null());
    let class_name = wide("MetaMystiaConfirmDialog");
    let window_class = WNDCLASSW {
        cbClsExtra: 0,
        cbWndExtra: 0,
        hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
        hIcon: ptr::null_mut(),
        hInstance: instance,
        hbrBackground: (COLOR_BTNFACE + 1) as usize as *mut c_void,
        lpfnWndProc: Some(confirm_dialog_proc),
        lpszClassName: class_name.as_ptr(),
        lpszMenuName: ptr::null(),
        style: 0,
    };
    if !CONFIRM_CLASS_REGISTERED.swap(true, Ordering::Relaxed) {
        RegisterClassW(&raw const window_class);
    }

    let shared = Box::into_raw(Box::new(ConfirmDialog {
        cancel_label: cancel_label.unwrap_or_default().to_string(),
        confirm_label: confirm_label.to_string(),
        confirmed: false,
        content: content.to_string(),
        dialog_font,
        font,
        has_cancel: cancel_label.is_some(),
        instruction: instruction.to_string(),
    }));

    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;

    let mut frame = RECT {
        bottom: (s(56) + confirm_content_height(font, content, s(400)) + s(60)).max(s(180)),
        left: 0,
        right: s(400),
        top: 0,
    };
    AdjustWindowRectEx(&raw mut frame, style, 0, WS_EX_DLGMODALFRAME);

    let mut owner_rect: RECT = mem::zeroed();
    GetWindowRect(owner, &raw mut owner_rect);

    let width = frame.right - frame.left;
    let height = frame.bottom - frame.top;
    let x = owner_rect.left + ((owner_rect.right - owner_rect.left) - width) / 2;
    let y = owner_rect.top + ((owner_rect.bottom - owner_rect.top) - height) / 2;

    let hwnd = CreateWindowExW(
        WS_EX_DLGMODALFRAME,
        class_name.as_ptr(),
        wide(title).as_ptr(),
        style,
        x,
        y,
        width,
        height,
        owner,
        ptr::null_mut(),
        instance,
        shared.cast::<c_void>(),
    );

    if hwnd.is_null() {
        drop(Box::from_raw(shared));
        return false;
    }

    let _modal = ModalScope::enter(owner);

    EnableWindow(owner, 0);
    ShowWindow(hwnd, SW_SHOW);

    let mut message: MSG = mem::zeroed();
    while IsWindow(hwnd) != 0 && GetMessageW(&raw mut message, ptr::null_mut(), 0, 0) > 0 {
        if IsDialogMessageW(hwnd, &raw const message) == 0 {
            TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }

    EnableWindow(owner, 1);
    SetForegroundWindow(owner);

    let confirmed = (*shared).confirmed;
    drop(Box::from_raw(shared));

    confirmed
}

unsafe extern "system" fn version_dialog_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_COMMAND => {
            let shared = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut VersionDialog;
            let id = wparam & 0xFFFF;
            let code = (wparam >> 16) & 0xFFFF;

            match id {
                1 if code == 0 => {
                    if !shared.is_null() {
                        let index = SendMessageW((*shared).list, LB_GETCURSEL, 0, 0);

                        if index >= 0 {
                            (*shared).selected = Some(index as usize);
                        }
                    }
                    DestroyWindow(hwnd);
                }
                2 if code == 0 => {
                    DestroyWindow(hwnd);
                }
                ID_VERSION_LIST if code == LBN_DBLCLK as usize && !shared.is_null() => {
                    let index = SendMessageW((*shared).list, LB_GETCURSEL, 0, 0);
                    if index >= 0 {
                        (*shared).selected = Some(index as usize);
                        DestroyWindow(hwnd);
                    }
                }
                _ => {}
            }
            0
        }
        WM_CREATE => {
            let create = lparam as *const CREATESTRUCTW;
            let shared = (*create).lpCreateParams.cast::<VersionDialog>();
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, shared as isize);

            let mut rect: RECT = mem::zeroed();
            GetClientRect(hwnd, &raw mut rect);
            let client_width = rect.right - rect.left;
            let font = (*shared).font;

            let list = create_child(
                hwnd,
                "LISTBOX",
                "",
                WS_CHILD | WS_VISIBLE | WS_BORDER | WS_VSCROLL | WS_TABSTOP | LBS_NOTIFY,
                0,
                s(16),
                s(16),
                client_width - s(32),
                s(180),
                ID_VERSION_LIST,
                font,
            );
            (*shared).list = list;

            // 确定/取消沿用 IDOK/IDCANCEL 的编号，Enter/Esc 由 IsDialogMessageW 直接命中
            let ok_width = button_width(font, &["确定"]);
            create_child(
                hwnd,
                "BUTTON",
                "确定",
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
                0,
                client_width - s(16) - ok_width,
                s(208),
                ok_width,
                s(28),
                1,
                font,
            );
            create_child(
                hwnd,
                "BUTTON",
                "取消",
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
                0,
                client_width - s(16) - ok_width * 2 - s(8),
                s(208),
                ok_width,
                s(28),
                2,
                font,
            );
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe fn pick_version(
    owner: HWND,
    title: &str,
    versions: &[String],
    font: HFONT,
) -> Option<usize> {
    let instance = GetModuleHandleW(ptr::null());
    let class_name = wide("MetaMystiaVersionDialog");
    let window_class = WNDCLASSW {
        cbClsExtra: 0,
        cbWndExtra: 0,
        hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
        hIcon: ptr::null_mut(),
        hInstance: instance,
        hbrBackground: (COLOR_BTNFACE + 1) as usize as *mut c_void,
        lpfnWndProc: Some(version_dialog_proc),
        lpszClassName: class_name.as_ptr(),
        lpszMenuName: ptr::null(),
        style: 0,
    };
    if !VERSION_CLASS_REGISTERED.swap(true, Ordering::Relaxed) {
        RegisterClassW(&raw const window_class);
    }

    let shared = Box::into_raw(Box::new(VersionDialog {
        font,
        list: ptr::null_mut(),
        selected: None,
    }));

    let mut frame = RECT {
        bottom: s(248),
        left: 0,
        right: s(320),
        top: 0,
    };
    AdjustWindowRectEx(
        &raw mut frame,
        WS_POPUP | WS_CAPTION | WS_SYSMENU,
        0,
        WS_EX_DLGMODALFRAME,
    );

    let mut owner_rect: RECT = mem::zeroed();
    GetWindowRect(owner, &raw mut owner_rect);

    let width = frame.right - frame.left;
    let height = frame.bottom - frame.top;
    let x = owner_rect.left + ((owner_rect.right - owner_rect.left) - width) / 2;
    let y = owner_rect.top + ((owner_rect.bottom - owner_rect.top) - height) / 2;

    let hwnd = CreateWindowExW(
        WS_EX_DLGMODALFRAME,
        class_name.as_ptr(),
        wide(title).as_ptr(),
        WS_POPUP | WS_CAPTION | WS_SYSMENU,
        x,
        y,
        width,
        height,
        owner,
        ptr::null_mut(),
        instance,
        shared.cast::<c_void>(),
    );

    if hwnd.is_null() {
        drop(Box::from_raw(shared));
        return None;
    }

    let _modal = ModalScope::enter(owner);

    for version in versions {
        SendMessageW(
            (*shared).list,
            LB_ADDSTRING,
            0,
            wide(version).as_ptr() as isize,
        );
    }
    SendMessageW((*shared).list, LB_SETCURSEL, 0, 0);

    EnableWindow(owner, 0);
    ShowWindow(hwnd, SW_SHOW);

    let mut message: MSG = mem::zeroed();
    while IsWindow(hwnd) != 0 && GetMessageW(&raw mut message, ptr::null_mut(), 0, 0) > 0 {
        if IsDialogMessageW(hwnd, &raw const message) == 0 {
            TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }

    EnableWindow(owner, 1);
    SetForegroundWindow(owner);

    let selected = (*shared).selected;
    drop(Box::from_raw(shared));

    selected
}

unsafe fn text_width(font: HFONT, text: &str) -> i32 {
    let dc = GetDC(ptr::null_mut());
    if dc.is_null() || font.is_null() {
        return text.chars().count() as i32 * 7;
    }

    let previous = SelectObject(dc, font as HGDIOBJ);
    let buffer = wide(text);
    let mut size: SIZE = mem::zeroed();
    GetTextExtentPoint32W(
        dc,
        buffer.as_ptr(),
        (buffer.len() - 1) as i32,
        &raw mut size,
    );
    SelectObject(dc, previous);
    ReleaseDC(ptr::null_mut(), dc);

    size.cx
}

unsafe fn widest(font: HFONT, texts: &[&str]) -> i32 {
    texts
        .iter()
        .map(|text| text_width(font, text))
        .max()
        .unwrap_or(0)
}

unsafe fn button_width(font: HFONT, texts: &[&str]) -> i32 {
    widest(font, texts) + s(28)
}

#[allow(clippy::too_many_lines, reason = "向导所有页面集中构建，便于对照布局")]
unsafe fn build_children(hwnd: HWND) -> Box<State> {
    let (font, title_font, dialog_font) = create_fonts();
    let margin = s(MARGIN);
    let gap = s(8);
    let logo_size = s(48);
    let uid_label = user_id();
    let uid_width = text_width(font, &uid_label) + s(12);
    let link_width = text_width(font, SITE_URL);
    let label_width = text_width(font, SITE_LABEL);
    let width = s(WINDOW_WIDTH)
        .max(margin + text_width(title_font, PRODUCT_NAME) + gap + logo_size + margin)
        .max(margin + uid_width + s(40) + label_width + s(2) + link_width + margin);
    let content = width - margin * 2;
    let mut secondary = Vec::new();

    let logo = create_child(
        hwnd,
        "STATIC",
        "",
        WS_CHILD | WS_VISIBLE | SS_ICON,
        0,
        width - margin - logo_size,
        s(14),
        logo_size,
        logo_size,
        ID_HEADER_LOGO,
        font,
    );
    SendMessageW(
        logo,
        STM_SETICON,
        load_app_icon(GetModuleHandleW(ptr::null()), logo_size, logo_size) as usize,
        0,
    );

    create_child(
        hwnd,
        "STATIC",
        PRODUCT_NAME,
        WS_CHILD | WS_VISIBLE | SS_CENTERIMAGE,
        0,
        margin,
        s(12),
        content - logo_size - gap,
        s(34),
        ID_TITLE,
        title_font,
    );
    let step_area = content - logo_size - gap;
    let step_text = create_child(
        hwnd,
        "STATIC",
        "",
        WS_CHILD | WS_VISIBLE,
        0,
        margin,
        s(44),
        step_area,
        s(20),
        ID_STEP_TEXT,
        font,
    );
    secondary.push(step_text);
    create_child(
        hwnd,
        "STATIC",
        "",
        WS_CHILD | WS_VISIBLE | SS_ETCHEDHORZ,
        0,
        margin,
        s(72),
        content,
        s(2),
        ID_SEPARATOR,
        font,
    );

    let mut page_op = Vec::new();
    let mut option_radios = [ptr::null_mut(); 4];
    let mut option_label_hwnds = [ptr::null_mut(); 4];
    let mut option_note_hwnds = [ptr::null_mut(); 4];
    let option_labels = ["安装/更新 Mod", "卸载 Mod", "启用/禁用 Mod", "导出诊断包"];
    let option_notes = [
        "未安装时安装，已安装时更新。",
        "卸载 MetaMystia Mod，可选择是否连同 BepInEx 一起清理。",
        "禁用已安装的 MetaMystia Mod，可随时重新启用。",
        "收集日志与配置用于反馈问题，不会修改游戏文件。",
    ];

    for i in 0..4 {
        let row = s(92) + s(60) * i as i32;
        option_radios[i] = create_child(
            hwnd,
            "BUTTON",
            "",
            WS_CHILD | WS_TABSTOP | BS_AUTORADIOBUTTON as u32,
            0,
            margin,
            row + s(3),
            s(18),
            s(18),
            OPTION_IDS[i],
            font,
        );
        if i == 0 {
            SendMessageW(option_radios[i], BM_SETCHECK, 1, 0);
        }
        page_op.push(option_radios[i]);

        option_label_hwnds[i] = create_child(
            hwnd,
            "STATIC",
            option_labels[i],
            WS_CHILD | SS_CENTERIMAGE | SS_NOTIFY,
            0,
            margin + s(18),
            row,
            s(280),
            s(22),
            OPTION_IDS[i],
            font,
        );
        page_op.push(option_label_hwnds[i]);

        let note = create_child(
            hwnd,
            "STATIC",
            option_notes[i],
            WS_CHILD | SS_CENTERIMAGE | SS_NOTIFY,
            0,
            margin + s(18),
            row + s(24),
            content - s(18),
            s(20),
            OPTION_IDS[i],
            font,
        );
        option_note_hwnds[i] = note;
        secondary.push(note);
        page_op.push(note);
    }

    let retry_width = button_width(font, &["重试"]);
    let net_banner = create_child(
        hwnd,
        "STATIC",
        "",
        WS_CHILD | WS_VISIBLE | SS_CENTERIMAGE,
        0,
        margin,
        s(336),
        content - retry_width - gap,
        s(20),
        ID_NET_BANNER,
        font,
    );
    let net_retry = create_child(
        hwnd,
        "BUTTON",
        "重试",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
        0,
        width - margin - retry_width,
        s(333),
        retry_width,
        s(26),
        ID_NET_RETRY,
        font,
    );
    ShowWindow(net_retry, SW_HIDE);
    page_op.push(net_banner);
    page_op.push(net_retry);

    let mut page0 = Vec::new();
    page0.push(create_child(
        hwnd,
        "STATIC",
        "游戏目录",
        WS_CHILD,
        0,
        margin,
        s(92),
        s(200),
        s(20),
        ID_PAGE0_LABEL,
        font,
    ));
    let browse_width = button_width(font, &["浏览…"]);
    let path_edit = create_child(
        hwnd,
        "EDIT",
        "",
        WS_CHILD | WS_TABSTOP | ES_READONLY as u32 | ES_AUTOHSCROLL as u32,
        WS_EX_CLIENTEDGE,
        margin,
        s(116),
        content - browse_width - gap,
        s(24),
        ID_PATH_EDIT,
        font,
    );
    page0.push(path_edit);
    page0.push(create_child(
        hwnd,
        "BUTTON",
        "浏览…",
        WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32,
        0,
        width - margin - browse_width,
        s(115),
        browse_width,
        s(26),
        ID_BROWSE,
        font,
    ));
    let path_hint = create_child(
        hwnd,
        "STATIC",
        "如需更换，请手动选择游戏根目录。",
        WS_CHILD,
        0,
        margin,
        s(148),
        content,
        s(20),
        0,
        font,
    );
    secondary.push(path_hint);
    page0.push(path_hint);

    page0.push(create_child(
        hwnd,
        "STATIC",
        "",
        WS_CHILD | WS_VISIBLE | SS_ETCHEDHORZ,
        0,
        margin,
        s(184),
        content,
        s(2),
        0,
        font,
    ));
    page0.push(create_child(
        hwnd,
        "STATIC",
        "当前状态",
        WS_CHILD,
        0,
        margin,
        s(202),
        s(200),
        s(20),
        0,
        font,
    ));
    let status_text = "正在检测当前安装…";
    let installed_line = create_child(
        hwnd,
        "STATIC",
        status_text,
        WS_CHILD,
        0,
        margin + s(20),
        s(228),
        content - s(20),
        s(20),
        0,
        font,
    );
    secondary.push(installed_line);
    page0.push(installed_line);
    let update_text = "";
    let update_line = create_child(
        hwnd,
        "STATIC",
        update_text,
        WS_CHILD,
        0,
        margin + s(20),
        s(250),
        content - s(20),
        s(20),
        0,
        font,
    );
    secondary.push(update_line);
    page0.push(update_line);

    let recheck_width = button_width(font, &["重新检测"]);
    let game_hint = create_child(
        hwnd,
        "STATIC",
        "检测到游戏正在运行，请先退出游戏。",
        WS_CHILD,
        0,
        margin,
        s(288),
        content - recheck_width - gap,
        s(20),
        ID_GAME_RUNNING_HINT,
        font,
    );
    let game_recheck = create_child(
        hwnd,
        "BUTTON",
        "重新检测",
        WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32,
        0,
        width - margin - recheck_width,
        s(285),
        recheck_width,
        s(26),
        ID_RECHECK_GAME,
        font,
    );
    page0.push(game_hint);
    page0.push(game_recheck);

    let nav_y = nav_top();

    let mut page1 = Vec::new();
    let mut component_checks = [ptr::null_mut(); 3];
    let mut component_version_controls = [ptr::null_mut(); 3];
    let component_names = [
        "BepInEx（Mod 框架）",
        "MetaMystia（Mod 本体）",
        "ResourceExample（可选内容）",
    ];
    // BepInEx 固定使用最新版，不提供历史版本
    let supports_history = [false, true, true];
    let component_versions = ["正在检测…", "正在检测…", "正在检测…"];

    let history_width = button_width(font, &["历史版本…"]);
    let check_width = widest(font, &component_names) + s(32);
    let version_x = margin + check_width + gap;
    let version_width = (width - margin - history_width - gap - version_x).max(s(80));

    for i in 0..3 {
        let row = s(92) + s(30) * i as i32;
        component_checks[i] = create_child(
            hwnd,
            "BUTTON",
            component_names[i],
            WS_CHILD | WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            0,
            margin,
            row,
            check_width,
            s(22),
            ID_CHECK_BEPINEX + i,
            font,
        );
        let check = component_checks[i];
        if i < 2 {
            SendMessageW(check, BM_SETCHECK, 1, 0);
        }
        // BepInEx 是框架，始终由核心逻辑处理；MetaMystia 在全新安装时也必选
        if i == 0 {
            EnableWindow(check, 0);
        }
        page1.push(check);

        component_version_controls[i] = create_child(
            hwnd,
            "STATIC",
            component_versions[i],
            WS_CHILD | SS_ENDELLIPSIS | SS_CENTERIMAGE,
            0,
            version_x,
            row,
            version_width,
            s(22),
            ID_VERSION_BEPINEX + i,
            font,
        );
        secondary.push(component_version_controls[i]);
        page1.push(component_version_controls[i]);

        let has_history = supports_history[i];
        let history = create_child(
            hwnd,
            "BUTTON",
            "历史版本…",
            WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32,
            0,
            width - margin - history_width,
            row - s(3),
            history_width,
            s(26),
            ID_HISTORY_BEPINEX + i,
            font,
        );
        if !has_history {
            EnableWindow(history, 0);
        }
        page1.push(history);
    }

    let resource_note = create_child(
        hwnd,
        "STATIC",
        "ResourceExample 是 MetaMystia 提供的内容扩展包，为游戏增加了新的剧情、稀客、料理与食材等内容，您可根据实际需要选择是否安装。",
        WS_CHILD,
        0,
        margin + s(15),
        s(176),
        content - s(15),
        s(40),
        0,
        font,
    );
    secondary.push(resource_note);
    page1.push(resource_note);

    page1.push(create_child(
        hwnd,
        "STATIC",
        "",
        WS_CHILD | SS_ETCHEDHORZ,
        0,
        margin,
        s(220),
        content,
        s(2),
        0,
        font,
    ));
    let console_label = "游戏启动时显示 BepInEx 日志窗口";
    let console_check = create_child(
        hwnd,
        "BUTTON",
        console_label,
        WS_CHILD | WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        0,
        margin,
        s(234),
        text_width(font, console_label) + s(32),
        s(22),
        ID_CHECK_CONSOLE,
        font,
    );
    page1.push(console_check);
    let console_note = create_child(
        hwnd,
        "STATIC",
        "用于排查加载失败等问题；需要看实时日志时再勾选。",
        WS_CHILD | SS_CENTERIMAGE,
        0,
        margin + s(15),
        s(256),
        content - s(20),
        s(20),
        0,
        font,
    );
    secondary.push(console_note);
    page1.push(console_note);

    let mut page_uninstall = Vec::new();
    let mut uninstall_radios = [ptr::null_mut(); 2];
    let uninstall_labels = ["轻量卸载（推荐）", "完全卸载"];
    let uninstall_ids = [ID_UNINSTALL_LIGHT, ID_UNINSTALL_FULL];
    let uninstall_notes = [
        "只删除 MetaMystia 相关文件；BepInEx 与其他 Mod 保持不变。",
        "彻底还原游戏：同时删除 BepInEx、其他 Mod 与配置文件。",
    ];

    for i in 0..2 {
        let row = s(92) + s(60) * i as i32;
        uninstall_radios[i] = create_child(
            hwnd,
            "BUTTON",
            "",
            WS_CHILD | WS_TABSTOP | BS_AUTORADIOBUTTON as u32,
            0,
            margin,
            row + s(3),
            s(18),
            s(18),
            uninstall_ids[i],
            font,
        );
        if i == 0 {
            SendMessageW(uninstall_radios[i], BM_SETCHECK, 1, 0);
        }
        page_uninstall.push(uninstall_radios[i]);

        page_uninstall.push(create_child(
            hwnd,
            "STATIC",
            uninstall_labels[i],
            WS_CHILD | SS_CENTERIMAGE | SS_NOTIFY,
            0,
            margin + s(18),
            row,
            s(280),
            s(22),
            uninstall_ids[i],
            font,
        ));

        let note = create_child(
            hwnd,
            "STATIC",
            uninstall_notes[i],
            WS_CHILD | SS_CENTERIMAGE,
            0,
            margin + s(18),
            row + s(24),
            content - s(18),
            s(20),
            0,
            font,
        );
        secondary.push(note);
        page_uninstall.push(note);
    }

    let mut page_notes = Vec::new();
    let notes_title = create_child(
        hwnd,
        "STATIC",
        "发行说明",
        WS_CHILD,
        0,
        margin,
        s(92),
        s(400),
        s(20),
        ID_NOTES_TITLE,
        font,
    );
    page_notes.push(notes_title);
    let notes_edit = create_child(
        hwnd,
        "EDIT",
        NOTES_PLACEHOLDER,
        WS_CHILD
            | WS_VSCROLL
            | WS_TABSTOP
            | ES_MULTILINE as u32
            | ES_READONLY as u32
            | ES_AUTOVSCROLL as u32,
        WS_EX_CLIENTEDGE,
        margin,
        s(118),
        content,
        s(168),
        ID_NOTES_EDIT,
        font,
    );
    page_notes.push(notes_edit);
    let notes_hint = create_child(
        hwnd,
        "STATIC",
        "安装将按以上版本进行，确认无误后继续。",
        WS_CHILD | SS_CENTERIMAGE,
        0,
        margin,
        s(294),
        content,
        s(20),
        0,
        font,
    );
    secondary.push(notes_hint);
    page_notes.push(notes_hint);

    let mut page_manage = Vec::new();
    let mut manage_buttons = [ptr::null_mut(); 3];
    let mut manage_statuses = [ptr::null_mut(); 3];
    let mut manage_versions = [ptr::null_mut(); 3];
    let manage_names = [
        "BepInEx（Mod 框架）",
        "MetaMystia（Mod 本体）",
        "ResourceExample（可选内容）",
    ];
    let manage_ids = [ID_MANAGE_BEPINEX, ID_MANAGE_DLL, ID_MANAGE_RES];
    let manage_toggle_ids = [
        ID_MANAGE_TOGGLE_BEPINEX,
        ID_MANAGE_TOGGLE_DLL,
        ID_MANAGE_TOGGLE_RES,
    ];

    let manage_toggle_width = button_width(font, &["禁用", "启用"]);
    let manage_name_width = widest(font, &manage_names) + s(16);
    let manage_version_x = margin + manage_name_width;
    let manage_version_width =
        (width - margin - manage_toggle_width - gap - manage_version_x - s(120)).max(s(80));

    for i in 0..3 {
        let row = s(92) + s(30) * i as i32;
        page_manage.push(create_child(
            hwnd,
            "STATIC",
            manage_names[i],
            WS_CHILD | SS_CENTERIMAGE,
            0,
            margin,
            row,
            manage_name_width,
            s(22),
            manage_ids[i],
            font,
        ));

        manage_versions[i] = create_child(
            hwnd,
            "STATIC",
            "正在检测…",
            WS_CHILD | SS_ENDELLIPSIS | SS_CENTERIMAGE,
            0,
            manage_version_x,
            row,
            manage_version_width,
            s(22),
            manage_ids[i],
            font,
        );
        secondary.push(manage_versions[i]);
        page_manage.push(manage_versions[i]);

        manage_statuses[i] = create_child(
            hwnd,
            "STATIC",
            "",
            WS_CHILD | SS_CENTERIMAGE | SS_RIGHT_CENTER,
            0,
            width - margin - manage_toggle_width - s(72) - gap,
            row,
            s(72),
            s(22),
            manage_ids[i],
            font,
        );
        secondary.push(manage_statuses[i]);
        page_manage.push(manage_statuses[i]);

        manage_buttons[i] = create_child(
            hwnd,
            "BUTTON",
            "禁用",
            WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32,
            0,
            width - margin - manage_toggle_width,
            row - s(3),
            manage_toggle_width,
            s(26),
            manage_toggle_ids[i],
            font,
        );
        EnableWindow(manage_buttons[i], 0);
        page_manage.push(manage_buttons[i]);
    }

    page_manage.push(create_child(
        hwnd,
        "STATIC",
        "",
        WS_CHILD | SS_ETCHEDHORZ,
        0,
        margin,
        s(186),
        content,
        s(2),
        0,
        font,
    ));
    let manage_note = create_child(
        hwnd,
        "STATIC",
        "禁用 BepInEx 会同时让所有依赖它的 Mod 停止加载（包括第三方的），可随时重新启用。",
        WS_CHILD,
        0,
        margin,
        s(198),
        content,
        s(40),
        ID_MANAGE_NOTE,
        font,
    );
    secondary.push(manage_note);
    page_manage.push(manage_note);

    let manage_recheck_width = button_width(font, &["重新检测"]);
    let manage_hint = create_child(
        hwnd,
        "STATIC",
        "检测到游戏正在运行，请先退出游戏。",
        WS_CHILD,
        0,
        margin,
        s(288),
        content - manage_recheck_width - gap,
        s(20),
        0,
        font,
    );
    secondary.push(manage_hint);
    page_manage.push(manage_hint);
    let manage_recheck = create_child(
        hwnd,
        "BUTTON",
        "重新检测",
        WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32,
        0,
        width - margin - manage_recheck_width,
        s(285),
        manage_recheck_width,
        s(26),
        ID_MANAGE_RECHECK,
        font,
    );
    page_manage.push(manage_recheck);

    let mut page2 = Vec::new();
    let install_hint = create_child(
        hwnd,
        "STATIC",
        "正在下载，可随时取消。",
        WS_CHILD | SS_ENDELLIPSIS,
        0,
        margin,
        s(92),
        content,
        s(20),
        ID_INSTALL_HINT,
        font,
    );
    page2.push(install_hint);

    let mut bars = [ptr::null_mut(); 3];
    let mut bar_labels = [ptr::null_mut(); 3];
    let mut bar_percent = [ptr::null_mut(); 3];
    let mut bar_speed = [ptr::null_mut(); 3];

    let percent_width = text_width(font, "100%") + s(8);
    let speed_width = widest(font, &["999.9 MiB/s", "已完成"]) + s(8);
    let name_width = widest(font, &JOB_NAME_SAMPLES).min(content * 42 / 100);
    let bar_width = (content - name_width - percent_width - speed_width - gap * 3).max(s(120));

    for i in 0..3 {
        let row = s(120) + s(32) * i as i32;
        let bar_x = margin + name_width + gap;
        bar_labels[i] = create_child(
            hwnd,
            "STATIC",
            JOB_NAME_SAMPLES[i],
            WS_CHILD | SS_ENDELLIPSIS | SS_CENTERIMAGE,
            0,
            margin,
            row,
            name_width,
            s(22),
            ID_BAR_LABEL + i,
            font,
        );
        page2.push(bar_labels[i]);
        bars[i] = create_child(
            hwnd,
            "msctls_progress32",
            "",
            WS_CHILD,
            0,
            bar_x,
            row,
            bar_width,
            s(22),
            ID_BAR + i,
            font,
        );
        SetWindowTheme(bars[i], ptr::null(), ptr::null());
        SendMessageW(bars[i], PBM_SETRANGE32, 0, 100);
        bar_percent[i] = create_child(
            hwnd,
            "STATIC",
            "",
            WS_CHILD | SS_CENTERIMAGE,
            0,
            bar_x + bar_width + gap,
            row + s(1),
            percent_width,
            s(20),
            ID_BAR_STATUS + i,
            font,
        );
        bar_speed[i] = create_child(
            hwnd,
            "STATIC",
            "",
            WS_CHILD | SS_RIGHT_CENTER,
            0,
            width - margin - speed_width,
            row + s(1),
            speed_width,
            s(20),
            ID_BAR_SPEED + i,
            font,
        );
        page2.push(bars[i]);
        page2.push(bar_percent[i]);
        page2.push(bar_speed[i]);
    }

    let details_label = "详细信息";
    let details_check = create_child(
        hwnd,
        "BUTTON",
        details_label,
        WS_CHILD | WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        0,
        margin,
        s(216),
        text_width(font, details_label) + s(32),
        s(22),
        ID_CHECK_DETAILS,
        font,
    );
    SendMessageW(details_check, BM_SETCHECK, 1, 0);
    page2.push(details_check);
    let log = create_child(
        hwnd,
        "EDIT",
        "",
        WS_CHILD
            | WS_VSCROLL
            | WS_TABSTOP
            | ES_MULTILINE as u32
            | ES_READONLY as u32
            | ES_AUTOVSCROLL as u32,
        WS_EX_CLIENTEDGE,
        margin,
        s(242),
        content,
        s(78),
        ID_LOG,
        font,
    );
    page2.push(log);

    let mut page3 = Vec::new();
    let finish_title = create_child(
        hwnd,
        "STATIC",
        "安装完成",
        WS_CHILD,
        0,
        margin,
        s(100),
        s(300),
        s(32),
        ID_FINISH_TITLE,
        title_font,
    );
    page3.push(finish_title);
    let finish_group = create_child(
        hwnd,
        "BUTTON",
        "已安装内容",
        WS_CHILD | BS_GROUPBOX as u32,
        0,
        margin,
        s(148),
        content,
        s(98),
        0,
        font,
    );
    page3.push(finish_group);

    let finish_items = [("BepInEx", ""), ("MetaMystia", ""), ("ResourceExample", "")];
    let finish_names = finish_items.map(|(name, _)| name);
    let finish_name_width = widest(font, &finish_names) + s(12);
    let mut finish_rows = Vec::new();

    for (index, (name, version)) in finish_items.iter().enumerate() {
        let row = s(176) + s(24) * index as i32;
        let name_row = create_child(
            hwnd,
            "STATIC",
            name,
            WS_CHILD,
            0,
            margin + s(12),
            row,
            finish_name_width,
            s(20),
            0,
            font,
        );
        finish_rows.push(name_row);
        page3.push(name_row);
        let version = create_child(
            hwnd,
            "STATIC",
            version,
            WS_CHILD,
            0,
            margin + s(12) + finish_name_width + gap,
            row,
            s(200),
            s(20),
            0,
            font,
        );
        secondary.push(version);
        finish_rows.push(version);
        page3.push(version);
    }

    let finish_note = create_child(
        hwnd,
        "STATIC",
        "首次启动加载较慢，请耐心等待。",
        WS_CHILD,
        0,
        margin,
        s(256),
        content,
        s(20),
        ID_FINISH_NOTE,
        font,
    );
    secondary.push(finish_note);
    page3.push(finish_note);
    let finish_path = create_child(
        hwnd,
        "EDIT",
        "",
        WS_CHILD | WS_TABSTOP | ES_READONLY as u32 | ES_AUTOHSCROLL as u32,
        WS_EX_CLIENTEDGE,
        margin,
        s(178),
        content,
        s(24),
        0,
        font,
    );
    page3.push(finish_path);
    let finish_open = create_child(
        hwnd,
        "BUTTON",
        "打开所在文件夹",
        WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32,
        0,
        margin,
        s(212),
        button_width(font, &["打开所在文件夹"]),
        s(28),
        ID_OPEN_DIAGNOSTICS,
        font,
    );
    page3.push(finish_open);

    let cancel_width = button_width(font, &["取消"]);
    let next_width = button_width(font, &["下一步 >", "开始安装 >", "完成"]);
    let back_width = button_width(font, &["< 上一步"]);

    let cancel = create_child(
        hwnd,
        "BUTTON",
        "取消",
        WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32,
        0,
        width - margin - cancel_width,
        nav_y,
        cancel_width,
        s(NAV_BUTTON_HEIGHT),
        ID_CANCEL,
        font,
    );
    let next = create_child(
        hwnd,
        "BUTTON",
        "下一步 >",
        WS_CHILD | WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
        0,
        width - margin - cancel_width - gap - next_width,
        nav_y,
        next_width,
        s(NAV_BUTTON_HEIGHT),
        ID_NEXT,
        font,
    );
    let back = create_child(
        hwnd,
        "BUTTON",
        "< 上一步",
        WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32,
        0,
        width - margin - cancel_width - gap - next_width - gap - back_width,
        nav_y,
        back_width,
        s(NAV_BUTTON_HEIGHT),
        ID_BACK,
        font,
    );

    let status_top = window_height() - s(STATUS_BAR_HEIGHT);
    create_child(
        hwnd,
        "STATIC",
        "",
        WS_CHILD | WS_VISIBLE | SS_ETCHEDHORZ,
        0,
        margin,
        status_top,
        content,
        s(2),
        0,
        font,
    );
    let uid_text = create_child(
        hwnd,
        "STATIC",
        &uid_label,
        WS_CHILD | WS_VISIBLE | SS_CENTERIMAGE,
        0,
        margin,
        status_top + s(2),
        uid_width,
        s(STATUS_BAR_HEIGHT) - s(4),
        ID_TRACE_TEXT,
        font,
    );
    secondary.push(uid_text);

    let link_text = format!("<a href=\"{SITE_URL}\">{SITE_URL}</a>");
    let link_x = width - margin - link_width;
    create_child(
        hwnd,
        "SysLink",
        &link_text,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP,
        0,
        link_x,
        status_top + s(3),
        link_width,
        s(STATUS_BAR_HEIGHT) - s(4),
        ID_SITE_LINK,
        font,
    );
    let link_label = create_child(
        hwnd,
        "STATIC",
        SITE_LABEL,
        WS_CHILD | WS_VISIBLE | SS_CENTERIMAGE,
        0,
        link_x - s(2) - label_width,
        status_top + s(2),
        label_width,
        s(STATUS_BAR_HEIGHT) - s(4),
        0,
        font,
    );
    secondary.push(link_label);

    Box::new(State {
        back,
        back_width,
        bepinex_gate: false,
        bar_labels,
        bar_percent,
        bar_speed,
        bars,
        busy: false,
        cancel,
        cancel_width,
        choices: Choices::default(),
        component_checks,
        component_version_controls,
        console_check,
        dialog_font,
        dll_versions: Vec::new(),
        failed: false,
        finish_group,
        finish_note,
        finish_open,
        finish_path,
        finish_rows,
        finish_title,
        font,
        game_hint,
        game_recheck,
        install_hint,
        installed_line,
        job_bar_width: bar_width,
        job_bytes: [0; JOB_SLOTS],
        job_name_width: name_width,
        job_percent_width: percent_width,
        job_speed: [0.0; JOB_SLOTS],
        job_speed_width: speed_width,
        job_total: [None; JOB_SLOTS],
        job_visible: [false; JOB_SLOTS],
        local: LocalInfo::default(),
        log,
        log_visible: true,
        manage_buttons,
        manage_applied: [false; 3],
        manage_hint,
        manage_note,
        manage_pending: [false; 3],
        manage_recheck,
        manage_statuses,
        manage_versions,
        net_attempt: 0,
        net_banner,
        net_error: None,
        net_phase: NET_LOADING,
        net_retry,
        next,
        next_width,
        no_update: false,
        notes_edit,
        notes_title,
        notes_version: None,
        op: OP_INSTALL,
        op_before_self_update: OP_INSTALL,
        option_notes: option_note_hwnds,
        option_labels: option_label_hwnds,
        option_radios,
        page0,
        page1,
        page2,
        page3,
        page_manage,
        page_notes,
        page_op,
        page_uninstall,
        path_edit,
        pending_manager_update: None,
        phase: 0,
        plan: plan_for(OP_INSTALL),
        prefetched: None,
        progress_shown_at: None,
        resource_note,
        resourceex_versions: Vec::new(),
        secondary,
        stage: None,
        step: 0,
        step_text,
        title_font,
        ui: Arc::new(GuiUi::new()),
        uninstall_full: false,
        uninstall_radios,
        update_line,
        width,
    })
}
unsafe fn place_right(hwnd: HWND, right: &mut i32, y: i32, width: i32, visible: bool) {
    if !visible {
        return;
    }

    *right -= width;
    MoveWindow(hwnd, *right, y, width, s(NAV_BUTTON_HEIGHT), 1);
    *right -= s(8);
}

unsafe fn set_close_enabled(hwnd: HWND, enabled: bool) {
    let menu = GetSystemMenu(hwnd, 0);
    if !menu.is_null() {
        let flags = MF_BYCOMMAND | if enabled { MF_ENABLED } else { MF_GRAYED };
        EnableMenuItem(menu, SC_CLOSE, flags);
    }
}

unsafe fn no_component_selected(state: &State) -> bool {
    state
        .component_checks
        .iter()
        .all(|check| SendMessageW(*check, BM_GETCHECK, 0, 0) != 1)
}

unsafe fn console_setting_changed(state: &State) -> bool {
    (SendMessageW(state.console_check, BM_GETCHECK, 0, 0) == 1) != state.local.bepinex_console
}

unsafe fn only_console_change(state: &State) -> bool {
    console_setting_changed(state) && expected_downloads(state).is_empty()
}

unsafe fn refresh_version_checkbox(state: &State, index: usize, selected: &str) {
    let installed = if index == 1 {
        state.local.dll_version.as_deref()
    } else {
        state.local.resourceex_version.as_deref()
    };
    let enabled = installed.map_or(index == 2, |current| {
        state.local.dll_version.is_some() && !VersionInfo::versions_match(current, selected)
    });

    SendMessageW(state.component_checks[index], BM_SETCHECK, 1, 0);
    EnableWindow(state.component_checks[index], i32::from(enabled));

    if index == 2 {
        RedrawWindow(
            state.resource_note,
            ptr::null(),
            ptr::null_mut(),
            RDW_INVALIDATE | RDW_UPDATENOW,
        );
    }
}

unsafe fn update_selection_state(state: &State) {
    EnableWindow(
        state.next,
        i32::from(!no_component_selected(state) || console_setting_changed(state)),
    );
}

fn plan_for(op: usize) -> Vec<usize> {
    match op {
        OP_DIAGNOSTICS => vec![KIND_OPERATION, KIND_DIRECTORY, KIND_PROGRESS, KIND_FINISH],
        OP_MANAGE => vec![
            KIND_OPERATION,
            KIND_DIRECTORY,
            KIND_MANAGE,
            KIND_PROGRESS,
            KIND_FINISH,
        ],
        OP_SELF_UPDATE => vec![KIND_PROGRESS],
        OP_UNINSTALL => vec![
            KIND_OPERATION,
            KIND_DIRECTORY,
            KIND_UNINSTALL,
            KIND_PROGRESS,
            KIND_FINISH,
        ],
        _ => vec![
            KIND_OPERATION,
            KIND_DIRECTORY,
            KIND_COMPONENTS,
            KIND_NOTES,
            KIND_PROGRESS,
            KIND_FINISH,
        ],
    }
}

fn manage_finish_values(state: &State) -> [String; 3] {
    let status = |installed: bool, disabled: bool| -> String {
        if !installed {
            "未安装"
        } else if disabled {
            "已禁用"
        } else {
            "已启用"
        }
        .to_string()
    };

    [
        status(state.local.bepinex_installed, state.local.bepinex_disabled),
        status(state.local.dll_version.is_some(), state.local.dll_disabled),
        status(
            state.local.resourceex_version.is_some(),
            state.local.resourceex_disabled,
        ),
    ]
}

unsafe fn finish_values(state: &State) -> [String; 3] {
    if state.op == OP_MANAGE {
        return manage_finish_values(state);
    }

    if state.op != OP_INSTALL {
        let local = &state.local;
        let result_text = |installed: bool, kept_when_light: bool| -> String {
            if !installed {
                "未安装"
            } else if kept_when_light && !state.uninstall_full {
                "已保留"
            } else {
                "已删除"
            }
            .to_string()
        };

        return [
            result_text(local.bepinex_installed, true),
            result_text(local.dll_version.is_some(), false),
            result_text(local.resourceex_version.is_some(), false),
        ];
    }

    let checked =
        |index: usize| SendMessageW(state.component_checks[index], BM_GETCHECK, 0, 0) == 1;
    let prefetched = state.prefetched.as_ref();
    let latest =
        |pick: fn(&RemoteInfo) -> String| prefetched.map_or_else(|| "未知".to_string(), pick);

    [
        if checked(0) {
            latest(|p| {
                p.version_info
                    .bepinex_version()
                    .unwrap_or("未知")
                    .to_string()
            })
        } else {
            "未升级".to_string()
        },
        if checked(1) {
            state.choices.dll_version.clone().unwrap_or_else(|| {
                latest(|p| p.version_info.latest_dll().unwrap_or("未知").to_string())
            })
        } else {
            "未升级".to_string()
        },
        if checked(2) {
            state.choices.resourceex_version.clone().unwrap_or_else(|| {
                latest(|p| {
                    p.version_info
                        .latest_resourceex()
                        .unwrap_or("未知")
                        .to_string()
                })
            })
        } else {
            "未选择".to_string()
        },
    ]
}

unsafe fn apply_manage_finish_text(state: &State) {
    SetWindowTextW(state.finish_title, wide("设置已应用").as_ptr());
    SetWindowTextW(
        state.finish_note,
        wide("已更新 Mod 的启用状态，随时可以再次修改。").as_ptr(),
    );
}

unsafe fn apply_finish_summary(state: &State) {
    let install = state.op == OP_INSTALL;
    let manage = state.op == OP_MANAGE;
    let upgraded = matches!(state.choices.operation, Some(OperationMode::Upgrade));
    let uninstall = state.op == OP_UNINSTALL;
    let show_summary = install || manage || uninstall;

    SetWindowTextW(
        state.finish_group,
        wide(if manage {
            "组件状态"
        } else if install {
            if upgraded {
                "更新结果"
            } else {
                "已安装内容"
            }
        } else {
            "卸载结果"
        })
        .as_ptr(),
    );

    let values = finish_values(state);

    for (index, value) in values.iter().enumerate() {
        SetWindowTextW(state.finish_rows[index * 2 + 1], wide(value).as_ptr());
    }

    for row in &state.finish_rows {
        ShowWindow(*row, if show_summary { SW_SHOW } else { SW_HIDE });
    }

    ShowWindow(
        state.finish_group,
        if show_summary { SW_SHOW } else { SW_HIDE },
    );
    let diagnostics = state.op == OP_DIAGNOSTICS;
    ShowWindow(
        state.finish_path,
        if diagnostics { SW_SHOW } else { SW_HIDE },
    );
    ShowWindow(
        state.finish_open,
        if diagnostics { SW_SHOW } else { SW_HIDE },
    );
    MoveWindow(
        state.finish_note,
        s(MARGIN),
        if show_summary { s(256) } else { s(150) },
        state.width - s(MARGIN * 2),
        s(20),
        1,
    );
}

unsafe fn apply_finish(state: &State) {
    match state.op {
        OP_DIAGNOSTICS => {
            SetWindowTextW(state.finish_title, wide("导出完成").as_ptr());
            SetWindowTextW(state.finish_note, wide("诊断包已保存到：").as_ptr());
            if let Some(path) = state.ui.diagnostics_path() {
                SetWindowTextW(state.finish_path, wide(&path).as_ptr());
            }
        }
        OP_MANAGE => apply_manage_finish_text(state),
        OP_UNINSTALL => {
            SetWindowTextW(state.finish_title, wide("卸载完成").as_ptr());
            SetWindowTextW(
                state.finish_note,
                wide(if state.uninstall_full {
                    "已还原为未安装 Mod 的状态。"
                } else {
                    "MetaMystia 已卸载；BepInEx 与其他 Mod 未受影响。"
                })
                .as_ptr(),
            );
        }
        _ => {
            let upgraded = matches!(state.choices.operation, Some(OperationMode::Upgrade));
            let console_only = only_console_change(state);
            let no_change = state.no_update && !console_only;

            SetWindowTextW(
                state.finish_title,
                wide(if no_change {
                    "已是最新版本"
                } else if upgraded {
                    "更新完成"
                } else {
                    "安装完成"
                })
                .as_ptr(),
            );
            SetWindowTextW(
                state.finish_note,
                wide(if console_only {
                    "已应用 BepInEx 日志设置；未更新任何组件。"
                } else if no_change {
                    "所有已安装组件均为最新版本，未做任何改动。"
                } else {
                    "现在可以启动游戏；首次启动加载较慢，请耐心等待。"
                })
                .as_ptr(),
            );
        }
    }

    apply_finish_summary(state);
}

unsafe fn update_step_text(state: &State) {
    let kind = state.plan[state.step];
    let name = if kind == KIND_PROGRESS && state.op == OP_INSTALL {
        if only_console_change(state) {
            "应用设置"
        } else if expected_downloads(state).is_empty() {
            "检查更新"
        } else {
            kind_name(kind, state.op, state.local.dll_version.is_some())
        }
    } else {
        kind_name(kind, state.op, state.local.dll_version.is_some())
    };

    SetWindowTextW(
        state.step_text,
        wide(&format!(
            "步骤 {}/{} · {name}",
            state.step + 1,
            state.plan.len()
        ))
        .as_ptr(),
    );
}

unsafe fn show_page(state: &mut State, step: usize) {
    state.step = step;
    let kind = state.plan[step];

    if kind == KIND_PROGRESS && state.progress_shown_at.is_none() {
        state.progress_shown_at = Some(Instant::now());
    }

    show_page_controls(state, kind);
    show_page_nav(state, kind, step);

    match kind {
        KIND_DIRECTORY => {
            let running = check_game_running_cached().unwrap_or(false);
            let has_path = !state.choices.game_root.as_os_str().is_empty();
            ShowWindow(state.game_hint, if running { SW_SHOW } else { SW_HIDE });
            ShowWindow(state.game_recheck, if running { SW_SHOW } else { SW_HIDE });
            if running || !has_path {
                EnableWindow(state.next, 0);
            }
        }
        KIND_MANAGE => refresh_manage_ui(state),
        KIND_OPERATION => update_net_ui(state),
        _ => {}
    }
}

unsafe fn show_page_controls(state: &State, kind: usize) {
    for (index, controls) in [
        (KIND_OPERATION, &state.page_op),
        (KIND_DIRECTORY, &state.page0),
        (KIND_COMPONENTS, &state.page1),
        (KIND_MANAGE, &state.page_manage),
        (KIND_NOTES, &state.page_notes),
        (KIND_PROGRESS, &state.page2),
        (KIND_FINISH, &state.page3),
        (KIND_UNINSTALL, &state.page_uninstall),
    ] {
        for control in controls.iter().copied() {
            ShowWindow(control, if index == kind { SW_SHOW } else { SW_HIDE });
        }
    }

    ShowWindow(
        state.log,
        if kind == KIND_PROGRESS && state.log_visible {
            SW_SHOW
        } else {
            SW_HIDE
        },
    );

    if kind == KIND_FINISH {
        apply_finish(state);
    }

    // 每次切页先恢复“下一步”的默认可用状态，再按当前页收紧；
    // 否则组件页/目录页禁用过的状态会被带到下一页
    EnableWindow(state.next, 1);

    if kind == KIND_COMPONENTS {
        update_selection_state(state);
    }

    update_step_text(state);
}

unsafe fn show_page_nav(state: &State, kind: usize, step: usize) {
    let (back_visible, next_visible, cancel_visible) = match kind {
        KIND_FINISH => (false, true, false),
        KIND_OPERATION => (false, true, true),
        KIND_PROGRESS => (false, false, true),
        _ => (true, true, true),
    };

    ShowWindow(state.back, if back_visible { SW_SHOW } else { SW_HIDE });
    ShowWindow(state.next, if next_visible { SW_SHOW } else { SW_HIDE });
    ShowWindow(state.cancel, if cancel_visible { SW_SHOW } else { SW_HIDE });
    EnableWindow(state.back, i32::from(step > 0));

    match kind {
        KIND_FINISH => {
            SetWindowTextW(state.next, wide("完成").as_ptr());
        }
        KIND_MANAGE => {
            SetWindowTextW(state.next, wide("开始应用 >").as_ptr());
        }
        KIND_NOTES => {
            SetWindowTextW(
                state.next,
                wide(if state.local.dll_version.is_some() {
                    "开始更新 >"
                } else {
                    "开始安装 >"
                })
                .as_ptr(),
            );
        }
        KIND_UNINSTALL => {
            SetWindowTextW(state.next, wide("开始卸载 >").as_ptr());
        }
        _ => {
            SetWindowTextW(state.next, wide("下一步 >").as_ptr());
        }
    }

    let nav_y = nav_top();
    let mut right = state.width - s(MARGIN);

    place_right(
        state.cancel,
        &mut right,
        nav_y,
        state.cancel_width,
        cancel_visible,
    );
    place_right(
        state.next,
        &mut right,
        nav_y,
        state.next_width,
        next_visible,
    );
    place_right(
        state.back,
        &mut right,
        nav_y,
        state.back_width,
        back_visible,
    );
}

unsafe fn set_notes_title(state: &State, version: &str) {
    SetWindowTextW(
        state.notes_title,
        wide(&format!("MetaMystia {version} 发行说明")).as_ptr(),
    );
}

/// 去掉 GitHub Release 里渲染不了的 Markdown 标记，文字内容原样保留。
fn render_notes(body: &str) -> String {
    let mut lines: Vec<String> = Vec::new();

    for raw in body.lines() {
        let trimmed = raw.trim();

        if trimmed.is_empty() {
            lines.push(String::new());
            continue;
        }
        if matches!(trimmed, "---" | "***" | "___") {
            continue;
        }

        if let Some(heading) = trimmed.strip_prefix('#') {
            lines.push(heading.trim_start_matches('#').trim().to_string());
            continue;
        }

        if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            lines.push(format!("· {}", strip_inline_markdown(item)));
            continue;
        }

        lines.push(strip_inline_markdown(trimmed));
    }

    let mut text = String::new();
    let mut blank = 0;

    for line in lines {
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }

        if !text.is_empty() {
            text.push_str("\r\n");
        }
        text.push_str(&line);
    }

    text.trim().to_string()
}

/// 去掉行内的 `**`、`__`、反引号，并把 Markdown 链接转成“文字（链接）”。
fn strip_inline_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("**").or_else(|| rest.strip_prefix("__")) {
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix('`') {
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix('[')
            && let Some(label_end) = after.find("](")
            && let Some(url_end) = after[label_end + 2..].find(')')
        {
            let label = &after[..label_end];
            let url = &after[label_end + 2..label_end + 2 + url_end];

            if label == url {
                out.push_str(url);
            } else {
                out.push_str(label);
                out.push('（');
                out.push_str(url);
                out.push('）');
            }

            rest = &after[label_end + 2 + url_end + 1..];
            continue;
        }

        let ch = rest.chars().next().unwrap_or_default();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }

    out
}

unsafe fn update_net_ui(state: &mut State) {
    let failed = state.net_phase == NET_FAILED;
    let loading = state.net_phase == NET_LOADING;

    let banner = match state.net_phase {
        NET_FAILED => state
            .net_error
            .clone()
            .unwrap_or_else(|| "无法连接服务器，请稍后重试。".to_string()),
        NET_LOADING if state.net_attempt == 0 => "正在获取版本信息…".to_string(),
        NET_LOADING => format!("正在重新获取（第 {} 次尝试）…", state.net_attempt + 1),
        _ => String::new(),
    };

    SetWindowTextW(state.net_banner, wide(&banner).as_ptr());
    ShowWindow(
        state.net_banner,
        if loading || failed { SW_SHOW } else { SW_HIDE },
    );
    ShowWindow(state.net_retry, if failed { SW_SHOW } else { SW_HIDE });
    EnableWindow(state.net_retry, i32::from(failed));

    let install_enabled = state.net_phase == NET_OK;
    EnableWindow(state.option_radios[0], i32::from(install_enabled));
    EnableWindow(state.option_labels[0], i32::from(install_enabled));
    EnableWindow(state.option_notes[0], i32::from(install_enabled));

    SetWindowTextW(
        state.option_notes[0],
        wide(match state.net_phase {
            NET_FAILED => "需要联网获取版本信息，请检查网络或代理后重试。",
            NET_OK => "未安装时安装，已安装时更新。",
            _ => "正在获取版本信息，请稍候…",
        })
        .as_ptr(),
    );

    for (index, radio) in state.option_radios.iter().enumerate() {
        SendMessageW(
            *radio,
            BM_SETCHECK,
            usize::from(option_operation(index) == state.op),
            0,
        );
    }

    if state.step == 0 && state.plan.first() == Some(&KIND_OPERATION) {
        let enabled = state.op != OP_INSTALL || state.net_phase == NET_OK;
        EnableWindow(state.next, i32::from(enabled));
    }

    update_step_text(state);
}

unsafe fn fail_progress(hwnd: HWND, state: &mut State, message: &str) {
    state.failed = true;
    // 操作已经停下，关窗不再拦截
    state.phase = 0;

    SetWindowTextW(state.install_hint, wide(message).as_ptr());
    append_log(state, &format!("错误：{message}"));

    SetWindowTextW(state.next, wide("重试 >").as_ptr());
    SetWindowTextW(state.cancel, wide("取消").as_ptr());
    ShowWindow(state.next, SW_SHOW);
    ShowWindow(state.back, SW_SHOW);
    ShowWindow(state.cancel, SW_SHOW);
    EnableWindow(state.back, 1);
    EnableWindow(state.cancel, 1);
    set_close_enabled(hwnd, true);
}

unsafe fn append_log(state: &State, line: &str) {
    let length = GetWindowTextLengthW(state.log) as usize;
    SendMessageW(state.log, EM_SETSEL, length, length as isize);
    let mut text = String::from(line);
    text.push_str("\r\n");
    SendMessageW(state.log, EM_REPLACESEL, 0, wide(&text).as_ptr() as isize);
}

unsafe fn drain_events(hwnd: HWND) {
    let state = window_state(hwnd);
    if state.is_null() {
        return;
    }
    let state = &mut *state;

    for event in state.ui.take_events() {
        apply_event(hwnd, state, event);
    }

    for (slot, downloaded, speed) in state.ui.take_progress() {
        if slot < JOB_SLOTS {
            state.job_bytes[slot] = downloaded;
            state.job_speed[slot] = speed;
            update_job_row(state, slot);
        }
    }
}

unsafe fn update_job_row(state: &State, slot: usize) {
    let downloaded = state.job_bytes[slot];
    let speed = human_speed(state.job_speed[slot]);

    match state.job_total[slot] {
        Some(total) if total > 0 => {
            let percent = (downloaded.min(total) * 100 / total) as usize;
            SendMessageW(state.bars[slot], PBM_SETPOS, percent, 0);
            SetWindowTextW(
                state.bar_percent[slot],
                wide(&format!("{percent}%")).as_ptr(),
            );
            SetWindowTextW(state.bar_speed[slot], wide(&speed).as_ptr());
        }
        _ => {
            SetWindowTextW(state.bar_percent[slot], wide("").as_ptr());
            SetWindowTextW(state.bar_speed[slot], wide(&speed).as_ptr());
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "后台事件分发，一个 match 覆盖所有事件"
)]
unsafe fn apply_event(hwnd: HWND, state: &mut State, event: Event) {
    match event {
        Event::Confirm(request) => {
            let confirmed = confirm_dialog(
                hwnd,
                &window_caption(),
                &request.instruction,
                &request.content,
                &request.confirm_label,
                &request.cancel_label,
                state.font,
                state.dialog_font,
            );
            let _ = request.reply.send(confirmed);
        }
        Event::Done(error) => {
            state.busy = false;
            state.phase = 0;
            state.stage = None;
            state.ui.set_download_aborted(false);
            EnableWindow(state.cancel, 1);
            set_close_enabled(hwnd, true);

            if let Some(shown_at) = state.progress_shown_at.take()
                && let Some(remaining) = PROGRESS_MIN_VISIBLE.checked_sub(shown_at.elapsed())
            {
                thread::sleep(remaining);
            }

            match error {
                None => show_page(state, state.plan.len() - 1),
                Some(message) => {
                    if state.ui.was_cancelled() {
                        state.ui.set_cancelled(false);
                        reset_job_rows(state);
                        show_page(state, state.step.saturating_sub(1));
                    } else {
                        fail_progress(hwnd, state, &message);
                    }
                }
            }

            if let Some(latest) = state.pending_manager_update.take() {
                prompt_manager_update(hwnd, state, &latest);
            }
        }
        Event::Error(text) => append_log(state, &format!("错误：{text}")),
        Event::JobFinish {
            message,
            outcome,
            slot,
        } => {
            if slot < JOB_SLOTS {
                match outcome {
                    JobOutcome::Failed => update_job_row(state, slot),
                    JobOutcome::Completed => {
                        SendMessageW(state.bars[slot], PBM_SETPOS, 100, 0);
                        SetWindowTextW(state.bar_percent[slot], wide("100%").as_ptr());
                    }
                }

                SetWindowTextW(
                    state.bar_speed[slot],
                    wide(match outcome {
                        JobOutcome::Failed => "下载失败",
                        JobOutcome::Completed => "已完成",
                    })
                    .as_ptr(),
                );
                append_log(state, &message);
            }
        }
        Event::JobPlan(items) => {
            for slot in 0..JOB_SLOTS {
                if state.job_visible[slot] && !items.iter().any(|(planned, _)| *planned == slot) {
                    set_job_row_visible(state, slot, false);
                }
            }

            for (slot, label) in items {
                if slot < JOB_SLOTS {
                    plan_job_row(state, slot, &label);
                }
            }
        }
        Event::JobStart { name, slot, total } => {
            if slot < JOB_SLOTS {
                state.job_total[slot] = total;
                state.job_bytes[slot] = 0;
                state.job_speed[slot] = 0.0;

                SetWindowTextW(state.bar_labels[slot], wide(&name).as_ptr());
                SetWindowTextW(state.bar_percent[slot], wide("0%").as_ptr());
                SetWindowTextW(state.bar_speed[slot], wide("").as_ptr());
                SendMessageW(state.bars[slot], PBM_SETPOS, 0, 0);
                SendMessageW(state.bars[slot], PBM_SETBARCOLOR, PBM_DEFAULT_BAR_COLOR, 0);

                set_job_row_visible(state, slot, true);
            }
        }
        Event::Log(text) => append_log(state, &text),
        Event::ManageCompleted {
            bepinex,
            dll,
            resourceex,
        } => {
            state.local.bepinex_disabled = !bepinex;
            state.local.dll_disabled = !dll;
            state.local.resourceex_disabled = !resourceex;
            state.manage_applied = [!bepinex, !dll, !resourceex];
            state.manage_pending = [false; 3];
            // 应用完成后重画说明文字，让“待禁用”时的红字恢复为灰字
            RedrawWindow(
                state.manage_note,
                ptr::null(),
                ptr::null_mut(),
                RDW_INVALIDATE | RDW_UPDATENOW,
            );
        }
        Event::NoUpdate => state.no_update = true,
        Event::Notes { notes, version } => {
            // 用户可能已经切换到别的版本，忽略过期请求的结果
            if state.notes_version.as_deref() == Some(version.as_str()) {
                set_notes_title(state, &version);
                let text = match notes {
                    None => format!("暂未获取到 {version} 的发行说明。"),
                    Some((_tag, _name, body)) => {
                        let rendered = render_notes(&body);
                        if rendered.is_empty() {
                            "该版本没有填写发行说明。".to_string()
                        } else {
                            rendered
                        }
                    }
                };
                SetWindowTextW(state.notes_edit, wide(&text).as_ptr());
            }
        }
        Event::Local(local) => apply_local_update(state, &local),
        Event::Remote(remote) => apply_remote(hwnd, state, remote),
        Event::SelfUpdate(error) => {
            state.busy = false;
            state.phase = 0;
            state.stage = None;
            EnableWindow(state.cancel, 1);
            set_close_enabled(hwnd, true);

            if let Some(message) = error {
                append_log(state, &format!("管理工具更新失败：{message}"));
                let _modal = ModalScope::enter(hwnd);

                MessageBoxW(
                    hwnd,
                    wide(&format!(
                        "管理工具更新失败：{message}\n可继续使用当前版本。"
                    ))
                    .as_ptr(),
                    wide(&window_caption()).as_ptr(),
                    MB_OK | MB_ICONINFORMATION,
                );
            }

            state.op = state.op_before_self_update;
            state.plan = plan_for(state.op);
            show_page(state, 0);
        }
        Event::Stage(next_stage) => {
            state.stage = Some(next_stage);

            match next_stage {
                Stage::Cleanup => {
                    state.phase = 1;
                    SetWindowTextW(state.install_hint, wide("正在清理旧文件…").as_ptr());
                    EnableWindow(state.cancel, 0);
                    set_close_enabled(hwnd, false);
                }
                Stage::Deploy => {
                    state.phase = 1;
                    SetWindowTextW(state.install_hint, wide("正在安装，请勿关闭窗口…").as_ptr());
                    EnableWindow(state.cancel, 0);
                    set_close_enabled(hwnd, false);
                }
                Stage::Download => {
                    SetWindowTextW(state.install_hint, wide("正在下载，可随时取消。").as_ptr());
                    EnableWindow(state.cancel, 1);
                    set_close_enabled(hwnd, true);
                }
                Stage::Login => {
                    SetWindowTextW(
                        state.install_hint,
                        wide("请在浏览器中完成登录并确认授权…").as_ptr(),
                    );
                    EnableWindow(state.cancel, 0);
                    set_close_enabled(hwnd, true);
                }
            }
        }
    }
}

unsafe fn reset_job_rows(state: &mut State) {
    for slot in 0..JOB_SLOTS {
        state.job_total[slot] = None;
        state.job_bytes[slot] = 0;
        state.job_speed[slot] = 0.0;
        state.job_visible[slot] = false;
        SendMessageW(state.bars[slot], PBM_SETPOS, 0, 0);
        SendMessageW(state.bars[slot], PBM_SETBARCOLOR, PBM_DEFAULT_BAR_COLOR, 0);
        SetWindowTextW(state.bar_percent[slot], wide("").as_ptr());
        SetWindowTextW(state.bar_speed[slot], wide("").as_ptr());
        ShowWindow(state.bar_labels[slot], SW_HIDE);
        ShowWindow(state.bars[slot], SW_HIDE);
        ShowWindow(state.bar_percent[slot], SW_HIDE);
        ShowWindow(state.bar_speed[slot], SW_HIDE);
    }

    layout_job_rows(state);
}

unsafe fn layout_job_rows(state: &State) {
    let mut row: i32 = 0;

    for slot in 0..JOB_SLOTS {
        if !state.job_visible[slot] {
            continue;
        }

        let y = s(120) + s(32) * row;
        let bar_x = s(MARGIN) + state.job_name_width + s(8);
        let percent_x = bar_x + state.job_bar_width + s(8);
        let speed_x = state.width - s(MARGIN) - state.job_speed_width;

        MoveWindow(
            state.bar_labels[slot],
            s(MARGIN),
            y,
            state.job_name_width,
            s(22),
            1,
        );
        MoveWindow(state.bars[slot], bar_x, y, state.job_bar_width, s(22), 1);
        MoveWindow(
            state.bar_percent[slot],
            percent_x,
            y + s(1),
            state.job_percent_width,
            s(20),
            1,
        );
        MoveWindow(
            state.bar_speed[slot],
            speed_x,
            y + s(1),
            state.job_speed_width,
            s(20),
            1,
        );

        row += 1;
    }
}

unsafe fn set_job_row_visible(state: &mut State, slot: usize, visible: bool) {
    state.job_visible[slot] = visible;

    for control in [
        state.bar_labels[slot],
        state.bars[slot],
        state.bar_percent[slot],
        state.bar_speed[slot],
    ] {
        ShowWindow(control, if visible { SW_SHOW } else { SW_HIDE });
    }

    layout_job_rows(state);
}

unsafe fn plan_job_row(state: &mut State, slot: usize, label: &str) {
    state.job_total[slot] = None;
    state.job_bytes[slot] = 0;
    state.job_speed[slot] = 0.0;

    SetWindowTextW(state.bar_labels[slot], wide(label).as_ptr());
    SetWindowTextW(state.bar_percent[slot], wide("").as_ptr());
    SetWindowTextW(state.bar_speed[slot], wide("等待中").as_ptr());
    SendMessageW(state.bars[slot], PBM_SETPOS, 0, 0);
    SendMessageW(state.bars[slot], PBM_SETBARCOLOR, PBM_DEFAULT_BAR_COLOR, 0);

    set_job_row_visible(state, slot, true);
}

fn version_label(installed: Option<&str>, latest: &str) -> String {
    match installed {
        None => format!("未安装 · 最新 {latest}"),
        Some(installed) if VersionInfo::versions_match(installed, latest) => {
            format!("已安装 {installed}（已是最新）")
        }
        Some(installed) => format!("已安装 {installed} → 最新 {latest}"),
    }
}

fn bepinex_version_label(installed: bool, version: Option<&str>, latest: &str) -> String {
    match (installed, version) {
        (false, _) => format!("未安装 · 最新 {latest}"),
        (true, Some(version)) if VersionInfo::versions_match(version, latest) => {
            format!("已安装 {version}（已是最新）")
        }
        (true, Some(version)) => format!("已安装 {version} → 最新 {latest}"),
        (true, None) => format!("已安装（版本未知） · 最新 {latest}"),
    }
}

fn disabled_suffix(label: String, disabled: bool) -> String {
    if disabled {
        format!("{label}（已禁用）")
    } else {
        label
    }
}

/// 本次操作预计要下载的组件；实际清单以下载开始时的登记为准。
unsafe fn expected_downloads(state: &State) -> Vec<(usize, &'static str)> {
    if state.op != OP_INSTALL {
        return Vec::new();
    }

    let Some(prefetched) = state.prefetched.as_ref() else {
        return Vec::new();
    };

    let local = &state.local;
    let upgrade = local.dll_version.is_some();
    let checked =
        |index: usize| SendMessageW(state.component_checks[index], BM_GETCHECK, 0, 0) == 1;

    let latest_bepinex = prefetched.version_info.bepinex_version().ok();
    let wanted_resourceex = state
        .choices
        .resourceex_version
        .as_deref()
        .or_else(|| prefetched.version_info.latest_resourceex().ok());

    let outdated = |installed: Option<&str>, wanted: Option<&str>| match (installed, wanted) {
        (Some(installed), Some(wanted)) => !VersionInfo::versions_match(installed, wanted),
        (None, Some(_)) => true,
        _ => false,
    };

    let mut items: Vec<(usize, &'static str)> = Vec::new();

    if checked(0)
        && let Some(latest) = latest_bepinex
        && (!upgrade
            || !local.bepinex_installed
            || local.bepinex_version.as_deref() != Some(latest))
    {
        let slot = items.len();
        items.push((slot, "BepInEx"));
    }

    if dll_will_change(state) {
        let slot = items.len();
        items.push((slot, "MetaMystia"));
    }

    if checked(2) && (!upgrade || outdated(local.resourceex_version.as_deref(), wanted_resourceex))
    {
        let slot = items.len();
        items.push((slot, "ResourceExample"));
    }

    items
}

/// 本次是否会更换 MetaMystia DLL；用于决定是否展示发行说明。
unsafe fn dll_will_change(state: &State) -> bool {
    if state.op != OP_INSTALL {
        return false;
    }

    let Some(prefetched) = state.prefetched.as_ref() else {
        return false;
    };

    if SendMessageW(state.component_checks[1], BM_GETCHECK, 0, 0) != 1 {
        return false;
    }

    let wanted = state
        .choices
        .dll_version
        .as_deref()
        .or_else(|| prefetched.version_info.latest_dll().ok());

    match (state.local.dll_version.as_deref(), wanted) {
        (Some(installed), Some(wanted)) => !VersionInfo::versions_match(installed, wanted),
        (None, Some(_)) => true,
        _ => false,
    }
}

/// 提示并进入管理工具自更新；版本升级不可跳过，但允许关闭窗口退出。
unsafe fn prompt_manager_update(hwnd: HWND, state: &mut State, latest: &str) {
    let current = env!("CARGO_PKG_VERSION");
    let go = notice_dialog(
        hwnd,
        &window_caption(),
        "需要更新管理工具",
        &format!(
            "当前 v{current}，最新 v{latest}。\n为保证兼容性，将先更新管理工具，更新完成后会自动重新打开。"
        ),
        "立即更新",
        state.font,
        state.dialog_font,
    );

    if go {
        start_self_update(hwnd, state);
    } else {
        // 不能跳过升级，但允许直接退出
        report_event("SelfUpdate.Declined", None);
        append_log(state, "已退出管理工具；请更新后再使用。");
        ShowWindow(hwnd, SW_HIDE);
        run_shutdown();
        process::exit(0);
    }
}

unsafe fn apply_local_update(state: &mut State, local: &LocalInfo) {
    apply_local(state, local);

    if state
        .plan
        .get(state.step)
        .is_some_and(|kind| *kind == KIND_DIRECTORY || *kind == KIND_MANAGE)
    {
        show_page(state, state.step);
    }

    update_net_ui(state);
}

#[allow(
    clippy::too_many_lines,
    reason = "远端结果一次性铺到组件页/发行说明页，集中在一起便于对照"
)]
unsafe fn apply_remote(hwnd: HWND, state: &mut State, remote: Result<RemoteInfo, ManagerError>) {
    match remote {
        Ok(prefetched) => {
            let local = state.local.clone();
            state.net_phase = NET_OK;
            state.net_error = None;
            state.net_attempt = 0;

            let bepinex_latest = prefetched.version_info.bepinex_version().ok();
            let bepinex_latest_text = bepinex_latest.unwrap_or("未知");
            let dll_latest = prefetched
                .version_info
                .latest_dll()
                .unwrap_or("未知")
                .to_string();
            let resourceex_latest = prefetched
                .version_info
                .latest_resourceex()
                .ok()
                .map(ToString::to_string);

            let labels = [
                disabled_suffix(
                    bepinex_version_label(
                        local.bepinex_installed,
                        local.bepinex_version.as_deref(),
                        bepinex_latest_text,
                    ),
                    local.bepinex_disabled,
                ),
                disabled_suffix(
                    version_label(local.dll_version.as_deref(), &dll_latest),
                    local.dll_disabled,
                ),
                disabled_suffix(
                    resourceex_latest.as_deref().map_or_else(
                        || "暂无可用版本".to_string(),
                        |latest| version_label(local.resourceex_version.as_deref(), latest),
                    ),
                    local.resourceex_disabled,
                ),
            ];

            for (index, label) in labels.iter().enumerate() {
                SetWindowTextW(
                    state.component_version_controls[index],
                    wide(label).as_ptr(),
                );
            }

            state.dll_versions.clone_from(&prefetched.version_info.dlls);
            state
                .resourceex_versions
                .clone_from(&prefetched.version_info.zips);

            let installed_any = local.bepinex_installed
                || local.dll_version.is_some()
                || local.resourceex_version.is_some();
            let upgrade = local.dll_version.is_some();

            let mut updates = Vec::new();
            let mut installs = Vec::new();
            if local.bepinex_installed {
                if let Some(latest) = bepinex_latest
                    && local.bepinex_version.as_deref() != Some(latest)
                {
                    updates.push(format!("BepInEx {latest}"));
                }
            } else if let Some(latest) = bepinex_latest
                && (local.dll_version.is_some() || local.resourceex_version.is_some())
            {
                installs.push(format!("BepInEx {latest}"));
            }
            if local.dll_version.is_some()
                && local.dll_version.as_deref() != Some(dll_latest.as_str())
            {
                updates.push(format!("MetaMystia {dll_latest}"));
            }
            if let Some(latest) = resourceex_latest.as_deref()
                && local.resourceex_version.is_some()
                && local.resourceex_version.as_deref() != Some(latest)
            {
                updates.push(format!("ResourceExample {latest}"));
            }

            let update_text = match (updates.is_empty(), installs.is_empty()) {
                (false, false) => format!(
                    "可更新：{}；将安装：{}",
                    updates.join("、"),
                    installs.join("、")
                ),
                (false, true) => format!("可更新：{}", updates.join("、")),
                (true, false) => format!("将安装：{}", installs.join("、")),
                (true, true) if installed_any => "所有已安装组件均为最新版本".to_string(),
                (true, true) => String::new(),
            };
            SetWindowTextW(state.update_line, wide(&update_text).as_ptr());

            let bepinex_actionable = upgrade
                && local.bepinex_installed
                && bepinex_latest
                    .is_some_and(|latest| local.bepinex_version.as_deref() != Some(latest));
            SendMessageW(state.component_checks[0], BM_SETCHECK, 1, 0);
            EnableWindow(state.component_checks[0], i32::from(bepinex_actionable));

            let dll_target = state
                .choices
                .dll_version
                .as_deref()
                .or_else(|| prefetched.version_info.latest_dll().ok());
            let dll_actionable = upgrade
                && dll_target.is_some_and(|target| {
                    local
                        .dll_version
                        .as_deref()
                        .is_some_and(|installed| !VersionInfo::versions_match(installed, target))
                });
            SendMessageW(state.component_checks[1], BM_SETCHECK, 1, 0);
            EnableWindow(state.component_checks[1], i32::from(dll_actionable));

            let has_resourceex = local.resourceex_version.is_some();
            let resourceex_target = state
                .choices
                .resourceex_version
                .as_deref()
                .or(resourceex_latest.as_deref());
            let resourceex_actionable = has_resourceex
                && resourceex_target.is_some_and(|target| {
                    local
                        .resourceex_version
                        .as_deref()
                        .is_some_and(|installed| !VersionInfo::versions_match(installed, target))
                });
            let resourceex_enabled = if has_resourceex {
                upgrade && resourceex_actionable
            } else {
                resourceex_target.is_some()
            };
            SendMessageW(
                state.component_checks[2],
                BM_SETCHECK,
                usize::from(has_resourceex || resourceex_enabled),
                0,
            );
            EnableWindow(state.component_checks[2], i32::from(resourceex_enabled));
            SendMessageW(
                state.console_check,
                BM_SETCHECK,
                usize::from(local.bepinex_console),
                0,
            );
            update_selection_state(state);

            set_notes_title(state, &dll_latest);
            state.notes_version = Some(dll_latest);
            if let Some((tag, name, body)) = &prefetched.release_notes {
                let _ = (tag, name);
                let rendered = render_notes(body);
                let text = if rendered.is_empty() {
                    "该版本没有填写发行说明。".to_string()
                } else {
                    rendered
                };
                SetWindowTextW(state.notes_edit, wide(&text).as_ptr());
            } else {
                SetWindowTextW(
                    state.notes_edit,
                    wide("暂未获取到发行说明，可直接继续安装。").as_ptr(),
                );
            }

            let manager_update = prefetched.manager_update.clone();
            state.prefetched = Some(prefetched);

            if let Some(latest) = manager_update {
                if state.busy {
                    // 正在执行操作：等结束后再提示，避免并发运行两套流程
                    state.pending_manager_update = Some(latest);
                } else {
                    prompt_manager_update(hwnd, state, &latest);
                }
            }
        }
        Err(e) => {
            state.net_phase = NET_FAILED;
            state.net_error = Some(format!("{e}"));
        }
    }

    update_net_ui(state);
}

unsafe fn apply_local(state: &mut State, local: &LocalInfo) {
    if let Some(root) = &local.game_root {
        state.choices.game_root.clone_from(root);
        SetWindowTextW(state.path_edit, wide(&root.display().to_string()).as_ptr());
    }

    let mut items = Vec::new();
    if local.bepinex_installed {
        let line = local.bepinex_version.as_deref().map_or_else(
            || "BepInEx（版本未知）".to_string(),
            |version| format!("BepInEx {version}"),
        );
        items.push(if local.bepinex_disabled {
            format!("{line}（已禁用）")
        } else {
            line
        });
    }
    if let Some(version) = local.dll_version.as_deref() {
        items.push(if local.dll_disabled {
            format!("MetaMystia {version}（已禁用）")
        } else {
            format!("MetaMystia {version}")
        });
    }
    if let Some(version) = local.resourceex_version.as_deref() {
        items.push(if local.resourceex_disabled {
            format!("ResourceExample {version}（已禁用）")
        } else {
            format!("ResourceExample {version}")
        });
    }

    let installed = if local.game_root.is_none() {
        "未找到游戏目录，请手动选择。".to_string()
    } else if local.detect_failed {
        "扫描已安装组件失败，请查看详细信息。".to_string()
    } else if items.is_empty() {
        "未检测到已安装的 MetaMystia Mod".to_string()
    } else {
        format!("已安装：{}", items.join("、"))
    };
    SetWindowTextW(state.installed_line, wide(&installed).as_ptr());

    state.local.clone_from(local);
    state.manage_applied = [
        local.bepinex_disabled,
        local.dll_disabled,
        local.resourceex_disabled,
    ];
    state.manage_pending = [false; 3];
}

/// 切换 Mod 的启用 / 禁用状态；如果游戏正在运行则提示先退出游戏。
unsafe fn toggle_manage_target(hwnd: HWND, state: &mut State, index: usize) {
    if check_game_running_cached().unwrap_or(false) {
        let _modal = ModalScope::enter(hwnd);

        MessageBoxW(
            hwnd,
            wide("检测到游戏正在运行，请先退出游戏再修改 Mod 状态。").as_ptr(),
            wide(&window_caption()).as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
        return;
    }

    let Some(disabled) = manage_target_disabled(state, index) else {
        return;
    };

    if index == 0 && !disabled {
        let confirmed = confirm_dialog(
            hwnd,
            &window_caption(),
            "禁用 BepInEx？",
            "禁用 BepInEx 会同时让所有依赖它的 Mod 停止加载（包括第三方的），可随时重新启用。",
            "禁用",
            "取消",
            state.font,
            state.dialog_font,
        );
        if !confirmed {
            return;
        }
    }

    let target = match index {
        0 => "bepinex",
        1 => "dll",
        _ => "resourceex",
    };
    report_event(
        "Manage.Toggle",
        Some(&format!(
            "{target}:{}",
            if disabled { "enabled" } else { "disabled" }
        )),
    );

    match index {
        0 => state.local.bepinex_disabled = !disabled,
        1 => state.local.dll_disabled = !disabled,
        _ => state.local.resourceex_disabled = !disabled,
    }

    state.manage_pending[index] = manage_target_disabled(state, index)
        .is_some_and(|disabled| disabled != state.manage_applied[index]);

    refresh_manage_ui(state);
}

/// 切到执行页并下载新版本；替换脚本会等本进程退出后覆盖并启动新版本。
unsafe fn start_self_update(hwnd: HWND, state: &mut State) {
    state.op_before_self_update = state.op;
    state.op = OP_SELF_UPDATE;
    state.plan = plan_for(OP_SELF_UPDATE);
    state.failed = false;
    state.busy = true;
    state.ui.set_cancelled(false);
    state.ui.set_download_aborted(false);
    state.ui.set_paused(false);
    SetWindowTextW(state.log, wide("").as_ptr());
    SetWindowTextW(
        state.install_hint,
        wide("正在更新管理工具，请勿关闭窗口…").as_ptr(),
    );
    EnableWindow(state.cancel, 0);
    set_close_enabled(hwnd, true);
    show_page(state, 0);

    // 页切换会把三行进度条都显示出来，这里只保留真正用到的那一行
    reset_job_rows(state);
    plan_job_row(state, 0, "管理工具");
    RedrawWindow(
        hwnd,
        ptr::null(),
        ptr::null_mut(),
        RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW,
    );

    let ui = Arc::clone(&state.ui);
    thread::spawn(move || {
        let task_ui = Arc::clone(&ui);
        let result = panic::catch_unwind(panic::AssertUnwindSafe(move || {
            bridge::run_self_update(task_ui.as_ref());
        }));

        if let Err(payload) = result {
            ui.push_event(Event::SelfUpdate(Some(format!(
                "内部错误：{}",
                panic_message(&*payload)
            ))));
        }
    });
}

#[allow(
    clippy::too_many_lines,
    reason = "操作启动：收集界面选择、布置进度行并启动后台线程集中在一处"
)]
unsafe fn start_operation(hwnd: HWND, state: &mut State) {
    if state.op == OP_INSTALL && state.net_phase != NET_OK {
        let _modal = ModalScope::enter(hwnd);

        MessageBoxW(
            hwnd,
            wide("需要先联网获取版本信息，请点击“重试”后继续。").as_ptr(),
            wide(&window_caption()).as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
        return;
    }

    state.failed = false;
    state.no_update = false;
    state.busy = true;
    state.phase = 0;
    SetWindowTextW(state.cancel, wide("取消").as_ptr());

    reset_job_rows(state);

    SetWindowTextW(state.log, wide("").as_ptr());

    let install = state.op == OP_INSTALL;
    let checked =
        |index: usize| SendMessageW(state.component_checks[index], BM_GETCHECK, 0, 0) == 1;

    let mut choices = state.choices.clone();
    let installed = state.local.dll_version.is_some();
    choices.operation = Some(match state.op {
        OP_DIAGNOSTICS => OperationMode::Diagnostics,
        OP_MANAGE => OperationMode::Manage,
        OP_UNINSTALL => OperationMode::Uninstall,
        _ if installed => OperationMode::Upgrade,
        _ => OperationMode::Install,
    });
    choices.install_resourceex = install && checked(2);
    choices.manage_bepinex = (state.op == OP_MANAGE)
        .then_some(state.local.bepinex_installed && !state.local.bepinex_disabled);
    choices.manage_dll = (state.op == OP_MANAGE)
        .then_some(state.local.dll_version.is_some() && !state.local.dll_disabled);
    choices.manage_resourceex = (state.op == OP_MANAGE)
        .then_some(state.local.resourceex_version.is_some() && !state.local.resourceex_disabled);
    choices.upgrade_bepinex = !install || checked(0);
    choices.upgrade_dll = !install || checked(1);
    choices.show_bepinex_console = SendMessageW(state.console_check, BM_GETCHECK, 0, 0) == 1;
    choices.uninstall_full = state.uninstall_full;

    if install {
        report_event(
            "UI.Install.ResourceEx.Choice",
            Some(bridge::yes_no(choices.install_resourceex)),
        );
        report_event(
            "UI.Install.BepInExConsole.Choice",
            Some(bridge::yes_no(choices.show_bepinex_console)),
        );
    }

    state.choices = choices.clone();
    state.ui.set_cancelled(false);
    state.ui.set_download_aborted(false);

    let only_settings = only_console_change(state);
    let planned_downloads = expected_downloads(state);
    let needs_download = !planned_downloads.is_empty();

    SetWindowTextW(
        state.install_hint,
        wide(match state.op {
            OP_DIAGNOSTICS => "正在导出诊断包…",
            OP_MANAGE => "正在应用更改…",
            OP_UNINSTALL => {
                if state.uninstall_full {
                    "正在完全卸载…"
                } else {
                    "正在轻量卸载…"
                }
            }
            _ if only_settings => "正在应用 BepInEx 日志设置…",
            _ if planned_downloads.is_empty() => "正在检查更新…",
            _ => "正在准备…",
        })
        .as_ptr(),
    );
    // 只有安装/升级的下载阶段可以取消，卸载与诊断导出中途不可中断
    let cancellable = state.op == OP_INSTALL;
    EnableWindow(state.cancel, i32::from(cancellable));
    set_close_enabled(hwnd, cancellable);

    show_page(state, state.plan.len() - 2);
    reset_job_rows(state);
    for (slot, label) in planned_downloads {
        plan_job_row(state, slot, label);
    }

    let input = Input {
        dll_version: choices.dll_version.take(),
        game_root: state.choices.game_root.clone(),
        install_resourceex: choices.install_resourceex,
        manage_bepinex: choices.manage_bepinex,
        manage_dll: choices.manage_dll,
        manage_resourceex: choices.manage_resourceex,
        needs_download,
        operation: choices.operation.unwrap_or(OperationMode::Install),
        resourceex_version: choices.resourceex_version.take(),
        show_bepinex_console: choices.show_bepinex_console,
        uninstall_full: choices.uninstall_full,
        upgrade_bepinex: choices.upgrade_bepinex,
        upgrade_dll: choices.upgrade_dll,
    };

    let ui = Arc::clone(&state.ui);
    thread::spawn(move || {
        let task_ui = Arc::clone(&ui);
        let result = panic::catch_unwind(panic::AssertUnwindSafe(move || {
            run_flow(task_ui.as_ref(), &input)
        }));

        if matches!(result, Ok(Err(ManagerError::UserCancelled))) {
            ui.set_cancelled(true);
        }

        let error = match result {
            Ok(Err(e)) => Some(e.to_string()),
            Ok(Ok(())) => None,
            Err(payload) => {
                let message = panic_message(&*payload);
                report_event("Run.Panic", Some(&message));
                Some(format!("内部错误：{message}"))
            }
        };
        ui.push_event(Event::Done(error));
    });
}

#[allow(
    clippy::too_many_lines,
    reason = "控件命令分发，一个 match 覆盖所有控件"
)]
unsafe fn on_command(hwnd: HWND, state: &mut State, id: usize) {
    match id {
        ID_BACK => {
            if state.step > 0 {
                state.failed = false;
                state.phase = 0;

                if state.plan.first() == Some(&KIND_OPERATION)
                    && state.plan.get(state.step) == Some(&KIND_MANAGE)
                    && let Some(root) = state.local.game_root.clone()
                {
                    let local = bridge::detect_local_at(&state.ui, root);
                    apply_local(state, &local);
                }

                show_page(state, state.step - 1);
            }
        }
        ID_BROWSE => {
            let _modal = ModalScope::enter(hwnd);

            if let Some(path) = pick_folder(hwnd, &state.choices.game_root) {
                let local = bridge::detect_local_at(&state.ui, path);
                apply_local(state, &local);
                show_page(state, state.step);
            }
        }
        ID_CANCEL => {
            if state.failed && state.plan[state.step] == KIND_PROGRESS {
                // 失败态下的“取消”就是退出程序，和普通页面一致
                let exit = confirm_dialog(
                    hwnd,
                    &window_caption(),
                    "退出管理工具？",
                    "退出不会修改游戏文件，可随时重新运行。",
                    "退出",
                    "取消",
                    state.font,
                    state.dialog_font,
                );

                if exit {
                    DestroyWindow(hwnd);
                }
                return;
            }
            if state.plan[state.step] != KIND_PROGRESS {
                let exit = confirm_dialog(
                    hwnd,
                    &window_caption(),
                    "退出管理工具？",
                    "退出不会修改游戏文件，可随时重新运行。",
                    "退出",
                    "取消",
                    state.font,
                    state.dialog_font,
                );

                if exit {
                    DestroyWindow(hwnd);
                }
                return;
            }

            if state.op != OP_INSTALL {
                return;
            }

            if state.stage == Some(Stage::Login) {
                let cancel_login = confirm_dialog(
                    hwnd,
                    &window_caption(),
                    "取消登录？",
                    "登录尚未完成，取消后会回到上一步。",
                    "取消登录",
                    "继续登录",
                    state.font,
                    state.dialog_font,
                );

                if cancel_login {
                    state.ui.set_cancelled(true);
                    SetWindowTextW(state.install_hint, wide("正在取消登录…").as_ptr());
                    EnableWindow(state.cancel, 0);
                }

                return;
            }

            state.ui.set_paused(true);
            let stop = confirm_dialog(
                hwnd,
                &window_caption(),
                "停止下载？",
                "已下载的临时文件会被删除，游戏文件不会被修改。",
                "停止下载",
                "继续下载",
                state.font,
                state.dialog_font,
            );
            state.ui.set_paused(false);

            if stop {
                // 通知后台线程在下一个数据块处停下；它结束后会回到上一页
                state.ui.set_cancelled(true);
                SetWindowTextW(state.install_hint, wide("正在停止下载…").as_ptr());
                EnableWindow(state.cancel, 0);
            }
        }
        ID_CHECK_BEPINEX | ID_CHECK_DLL | ID_CHECK_RES | ID_CHECK_CONSOLE => {
            update_selection_state(state);
        }
        ID_CHECK_DETAILS => {
            state.log_visible = !state.log_visible;
            ShowWindow(state.log, if state.log_visible { SW_SHOW } else { SW_HIDE });
        }
        ID_NET_RETRY => {
            if state.net_phase == NET_FAILED {
                state.net_phase = NET_LOADING;
                state.net_attempt += 1;
                update_net_ui(state);
                start_prefetch(Arc::clone(&state.ui));
            }
        }
        ID_NEXT if state.failed && state.plan[state.step] == KIND_PROGRESS => {
            start_operation(hwnd, state);
        }
        ID_NEXT => {
            let kind = state.plan[state.step];
            match kind {
                KIND_COMPONENTS => {
                    let mut plan = plan_for(state.op);
                    if !dll_will_change(state) {
                        plan.retain(|kind| *kind != KIND_NOTES);
                    }
                    state.plan = plan;

                    let next_step = state.step + 1;
                    if state.plan[next_step] == KIND_PROGRESS {
                        start_operation(hwnd, state);
                    } else {
                        show_page(state, next_step);
                    }
                }
                KIND_OPERATION => {
                    state.plan = plan_for(state.op);
                    show_page(state, state.step + 1);
                }
                KIND_FINISH => {
                    DestroyWindow(hwnd);
                }
                KIND_UNINSTALL | KIND_NOTES => start_operation(hwnd, state),
                _ => {
                    let next_step = state.step + 1;

                    if state.plan[next_step] == KIND_PROGRESS {
                        start_operation(hwnd, state);
                    } else {
                        show_page(state, next_step);
                    }
                }
            }
        }
        ID_OPEN_DIAGNOSTICS => {
            if let Some(path) = state.ui.diagnostics_path() {
                let arguments = wide(&format!("/select,\"{path}\""));
                ShellExecuteW(
                    hwnd,
                    wide("open").as_ptr(),
                    wide("explorer.exe").as_ptr(),
                    arguments.as_ptr(),
                    ptr::null(),
                    SW_SHOWNORMAL,
                );
            }
        }
        ID_OP_INSTALL | ID_OP_MANAGE | ID_OP_UNINSTALL | ID_OP_DIAGNOSTICS => {
            // 只有停在“选择操作”页且没有操作在跑时才切换操作类型；
            // 否则控件通知（例如编辑框的 EN_CHANGE）会把正在执行的流程改掉
            let on_operation_step = state.step == 0 && state.plan.first() == Some(&KIND_OPERATION);
            if state.busy || !on_operation_step {
                return;
            }

            let Some(operation) = option_by_id(id) else {
                return;
            };
            state.op = operation;
            state.plan = plan_for(state.op);
            update_net_ui(state);
        }
        ID_RECHECK_GAME => {
            if !check_game_running().unwrap_or(false) {
                ShowWindow(state.game_hint, SW_HIDE);
                ShowWindow(state.game_recheck, SW_HIDE);
                let has_path = !state.choices.game_root.as_os_str().is_empty();
                EnableWindow(state.next, i32::from(has_path));
            }
        }
        ID_MANAGE_RECHECK => {
            if check_game_running().unwrap_or(false) {
                let _modal = ModalScope::enter(hwnd);

                MessageBoxW(
                    hwnd,
                    wide("游戏仍在运行，请先退出游戏。").as_ptr(),
                    wide(&window_caption()).as_ptr(),
                    MB_OK | MB_ICONINFORMATION,
                );
                return;
            }

            ShowWindow(state.manage_hint, SW_HIDE);
            ShowWindow(state.manage_recheck, SW_HIDE);
            EnableWindow(state.next, 1);
            refresh_manage_ui(state);
        }
        ID_MANAGE_TOGGLE_BEPINEX => toggle_manage_target(hwnd, state, 0),
        ID_MANAGE_TOGGLE_DLL => toggle_manage_target(hwnd, state, 1),
        ID_MANAGE_TOGGLE_RES => toggle_manage_target(hwnd, state, 2),
        ID_UNINSTALL_LIGHT | ID_UNINSTALL_FULL => {
            state.uninstall_full = id == ID_UNINSTALL_FULL;
            SendMessageW(
                state.uninstall_radios[0],
                BM_SETCHECK,
                usize::from(!state.uninstall_full),
                0,
            );
            SendMessageW(
                state.uninstall_radios[1],
                BM_SETCHECK,
                usize::from(state.uninstall_full),
                0,
            );
        }
        id if (ID_HISTORY_BEPINEX..ID_HISTORY_BEPINEX + 3).contains(&id) => {
            let index = id - ID_HISTORY_BEPINEX;
            let (title, available): (&str, &Vec<String>) = match index {
                1 => ("选择 MetaMystia 版本", &state.dll_versions),
                2 => ("选择 ResourceExample 版本", &state.resourceex_versions),
                _ => return,
            };

            if available.is_empty() {
                return;
            }

            let versions: Vec<String> = available
                .iter()
                .enumerate()
                .map(|(position, version)| {
                    if position == 0 {
                        format!("{version}（最新）")
                    } else {
                        version.clone()
                    }
                })
                .collect();

            let component = if index == 1 {
                "MetaMystia DLL"
            } else {
                "ResourceEx ZIP"
            };
            let choice_event = format!("UI.SelectHistoricalVersion.Choice.{component}");

            if let Some(choice) = pick_version(hwnd, title, &versions, state.font) {
                let selected = available[choice].clone();
                report_event(&choice_event, Some("yes"));
                report_event("UI.SelectHistoricalVersion.Selected", Some(&selected));

                if index == 1 {
                    state.choices.dll_version = Some(selected.clone());
                    state.notes_version = Some(selected.clone());
                    set_notes_title(state, &selected);
                    SetWindowTextW(state.notes_edit, wide("正在获取该版本的发行说明…").as_ptr());
                    let ui = Arc::clone(&state.ui);
                    let version = selected.clone();
                    thread::spawn(move || {
                        let task_ui = Arc::clone(&ui);
                        let task_version = version.clone();
                        let result = panic::catch_unwind(panic::AssertUnwindSafe(move || {
                            bridge::fetch_release_notes(task_ui.as_ref(), task_version);
                        }));

                        if result.is_err() {
                            ui.push_event(Event::Notes {
                                notes: None,
                                version,
                            });
                        }
                    });
                } else {
                    state.choices.resourceex_version = Some(selected.clone());
                }

                let installed = if index == 1 {
                    state.local.dll_version.as_deref()
                } else {
                    state.local.resourceex_version.as_deref()
                };
                let text = match installed {
                    Some(current) if VersionInfo::versions_match(current, &selected) => {
                        format!("已安装 {current}（与所选版本一致）")
                    }
                    Some(current) => format!("已安装 {current} → 所选 {selected}"),
                    None => format!("将安装 {selected}"),
                };
                SetWindowTextW(
                    state.component_version_controls[index],
                    wide(&text).as_ptr(),
                );

                refresh_version_checkbox(state, index, &selected);
                update_selection_state(state);
            } else {
                report_event(&choice_event, Some("no"));
            }
        }
        _ => {}
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLOSE => {
            let state = window_state(hwnd);
            if !state.is_null() {
                let state = &*state;
                let kind = state.plan[state.step];

                if kind == KIND_PROGRESS {
                    // 自升级：关窗就是放弃升级并退出
                    if state.op == OP_SELF_UPDATE {
                        DestroyWindow(hwnd);
                        return 0;
                    }
                    // 部署/卸载/打包阶段：窗口关闭按钮置灰，这里再兜一层
                    if state.phase == 1 && !state.failed {
                        return 0;
                    }
                    // 卸载/诊断导出进行中不可关闭
                    if !state.failed && state.op != OP_INSTALL {
                        return 0;
                    }
                    // 下载阶段关窗等同于“取消”，走同一套确认流程
                    if !state.failed {
                        SendMessageW(hwnd, WM_COMMAND, ID_CANCEL, 0);
                        return 0;
                    }
                }
            }
            DestroyWindow(hwnd);
            0
        }
        WM_COMMAND => {
            let state = window_state(hwnd);
            if !state.is_null() {
                let id = wparam & 0xFFFF;
                let code = (wparam >> 16) & 0xFFFF;
                if code == 0 {
                    on_command(hwnd, &mut *state, id);
                }
            }
            0
        }
        WM_CREATE => 0,
        WM_CTLCOLORSTATIC => {
            let control = lparam as HWND;
            let state = window_state(hwnd);
            SetBkMode(wparam as *mut c_void, TRANSPARENT as i32);

            if !state.is_null() {
                let state = &*state;
                let color = if is_error_text(state, control) {
                    Some(ERROR_COLOR)
                } else if is_blocked_install(state, control) || state.secondary.contains(&control) {
                    Some(GetSysColor(COLOR_GRAYTEXT))
                } else {
                    None
                };

                if let Some(color) = color {
                    SetTextColor(wparam as *mut c_void, color);
                }
            }

            GetSysColorBrush(COLOR_BTNFACE) as LRESULT
        }
        WM_DESTROY => {
            let state = window_state(hwnd);
            if !state.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let state = Box::from_raw(state);
                if !state.font.is_null() {
                    DeleteObject(state.font);
                }
                if !state.title_font.is_null() {
                    DeleteObject(state.title_font);
                }
                if !state.dialog_font.is_null() {
                    DeleteObject(state.dialog_font);
                }
            }
            PostQuitMessage(0);
            0
        }
        WM_NOTIFY => {
            let header = &*(lparam as *const NMHDR);
            if header.idFrom == ID_SITE_LINK
                && (header.code == NM_CLICK || header.code == NM_RETURN)
            {
                open_url(SITE_URL);
                return 0;
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_UI_EVENT => {
            if MODAL_DEPTH.load(Ordering::Relaxed) == 0 {
                drain_events(hwnd);
            }
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}
