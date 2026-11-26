//! Win32 platform integration: native Win11 frameless chrome (Snap Layouts,
//! Aero Snap, Mica backdrop), system tray, standard global hotkey (Ctrl+Alt+E),
//! DPI geometry, theme detection, IME composition tracking, shell message hooks.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI8, AtomicI32, AtomicIsize, AtomicU8, AtomicU64, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWM_WINDOW_CORNER_PREFERENCE, DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMWA_BORDER_COLOR,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, ScreenToClient,
};
use windows::Win32::System::DataExchange::AddClipboardFormatListener;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    SetCurrentProcessExplicitAppUserModelID, SetWindowSubclass, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, BringWindowToTop, CreatePopupMenu, DestroyMenu, GetClientRect, GetCursorPos, GetForegroundWindow,
    GetWindowPlacement, GetWindowThreadProcessId, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTLEFT,
    HTMAXBUTTON, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, IsIconic, IsWindowVisible, IsZoomed, LoadIconW, MF_SEPARATOR,
    MF_STRING, PostMessageW, SW_HIDE, SW_MAXIMIZE, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WINDOWPLACEMENT, WM_APP,
    WM_CLIPBOARDUPDATE, WM_COMMAND, WM_DEVICECHANGE, WM_DRAWITEM, WM_HOTKEY, WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION,
    WM_IME_STARTCOMPOSITION, WM_INITMENUPOPUP, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_MEASUREITEM, WM_MENUSELECT,
    WM_MOUSEMOVE, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE,
    WM_NULL, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_XBUTTONUP,
};
use windows::core::{HSTRING, PCWSTR};

const WM_TRAY: u32 = WM_APP + 1;
const GLOBAL_HOTKEY_ID: i32 = 0x4C45; // 'LE'
const TRAY_UID: u32 = 1;
const TRAY_CMD_OPEN: u32 = 1;
const TRAY_CMD_NEW_WINDOW: u32 = 2;
const TRAY_CMD_QUIT: u32 = 3;

static HWND_RAW: AtomicIsize = AtomicIsize::new(0);
static IME_CONTEXT: AtomicIsize = AtomicIsize::new(0);
static IME_COMPOSING: AtomicBool = AtomicBool::new(false);
static IME_END_TICK: AtomicU64 = AtomicU64::new(0);
static THEME_DIRTY: AtomicBool = AtomicBool::new(false);
static CLIPBOARD_DIRTY: AtomicBool = AtomicBool::new(false);
static DEVICES_DIRTY: AtomicBool = AtomicBool::new(false);
static MOUSE_NAV: AtomicI8 = AtomicI8::new(0);
/// Left edge (logical px) of empty draggable titlebar space (updated each frame by chrome).
static TITLEBAR_DRAG_LEFT: AtomicI32 = AtomicI32::new(320);
static MAX_BTN_HOVERED: AtomicBool = AtomicBool::new(false);
static TRAY_INSTALLED: AtomicBool = AtomicBool::new(false);
/// 0 = none, 1 = open, 2 = new window, 3 = force quit.
static TRAY_ACTION: AtomicU8 = AtomicU8::new(0);
static HOTKEY_TRIGGERED: AtomicBool = AtomicBool::new(false);
static HOTKEY_INSTALLED: AtomicBool = AtomicBool::new(false);
static EGUI_CTX: OnceLock<egui::Context> = OnceLock::new();

/// RGBA accent used by the shell when the registry has no accent color.
const DEFAULT_ACCENT_ABGR: u32 = 0x00d47800; // #0078D4

pub struct SystemTheme {
    pub dark: bool,
    /// ABGR as stored by the shell.
    pub accent_abgr: u32,
}

pub fn hwnd() -> HWND {
    HWND(HWND_RAW.load(Ordering::Relaxed) as *mut c_void)
}

