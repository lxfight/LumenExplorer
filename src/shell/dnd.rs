//! OLE drag & drop: drop target registration, live hover feedback, and drag source.
//!
//! Both COM objects are hand-written vtable objects: the windows-rs
//! `#[implement]` macro emits absolute `::windows_core::` paths that only
//! resolve when the crate depends on `windows-core` directly, and the pinned
//! manifest has no such dependency. Only `windows::core` re-exports are used.

use std::ffi::{OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::Context;
use egui::{Pos2, Rect};
use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, E_NOINTERFACE, GlobalFree, POINT, POINTL, S_OK,
};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::Storage::FileSystem::GetVolumePathNameW;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, DVASPECT_CONTENT, FORMATETC, IBindCtx, IDataObject,
    STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DROPEFFECT_NONE, DoDragDrop, IDropSource,
    IDropSource_Vtbl, IDropTarget, IDropTarget_Vtbl, RegisterDragDrop, ReleaseStgMedium,
};
use windows::Win32::System::SystemServices::{MK_CONTROL, MK_LBUTTON, MK_RBUTTON, MK_SHIFT, MODIFIERKEYS_FLAGS};
/// `MK_ALT` (0x0020) from WinUser/OLE drag-drop key state flags.
const MK_ALT: MODIFIERKEYS_FLAGS = MODIFIERKEYS_FLAGS(0x0020);
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    BHID_DataObject, CLSID_DragDropHelper, DragQueryFileW, HDROP, IDragSourceHelper, IShellItemArray,
    SHCreateShellItemArrayFromIDLists, SHParseDisplayName,
};
use windows::core::{BOOL, GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, PCWSTR, w};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropAction {
    Copy,
    Move,
    Link,
}

#[derive(Clone, Debug)]
pub struct DropEvent {
    pub paths: Vec<PathBuf>,
    pub position: Pos2,
    pub action: DropAction,
}

#[derive(Clone, Debug)]
pub struct DragHover {
    pub position: Pos2,
    pub target: Option<PathBuf>,
    pub action: Option<DropAction>,
}

struct DropContext {
    zones: Vec<(Rect, PathBuf)>,
    current_dir: Option<PathBuf>,
    pin_drop_rect: Option<Rect>,
}

static DROPS: Mutex<Vec<DropEvent>> = Mutex::new(Vec::new());
static DRAG_HOVER: Mutex<Option<DragHover>> = Mutex::new(None);
static DROP_CTX: Mutex<DropContext> =
    Mutex::new(DropContext { zones: Vec::new(), current_dir: None, pin_drop_rect: None });
static DROP_TARGET: OnceLock<TargetHandle> = OnceLock::new();

#[allow(dead_code)]
struct TargetHandle(IDropTarget);

unsafe impl Send for TargetHandle {}
unsafe impl Sync for TargetHandle {}

/// Updates the active window's drop zones and current directory so OLE
/// `DragOver` can apply Explorer's same-volume/cross-volume rules and highlight
/// the hovered drop target in real time.
pub fn update_drop_context(zones: &[(Rect, PathBuf)], current_dir: Option<PathBuf>) {
    if let Ok(mut ctx) = DROP_CTX.lock() {
        ctx.zones.clear();
        ctx.zones.extend_from_slice(zones);
        ctx.current_dir = current_dir;
    }
}

/// Updates the Quick Access header rect where dragged folders can be pinned,
/// independent of the current directory or `is_self_or_ancestor` checks.
pub fn set_pin_drop_rect(rect: Option<Rect>) {
    if let Ok(mut ctx) = DROP_CTX.lock() {
        ctx.pin_drop_rect = rect;
    }
}

/// Live drag-over target info for UI visual feedback (cleared on leave/drop).
pub fn drag_hover() -> Option<DragHover> {
    DRAG_HOVER.lock().ok().and_then(|g| g.clone())
}

enum ResolvedDrop {
    PinQuickAccess,
    Dir(PathBuf),
    None,
}

impl ResolvedDrop {
    fn dir_path(&self) -> Option<PathBuf> {
        match self {
            Self::Dir(path) => Some(path.clone()),
            Self::PinQuickAccess | Self::None => None,
        }
    }
}

fn resolve_drop_target(pos: Pos2) -> ResolvedDrop {
    let Ok(ctx) = DROP_CTX.lock() else {
        return ResolvedDrop::None;
    };
    if ctx.pin_drop_rect.is_some_and(|r| r.contains(pos)) {
        return ResolvedDrop::PinQuickAccess;
    }
    if let Some((_, path)) = ctx.zones.iter().rev().find(|(rect, _)| rect.contains(pos)) {
        return ResolvedDrop::Dir(path.clone());
    }
    if let Some(dir) = &ctx.current_dir {
        return ResolvedDrop::Dir(dir.clone());
    }
    ResolvedDrop::None
}

