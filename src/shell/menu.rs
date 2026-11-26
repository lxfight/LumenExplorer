//! Classic Win32 native Shell context menu ("显示更多选项" / Shift+F10).
//!
//! Lumen's primary right-click menu is now a zero-latency Fluent 2 overlay
//! (`ui::context_menu`). This module owns the lazily initialized Win32
//! `IContextMenu` popup so third-party shell extensions (7-Zip, Git, VS Code,
//! WinRAR, OneDrive, etc.) and virtual shell verbs remain 100% accessible.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    BHID_SFObject, BHID_SFUIObject, CMF_EXTENDEDVERBS, CMF_NORMAL, CMIC_MASK_PTINVOKE, CMINVOKECOMMANDINFOEX,
    IContextMenu, IContextMenu2, IContextMenu3, IShellFolder, IShellItem, IShellItemArray, SHBindToParent,
    SHCreateItemFromParsingName, SHCreateShellItemArrayFromIDLists, SHParseDisplayName,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CreatePopupMenu, DestroyMenu, GetCursorPos, GetForegroundWindow, GetWindowThreadProcessId, HMENU,
    PostMessageW, SW_SHOWNORMAL, SetForegroundWindow, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx,
    WM_INITMENUPOPUP, WM_MENUSELECT, WM_NULL,
};
use windows::core::{HSTRING, Interface, PCSTR};

use super::location::Pidl;

const SHELL_ID_FIRST: u32 = 1;
const SHELL_ID_LAST: u32 = 0x6FFF;
const CMIC_MASK_ASYNC_OK: u32 = 0x0010_0000;

struct MenuState {
    cm: IContextMenu,
    cm2: Option<IContextMenu2>,
    cm3: Option<IContextMenu3>,
    hwnd: HWND,
    pt: POINT,
}

unsafe impl Send for MenuState {}

static MENU_STATE: Mutex<Option<MenuState>> = Mutex::new(None);

struct PopupMenu(HMENU);

impl Drop for PopupMenu {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyMenu(self.0);
        }
    }
}

struct OwnedPidls(Vec<*mut ITEMIDLIST>);

impl Drop for OwnedPidls {
    fn drop(&mut self) {
        for pidl in &self.0 {
            unsafe { CoTaskMemFree(Some(*pidl as *const c_void)) };
        }
    }
}

/// Pops the full native Windows Shell classic menu for filesystem items.
pub fn show_classic_for_items(items: &[PathBuf], extended: bool) {
    if items.is_empty() {
        return;
    }
    report((|| -> Result<()> {
        let hwnd = window_handle()?;
        let cm = unsafe { menu_for_items(items, hwnd)? };
        run_classic(cm, hwnd, extended)
    })());
}

/// Pops the full native Windows Shell classic menu for virtual shell PIDLs
/// (Recycle Bin items, This PC drives, Network shares, etc.).
pub fn show_classic_for_pidls(pidls: &[Arc<Pidl>], extended: bool) {
    if pidls.is_empty() {
        return;
    }
    report((|| -> Result<()> {
        let hwnd = window_handle()?;
        let raw: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| p.as_ptr()).collect();
        let array: IShellItemArray = unsafe { SHCreateShellItemArrayFromIDLists(&raw) }
            .context("cannot create shell item array for virtual items")?;
        let cm: IContextMenu = unsafe { array.BindToHandler(None::<&IBindCtx>, &BHID_SFUIObject) }
            .context("cannot bind context menu for virtual items")?;
        run_classic(cm, hwnd, extended)
    })());
}

/// Pops the full native Windows Shell background context menu for a folder.
pub fn show_classic_background(dir: &Path, extended: bool) {
    report((|| -> Result<()> {
        let hwnd = window_handle()?;
        let cm = unsafe { background_menu(dir, hwnd)? };
        run_classic(cm, hwnd, extended)
    })());
}

fn run_classic(cm: IContextMenu, hwnd: HWND, extended: bool) -> Result<()> {
    let menu = PopupMenu(unsafe { CreatePopupMenu() }.context("cannot create popup menu")?);
    let flags = if extended { CMF_NORMAL | CMF_EXTENDEDVERBS } else { CMF_NORMAL };
    unsafe { cm.QueryContextMenu(menu.0, 0, SHELL_ID_FIRST, SHELL_ID_LAST, flags) }
        .ok()
        .context("QueryContextMenu failed")?;

    let pt = cursor_position()?;
    let (cm2, cm3) = (cm.cast().ok(), cm.cast().ok());
    {
        let mut guard = MENU_STATE.lock().map_err(|_| anyhow::anyhow!("menu state poisoned"))?;
        *guard = Some(MenuState { cm, cm2, cm3, hwnd, pt });
    }
    force_foreground(hwnd);
    let command = unsafe {
        TrackPopupMenuEx(menu.0, TPM_RETURNCMD.0 | TPM_RIGHTBUTTON.0 | TPM_LEFTALIGN.0, pt.x, pt.y, hwnd, None).0 as u32
    };
    let _ = unsafe { PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0)) };
    let state = MENU_STATE.lock().ok().and_then(|mut guard| guard.take());

    if (SHELL_ID_FIRST..=SHELL_ID_LAST).contains(&command)
        && let Some(state) = &state
    {
        unsafe { invoke_shell_command(&state.cm, command - SHELL_ID_FIRST, state.pt, state.hwnd)? };
    }
    Ok(())
}