pub fn init_process() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let app_id = HSTRING::from("Lumen.Explorer");
        let _ = SetCurrentProcessExplicitAppUserModelID(PCWSTR(app_id.as_ptr()));
        OleInitialize(None).expect("OLE init failed");
    }
}

/// Called once the winit window exists: installs the Fluent frame and hooks.
pub fn attach(raw: isize) {
    HWND_RAW.store(raw, Ordering::Relaxed);
    let hwnd = hwnd();
    let theme = system_theme();
    unsafe {
        apply_frame(hwnd, theme.dark, false);
        let _ = SetWindowSubclass(hwnd, Some(subclass_proc), 0x4C55_4D45, 0);
        let _ = AddClipboardFormatListener(hwnd);
    }
}

/// The UI thread's egui context, so window messages can wake the renderer.
pub fn set_repaint_ctx(ctx: &egui::Context) {
    let _ = EGUI_CTX.set(ctx.clone());
}

/// Wakes the renderer. Background threads call this whenever they make state
/// that the next frame must show, so an idle window can stay asleep.
pub fn wake() {
    if let Some(ctx) = EGUI_CTX.get() {
        ctx.request_repaint();
    }
}

/// Sets the right edge of the tab strip (`+` button) so `WM_NCHITTEST` marks
/// the remaining empty titlebar as `HTCAPTION` for native Aero Snap.
pub fn set_titlebar_drag_left(logical_x: f32) {
    TITLEBAR_DRAG_LEFT.store(logical_x.round() as i32, Ordering::Relaxed);
}

/// True when the pointer is hovering over the native `HTMAXBUTTON` region
/// (Windows 11 Snap Layouts trigger).
pub fn is_max_button_hovered() -> bool {
    MAX_BTN_HOVERED.load(Ordering::Relaxed)
}

/// Cached undocumented uxtheme dark-mode exports so repeated theme switches do
/// not leak module reference counts via `LoadLibraryW`.
struct UxThemeFns {
    set_mode: Option<unsafe extern "system" fn(i32) -> i32>,
    flush: Option<unsafe extern "system" fn()>,
}

unsafe impl Send for UxThemeFns {}
unsafe impl Sync for UxThemeFns {}

static UXTHEME: OnceLock<UxThemeFns> = OnceLock::new();

fn uxtheme_fns() -> &'static UxThemeFns {
    UXTHEME.get_or_init(|| {
        use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
        use windows::core::PCSTR;
        unsafe {
            let Ok(module) = LoadLibraryW(windows::core::w!("uxtheme.dll")) else {
                return UxThemeFns { set_mode: None, flush: None };
            };
            let set_mode = GetProcAddress(module, PCSTR(135 as *const u8)).map(|f| std::mem::transmute(f));
            let flush = GetProcAddress(module, PCSTR(136 as *const u8)).map(|f| std::mem::transmute(f));
            UxThemeFns { set_mode, flush }
        }
    })
}

/// Switches the process-wide uxtheme dark mode so shell menus and dialogs match.
pub fn set_native_theme(dark: bool) {
    let fns = uxtheme_fns();
    unsafe {
        if let Some(set_mode) = fns.set_mode {
            let mode: i32 = if dark { 2 } else { 3 };
            set_mode(mode);
        }
        if let Some(flush) = fns.flush {
            flush();
        }
    }
}

pub fn apply_frame(hwnd: HWND, dark: bool, mica: bool) {
    set_native_theme(dark);
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let dark_flag: i32 = dark as i32;
        let _ =
            DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark_flag as *const i32 as *const c_void, 4);
        let backdrop = if mica { DWMSBT_MAINWINDOW } else { DWMSBT_NONE };
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const DWM_SYSTEMBACKDROP_TYPE as *const c_void,
            4,
        );
        let margins = if mica {
            MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 }
        } else {
            MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 0, cyBottomHeight: 0 }
        };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const DWM_WINDOW_CORNER_PREFERENCE as *const c_void,
            4,
        );
        let no_border: u32 = 0xFFFF_FFFE; // DWMWA_COLOR_NONE
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &no_border as *const u32 as *const c_void, 4);
    }
}