/// Returns the volume mount root (e.g. `C:\` or `\\server\share\`) for a path.
fn volume_root(path: &Path) -> Option<String> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut buf = [0u16; 260];
    if unsafe { GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut buf) }.is_ok() {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        return Some(String::from_utf16_lossy(&buf[..len]).to_ascii_lowercase());
    }
    // Fallback for non-existent paths or UNC prefixes: compare prefix component.
    use std::path::Component;
    match path.components().next() {
        Some(Component::Prefix(prefix)) => Some(prefix.as_os_str().to_string_lossy().to_ascii_lowercase()),
        _ => None,
    }
}

fn same_volume(src: &Path, dst: &Path) -> bool {
    match (volume_root(src), volume_root(dst)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn is_self_or_ancestor(src: &Path, dst: &Path) -> bool {
    let s = src.to_string_lossy().trim_end_matches('\\').to_ascii_lowercase();
    let d = dst.to_string_lossy().trim_end_matches('\\').to_ascii_lowercase();
    d == s || (d.len() > s.len() && d.starts_with(&s) && d.as_bytes().get(s.len()) == Some(&b'\\'))
}

/// Explorer drag-drop effect rules:
/// - Over the Quick Access pin target: allow pinning directories with `Link` (or allowed effect).
/// - Over a non-filesystem view with no drop zone (`This PC` empty area, Network): disallow (`NONE`).
/// - `Ctrl` forces Copy; `Shift` forces Move; `Alt` forces Link (shortcut).
/// - Otherwise same volume defaults to Move, cross volume defaults to Copy.
/// - Intersected with `allowed` (`dwOkEffect` from the drag source).
fn choose_effect(
    modifiers: MODIFIERKEYS_FLAGS,
    allowed: DROPEFFECT,
    preferred: DROPEFFECT,
    paths: &[PathBuf],
    target: &ResolvedDrop,
) -> DROPEFFECT {
    if paths.is_empty() {
        return DROPEFFECT_NONE;
    }
    let has = |e: DROPEFFECT| allowed.0 == 0 || (allowed.0 & e.0) != 0;
    let dst = match target {
        ResolvedDrop::PinQuickAccess => {
            if !paths.iter().any(|p| p.is_dir()) {
                return DROPEFFECT_NONE;
            }
            return if has(DROPEFFECT_LINK) {
                DROPEFFECT_LINK
            } else if has(DROPEFFECT_COPY) {
                DROPEFFECT_COPY
            } else if has(DROPEFFECT_MOVE) {
                DROPEFFECT_MOVE
            } else {
                DROPEFFECT_NONE
            };
        }
        ResolvedDrop::None => return DROPEFFECT_NONE,
        ResolvedDrop::Dir(dir) => dir.as_path(),
    };
    // Reject dropping a folder onto itself or inside itself.
    if paths.iter().any(|p| p.is_dir() && is_self_or_ancestor(p, dst)) {
        return DROPEFFECT_NONE;
    }
    let chosen = if modifiers.contains(MK_ALT) && has(DROPEFFECT_LINK) {
        DROPEFFECT_LINK
    } else if modifiers.contains(MK_CONTROL) && has(DROPEFFECT_COPY) {
        DROPEFFECT_COPY
    } else if modifiers.contains(MK_SHIFT) && has(DROPEFFECT_MOVE) {
        DROPEFFECT_MOVE
    } else {
        // Preferred DropEffect is advisory, not the source's allowed-effect mask.
        // Chat clients prefer Copy; folder drops can still move on the same volume.
        let source_only_copies = preferred.contains(DROPEFFECT_COPY) && !has(DROPEFFECT_MOVE);
        let same_vol = paths.first().is_some_and(|src| same_volume(src, dst));
        if source_only_copies || !same_vol {
            if has(DROPEFFECT_COPY) {
                DROPEFFECT_COPY
            } else if has(DROPEFFECT_MOVE) {
                DROPEFFECT_MOVE
            } else {
                DROPEFFECT_NONE
            }
        } else if has(DROPEFFECT_MOVE) {
            DROPEFFECT_MOVE
        } else if has(DROPEFFECT_COPY) {
            DROPEFFECT_COPY
        } else {
            DROPEFFECT_NONE
        }
    };
    // Moving files directly back onto their own parent directory is a no-op.
    if chosen == DROPEFFECT_MOVE
        && paths
            .iter()
            .all(|p| p.parent().is_some_and(|parent| parent.as_os_str().eq_ignore_ascii_case(dst.as_os_str())))
    {
        return DROPEFFECT_NONE;
    }
    chosen
}

fn effect_to_action(effect: DROPEFFECT) -> Option<DropAction> {
    if effect == DROPEFFECT_MOVE {
        Some(DropAction::Move)
    } else if effect == DROPEFFECT_COPY {
        Some(DropAction::Copy)
    } else if effect == DROPEFFECT_LINK {
        Some(DropAction::Link)
    } else {
        None
    }
}

/// Formats handed out by the platform for shell file transfer.
fn hdrop_format() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

fn hglobal_format(format: u32) -> FORMATETC {
    FORMATETC {
        cfFormat: format as u16,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

fn object_paths(data: &IDataObject) -> Vec<PathBuf> {
    let mut medium = match unsafe { data.GetData(&hdrop_format()) } {
        Ok(medium) => medium,
        Err(_) => return Vec::new(),
    };
    if medium.tymed != TYMED_HGLOBAL.0 as u32 {
        unsafe { ReleaseStgMedium(&mut medium) };
        return Vec::new();
    }
    let hdrop = HDROP(unsafe { medium.u.hGlobal.0 });
    if hdrop.0.is_null() {
        unsafe { ReleaseStgMedium(&mut medium) };
        return Vec::new();
    }
    let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
    let mut paths = Vec::with_capacity(count as usize);
    for index in 0..count {
        let len = unsafe { DragQueryFileW(hdrop, index, None) } as usize;
        let mut buffer = vec![0u16; len + 1];
        let written = unsafe { DragQueryFileW(hdrop, index, Some(&mut buffer)) } as usize;
        buffer.truncate(written);
        paths.push(PathBuf::from(OsString::from_wide(&buffer)));
    }
    unsafe { ReleaseStgMedium(&mut medium) };
    paths
}

unsafe fn read_preferred_effect_medium(medium: &STGMEDIUM) -> DROPEFFECT {
    if medium.tymed != TYMED_HGLOBAL.0 as u32 {
        return DROPEFFECT_NONE;
    }
    let global = unsafe { medium.u.hGlobal };
    if global.0.is_null() || unsafe { GlobalSize(global) } < std::mem::size_of::<u32>() {
        return DROPEFFECT_NONE;
    }
    let ptr = unsafe { GlobalLock(global) } as *const u32;
    if ptr.is_null() {
        return DROPEFFECT_NONE;
    }
    let effect = unsafe { ptr.read_unaligned() };
    let _ = unsafe { GlobalUnlock(global) };
    DROPEFFECT(effect)
}

fn object_preferred_effect(data: &IDataObject) -> DROPEFFECT {
    let format = hglobal_format(unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) });
    let Ok(mut medium) = (unsafe { data.GetData(&format) }) else {
        return DROPEFFECT_NONE;
    };
    let effect = unsafe { read_preferred_effect_medium(&medium) };
    unsafe { ReleaseStgMedium(&mut medium) };
    effect
}

fn client_position(pt: POINTL) -> Pos2 {
    let hwnd = crate::platform::hwnd();
    if hwnd.0.is_null() {
        return Pos2::new(pt.x as f32, pt.y as f32);
    }
    let mut point = POINT { x: pt.x, y: pt.y };
    unsafe {
        let _ = ScreenToClient(hwnd, &mut point);
        let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
        Pos2::new(point.x as f32 / scale, point.y as f32 / scale)
    }
}

#[repr(C)]
#[allow(dead_code)]
struct TargetObject {
    vtable: *const IDropTarget_Vtbl,
    refs: AtomicU32,
    paths: Mutex<Vec<PathBuf>>,
    preferred: AtomicU32,
}

unsafe extern "system" fn target_query_interface(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    let iid = unsafe { &*iid };
    if *iid == IUnknown::IID || *iid == IDropTarget::IID {
        unsafe { *out = this };
        unsafe { target_add_ref(this) };
        S_OK
    } else {
        unsafe { *out = std::ptr::null_mut() };
        E_NOINTERFACE
    }
}

unsafe extern "system" fn target_add_ref(this: *mut c_void) -> u32 {
    let target = unsafe { &*(this as *const TargetObject) };
    target.refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn target_release(this: *mut c_void) -> u32 {
    let target = unsafe { &*(this as *const TargetObject) };
    let remaining = target.refs.fetch_sub(1, Ordering::Release) - 1;
    if remaining == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        unsafe { drop(Box::from_raw(this as *mut TargetObject)) };
    }
    remaining
}

unsafe extern "system" fn target_drag_enter(
    this: *mut c_void,
    data: *mut c_void,
    modifiers: MODIFIERKEYS_FLAGS,
    pt: POINTL,
    effect: *mut DROPEFFECT,
) -> HRESULT {
    let target = unsafe { &*(this as *const TargetObject) };
    let object = unsafe { IDataObject::from_raw_borrowed(&data) };
    let (paths, preferred) = match object {
        Some(object) => (object_paths(object), object_preferred_effect(object)),
        None => (Vec::new(), DROPEFFECT_NONE),
    };
    let pos = client_position(pt);
    let resolved = resolve_drop_target(pos);
    let allowed = unsafe { *effect };
    let chosen = choose_effect(modifiers, allowed, preferred, &paths, &resolved);
    *target.paths.lock().unwrap() = paths;
    target.preferred.store(preferred.0, Ordering::Relaxed);
    unsafe { *effect = chosen };
    *DRAG_HOVER.lock().unwrap() =
        Some(DragHover { position: pos, target: resolved.dir_path(), action: effect_to_action(chosen) });
    crate::platform::wake();
    S_OK
}

unsafe extern "system" fn target_drag_over(
    this: *mut c_void,
    modifiers: MODIFIERKEYS_FLAGS,
    pt: POINTL,
    effect: *mut DROPEFFECT,
) -> HRESULT {
    let target = unsafe { &*(this as *const TargetObject) };
    let paths = target.paths.lock().unwrap();
    let preferred = DROPEFFECT(target.preferred.load(Ordering::Relaxed));
    let pos = client_position(pt);
    let resolved = resolve_drop_target(pos);
    let allowed = unsafe { *effect };
    let chosen = choose_effect(modifiers, allowed, preferred, &paths, &resolved);
    unsafe { *effect = chosen };
    *DRAG_HOVER.lock().unwrap() =
        Some(DragHover { position: pos, target: resolved.dir_path(), action: effect_to_action(chosen) });
    crate::platform::wake();
    S_OK
}

unsafe extern "system" fn target_drag_leave(this: *mut c_void) -> HRESULT {
    let target = unsafe { &*(this as *const TargetObject) };
    target.paths.lock().unwrap().clear();
    target.preferred.store(DROPEFFECT_NONE.0, Ordering::Relaxed);
    *DRAG_HOVER.lock().unwrap() = None;
    crate::platform::wake();
    S_OK
}

unsafe extern "system" fn target_drop(
    this: *mut c_void,
    _data: *mut c_void,
    modifiers: MODIFIERKEYS_FLAGS,
    pt: POINTL,
    effect: *mut DROPEFFECT,
) -> HRESULT {
    let target = unsafe { &*(this as *const TargetObject) };
    let paths = std::mem::take(&mut *target.paths.lock().unwrap());
    let preferred = DROPEFFECT(target.preferred.swap(DROPEFFECT_NONE.0, Ordering::Relaxed));
    *DRAG_HOVER.lock().unwrap() = None;

    let pos = client_position(pt);
    let resolved = resolve_drop_target(pos);
    let allowed = unsafe { *effect };
    let chosen = choose_effect(modifiers, allowed, preferred, &paths, &resolved);
    unsafe { *effect = chosen };
    let Some(action) = effect_to_action(chosen) else {
        crate::platform::wake();
        return S_OK;
    };
    DROPS.lock().unwrap().push(DropEvent { paths, position: pos, action });
    // Nothing else tells an idle window that a drop just landed.
    crate::platform::wake();
    S_OK
}

static TARGET_VTABLE: IDropTarget_Vtbl = IDropTarget_Vtbl {
    base__: IUnknown_Vtbl { QueryInterface: target_query_interface, AddRef: target_add_ref, Release: target_release },
    DragEnter: target_drag_enter,
    DragOver: target_drag_over,
    DragLeave: target_drag_leave,
    Drop: target_drop,
};

fn new_target() -> IDropTarget {
    let object = Box::new(TargetObject {
        vtable: &TARGET_VTABLE,
        refs: AtomicU32::new(1),
        paths: Mutex::new(Vec::new()),
        preferred: AtomicU32::new(DROPEFFECT_NONE.0),
    });
    unsafe { IDropTarget::from_raw(Box::into_raw(object).cast::<c_void>()) }
}

#[repr(C)]
#[allow(dead_code)]
struct SourceObject {
    vtable: *const IDropSource_Vtbl,
    refs: AtomicU32,
}

unsafe extern "system" fn source_query_interface(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    let iid = unsafe { &*iid };
    if *iid == IUnknown::IID || *iid == IDropSource::IID {
        unsafe { *out = this };
        unsafe { source_add_ref(this) };
        S_OK
    } else {
        unsafe { *out = std::ptr::null_mut() };
        E_NOINTERFACE
    }
}

unsafe extern "system" fn source_add_ref(this: *mut c_void) -> u32 {
    let source = unsafe { &*(this as *const SourceObject) };
    source.refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn source_release(this: *mut c_void) -> u32 {
    let source = unsafe { &*(this as *const SourceObject) };
    let remaining = source.refs.fetch_sub(1, Ordering::Release) - 1;
    if remaining == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        unsafe { drop(Box::from_raw(this as *mut SourceObject)) };
    }
    remaining
}

unsafe extern "system" fn source_query_continue_drag(
    _this: *mut c_void,
    escape: BOOL,
    keys: MODIFIERKEYS_FLAGS,
) -> HRESULT {
    if escape.as_bool() {
        DRAGDROP_S_CANCEL
    } else if !keys.contains(MK_LBUTTON) && !keys.contains(MK_RBUTTON) {
        DRAGDROP_S_DROP
    } else {
        S_OK
    }
}

unsafe extern "system" fn source_give_feedback(_this: *mut c_void, _effect: DROPEFFECT) -> HRESULT {
    DRAGDROP_S_USEDEFAULTCURSORS
}

static SOURCE_VTABLE: IDropSource_Vtbl = IDropSource_Vtbl {
    base__: IUnknown_Vtbl { QueryInterface: source_query_interface, AddRef: source_add_ref, Release: source_release },
    QueryContinueDrag: source_query_continue_drag,
    GiveFeedback: source_give_feedback,
};

fn new_source() -> IDropSource {
    let object = Box::new(SourceObject { vtable: &SOURCE_VTABLE, refs: AtomicU32::new(1) });
    unsafe { IDropSource::from_raw(Box::into_raw(object).cast::<c_void>()) }
}

/// Registers the window as an OLE drop target. Call once after the window exists.
pub fn register_drop_target() -> anyhow::Result<()> {
    if DROP_TARGET.get().is_some() {
        return Ok(());
    }
    let hwnd = crate::platform::hwnd();
    anyhow::ensure!(!hwnd.0.is_null(), "register_drop_target called before the window was attached");
    let target = new_target();
    unsafe { RegisterDragDrop(hwnd, &target) }.context("RegisterDragDrop failed")?;
    let _ = DROP_TARGET.set(TargetHandle(target));
    Ok(())
}

/// Pops the oldest finished drop, one per call.
pub fn take_drop() -> Option<DropEvent> {
    let mut drops = DROPS.lock().unwrap();
    if drops.is_empty() { None } else { Some(drops.remove(0)) }
}

struct AbsolutePidl(*mut ITEMIDLIST);

impl Drop for AbsolutePidl {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast::<c_void>())) };
    }
}

