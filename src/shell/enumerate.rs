//! Directory enumeration: filesystem folders via std::fs, virtual shell folders via IShellFolder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{FILETIME, HWND};
use windows::Win32::Storage::EnhancedStorage::{PKEY_DateCreated, PKEY_DateModified, PKEY_ItemTypeText, PKEY_Size};
use windows::Win32::System::Registry::{HKEY_CLASSES_ROOT, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::SystemServices::{SFGAO_FOLDER, SFGAO_HIDDEN, SFGAO_STREAM};
use windows::Win32::UI::Shell::Common::{ITEMIDLIST, STRRET};
use windows::Win32::UI::Shell::{
    ASSOCF_NONE, ASSOCSTR_FRIENDLYDOCNAME, AssocQueryStringW, BHID_SFObject, IEnumIDList, ILCombine, ILFree,
    IShellFolder, IShellItem2, SHCONTF_FOLDERS, SHCONTF_INCLUDEHIDDEN, SHCONTF_NONFOLDERS, SHGDN_NORMAL, StrRetToBufW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use crate::core::Entry;
use crate::core::model::attrs;

use super::location::{Location, Pidl, item_path, take_pwstr};

/// Filesystem-only listing used by the background worker.
pub fn list_fs(path: &Path, show_hidden: bool, show_system: bool, cancel: &AtomicBool) -> Result<Vec<Entry>> {
    let read = std::fs::read_dir(path).with_context(|| format!("无法读取 {}", path.display()))?;
    Ok(collect_fs(read, show_hidden, show_system, cancel))
}

fn collect_fs(read: std::fs::ReadDir, show_hidden: bool, show_system: bool, cancel: &AtomicBool) -> Vec<Entry> {
    use std::os::windows::fs::MetadataExt;
    let mut out = Vec::new();
    for (index, entry) in read.flatten().enumerate() {
        // Only check periodically: an atomic load per entry costs more than the
        // entry itself, and a stale listing is dropped by generation anyway.
        if index % 256 == 0 && cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return out;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        let file_attrs = meta.file_attributes();
        // Follow real Windows Explorer visibility rules:
        // - Protected OS files (HIDDEN + SYSTEM, e.g. desktop.ini, $RECYCLE.BIN) stay hidden unless `show_system`.
        // - Regular hidden files (FILE_ATTRIBUTE_HIDDEN) follow `show_hidden`.
        // - Dotfiles like `.git` / `.env` are normal files on Windows unless they carry FILE_ATTRIBUTE_HIDDEN.
        let is_hidden = file_attrs & attrs::HIDDEN != 0;
        let is_system = file_attrs & attrs::SYSTEM != 0;
        if is_system && is_hidden && !show_system {
            continue;
        }
        if is_hidden && !show_hidden {
            continue;
        }
        let is_folder = meta.is_dir();
        let ext = Path::new(&name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let kind = if is_folder { "文件夹".to_string() } else { type_name_for_ext(&ext) };
        out.push(Entry {
            name,
            path: Some(entry.path()),
            is_folder,
            size: if is_folder { 0 } else { meta.len() },
            modified: meta.modified().ok(),
            created: meta.created().ok(),
            kind,
            ext,
            free_space: 0,
            file_attrs,
            pidl: None,
        });
    }
    out
}

pub fn list_shell(loc: &Location, show_hidden: bool) -> Result<Vec<Entry>> {
    let folder: IShellFolder =
        unsafe { loc.shell_item().BindToHandler(None, &BHID_SFObject) }.context("无法打开 Shell 文件夹")?;
    let mut flags: u32 = SHCONTF_FOLDERS.0 as u32 | SHCONTF_NONFOLDERS.0 as u32;
    if show_hidden {
        flags |= SHCONTF_INCLUDEHIDDEN.0 as u32;
    }
    let mut enumerator: Option<IEnumIDList> = None;
    unsafe { folder.EnumObjects(HWND(std::ptr::null_mut()), flags, &mut enumerator) }
        .ok()
        .context("无法枚举 Shell 文件夹")?;
    let Some(enumerator) = enumerator else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    loop {
        // `Next` writes through the slice element, so it must be a named array.
        let mut items: [*mut ITEMIDLIST; 1] = [std::ptr::null_mut()];
        let mut fetched = 0u32;
        let hr = unsafe { enumerator.Next(&mut items, Some(&mut fetched)) };
        let child = items[0];
        if hr.0 < 0 || fetched == 0 || child.is_null() {
            break;
        }
        if let Some(entry) = shell_entry(loc, &folder, child, show_hidden) {
            out.push(entry);
        }
        unsafe { ILFree(Some(child as *const ITEMIDLIST)) };
    }
    Ok(out)
}

fn shell_entry(loc: &Location, folder: &IShellFolder, child: *mut ITEMIDLIST, show_hidden: bool) -> Option<Entry> {
    unsafe {
        let mut sfgao: u32 = SFGAO_FOLDER.0 | SFGAO_HIDDEN.0 | SFGAO_STREAM.0;
        folder.GetAttributesOf(&[child], &mut sfgao).ok()?;
        let hidden = sfgao & SFGAO_HIDDEN.0 != 0;
        if hidden && !show_hidden {
            return None;
        }
        let is_folder = sfgao & SFGAO_FOLDER.0 != 0;

        let mut strret = STRRET::default();
        folder.GetDisplayNameOf(child, SHGDN_NORMAL, &mut strret).ok()?;
        let mut buf = [0u16; 1024];
        if StrRetToBufW(&mut strret, None, &mut buf).is_err() {
            return None;
        }
        let name = String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]);

        let absolute_ptr = ILCombine(Some(loc.pidl()), Some(child));
        if absolute_ptr.is_null() {
            return None;
        }
        let absolute = Arc::new(Pidl::from_raw(absolute_ptr));
        let item: IShellItem2 = wire_item(absolute_ptr)?;
        let path: Option<PathBuf> = item
            .GetDisplayName(windows::Win32::UI::Shell::SIGDN_FILESYSPATH)
            .ok()
            .map(take_pwstr)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| item_path(&item));
        let mut size = item.GetUInt64(&PKEY_Size).unwrap_or(0);
        let mut free_space = 0;
        if let Some(path) = &path {
            let is_drive_root = path.parent().is_none() && path.has_root();
            if is_drive_root {
                let (total, free) = crate::shell::drives::disk_space(path);
                size = total;
                free_space = free;
            }
        }
        let modified = item.GetFileTime(&PKEY_DateModified).ok().and_then(filetime_to_system);
        let created = item.GetFileTime(&PKEY_DateCreated).ok().and_then(filetime_to_system);
        let kind = item
            .GetString(&PKEY_ItemTypeText)
            .map(take_pwstr)
            .unwrap_or_else(|_| if is_folder { "文件夹".to_string() } else { "文件".to_string() });
        let ext =
            path.as_ref().and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let file_attrs = if hidden { attrs::HIDDEN } else { 0 };
        Some(Entry {
            name,
            path,
            is_folder,
            size,
            modified,
            created,
            kind,
            ext,
            free_space,
            file_attrs,
            pidl: Some(absolute),
        })
    }
}

