//! Windows `ShellNew` registry template discovery and file creation.
//!
//! Scans extension and ProgID `ShellNew` keys on a background thread so the
//! Fluent 2 "新建" submenu lists the user's real installed document templates
//! (Word, Excel, PowerPoint, Markdown, ZIP, etc.) without stalling the UI.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CLASSES_ROOT, KEY_READ, RRF_RT_ANY, RRF_RT_REG_BINARY, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW,
    RegGetValueW, RegOpenKeyExW,
};
use windows::core::{HSTRING, PCWSTR};

#[derive(Clone, Debug)]
pub enum TemplatePayload {
    Empty,
    Bytes(Vec<u8>),
    File(PathBuf),
}

#[derive(Clone, Debug)]
pub struct NewTemplate {
    pub ext: String,
    pub label: String,
    pub payload: TemplatePayload,
}

static TEMPLATES: OnceLock<Mutex<Option<Vec<NewTemplate>>>> = OnceLock::new();
static LOADING: OnceLock<()> = OnceLock::new();

/// Returns discovered `ShellNew` templates if ready; starts background scan on
/// the first call so it never blocks the UI thread.
pub fn templates() -> Vec<NewTemplate> {
    let store = TEMPLATES.get_or_init(|| Mutex::new(None));
    if let Some(list) = store.lock().unwrap().as_ref() {
        return list.clone();
    }
    LOADING.get_or_init(|| {
        std::thread::Builder::new()
            .name("lumen-shellnew".to_string())
            .spawn(move || {
                let discovered = scan_shell_new();
                if let Some(s) = TEMPLATES.get() {
                    *s.lock().unwrap() = Some(discovered);
                    crate::platform::wake();
                }
            })
            .expect("spawn shellnew scanner");
    });
    Vec::new()
}

/// Creates a uniquely named file in `dir` from a `NewTemplate`.
pub fn create_from_template(dir: &Path, tpl: &NewTemplate) -> Result<(PathBuf, String)> {
    let base = format!("新建{}", tpl.label);
    let name = crate::app::unique_name(dir, &base, &tpl.ext);
    let dest = dir.join(&name);
    match &tpl.payload {
        TemplatePayload::Empty => {
            crate::shell::ops::new_file(dir, &name, b"")?;
        }
        TemplatePayload::Bytes(data) => {
            crate::shell::ops::new_file(dir, &name, data)?;
        }
        TemplatePayload::File(src) => {
            let mut source = std::fs::File::open(src).with_context(|| format!("打开模板失败: {}", src.display()))?;
            let mut destination = std::fs::OpenOptions::new().write(true).create_new(true).open(&dest)?;
            if let Err(err) = std::io::copy(&mut source, &mut destination) {
                drop(destination);
                let _ = std::fs::remove_file(&dest);
                return Err(err).with_context(|| format!("复制模板失败: {}", src.display()));
            }
        }
    }
    Ok((dest, name))
}

/// Common extensions checked first in priority order (fast, covers 95% of real
/// user templates without enumerating thousands of obscure HKCR entries).
const PRIORITY_EXTS: [&str; 16] =
    ["docx", "xlsx", "pptx", "md", "rtf", "bmp", "png", "zip", "json", "py", "rs", "html", "css", "js", "xml", "ps1"];

fn scan_shell_new() -> Vec<NewTemplate> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // Skip .txt (already pinned explicitly at top of "新建" menu) and .lnk.
    seen.insert("txt".to_string());
    seen.insert("lnk".to_string());

    for ext in PRIORITY_EXTS {
        if let Some(tpl) = probe_ext(ext) {
            seen.insert(ext.to_string());
            out.push(tpl);
        }
    }

    // Also scan top-level HKCR extensions with a ShellNew subkey (bounded).
    let mut index = 0u32;
    let mut name_buf = [0u16; 128];
    while out.len() < 12 && index < 16_384 {
        let mut len = name_buf.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                HKEY_CLASSES_ROOT,
                index,
                Some(windows::core::PWSTR(name_buf.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
        };
        if status.0 != 0 {
            break;
        }
        index += 1;
        let key_name = String::from_utf16_lossy(&name_buf[..len as usize]);
        if !key_name.starts_with('.') {
            continue;
        }
        let ext = key_name[1..].to_ascii_lowercase();
        if ext.is_empty() || ext.len() > 10 || seen.contains(&ext) {
            continue;
        }
        if let Some(tpl) = probe_ext(&ext) {
            seen.insert(ext);
            out.push(tpl);
        }
    }
    out
}

fn probe_ext(ext: &str) -> Option<NewTemplate> {
    probe_ext_at(HKEY_CLASSES_ROOT, ext)
}

fn probe_ext_at(root: HKEY, ext: &str) -> Option<NewTemplate> {
    let extension = format!(".{ext}");
    // WPS and Office register templates below the extension's ProgID.
    let mut candidates = vec![format!("{extension}\\ShellNew")];
    let extension_wide = HSTRING::from(extension.as_str());
    let mut key = HKEY::default();
    if unsafe { RegOpenKeyExW(root, &extension_wide, None, KEY_READ, &mut key) }.is_ok() {
        if let Some(progid) = read_reg_string(key, "")
            && !progid.is_empty()
        {
            candidates.push(format!("{extension}\\{progid}\\ShellNew"));
            candidates.push(format!("{progid}\\ShellNew"));
        }
        let _ = unsafe { RegCloseKey(key) };
    }
    candidates.into_iter().find_map(|subkey| probe_template(root, &subkey, ext))
}

