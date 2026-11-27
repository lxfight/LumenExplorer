//! Shell locations: filesystem paths and virtual shell namespace items (PIDLs).

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use windows::Win32::System::Com::{CoTaskMemAlloc, CoTaskMemFree};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    ILClone, ILFree, ILGetSize, IShellItem, KF_FLAG_DEFAULT, SHCreateItemFromIDList, SHCreateItemFromParsingName,
    SHGetIDListFromObject, SHGetKnownFolderItem, SHParseDisplayName, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH,
    SIGDN_NORMALDISPLAY,
};
use windows::core::{GUID, HSTRING, PCWSTR, PWSTR};

/// Canonical desktop parsing name for the Windows Recycle Bin virtual folder.
pub const RECYCLE_BIN_PARSING: &str = "::{645FF040-5081-101B-9F08-00AA002F954E}";

/// Owned absolute shell item id list.
pub struct Pidl {
    raw: *mut ITEMIDLIST,
}

impl Pidl {
    pub fn from_raw(raw: *mut ITEMIDLIST) -> Self {
        Self { raw }
    }

    pub fn as_ptr(&self) -> *const ITEMIDLIST {
        self.raw
    }

    /// Serializes the raw PIDL bytes so another COM apartment can reconstruct
    /// an independent `Pidl` without sharing apartment-bound COM objects.
    pub fn to_bytes(&self) -> Vec<u8> {
        if self.raw.is_null() {
            return Vec::new();
        }
        let size = unsafe { ILGetSize(Some(self.raw as *const ITEMIDLIST)) } as usize;
        if size == 0 {
            return Vec::new();
        }
        unsafe { std::slice::from_raw_parts(self.raw as *const u8, size) }.to_vec()
    }

    /// Allocates an owned COM-task-memory PIDL from raw bytes produced by `to_bytes`.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 2 {
            return None;
        }
        let ptr = unsafe { CoTaskMemAlloc(bytes.len()) };
        if ptr.is_null() {
            return None;
        }
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast::<u8>(), bytes.len()) };
        Some(Self { raw: ptr.cast::<ITEMIDLIST>() })
    }
}

impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe { ILFree(Some(self.raw as *const ITEMIDLIST)) };
    }
}

// A PIDL is an immutable COM-allocated blob that this type owns exclusively:
// moving the owner across threads is safe as long as no COM call is made with it
// from the receiving apartment. Background listing sends filesystem entries
// (which carry no PIDL) to the UI thread, so `Entry` must be `Send`; the shell
// never uses one of these away from the STA that produced it.
unsafe impl Send for Pidl {}
unsafe impl Sync for Pidl {}

impl Clone for Pidl {
    fn clone(&self) -> Self {
        unsafe { Self::from_raw(ILClone(self.raw as *const ITEMIDLIST)) }
    }
}

/// A navigation target: a filesystem directory or a shell namespace item.
#[derive(Clone)]
pub struct Location {
    item: IShellItem,
    pidl: Arc<Pidl>,
    pub path: Option<PathBuf>,
    pub display: String,
}

