//! Read-only Windows desktop integration probes & coexistence helpers:
//! - Read-only `IShellWindows` COM query to import currently open Windows File
//!   Explorer folders into Lumen tabs with one click (`Ctrl+Shift+O`).
//! - Read-only detection of installer-managed integrations (install mode,
//!   "Open with Lumen" folder context menu, and auto-start entry).
//!
//! `lumen.exe` itself never writes Windows Shell registry keys or persistence
//! files; optional shell integration is cleanly installed and removed by the
//! Windows Installer (`LumenSetup`).

use std::path::PathBuf;

use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegGetValueW, RegOpenKeyExW,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{IShellWindows, IWebBrowserApp, ShellWindows};
use windows::core::{HSTRING, Interface, PCWSTR};

/// Returns deduplicated local directory paths currently open in Windows File
/// Explorer (`explorer.exe`) windows via read-only `IShellWindows` COM.
pub fn list_open_explorer_folders() -> Vec<PathBuf> {
    let Ok(windows) = (unsafe { CoCreateInstance::<_, IShellWindows>(&ShellWindows, None, CLSCTX_ALL) }) else {
        return Vec::new();
    };
    let count = unsafe { windows.Count() }.unwrap_or(0);
    let mut dirs: Vec<PathBuf> = Vec::new();
    for i in 0..count {
        let idx = VARIANT::from(i);
        let Ok(disp) = (unsafe { windows.Item(&idx) }) else { continue };
        let Ok(browser) = disp.cast::<IWebBrowserApp>() else { continue };
        let Ok(url) = (unsafe { browser.LocationURL() }) else { continue };
        let url_str = url.to_string();
        if let Some(path) = decode_file_url(&url_str)
            && path.is_dir()
            && !dirs.iter().any(|p| p.as_os_str().eq_ignore_ascii_case(path.as_os_str()))
        {
            dirs.push(path);
        }
    }
    dirs
}

/// Converts a Windows Shell `file:///C:/Users/...` or `file://server/share` URL
/// (percent-encoded UTF-8) into a local `PathBuf`.
pub fn decode_file_url(url: &str) -> Option<PathBuf> {
    let rest = if let Some(local) = url.strip_prefix("file:///") {
        local
    } else {
        let unc = url.strip_prefix("file://")?;
        // UNC path: file://server/share -> \\server\share
        let decoded = percent_decode(unc)?;
        let normalized = decoded.replace('/', "\\");
        return Some(PathBuf::from(format!(r"\\{normalized}")));
    };
    let decoded = percent_decode(rest)?;
    let normalized = decoded.replace('/', "\\");
    if normalized.is_empty() {
        return None;
    }
    Some(PathBuf::from(normalized))
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_val(bytes[i + 1])?;
            let lo = hex_val(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Returns the current user's Windows Startup folder (`shell:startup`) if available.
pub fn startup_folder() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let dir = PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\Startup");
    dir.is_dir().then_some(dir)
}

/// Returns `true` when running from an Inno Setup installation (uninstaller present beside `lumen.exe`).
pub fn is_installed() -> bool {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("unins000.exe").is_file())).unwrap_or(false)
}

/// Read-only check whether the installer-managed "Open with Lumen" folder context menu is registered.
pub fn has_context_menu() -> bool {
    const SUBKEY: &str = r"Software\Classes\Directory\shell\Lumen";
    reg_key_exists(HKEY_CURRENT_USER, SUBKEY) || reg_key_exists(HKEY_LOCAL_MACHINE, SUBKEY)
}

/// Read-only check whether Lumen is configured to auto-start (via installer `Run` entry or `shell:startup`).
pub fn is_auto_start() -> bool {
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    if reg_sz_exists(HKEY_CURRENT_USER, RUN_KEY, "Lumen") || reg_sz_exists(HKEY_LOCAL_MACHINE, RUN_KEY, "Lumen") {
        return true;
    }
    startup_folder().is_some_and(|dir| {
        std::fs::read_dir(dir).ok().into_iter().flatten().flatten().any(|e| {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            name.contains("lumen") && name.ends_with(".lnk")
        })
    })
}

fn reg_key_exists(root: HKEY, subkey: &str) -> bool {
    let wide = HSTRING::from(subkey);
    let mut handle = HKEY::default();
    let ok = unsafe { RegOpenKeyExW(root, PCWSTR(wide.as_ptr()), Some(0), KEY_READ, &mut handle) }.is_ok();
    if ok {
        let _ = unsafe { RegCloseKey(handle) };
    }
    ok
}

fn reg_sz_exists(root: HKEY, subkey: &str, value: &str) -> bool {
    let sub = HSTRING::from(subkey);
    let val = HSTRING::from(value);
    let mut size: u32 = 0;
    unsafe {
        RegGetValueW(root, PCWSTR(sub.as_ptr()), PCWSTR(val.as_ptr()), RRF_RT_REG_SZ, None, None, Some(&mut size))
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_local_and_unc_file_urls() {
        assert_eq!(
            decode_file_url("file:///C:/Users/Test%20User/Docs"),
            Some(PathBuf::from(r"C:\Users\Test User\Docs"))
        );
        assert_eq!(decode_file_url("file:///D:/%E6%96%87%E6%A1%A3/Lumen"), Some(PathBuf::from(r"D:\文档\Lumen")));
        assert_eq!(decode_file_url("file://nas/shared/photos"), Some(PathBuf::from(r"\\nas\shared\photos")));
        assert_eq!(decode_file_url("https://example.com"), None);
    }
}