pub fn system_theme() -> SystemTheme {
    let dark = reg_dword(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", "AppsUseLightTheme")
        .map(|v| v == 0)
        .unwrap_or(false);
    let accent_abgr = reg_dword(r"Software\Microsoft\Windows\DWM", "AccentColor").unwrap_or(DEFAULT_ACCENT_ABGR);
    SystemTheme { dark, accent_abgr }
}

pub fn take_theme_change() -> bool {
    THEME_DIRTY.swap(false, Ordering::Relaxed)
}

pub fn take_clipboard_change() -> bool {
    CLIPBOARD_DIRTY.swap(false, Ordering::Relaxed)
}

pub fn take_devices_change() -> bool {
    DEVICES_DIRTY.swap(false, Ordering::Relaxed)
}

/// Returns -1 for Back (XButton1), +1 for Forward (XButton2), 0 otherwise.
pub fn take_mouse_nav() -> i8 {
    MOUSE_NAV.swap(0, Ordering::Relaxed)
}

/// 0 = none, 1 = open/restore, 2 = new window, 3 = quit cleanly.
pub fn take_tray_action() -> u8 {
    TRAY_ACTION.swap(0, Ordering::Relaxed)
}

pub fn take_global_hotkey() -> bool {
    HOTKEY_TRIGGERED.swap(false, Ordering::Relaxed)
}

/// Shows or hides the window from the system tray and brings it to foreground.
pub fn restore_and_focus() {
    let hwnd = hwnd();
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let fg = GetForegroundWindow();
        let fg_tid = GetWindowThreadProcessId(fg, None);
        let cur_tid = GetCurrentThreadId();
        let attached = fg_tid != 0 && fg_tid != cur_tid;
        if attached {
            let _ = AttachThreadInput(fg_tid, cur_tid, true);
        }
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if attached {
            let _ = AttachThreadInput(fg_tid, cur_tid, false);
        }
    }
}

pub fn hide_to_tray() {
    let hwnd = hwnd();
    if !hwnd.0.is_null() {
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    }
}

/// Installs or removes the Windows notification-area tray icon.
pub fn set_tray_enabled(enabled: bool) {
    let hwnd = hwnd();
    if hwnd.0.is_null() {
        return;
    }
    let currently = TRAY_INSTALLED.load(Ordering::Relaxed);
    if enabled == currently {
        return;
    }
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    };
    if enabled {
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        if let Ok(module) = unsafe { GetModuleHandleW(None) }
            && let Ok(icon) = unsafe { LoadIconW(Some(module.into()), PCWSTR(std::ptr::without_provenance::<u16>(1))) }
        {
            data.hIcon = icon;
        }
        let tip = "Lumen 文件资源管理器";
        for (dst, src) in data.szTip.iter_mut().zip(tip.encode_utf16()) {
            *dst = src;
        }
        if unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
            TRAY_INSTALLED.store(true, Ordering::Relaxed);
        }
    } else {
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        TRAY_INSTALLED.store(false, Ordering::Relaxed);
    }
}

/// Registers or unregisters the standard Win32 global hotkey (`Ctrl+Alt+E`)
/// using `RegisterHotKey` (no low-level keyboard hook, zero AV false-positives).
pub fn set_global_hotkey(enabled: bool) -> bool {
    let hwnd = hwnd();
    if hwnd.0.is_null() {
        return false;
    }
    let currently = HOTKEY_INSTALLED.load(Ordering::Relaxed);
    if enabled == currently {
        return true;
    }
    if enabled {
        let ok =
            unsafe { RegisterHotKey(Some(hwnd), GLOBAL_HOTKEY_ID, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 0x45) }.is_ok();
        if ok {
            HOTKEY_INSTALLED.store(true, Ordering::Relaxed);
        }
        ok
    } else {
        let _ = unsafe { UnregisterHotKey(Some(hwnd), GLOBAL_HOTKEY_ID) };
        HOTKEY_INSTALLED.store(false, Ordering::Relaxed);
        true
    }
}