fn absolute_pidl(path: &Path) -> anyhow::Result<AbsolutePidl> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None::<&IBindCtx>, &mut pidl, 0, None) }
        .with_context(|| format!("SHParseDisplayName failed for {}", path.display()))?;
    anyhow::ensure!(!pidl.is_null(), "SHParseDisplayName produced no PIDL for {}", path.display());
    Ok(AbsolutePidl(pidl))
}

/// Shell data object for the given files, carrying CF_HDROP and shell ID lists.
fn data_object(paths: &[PathBuf]) -> anyhow::Result<IDataObject> {
    anyhow::ensure!(!paths.is_empty(), "no files to drag");
    let pidls = paths.iter().map(|path| absolute_pidl(path)).collect::<anyhow::Result<Vec<_>>>()?;
    let raw: Vec<*const ITEMIDLIST> = pidls.iter().map(|pidl| pidl.0.cast_const()).collect();
    let items: IShellItemArray =
        unsafe { SHCreateShellItemArrayFromIDLists(&raw) }.context("SHCreateShellItemArrayFromIDLists failed")?;
    let data: IDataObject = unsafe { items.BindToHandler(None::<&IBindCtx>, &BHID_DataObject) }
        .context("IShellItemArray::BindToHandler(BHID_DataObject) failed")?;
    // Materialize file paths before crossing process boundaries. Chat clients
    // consume CF_HDROP directly instead of resolving Shell namespace items.
    set_object_bytes(&data, &hdrop_format(), &super::clipboard::hdrop_bytes(paths))?;
    let preferred = hglobal_format(unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) });
    set_object_bytes(&data, &preferred, &DROPEFFECT_COPY.0.to_ne_bytes())?;
    Ok(data)
}