unsafe fn wire_item(pidl: *mut ITEMIDLIST) -> Option<IShellItem2> {
    unsafe { windows::Win32::UI::Shell::SHCreateItemFromIDList(pidl).ok() }
}

pub fn filetime_to_system(ft: FILETIME) -> Option<SystemTime> {
    let ticks = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
    if ticks < 116_444_736_000_000_000 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_nanos((ticks - 116_444_736_000_000_000) * 100))
}

/// Shell localized "type" name for an extension, e.g. "PNG 图像" / "文本文档".
pub fn type_name_for_ext(ext: &str) -> String {
    if ext.is_empty() {
        return "文件".to_string();
    }
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().unwrap().get(ext) {
        return hit.clone();
    }
    let resolved = resolve_type_name(ext);
    cache.lock().unwrap().insert(ext.to_string(), resolved.clone());
    resolved
}

fn resolve_type_name(ext: &str) -> String {
    let dotted = format!(".{ext}");
    // First ask Shell association API: respects UserChoice and localized MUI FriendlyTypeName.
    if let Some(friendly) = assoc_friendly_name(&dotted)
        && !friendly.is_empty()
    {
        return friendly;
    }
    if let Some(progid) = registry_string(&dotted, "")
        && !progid.is_empty()
        && let Some(desc) = registry_string(&progid, "")
        && !desc.is_empty()
    {
        return desc;
    }
    fallback_kind(ext).to_string()
}

fn assoc_friendly_name(dotted_ext: &str) -> Option<String> {
    let wide = HSTRING::from(dotted_ext);
    let mut len = 0u32;
    let hr = unsafe {
        AssocQueryStringW(ASSOCF_NONE, ASSOCSTR_FRIENDLYDOCNAME, PCWSTR(wide.as_ptr()), PCWSTR::null(), None, &mut len)
    };
    if len == 0 || hr.is_err() {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    let hr = unsafe {
        AssocQueryStringW(
            ASSOCF_NONE,
            ASSOCSTR_FRIENDLYDOCNAME,
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            Some(PWSTR(buf.as_mut_ptr())),
            &mut len,
        )
    };
    if hr.is_err() {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

fn registry_string(subkey: &str, value: &str) -> Option<String> {
    let subkey = HSTRING::from(subkey);
    let value = HSTRING::from(value);
    let mut buf = [0u16; 512];
    let mut size: u32 = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CLASSES_ROOT,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
            Some(&mut size),
        )
    };
    if status.0 != 0 {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..len.min(buf.len())]))
}

pub fn fallback_kind(ext: &str) -> &'static str {
    match ext {
        "txt" | "log" | "md" | "rtf" => "文本文档",
        "pdf" => "PDF 文档",
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "ico" => "图像",
        "mp4" | "mkv" | "avi" | "mov" | "webm" => "视频",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" => "音频",
        "zip" | "7z" | "rar" | "tar" | "gz" => "压缩文件夹",
        "exe" | "msi" => "应用程序",
        "rs" => "Rust 源文件",
        "toml" => "TOML 文档",
        "json" => "JSON 文档",
        _ => "文件",
    }
}