/// Called from the window subclass for owner-drawn shell extension submenus.
pub fn handle_menu_message(msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let (cm2, cm3) = {
        let guard = MENU_STATE.lock().ok()?;
        let state = guard.as_ref()?;
        (state.cm2.clone(), state.cm3.clone())
    };
    if matches!(msg, WM_MENUSELECT) || msg == 0x002B || msg == 0x002C || msg == WM_INITMENUPOPUP {
        if let Some(cm3) = cm3 {
            let mut result = LRESULT(0);
            return unsafe { cm3.HandleMenuMsg2(msg, wparam, lparam, Some(&mut result as *mut LRESULT)) }
                .ok()
                .map(|_| result);
        }
        if let Some(cm2) = cm2 {
            return unsafe { cm2.HandleMenuMsg(msg, wparam, lparam) }.ok().map(|_| LRESULT(0));
        }
    }
    None
}

fn window_handle() -> Result<HWND> {
    let hwnd = crate::platform::hwnd();
    if hwnd.0.is_null() {
        anyhow::bail!("the window is not attached yet");
    }
    Ok(hwnd)
}

fn report(result: Result<()>) {
    if let Err(err) = result {
        eprintln!("context menu: {err:#}");
    }
}

fn cursor_position() -> Result<POINT> {
    let mut pt = POINT::default();
    unsafe { GetCursorPos(&mut pt) }.context("cannot read cursor position")?;
    Ok(pt)
}

fn force_foreground(hwnd: HWND) {
    unsafe {
        let foreground = GetForegroundWindow();
        let foreground_thread = GetWindowThreadProcessId(foreground, None);
        let current_thread = GetCurrentThreadId();
        let attached = foreground_thread != 0 && foreground_thread != current_thread;
        if attached {
            let _ = AttachThreadInput(foreground_thread, current_thread, true);
        }
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if attached {
            let _ = AttachThreadInput(foreground_thread, current_thread, false);
        }
    }
}

unsafe fn menu_for_items(items: &[PathBuf], hwnd: HWND) -> Result<IContextMenu> {
    unsafe {
        let mut pidls = OwnedPidls(Vec::with_capacity(items.len()));
        for path in items {
            let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
            SHParseDisplayName(&HSTRING::from(path.to_string_lossy().as_ref()), None::<&IBindCtx>, &mut pidl, 0, None)
                .with_context(|| format!("cannot resolve {}", path.display()))?;
            if pidl.is_null() {
                anyhow::bail!("cannot resolve {}", path.display());
            }
            pidls.0.push(pidl);
        }
        let first_parent = items[0].parent();
        let same_parent = items.iter().all(|p| p.parent() == first_parent);
        if same_parent {
            let mut children: Vec<*const ITEMIDLIST> = Vec::with_capacity(items.len());
            let mut parent: Option<IShellFolder> = None;
            for &pidl in &pidls.0 {
                let mut relative: *mut ITEMIDLIST = std::ptr::null_mut();
                let folder: IShellFolder =
                    SHBindToParent(pidl, Some(&mut relative)).context("cannot bind the parent shell folder")?;
                if parent.is_none() {
                    parent = Some(folder);
                }
                children.push(relative as *const ITEMIDLIST);
            }
            let parent = parent.context("cannot resolve the parent shell folder")?;
            parent.GetUIObjectOf::<IContextMenu>(hwnd, &children, None).context("cannot create the shell context menu")
        } else {
            let raw: Vec<*const ITEMIDLIST> = pidls.0.iter().map(|&p| p as *const ITEMIDLIST).collect();
            let array: IShellItemArray = SHCreateShellItemArrayFromIDLists(&raw)
                .context("cannot create shell item array for cross-folder selection")?;
            array.BindToHandler(None::<&IBindCtx>, &BHID_SFUIObject).context("cannot bind cross-folder context menu")
        }
    }
}

unsafe fn background_menu(dir: &Path, hwnd: HWND) -> Result<IContextMenu> {
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(dir), None)
            .with_context(|| format!("cannot create a shell item for {}", dir.display()))?;
        let folder: IShellFolder = item
            .BindToHandler(None, &BHID_SFObject)
            .with_context(|| format!("cannot open the shell folder {}", dir.display()))?;
        folder.CreateViewObject(hwnd).context("cannot create the background shell menu")
    }
}

unsafe fn invoke_shell_command(cm: &IContextMenu, offset: u32, pt: POINT, hwnd: HWND) -> Result<()> {
    let info = CMINVOKECOMMANDINFOEX {
        cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: CMIC_MASK_ASYNC_OK | CMIC_MASK_PTINVOKE,
        hwnd,
        lpVerb: PCSTR(offset as usize as *const u8),
        nShow: SW_SHOWNORMAL.0,
        ptInvoke: pt,
        ..Default::default()
    };
    unsafe { cm.InvokeCommand(&info as *const _ as *const _) }.context("the shell command failed")?;
    Ok(())
}
