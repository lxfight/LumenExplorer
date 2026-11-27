//! Win32 platform integration: native Win11 frameless chrome (Snap Layouts,
//! Aero Snap, Mica backdrop), system tray, standard global hotkey (Ctrl+Alt+E),
//! DPI geometry, theme detection, IME composition tracking, shell message hooks.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI8, AtomicI32, AtomicIsize, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWM_WINDOW_CORNER_PREFERENCE, DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMWA_BORDER_COLOR,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, MonitorFromRect,
    MonitorFromWindow, ScreenToClient,
};
use windows::Win32::System::DataExchange::AddClipboardFormatListener;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{AttachThreadInput, CreateMutexW, OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, GetDpiForSystem, GetDpiForWindow, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
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
    MF_STRING, PostMessageW, SW_HIDE, SW_MAXIMIZE, SW_RESTORE, SW_SHOW, SendMessageW, SetForegroundWindow, ShowWindow,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WINDOWPLACEMENT, WM_APP,
    WM_CLIPBOARDUPDATE, WM_CLOSE, WM_COMMAND, WM_DEVICECHANGE, WM_DRAWITEM, WM_ENDSESSION, WM_HOTKEY,
    WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION, WM_IME_STARTCOMPOSITION, WM_INITMENUPOPUP, WM_LBUTTONDBLCLK,
    WM_LBUTTONUP, WM_MEASUREITEM, WM_MENUSELECT, WM_MOUSEMOVE, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN,
    WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_NULL, WM_QUERYENDSESSION, WM_RBUTTONUP, WM_SETTINGCHANGE,
    WM_XBUTTONUP,
};
use windows::core::{HSTRING, PCWSTR};

const WM_TRAY: u32 = WM_APP + 1;
const GLOBAL_HOTKEY_ID: i32 = 0x4C45; // 'LE'
const TRAY_UID: u32 = 1;
const TRAY_CMD_OPEN: u32 = 1;
const TRAY_CMD_NEW_WINDOW: u32 = 2;
const TRAY_CMD_QUIT: u32 = 3;

/// Named Win32 Mutex shared with the Inno Setup installer (`AppMutex`) so
/// upgrades and uninstalls reliably detect running instances (including in tray).
const INSTANCE_MUTEX_NAME: &str = "LumenExplorer.SingleInstance.v1";

static HWND_RAW: AtomicIsize = AtomicIsize::new(0);
static INSTANCE_MUTEX: AtomicIsize = AtomicIsize::new(0);
static FORCE_QUIT: AtomicBool = AtomicBool::new(false);
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
static MODAL_ACTIVE: AtomicBool = AtomicBool::new(false);
static TRAY_INSTALLED: AtomicBool = AtomicBool::new(false);
/// 0 = none, 1 = open, 2 = new window, 3 = force quit.
static TRAY_ACTION: AtomicU8 = AtomicU8::new(0);
static HOTKEY_TRIGGERED: AtomicBool = AtomicBool::new(false);
static HOTKEY_INSTALLED: AtomicBool = AtomicBool::new(false);
static EGUI_CTX: OnceLock<egui::Context> = OnceLock::new();
type ShutdownHook = Arc<dyn Fn() + Send + Sync + 'static>;
static SHUTDOWN_HOOK: Mutex<Option<ShutdownHook>> = Mutex::new(None);

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
        let mutex_name = HSTRING::from(INSTANCE_MUTEX_NAME);
        if let Ok(handle) = CreateMutexW(None, false, PCWSTR(mutex_name.as_ptr()))
            && !handle.is_invalid()
        {
            INSTANCE_MUTEX.store(handle.0 as isize, Ordering::Relaxed);
        }
        OleInitialize(None).expect("OLE init failed");
    }
}

/// Requests an unconditional clean shutdown (bypasses tray-resident hide-on-close),
/// safe to invoke even while the window is hidden in the notification area.
pub fn request_force_quit() {
    FORCE_QUIT.store(true, Ordering::Relaxed);
    let hwnd = hwnd();
    if !hwnd.0.is_null() {
        let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
    wake();
}

pub fn force_quit_requested() -> bool {
    FORCE_QUIT.load(Ordering::Relaxed)
}

/// Returns true when a running primary Lumen instance holds the single-instance mutex.
pub fn is_primary_instance_running() -> bool {
    let mutex_name = HSTRING::from(INSTANCE_MUTEX_NAME);
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(mutex_name.as_ptr())) } {
        Ok(handle) if !handle.is_invalid() => {
            let _ = unsafe { CloseHandle(handle) };
            true
        }
        _ => false,
    }
}