impl Location {
    /// Accepts absolute paths, environment-variable paths, `shell:` names,
    /// CLSID strings and shell parsing names.
    pub fn parse(input: &str) -> Result<Self> {
        let trimmed = input.trim().trim_matches('"');
        if trimmed.is_empty() {
            bail!("路径为空");
        }
        let expanded = expand_env(trimmed);
        if Path::new(&expanded).is_dir() {
            return Self::from_path(Path::new(&expanded));
        }
        let wide = HSTRING::from(expanded.as_str());
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        unsafe {
            SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None)
                .with_context(|| format!("无法定位: {expanded}"))?;
        }
        if pidl.is_null() {
            bail!("无法定位: {expanded}");
        }
        Self::from_pidl(pidl)
    }

    pub fn from_path(path: &Path) -> Result<Self> {
        let wide = HSTRING::from(path.as_os_str().to_string_lossy().as_ref());
        let item: IShellItem = unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None) }
            .with_context(|| format!("无法打开: {}", path.display()))?;
        Self::from_item(item)
    }

    pub fn known(folder: &GUID) -> Result<Self> {
        let item: IShellItem =
            unsafe { SHGetKnownFolderItem(folder, KF_FLAG_DEFAULT, None) }.context("无法定位已知文件夹")?;
        Self::from_item(item)
    }

    pub fn from_item(item: IShellItem) -> Result<Self> {
        let raw = unsafe { SHGetIDListFromObject(&item) }.context("无法获取 Shell 项目 ID 列表")?;
        let path = item_path(&item).filter(|path| path.is_dir());
        let display = item_display(&item);
        Ok(Self { item, pidl: Arc::new(Pidl::from_raw(raw)), path, display })
    }

    pub fn from_pidl(pidl: *mut ITEMIDLIST) -> Result<Self> {
        let item: IShellItem = unsafe { SHCreateItemFromIDList(pidl) }.context("无法由 PIDL 创建 Shell 项目")?;
        let path = item_path(&item).filter(|path| path.is_dir());
        let display = item_display(&item);
        Ok(Self { item, pidl: Arc::new(Pidl::from_raw(pidl)), path, display })
    }

    pub fn from_absolute_pidl(pidl: Arc<Pidl>) -> Result<Self> {
        let item: IShellItem =
            unsafe { SHCreateItemFromIDList(pidl.as_ptr()) }.context("无法由 PIDL 创建 Shell 项目")?;
        let path = item_path(&item).filter(|path| path.is_dir());
        let display = item_display(&item);
        Ok(Self { item, pidl, path, display })
    }

    pub fn pidl(&self) -> *const ITEMIDLIST {
        self.pidl.as_ptr()
    }

    pub fn pidl_bytes(&self) -> Vec<u8> {
        self.pidl.to_bytes()
    }

    pub fn shell_item(&self) -> IShellItem {
        self.item.clone()
    }

    /// Canonical parsing name; used for tab persistence and window titles.
    pub fn parsing_name(&self) -> String {
        if let Some(path) = &self.path {
            return path.to_string_lossy().to_string();
        }
        unsafe {
            self.item
                .GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING)
                .map(take_pwstr)
                .unwrap_or_else(|_| self.display.clone())
        }
    }

    /// True when this location is the Windows Recycle Bin virtual folder.
    pub fn is_recycle_bin(&self) -> bool {
        self.path.is_none() && self.parsing_name().eq_ignore_ascii_case(RECYCLE_BIN_PARSING)
    }
}

/// True when a physical filesystem path lies inside any volume's `$Recycle.Bin`
/// directory (defense-in-depth when browsing with system files visible).
pub fn is_in_recycle_bin_path(path: &Path) -> bool {
    path.components()
        .any(|c| matches!(c, std::path::Component::Normal(seg) if seg.to_string_lossy().eq_ignore_ascii_case("$Recycle.Bin")))
}

pub fn item_path(item: &IShellItem) -> Option<PathBuf> {
    unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.ok().map(take_pwstr).filter(|s| !s.is_empty()).map(PathBuf::from)
}

pub fn item_display(item: &IShellItem) -> String {
    unsafe { item.GetDisplayName(SIGDN_NORMALDISPLAY) }.map(take_pwstr).unwrap_or_default()
}

pub fn take_pwstr(p: PWSTR) -> String {
    let s = unsafe { p.to_string().unwrap_or_default() };
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    s
}

/// Expands `%VARS%`; other input passes through untouched.
pub fn expand_env(input: &str) -> String {
    if !input.contains('%') {
        return input.to_owned();
    }
    use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
    let wide = HSTRING::from(input);
    let needed = unsafe { ExpandEnvironmentStringsW(PCWSTR(wide.as_ptr()), None) };
    if needed == 0 {
        return input.to_owned();
    }
    let mut buf = vec![0u16; needed as usize];
    let written = unsafe { ExpandEnvironmentStringsW(PCWSTR(wide.as_ptr()), Some(&mut buf)) };
    if written == 0 {
        return input.to_owned();
    }
    String::from_utf16_lossy(&buf[..(written as usize).saturating_sub(1)])
}

pub fn known_path(folder: &GUID) -> Option<PathBuf> {
    Location::known(folder).ok().and_then(|l| l.path)
}

pub fn user_profile() -> PathBuf {
    std::env::var("USERPROFILE").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("C:\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_physical_recycle_bin_paths() {
        assert!(is_in_recycle_bin_path(Path::new(r"C:\$Recycle.Bin\S-1-5-21-123\$RABC123.txt")));
        assert!(is_in_recycle_bin_path(Path::new(r"D:\$recycle.bin\S-1-5-21-999")));
        assert!(!is_in_recycle_bin_path(Path::new(r"C:\Users\Test\Recycle.Bin\a.txt")));
        assert!(!is_in_recycle_bin_path(Path::new(r"D:\Work\notes.md")));
    }
}