fn probe_template(root: HKEY, subkey: &str, ext: &str) -> Option<NewTemplate> {
    let wide = HSTRING::from(subkey);
    let mut hkey = Default::default();
    if unsafe { RegOpenKeyExW(root, PCWSTR(wide.as_ptr()), None, KEY_READ, &mut hkey) }.0 != 0 {
        return None;
    }
    let payload = if reg_value_exists(hkey, "NullFile") {
        Some(TemplatePayload::Empty)
    } else if let Some(file_name) = read_reg_string(hkey, "FileName") {
        resolve_template_file(&file_name).map(TemplatePayload::File)
    } else {
        read_reg_binary(hkey, "Data").map(TemplatePayload::Bytes)
    };
    let _ = unsafe { RegCloseKey(hkey) };
    let payload = payload?;
    let label = crate::shell::enumerate::type_name_for_ext(ext);
    Some(NewTemplate { ext: ext.to_string(), label, payload })
}

fn resolve_template_file(name: &str) -> Option<PathBuf> {
    let expanded = crate::shell::location::expand_env(name);
    let name = expanded.trim_matches('"');
    let direct = PathBuf::from(name);
    if direct.is_absolute() && direct.exists() {
        return Some(direct);
    }
    if let Ok(windir) = std::env::var("WINDIR") {
        let p = Path::new(&windir).join("ShellNew").join(name);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        let p = Path::new(&appdata).join("Microsoft\\Windows\\Templates").join(name);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(programdata) = std::env::var("PROGRAMDATA") {
        let p = Path::new(&programdata).join("Microsoft\\Windows\\Templates").join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn reg_value_exists(hkey: HKEY, name: &str) -> bool {
    let name = HSTRING::from(name);
    unsafe { RegGetValueW(hkey, PCWSTR::null(), &name, RRF_RT_ANY, None, None, None) }.is_ok()
}

fn read_reg_string(hkey: windows::Win32::System::Registry::HKEY, name: &str) -> Option<String> {
    let name_w = HSTRING::from(name);
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            hkey,
            PCWSTR::null(),
            PCWSTR(name_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            Some(&mut size),
        )
    };
    if status.0 != 0 {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..len.min(buf.len())]))
}

fn read_reg_binary(hkey: windows::Win32::System::Registry::HKEY, name: &str) -> Option<Vec<u8>> {
    let name_w = HSTRING::from(name);
    let mut size = 0u32;
    let status = unsafe {
        RegGetValueW(hkey, PCWSTR::null(), PCWSTR(name_w.as_ptr()), RRF_RT_REG_BINARY, None, None, Some(&mut size))
    };
    if status.0 != 0 || size == 0 || size > 64 * 1024 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let status = unsafe {
        RegGetValueW(
            hkey,
            PCWSTR::null(),
            PCWSTR(name_w.as_ptr()),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            Some(&mut size),
        )
    };
    (status.0 == 0).then_some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, KEY_ALL_ACCESS, REG_NONE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCreateKeyExW, RegDeleteTreeW,
        RegSetValueExW,
    };

    struct RegistryFixture {
        path: HSTRING,
        key: HKEY,
    }

    impl RegistryFixture {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let path = HSTRING::from(format!("Software\\LumenTests\\ShellNew-{stamp}"));
            let key = Self::create(HKEY_CURRENT_USER, &path);
            Self { path, key }
        }

        fn create(root: HKEY, path: &HSTRING) -> HKEY {
            let mut key = HKEY::default();
            unsafe {
                RegCreateKeyExW(
                    root,
                    path,
                    None,
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_ALL_ACCESS,
                    None,
                    &mut key,
                    None,
                )
            }
            .ok()
            .unwrap();
            key
        }

        fn set_string(&self, path: &str, name: &str, value: &str) {
            let key = Self::create(self.key, &HSTRING::from(path));
            let bytes: Vec<u8> = value.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect();
            unsafe {
                RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(&bytes)).ok().unwrap();
                RegCloseKey(key).ok().unwrap();
            }
        }
    }

    impl Drop for RegistryFixture {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.key);
                let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &self.path);
            }
        }
    }

    #[test]
    fn nested_progid_nullfile_and_file_templates_create_documents() {
        let registry = RegistryFixture::new();
        registry.set_string(".docx", "", "Test.Document");
        let key = RegistryFixture::create(registry.key, &HSTRING::from(".docx\\Test.Document\\ShellNew"));
        unsafe {
            RegSetValueExW(key, windows::core::w!("NullFile"), None, REG_NONE, Some(&[])).ok().unwrap();
            RegCloseKey(key).ok().unwrap();
        }
        let empty = probe_ext_at(registry.key, "docx").expect("nested REG_NONE NullFile");
        assert!(matches!(empty.payload, TemplatePayload::Empty));
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("tmp")
            .join(registry.path.to_string().replace('\\', "-"));
        std::fs::create_dir_all(&root).unwrap();
        let (created, _) = create_from_template(&root, &empty).unwrap();
        assert!(created.is_file());
        assert_eq!(created.extension().unwrap(), "docx");

        let template_path = root.join("template.xlsx");
        std::fs::write(&template_path, b"template contents").unwrap();
        registry.set_string(".xlsx", "", "Test.Sheet");
        registry.set_string("Test.Sheet\\ShellNew", "FileName", template_path.to_str().unwrap());
        let template = probe_ext_at(registry.key, "xlsx").expect("ProgID FileName template");
        let (created, _) = create_from_template(&root, &template).unwrap();
        assert_eq!(std::fs::read(created).unwrap(), b"template contents");
        std::fs::remove_dir_all(root).unwrap();
    }
}