/// Registers a synchronous hook invoked on `WM_ENDSESSION` so the app can flush
/// settings and clean up resources before Windows terminates the process.
#[allow(dead_code)] // Wired in Wave 2 app layer
pub fn register_shutdown_hook(hook: impl Fn() + Send + Sync + 'static) {
    if let Ok(mut slot) = SHUTDOWN_HOOK.lock() {
        *slot = Some(Arc::new(hook));
    }
}

fn run_shutdown_hook() {
    let hook = SHUTDOWN_HOOK.lock().ok().and_then(|slot| slot.clone());
    if let Some(hook) = hook {
        hook();
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

/// Suppresses native `HTCAPTION` / `HTMAXBUTTON` hit-testing while a modal or
/// popup overlay is open so clicks/hovers cannot drag or trigger Snap Layouts
/// through the overlay.
#[allow(dead_code)] // Wired in Wave 2 app layer
pub fn set_modal_active(active: bool) {
    MODAL_ACTIVE.store(active, Ordering::Relaxed);
    if active {
        MAX_BTN_HOVERED.store(false, Ordering::Relaxed);
    }
}

pub fn is_modal_active() -> bool {
    MODAL_ACTIVE.load(Ordering::Relaxed)
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
        let fg_tid = if fg.0.is_null() { 0 } else { GetWindowThreadProcessId(fg, None) };
        let target_tid = GetWindowThreadProcessId(hwnd, None);
        let attached = fg_tid != 0 && target_tid != 0 && fg_tid != target_tid;
        if attached {
            let _ = AttachThreadInput(fg_tid, target_tid, true);
        }
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if attached {
            let _ = AttachThreadInput(fg_tid, target_tid, false);
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
        let tip = "Lumen 文件工作台";
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
    let raw = INSTANCE_MUTEX.swap(0, Ordering::Relaxed);
    if raw != 0 {
        let _ = unsafe { CloseHandle(HANDLE(raw as *mut c_void)) };
    }
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
            } else if cmd == TRAY_CMD_QUIT {
                request_force_quit();
                return;
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

fn monitor_dpi(monitor: HMONITOR) -> f32 {
    let mut dpi_x = 0u32;
    let mut dpi_y = 0u32;
    if !monitor.is_invalid()
        && unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_ok()
        && dpi_x >= 96
    {
        return dpi_x as f32;
    }
    unsafe { GetDpiForSystem() }.max(96) as f32
}

/// Converts a `WINDOWPLACEMENT::rcNormalPosition` (workspace coordinates)
/// into physical screen coordinates using the owning monitor's work-area offset.
fn workarea_rect_to_screen(normal: RECT, monitor: RECT, work: RECT) -> RECT {
    let dx = work.left - monitor.left;
    let dy = work.top - monitor.top;
    RECT { left: normal.left + dx, top: normal.top + dy, right: normal.right + dx, bottom: normal.bottom + dy }
}

/// Converts a physical screen rect into saved logical coordinates:
/// `(x, y)` in primary/system logical coordinates (matching `eframe`/`winit`'s
/// `with_position` startup scaling) and `(w, h)` in target monitor logical points.
fn screen_rect_to_logical(screen: RECT, sys_scale: f32, target_scale: f32) -> (f32, f32, f32, f32) {
    let sys_scale = sys_scale.max(1.0);
    let target_scale = target_scale.max(1.0);
    let x = screen.left as f32 / sys_scale;
    let y = screen.top as f32 / sys_scale;
    let w = ((screen.right - screen.left) as f32 / target_scale).max(820.0);
    let h = ((screen.bottom - screen.top) as f32 / target_scale).max(520.0);
    (x, y, w, h)
}

/// Clamps saved logical window geometry to a target monitor's physical work area
/// using the target monitor's effective DPI for size scaling and system DPI for
/// virtual-desktop position mapping.
fn clamp_window_to_workarea(
    x: Option<f32>,
    y: Option<f32>,
    w: f32,
    h: f32,
    work: RECT,
    sys_scale: f32,
    target_scale: f32,
) -> (Option<f32>, Option<f32>, f32, f32) {
    let sys_scale = sys_scale.max(1.0);
    let target_scale = target_scale.max(1.0);
    let w = if w.is_finite() { w } else { 1440.0 };
    let h = if h.is_finite() { h } else { 900.0 };
    let work_w_logical = ((work.right - work.left) as f32 / target_scale).max(820.0);
    let work_h_logical = ((work.bottom - work.top) as f32 / target_scale).max(520.0);
    let clamped_w = w.clamp(820.0, work_w_logical);
    let clamped_h = h.clamp(520.0, work_h_logical);

    let (cx, cy) = match (x.filter(|v| v.is_finite()), y.filter(|v| v.is_finite())) {
        (Some(px), Some(py)) => {
            let phys_w = clamped_w * target_scale;
            let phys_h = clamped_h * target_scale;
            let work_left = work.left as f32;
            let work_top = work.top as f32;
            let max_phys_x = (work.right as f32 - phys_w).max(work_left);
            let max_phys_y = (work.bottom as f32 - phys_h).max(work_top);
            let clamped_phys_x = (px * sys_scale).clamp(work_left, max_phys_x);
            let clamped_phys_y = (py * sys_scale).clamp(work_top, max_phys_y);
            (Some(clamped_phys_x / sys_scale), Some(clamped_phys_y / sys_scale))
        }
        _ => (None, None),
    };
    (cx, cy, clamped_w, clamped_h)
}

/// Returns the normal (non-maximized) window geometry in **logical points**
/// using `GetWindowPlacement` converted from workspace to screen coordinates
/// and scaled with the owning monitor's DPI, so saving and restoring never
/// drifts across top/left taskbars or mixed-DPI multi-monitor setups.
pub fn window_logical_rect() -> Option<(f32, f32, f32, f32)> {
    let hwnd = hwnd();
    if hwnd.0.is_null() {
        return None;
    }
    let mut placement = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    unsafe { GetWindowPlacement(hwnd, &mut placement) }.ok()?;
    let normal = placement.rcNormalPosition;
    let monitor = unsafe {
        let by_rect = MonitorFromRect(&normal, MONITOR_DEFAULTTONEAREST);
        if by_rect.is_invalid() { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) } else { by_rect }
    };
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let screen = if !monitor.is_invalid() && unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        workarea_rect_to_screen(normal, info.rcMonitor, info.rcWork)
    } else {
        normal
    };
    let sys_scale = unsafe { GetDpiForSystem() }.max(96) as f32 / 96.0;
    let target_scale = if !monitor.is_invalid() {
        monitor_dpi(monitor) / 96.0
    } else {
        unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0
    };
    Some(screen_rect_to_logical(screen, sys_scale, target_scale))
}

/// Clamps a saved logical window rect to the nearest connected monitor's
/// work area using the target monitor's effective DPI so the window never
/// spawns off-screen, drifts under top/left taskbars, or snaps back to primary.
pub fn clamp_logical_window(x: Option<f32>, y: Option<f32>, w: f32, h: f32) -> (Option<f32>, Option<f32>, f32, f32) {
    let sys_scale = unsafe { GetDpiForSystem() }.max(96) as f32 / 96.0;
    let px = x.filter(|v| v.is_finite()).unwrap_or(100.0);
    let py = y.filter(|v| v.is_finite()).unwrap_or(100.0);
    let probe = POINT {
        x: (px * sys_scale).round().clamp(-1_000_000.0, 1_000_000.0) as i32,
        y: (py * sys_scale).round().clamp(-1_000_000.0, 1_000_000.0) as i32,
    };
    let monitor = unsafe { MonitorFromPoint(probe, MONITOR_DEFAULTTONEAREST) };
    let target_scale = monitor_dpi(monitor) / 96.0;
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let work: RECT = if !monitor.is_invalid() && unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        info.rcWork
    } else {
        RECT { left: 0, top: 0, right: (1920.0 * target_scale) as i32, bottom: (1080.0 * target_scale) as i32 }
    };
    clamp_window_to_workarea(x, y, w, h, work, sys_scale, target_scale)
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

    if !is_modal_active() && (0.0..crate::theme::TITLEBAR_H).contains(&ly) {
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
        WM_QUERYENDSESSION => return LRESULT(1),
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                FORCE_QUIT.store(true, Ordering::Relaxed);
                run_shutdown_hook();
                cleanup_on_exit();
                let _ = unsafe { SendMessageW(hwnd, WM_CLOSE, Some(WPARAM(0)), Some(LPARAM(0))) };
                wake();
            }
            return LRESULT(0);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn workarea_rect_to_screen_offsets_top_and_left_taskbars() {
        let mon = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        // Top taskbar (48px tall)
        let top_work = RECT { left: 0, top: 48, right: 1920, bottom: 1080 };
        let normal = RECT { left: 100, top: 0, right: 1540, bottom: 900 };
        let screen = workarea_rect_to_screen(normal, mon, top_work);
        assert_eq!((screen.left, screen.top, screen.right, screen.bottom), (100, 48, 1540, 948));

        // Left taskbar (64px wide)
        let left_work = RECT { left: 64, top: 0, right: 1920, bottom: 1080 };
        let normal_left = RECT { left: 0, top: 80, right: 1440, bottom: 980 };
        let screen_left = workarea_rect_to_screen(normal_left, mon, left_work);
        assert_eq!((screen_left.left, screen_left.top, screen_left.right, screen_left.bottom), (64, 80, 1504, 980));

        // Bottom taskbar (no origin offset)
        let bottom_work = RECT { left: 0, top: 0, right: 1920, bottom: 1032 };
        let screen_bottom = workarea_rect_to_screen(normal, mon, bottom_work);
        assert_eq!(
            (screen_bottom.left, screen_bottom.top, screen_bottom.right, screen_bottom.bottom),
            (100, 0, 1540, 900)
        );
    }

    #[test]
    fn roundtrip_window_geometry_with_top_and_left_taskbar_has_zero_drift() {
        // 150% HiDPI display with 60px top taskbar
        let mon = RECT { left: 0, top: 0, right: 2560, bottom: 1440 };
        let work = RECT { left: 0, top: 60, right: 2560, bottom: 1440 };
        let scale = 1.5f32;
        let normal = RECT { left: 150, top: 0, right: 150 + 2160, bottom: 1350 };
        let screen = workarea_rect_to_screen(normal, mon, work);
        let (x, y, w, h) = screen_rect_to_logical(screen, scale, scale);
        assert_eq!((x, y, w, h), (100.0, 40.0, 1440.0, 900.0));

        let (cx, cy, cw, ch) = clamp_window_to_workarea(Some(x), Some(y), w, h, work, scale, scale);
        assert_eq!((cx, cy, cw, ch), (Some(100.0), Some(40.0), 1440.0, 900.0));
    }

    #[test]
    fn mixed_dpi_multi_monitor_clamping_preserves_secondary_monitor() {
        // Case A: Primary 100% (sys_scale = 1.0), Secondary 200% (target_scale = 2.0) on the right
        let sec_mon = RECT { left: 1920, top: 0, right: 5760, bottom: 2160 };
        let sec_work = RECT { left: 1920, top: 0, right: 5760, bottom: 2080 };
        let normal = RECT { left: 2020, top: 100, right: 2020 + 2880, bottom: 100 + 1800 };
        let screen = workarea_rect_to_screen(normal, sec_mon, sec_work);
        let (x, y, w, h) = screen_rect_to_logical(screen, 1.0, 2.0);
        assert_eq!((x, y, w, h), (2020.0, 100.0, 1440.0, 900.0));

        let (cx, cy, cw, ch) = clamp_window_to_workarea(Some(x), Some(y), w, h, sec_work, 1.0, 2.0);
        assert_eq!((cx, cy, cw, ch), (Some(2020.0), Some(100.0), 1440.0, 900.0));

        // Oversized window on 200% secondary monitor clamps to target monitor's logical workarea (1920x1040), not 3840x2080!
        let (ox, oy, ow, oh) = clamp_window_to_workarea(Some(2500.0), Some(500.0), 3000.0, 2000.0, sec_work, 1.0, 2.0);
        assert_eq!((ox, oy, ow, oh), (Some(1920.0), Some(0.0), 1920.0, 1040.0));

        // Case B: Primary 200% (sys_scale = 2.0), Secondary 100% (target_scale = 1.0) on the left (negative coords)
        let left_mon = RECT { left: -1920, top: 0, right: 0, bottom: 1080 };
        let left_work = RECT { left: -1920, top: 0, right: 0, bottom: 1040 };
        let normal_neg = RECT { left: -1820, top: 100, right: -380, bottom: 1000 };
        let screen_neg = workarea_rect_to_screen(normal_neg, left_mon, left_work);
        let (nx, ny, nw, nh) = screen_rect_to_logical(screen_neg, 2.0, 1.0);
        assert_eq!((nx, ny, nw, nh), (-910.0, 50.0, 1440.0, 900.0));

        let (cnx, cny, cnw, cnh) = clamp_window_to_workarea(Some(nx), Some(ny), nw, nh, left_work, 2.0, 1.0);
        assert_eq!((cnx, cny, cnw, cnh), (Some(-910.0), Some(50.0), 1440.0, 900.0));
    }

    #[test]
    fn modal_active_flag_toggles_and_clears_max_hover() {
        let _guard = TEST_LOCK.lock().unwrap();
        MAX_BTN_HOVERED.store(true, Ordering::Relaxed);
        set_modal_active(true);
        assert!(is_modal_active());
        assert!(!is_max_button_hovered());
        set_modal_active(false);
        assert!(!is_modal_active());
    }

    #[test]
    fn shutdown_hook_invokes_registered_callback() {
        let _guard = TEST_LOCK.lock().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = Arc::clone(&hits);
        register_shutdown_hook(move || {
            hits_clone.fetch_add(1, Ordering::Relaxed);
        });
        run_shutdown_hook();
        assert_eq!(hits.load(Ordering::Relaxed), 1);
        *SHUTDOWN_HOOK.lock().unwrap() = None;
    }
}