fn set_object_bytes(data: &IDataObject, format: &FORMATETC, bytes: &[u8]) -> anyhow::Result<()> {
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }.context("allocate drag data")?;
    let ptr = unsafe { GlobalLock(memory) }.cast::<u8>();
    if ptr.is_null() {
        let _ = unsafe { GlobalFree(Some(memory)) };
        anyhow::bail!("lock drag data failed");
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let _ = GlobalUnlock(memory);
    }
    let medium = STGMEDIUM {
        tymed: TYMED_HGLOBAL.0 as u32,
        u: STGMEDIUM_0 { hGlobal: memory },
        pUnkForRelease: std::mem::ManuallyDrop::new(None),
    };
    if let Err(err) = unsafe { data.SetData(format, &medium, true) } {
        let _ = unsafe { GlobalFree(Some(memory)) };
        return Err(err).context("publish drag data");
    }
    Ok(())
}

/// Starts an OLE drag of the given files and blocks until the drag finishes.
pub fn drag_out(paths: &[PathBuf]) -> anyhow::Result<()> {
    anyhow::ensure!(!paths.is_empty(), "drag_out needs at least one path");
    let data = data_object(paths)?;
    // Attach Windows shell drag ghost helper when available.
    if let Ok(helper) =
        unsafe { CoCreateInstance::<_, IDragSourceHelper>(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER) }
    {
        let mut pt = POINT::default();
        let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt) };
        let _ = unsafe { ScreenToClient(crate::platform::hwnd(), &mut pt) };
        let _ = unsafe { helper.InitializeFromWindow(Some(crate::platform::hwnd()), Some(&pt), &data) };
    }
    let source = new_source();
    let mut effect = DROPEFFECT_NONE;
    // winit captures the mouse for the file-row press. OLE must own capture
    // while routing DragEnter/DragOver to windows in other processes.
    let _ = unsafe { ReleaseCapture() };
    unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK, &mut effect) }
        .ok()
        .context("DoDragDrop failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{GlobalFree, HGLOBAL};
    use windows::Win32::System::Com::{STGMEDIUM_0, TYMED_ISTREAM};
    use windows::Win32::System::Memory::{GMEM_FIXED, GlobalAlloc};
    use windows::Win32::System::SystemServices::{MK_CONTROL, MK_SHIFT};

    #[test]
    fn outgoing_files_advertise_hdrop_to_external_receivers() {
        unsafe { windows::Win32::System::Ole::OleInitialize(None) }.unwrap();
        let (_, _, files) = fixture_files();
        let object = data_object(&files).unwrap();
        unsafe { object.QueryGetData(&hdrop_format()) }.ok().expect("CF_HDROP must be advertised");
        let formats = unsafe { object.EnumFormatEtc(windows::Win32::System::Com::DATADIR_GET.0 as u32) }.unwrap();
        let mut advertised = false;
        loop {
            let mut entry = [FORMATETC::default()];
            let mut fetched = 0;
            if unsafe { formats.Next(&mut entry, Some(&mut fetched)) }.is_err() || fetched == 0 {
                break;
            }
            if entry[0].cfFormat == CF_HDROP.0 && entry[0].tymed & TYMED_HGLOBAL.0 as u32 != 0 {
                advertised = true;
            }
            unsafe { CoTaskMemFree(Some(entry[0].ptd.cast())) };
        }
        assert!(advertised, "external drop receivers must find CF_HDROP while enumerating formats");
        assert_eq!(object_paths(&object), files);
        assert_eq!(object_preferred_effect(&object), DROPEFFECT_COPY);
        assert_eq!(object_paths(&object), files, "GetData may be called repeatedly by a receiver");

        // A receiver in another COM apartment must obtain the same file list.
        use windows::Win32::System::Com::Marshal::CoMarshalInterThreadInterfaceInStream;
        use windows::Win32::System::Com::StructuredStorage::CoGetInterfaceAndReleaseStream;
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
        let stream = unsafe { CoMarshalInterThreadInterfaceInStream(&IDataObject::IID, &object) }.unwrap();
        let stream_raw = stream.into_raw() as usize;
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok().unwrap();
            let stream = unsafe { windows::Win32::System::Com::IStream::from_raw(stream_raw as *mut c_void) };
            // CoGetInterfaceAndReleaseStream consumes the stream reference itself.
            let stream = std::mem::ManuallyDrop::new(stream);
            let receiver: IDataObject = unsafe { CoGetInterfaceAndReleaseStream(&*stream) }.unwrap();
            let received = (object_paths(&receiver), object_preferred_effect(&receiver));
            drop(receiver);
            unsafe { CoUninitialize() };
            tx.send(received).unwrap();
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let received = loop {
            if let Ok(received) = rx.try_recv() {
                break received;
            }
            assert!(std::time::Instant::now() < deadline, "receiver timed out");
            let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
            while unsafe {
                windows::Win32::UI::WindowsAndMessaging::PeekMessageW(
                    &mut msg,
                    None,
                    0,
                    0,
                    windows::Win32::UI::WindowsAndMessaging::PM_REMOVE,
                )
            }
            .as_bool()
            {
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
                    windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        thread.join().unwrap();
        assert_eq!(received, (files, DROPEFFECT_COPY));
    }

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn fixture_files() -> (PathBuf, PathBuf, Vec<PathBuf>) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("tmp").join("dnd-test");
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let files = vec![src.join("one.txt"), src.join("二.txt")];
        for file in &files {
            std::fs::write(file, b"lumen").unwrap();
        }
        (src, dst, files)
    }

    #[test]
    fn drop_target_respects_ole_allowed_mask_and_modifiers() {
        let _guard = TEST_LOCK.lock().unwrap();
        unsafe { windows::Win32::System::Ole::OleInitialize(None) }.unwrap();
        let (_src, dst, files) = fixture_files();
        set_pin_drop_rect(None);
        update_drop_context(&[], Some(dst.clone()));
        while take_drop().is_some() {}
        let data = data_object(&files).unwrap();
        assert_eq!(object_paths(&data), files);

        let target = new_target();
        let point = POINTL { x: 0, y: 0 };
        let all_allowed = DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK;

        // Default same-volume drop with all effects allowed -> Move (NOT Copy!).
        let mut effect = all_allowed;
        unsafe { target.DragEnter(&data, MODIFIERKEYS_FLAGS(0), point, &mut effect) }.unwrap();
        assert_eq!(effect, DROPEFFECT_MOVE);

        let mut copy_only = DROPEFFECT_COPY;
        unsafe { target.DragOver(MODIFIERKEYS_FLAGS(0), point, &mut copy_only) }.unwrap();
        assert_eq!(copy_only, DROPEFFECT_COPY, "copy-only receivers must not be offered a move");

        // Windows passes `dwOkEffect` (bitmask) into Drop; target must recompute and write back.
        let mut drop_effect = all_allowed;
        unsafe { target.Drop(&data, MODIFIERKEYS_FLAGS(0), point, &mut drop_effect) }.unwrap();
        assert_eq!(drop_effect, DROPEFFECT_MOVE);
        let event = take_drop().expect("drop was not queued");
        assert_eq!(event.paths, files);
        assert_eq!(event.action, DropAction::Move);
        assert!(take_drop().is_none());

        // Ctrl forces Copy.
        let mut effect = all_allowed;
        unsafe { target.DragEnter(&data, MK_CONTROL, point, &mut effect) }.unwrap();
        assert_eq!(effect, DROPEFFECT_COPY);
        let mut drop_effect = all_allowed;
        unsafe { target.Drop(&data, MK_CONTROL, point, &mut drop_effect) }.unwrap();
        assert_eq!(drop_effect, DROPEFFECT_COPY);
        assert_eq!(take_drop().unwrap().action, DropAction::Copy);

        // Alt forces Link (shortcut).
        let mut effect = all_allowed;
        unsafe { target.DragEnter(&data, MK_ALT, point, &mut effect) }.unwrap();
        assert_eq!(effect, DROPEFFECT_LINK);
        let mut drop_effect = all_allowed;
        unsafe { target.Drop(&data, MK_ALT, point, &mut drop_effect) }.unwrap();
        assert_eq!(drop_effect, DROPEFFECT_LINK);
        assert_eq!(take_drop().unwrap().action, DropAction::Link);

        // Shift forces Move even when source prefers Copy.
        let mut effect = all_allowed;
        unsafe { target.DragEnter(&data, MK_SHIFT, point, &mut effect) }.unwrap();
        assert_eq!(effect, DROPEFFECT_MOVE);
        unsafe { target.DragLeave() }.unwrap();
        assert!(drag_hover().is_none());
    }

    #[test]
    fn pin_drop_rect_allows_ancestor_dir_and_rejects_plain_files() {
        let _guard = TEST_LOCK.lock().unwrap();
        unsafe { windows::Win32::System::Ole::OleInitialize(None) }.unwrap();
        let (src, _dst, files) = fixture_files();
        let parent = src.parent().unwrap().to_path_buf();
        let pin_rect = Rect::from_min_max(Pos2::new(10.0, 10.0), Pos2::new(200.0, 40.0));
        // Current dir is a descendant of `parent`, which would normally fail `is_self_or_ancestor`.
        update_drop_context(&[], Some(src.clone()));
        set_pin_drop_rect(Some(pin_rect));
        while take_drop().is_some() {}

        let all_allowed = DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK;
        let dir_data = data_object(std::slice::from_ref(&parent)).unwrap();
        let target = new_target();
        let inside_pin = POINTL { x: 50, y: 20 };

        // Dragging ancestor folder over pin_drop_rect succeeds with DROPEFFECT_LINK.
        let mut effect = all_allowed;
        unsafe { target.DragEnter(&dir_data, MODIFIERKEYS_FLAGS(0), inside_pin, &mut effect) }.unwrap();
        assert_eq!(effect, DROPEFFECT_LINK);
        assert!(drag_hover().is_some_and(|h| h.target.is_none() && h.action == Some(DropAction::Link)));

        let mut drop_effect = all_allowed;
        unsafe { target.Drop(&dir_data, MODIFIERKEYS_FLAGS(0), inside_pin, &mut drop_effect) }.unwrap();
        assert_eq!(drop_effect, DROPEFFECT_LINK);
        let ev = take_drop().expect("pin drop should enqueue DropEvent");
        assert_eq!(ev.paths, vec![parent.clone()]);
        assert_eq!(ev.action, DropAction::Link);

        // Dragging the same ancestor folder outside pin_drop_rect (onto child `src`) is still rejected!
        let outside_pin = POINTL { x: 300, y: 300 };
        let mut effect = all_allowed;
        unsafe { target.DragEnter(&dir_data, MODIFIERKEYS_FLAGS(0), outside_pin, &mut effect) }.unwrap();
        assert_eq!(effect, DROPEFFECT_NONE);
        unsafe { target.DragLeave() }.unwrap();

        // Dragging only non-directory files over pin_drop_rect is disallowed.
        let file_data = data_object(&files).unwrap();
        let mut effect = all_allowed;
        unsafe { target.DragEnter(&file_data, MODIFIERKEYS_FLAGS(0), inside_pin, &mut effect) }.unwrap();
        assert_eq!(effect, DROPEFFECT_NONE);
        unsafe { target.DragLeave() }.unwrap();

        set_pin_drop_rect(None);
    }

    #[test]
    fn non_filesystem_empty_area_returns_dropeffect_none_while_zone_still_accepts() {
        let _guard = TEST_LOCK.lock().unwrap();
        unsafe { windows::Win32::System::Ole::OleInitialize(None) }.unwrap();
        let (_src, dst, files) = fixture_files();
        let drive_zone = Rect::from_min_max(Pos2::new(20.0, 20.0), Pos2::new(220.0, 60.0));
        // Simulates `This PC` / Network view: current_dir is None, but drive row has a zone.
        set_pin_drop_rect(None);
        update_drop_context(&[(drive_zone, dst.clone())], None);
        while take_drop().is_some() {}

        let all_allowed = DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK;
        let data = data_object(&files).unwrap();
        let target = new_target();

        // Empty area outside any zone -> DROPEFFECT_NONE on both DragEnter and Drop.
        let empty_pt = POINTL { x: 400, y: 400 };
        let mut effect = all_allowed;
        unsafe { target.DragEnter(&data, MODIFIERKEYS_FLAGS(0), empty_pt, &mut effect) }.unwrap();
        assert_eq!(effect, DROPEFFECT_NONE);
        assert!(drag_hover().is_some_and(|h| h.target.is_none() && h.action.is_none()));

        let mut drop_effect = all_allowed;
        unsafe { target.Drop(&data, MODIFIERKEYS_FLAGS(0), empty_pt, &mut drop_effect) }.unwrap();
        assert_eq!(drop_effect, DROPEFFECT_NONE);
        assert!(take_drop().is_none());

        // Inside the drive zone -> allowed!
        let zone_pt = POINTL { x: 80, y: 40 };
        let mut effect = all_allowed;
        unsafe { target.DragEnter(&data, MODIFIERKEYS_FLAGS(0), zone_pt, &mut effect) }.unwrap();
        assert_eq!(effect, DROPEFFECT_MOVE);
        assert!(drag_hover().is_some_and(|h| h.target.as_deref() == Some(dst.as_path())));
        unsafe { target.DragLeave() }.unwrap();
    }

    #[test]
    fn preferred_effect_validates_tymed_and_hglobal_size() {
        // Non-HGLOBAL tymed must be ignored safely.
        let bad_tymed = STGMEDIUM {
            tymed: TYMED_ISTREAM.0 as u32,
            u: STGMEDIUM_0 { hGlobal: HGLOBAL(std::ptr::null_mut()) },
            pUnkForRelease: std::mem::ManuallyDrop::new(None),
        };
        assert_eq!(unsafe { read_preferred_effect_medium(&bad_tymed) }, DROPEFFECT_NONE);

        // Undersized HGLOBAL (< 4 bytes) must return DROPEFFECT_NONE without out-of-bounds read.
        let tiny = unsafe { GlobalAlloc(GMEM_FIXED, 2) }.unwrap();
        let tiny_med = STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: tiny },
            pUnkForRelease: std::mem::ManuallyDrop::new(None),
        };
        assert_eq!(unsafe { read_preferred_effect_medium(&tiny_med) }, DROPEFFECT_NONE);
        let _ = unsafe { GlobalFree(Some(tiny)) };

        // Valid 4-byte HGLOBAL returns the stored DROPEFFECT.
        let valid = unsafe { GlobalAlloc(GMEM_FIXED, std::mem::size_of::<u32>()) }.unwrap();
        unsafe {
            *(valid.0 as *mut u32) = DROPEFFECT_COPY.0;
        }
        let valid_med = STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: valid },
            pUnkForRelease: std::mem::ManuallyDrop::new(None),
        };
        assert_eq!(unsafe { read_preferred_effect_medium(&valid_med) }, DROPEFFECT_COPY);
        let _ = unsafe { GlobalFree(Some(valid)) };
    }
}