pub fn cleanup_on_exit() {
    let _ = set_global_hotkey(false);
    set_tray_enabled(false);
}

fn show_tray_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let open_label = HSTRING::from("打开 Lumen");
        let new_label = HSTRING::from("新建窗口");
        let quit_label = HSTRING::from("退出 Lumen");
        let _ = AppendMenuW(menu, MF_STRING, TRAY_CMD_OPEN as usize, PCWSTR(open_label.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, TRAY_CMD_NEW_WINDOW as usize, PCWSTR(new_label.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, TRAY_CMD_QUIT as usize, PCWSTR(quit_label.as_ptr()));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenuEx(
            menu,
            TPM_RETURNCMD.0 | TPM_RIGHTBUTTON.0 | TPM_LEFTALIGN.0 | TPM_BOTTOMALIGN.0,
            pt.x,
            pt.y,
            hwnd,
            None,
        )
        .0 as u32;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        if cmd != 0 {
            TRAY_ACTION.store(cmd as u8, Ordering::Relaxed);
            if cmd == TRAY_CMD_OPEN {
                restore_and_focus();
            }
            wake();
        }
    }
}

/// True while an IME composition string is active or just committed within the
/// current keystroke window, so Enter confirms the IME candidate instead of
/// submitting the address bar or inline rename editor.
pub fn is_ime_composing() -> bool {
    if IME_COMPOSING.load(Ordering::Relaxed) {
        return true;
    }
    let ended = IME_END_TICK.load(Ordering::Relaxed);
    if ended == 0 {
        return false;
    }
    let now = unsafe { GetTickCount64() };
    now.saturating_sub(ended) < 80
}

fn reg_dword(path: &str, name: &str) -> Option<u32> {
    let path = HSTRING::from(path);
    let name = HSTRING::from(name);
    let mut data: u32 = 0;
    let mut size: u32 = 4;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    (status.0 == 0).then_some(data)
}

/// Detaches the IME context so keyboard shortcuts behave like a native window
/// when no text field is focused; re-attaches it while editing.
pub fn set_ime_enabled(enabled: bool) {
    use windows::Win32::UI::Input::Ime::{HIMC, ImmAssociateContext};
    let hwnd = hwnd();
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        if enabled {
            let saved = IME_CONTEXT.load(Ordering::Relaxed);
            if saved != 0 {
                ImmAssociateContext(hwnd, HIMC(saved as *mut c_void));
            }
        } else {
            IME_COMPOSING.store(false, Ordering::Relaxed);
            let previous = ImmAssociateContext(hwnd, HIMC(std::ptr::null_mut()));
            let raw = previous.0 as isize;
            if raw != 0 {
                IME_CONTEXT.store(raw, Ordering::Relaxed);
            }
        }
    }
}

/// Returns the normal (non-maximized) window geometry in **logical points**
/// using `GetWindowPlacement` and the window's DPI scale, so saving and
/// restoring never drifts across high-DPI displays.
pub fn window_logical_rect() -> Option<(f32, f32, f32, f32)> {
    let hwnd = hwnd();
    if hwnd.0.is_null() {
        return None;
    }
    let mut placement = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    unsafe { GetWindowPlacement(hwnd, &mut placement) }.ok()?;
    let rect = placement.rcNormalPosition;
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32;
    let scale = dpi / 96.0;
    let w = ((rect.right - rect.left) as f32 / scale).max(820.0);
    let h = ((rect.bottom - rect.top) as f32 / scale).max(520.0);
    Some((rect.left as f32 / scale, rect.top as f32 / scale, w, h))
}

/// Clamps a saved logical window rect to the nearest connected monitor's
/// logical work area so the window never spawns off-screen or oversized.
pub fn clamp_logical_window(x: Option<f32>, y: Option<f32>, w: f32, h: f32) -> (Option<f32>, Option<f32>, f32, f32) {
    let dpi = unsafe { GetDpiForSystem() }.max(96) as f32;
    let scale = dpi / 96.0;
    let probe = POINT { x: (x.unwrap_or(100.0) * scale) as i32, y: (y.unwrap_or(100.0) * scale) as i32 };
    let monitor = unsafe { MonitorFromPoint(probe, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let work: RECT = if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        info.rcWork
    } else {
        RECT { left: 0, top: 0, right: (1920.0 * scale) as i32, bottom: (1080.0 * scale) as i32 }
    };
    let work_left = work.left as f32 / scale;
    let work_top = work.top as f32 / scale;
    let work_w = ((work.right - work.left) as f32 / scale).max(820.0);
    let work_h = ((work.bottom - work.top) as f32 / scale).max(520.0);
    let clamped_w = w.clamp(820.0, work_w);
    let clamped_h = h.clamp(520.0, work_h);
    let (cx, cy) = match (x, y) {
        (Some(px), Some(py)) => {
            let max_x = (work_left + work_w - clamped_w).max(work_left);
            let max_y = (work_top + work_h - clamped_h).max(work_top);
            (Some(px.clamp(work_left, max_x)), Some(py.clamp(work_top, max_y)))
        }
        _ => (None, None),
    };
    (cx, cy, clamped_w, clamped_h)
}

pub fn is_maximized() -> bool {
    !hwnd().0.is_null() && unsafe { IsZoomed(hwnd()).as_bool() }
}

pub fn toggle_maximize() {
    unsafe {
        if IsZoomed(hwnd()).as_bool() {
            let _ = ShowWindow(hwnd(), SW_RESTORE);
        } else {
            let _ = ShowWindow(hwnd(), SW_MAXIMIZE);
        }
    }
}

fn hit_test_chrome(hwnd: HWND, lparam: LPARAM) -> Option<LRESULT> {
    let sx = (lparam.0 & 0xFFFF) as i16 as i32;
    let sy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
    let mut pt = POINT { x: sx, y: sy };
    let mut client = RECT::default();
    unsafe {
        let _ = ScreenToClient(hwnd, &mut pt);
        if GetClientRect(hwnd, &mut client).is_err() {
            return None;
        }
    }
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32;
    let scale = dpi / 96.0;
    let lx = pt.x as f32 / scale;
    let ly = pt.y as f32 / scale;
    let lw = (client.right - client.left) as f32 / scale;
    let lh = (client.bottom - client.top) as f32 / scale;

    if !unsafe { IsZoomed(hwnd).as_bool() } {
        let border = 6.0;
        let left = lx < border;
        let right = lx > lw - border;
        let top = ly < border;
        let bottom = ly > lh - border;
        match (left, right, top, bottom) {
            (true, _, true, _) => return Some(LRESULT(HTTOPLEFT as isize)),
            (_, true, true, _) => return Some(LRESULT(HTTOPRIGHT as isize)),
            (true, _, _, true) => return Some(LRESULT(HTBOTTOMLEFT as isize)),
            (_, true, _, true) => return Some(LRESULT(HTBOTTOMRIGHT as isize)),
            (true, _, _, _) => return Some(LRESULT(HTLEFT as isize)),
            (_, true, _, _) => return Some(LRESULT(HTRIGHT as isize)),
            (_, _, true, _) => return Some(LRESULT(HTTOP as isize)),
            (_, _, _, true) => return Some(LRESULT(HTBOTTOM as isize)),
            _ => {}
        }
    }

    if (0.0..crate::theme::TITLEBAR_H).contains(&ly) {
        // Native HTMAXBUTTON enables Windows 11 Snap Layouts flyout on hover!
        if lx >= lw - 92.0 && lx < lw - 46.0 {
            return Some(LRESULT(HTMAXBUTTON as isize));
        }
        // Empty drag area between the "+" tab button and the 3 caption buttons.
        let drag_left = TITLEBAR_DRAG_LEFT.load(Ordering::Relaxed) as f32;
        if lx >= drag_left && lx < lw - 138.0 {
            return Some(LRESULT(HTCAPTION as isize));
        }
    }
    None
}

/// Subclass proc: handles native Win11 hit-testing (Snap Layouts / Aero Snap),
/// tray callbacks, Win+E wakeups, shell menus, IME composition, and devices.
unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_NCHITTEST => {
            if let Some(hit) = hit_test_chrome(hwnd, lparam) {
                return hit;
            }
        }
        WM_NCMOUSEMOVE => {
            let over_max = wparam.0 == HTMAXBUTTON as usize;
            if MAX_BTN_HOVERED.swap(over_max, Ordering::Relaxed) != over_max {
                wake();
            }
        }
        WM_NCMOUSELEAVE | WM_MOUSEMOVE => {
            if MAX_BTN_HOVERED.swap(false, Ordering::Relaxed) {
                wake();
            }
        }
        WM_NCLBUTTONDOWN => {
            if wparam.0 == HTMAXBUTTON as usize {
                return LRESULT(0);
            }
        }
        WM_NCLBUTTONUP => {
            if wparam.0 == HTMAXBUTTON as usize {
                toggle_maximize();
                return LRESULT(0);
            }
        }
        WM_NCLBUTTONDBLCLK => {
            if wparam.0 == HTCAPTION as usize {
                toggle_maximize();
                return LRESULT(0);
            }
        }
        WM_TRAY => {
            let event = (lparam.0 & 0xFFFF) as u32;
            if event == WM_LBUTTONUP || event == WM_LBUTTONDBLCLK {
                restore_and_focus();
                wake();
                return LRESULT(0);
            }
            if event == WM_RBUTTONUP {
                show_tray_menu(hwnd);
                return LRESULT(0);
            }
        }
        WM_HOTKEY => {
            if wparam.0 as i32 == GLOBAL_HOTKEY_ID {
                HOTKEY_TRIGGERED.store(true, Ordering::Relaxed);
                restore_and_focus();
                wake();
                return LRESULT(0);
            }
        }
        WM_SETTINGCHANGE => {
            THEME_DIRTY.store(true, Ordering::Relaxed);
            wake();
        }
        WM_CLIPBOARDUPDATE => {
            CLIPBOARD_DIRTY.store(true, Ordering::Relaxed);
            wake();
        }
        WM_DEVICECHANGE => {
            DEVICES_DIRTY.store(true, Ordering::Relaxed);
            wake();
        }
        WM_IME_STARTCOMPOSITION | WM_IME_COMPOSITION => {
            IME_COMPOSING.store(true, Ordering::Relaxed);
        }
        WM_IME_ENDCOMPOSITION => {
            IME_COMPOSING.store(false, Ordering::Relaxed);
            IME_END_TICK.store(unsafe { GetTickCount64() }, Ordering::Relaxed);
        }
        WM_XBUTTONUP => {
            let button = ((wparam.0 >> 16) & 0xFFFF) as u16;
            if button == 1 {
                MOUSE_NAV.store(-1, Ordering::Relaxed);
                wake();
                return LRESULT(1);
            } else if button == 2 {
                MOUSE_NAV.store(1, Ordering::Relaxed);
                wake();
                return LRESULT(1);
            }
        }
        WM_COMMAND | WM_INITMENUPOPUP | WM_MEASUREITEM | WM_DRAWITEM | WM_MENUSELECT => {
            if let Some(result) = crate::shell::menu::handle_menu_message(msg, wparam, lparam) {
                return result;
            }
        }
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}
