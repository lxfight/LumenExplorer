//! Pure data model shared by the shell and UI layers.

use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ViewMode {
    #[default]
    Details,
    List,
    Tiles,
    Icons,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    DateModified,
    DateCreated,
    Type,
    Size,
    Extension,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sort {
    pub key: SortKey,
    pub ascending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self { key: SortKey::Name, ascending: true }
    }
}

/// Win32 file attribute bit flags mirrored on `Entry` for visual badges and
/// Windows-accurate hidden/system filtering without extra `stat` syscalls.
pub mod attrs {
    pub const READONLY: u32 = 0x0000_0001;
    pub const HIDDEN: u32 = 0x0000_0002;
    pub const SYSTEM: u32 = 0x0000_0004;
    pub const REPARSE_POINT: u32 = 0x0000_0400;
    pub const OFFLINE: u32 = 0x0000_1000;
    pub const PINNED: u32 = 0x0008_0000;
    pub const UNPINNED: u32 = 0x0010_0000;
    pub const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
}

/// A single displayed entry.
#[derive(Clone)]
pub struct Entry {
    pub name: String,
    /// Absolute filesystem path for real files.
    pub path: Option<PathBuf>,
    pub is_folder: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub kind: String,
    /// Free bytes for drive roots, 0 otherwise.
    pub free_space: u64,
    pub ext: String,
    /// Raw Win32 `FILE_ATTRIBUTE_*` flags (0 for virtual items without FS attrs).
    pub file_attrs: u32,
    /// Absolute shell item id list for items that do not live on the filesystem.
    pub pidl: Option<std::sync::Arc<crate::shell::Pidl>>,
}

impl Entry {
    pub fn is_readonly(&self) -> bool {
        self.file_attrs & attrs::READONLY != 0
    }

    pub fn is_hidden(&self) -> bool {
        self.file_attrs & attrs::HIDDEN != 0
    }

    pub fn is_system(&self) -> bool {
        self.file_attrs & attrs::SYSTEM != 0
    }

    pub fn is_symlink_or_shortcut(&self) -> bool {
        self.file_attrs & attrs::REPARSE_POINT != 0 || self.ext.eq_ignore_ascii_case("lnk")
    }

    /// Cloud-only placeholder (e.g. OneDrive Files On-Demand not yet hydrated).
    pub fn is_cloud_only(&self) -> bool {
        self.file_attrs & (attrs::OFFLINE | attrs::RECALL_ON_DATA_ACCESS | attrs::UNPINNED) != 0
    }

    /// Explicitly pinned locally (e.g. OneDrive "Always keep on this device").
    pub fn is_cloud_pinned(&self) -> bool {
        self.file_attrs & attrs::PINNED != 0
    }

    /// Cache-busting stamp for thumbnails so overwritten files refresh.
    pub fn thumb_stamp(&self) -> u64 {
        let secs =
            self.modified.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
        secs ^ self.size.rotate_left(17)
    }
}

/// Builds a binary Windows locale sort key (`LCMAP_SORTKEY | SORT_DIGITSASNUMBERS | NORM_IGNORECASE`).
///
/// Plain byte comparison (`key_a.cmp(&key_b)`) matches Windows Explorer's exact
/// ordering — including **Chinese Pinyin** sorting on zh-CN systems and natural
/// numeric runs (`file2` < `file10`) — with O(N) Win32 calls instead of O(N log N).
pub fn locale_sort_key(text: &str) -> Vec<u8> {
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::Globalization::{LCMAP_SORTKEY, LCMapStringEx, NORM_IGNORECASE, SORT_DIGITSASNUMBERS};
    use windows::core::PCWSTR;
    if text.is_empty() {
        return Vec::new();
    }
    let wide: Vec<u16> = text.encode_utf16().collect();
    // `PCWSTR::null()` is Win32 `LOCALE_NAME_USER_DEFAULT`.
    let flags = LCMAP_SORTKEY | SORT_DIGITSASNUMBERS.0 | NORM_IGNORECASE.0;
    let needed = unsafe { LCMapStringEx(PCWSTR::null(), flags, &wide, None, None, None, LPARAM(0)) };
    if needed <= 0 {
        return text.to_lowercase().into_bytes();
    }
    // For `LCMAP_SORTKEY`, Win32 interprets `cchDest` as byte count, and
    // `windows-rs` passes `buf.len()` as `cchDest`, so allocate `byte_len` u16s.
    let byte_len = needed as usize;
    let mut buf = vec![0u16; byte_len];
    let written = unsafe { LCMapStringEx(PCWSTR::null(), flags, &wide, Some(&mut buf), None, None, LPARAM(0)) };
    if written <= 0 {
        return text.to_lowercase().into_bytes();
    }
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), (written as usize).min(byte_len)) };
    bytes.to_vec()
}

/// Human readable byte size, Explorer style ("1.24 MB").
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.2} {}", UNITS[unit])
}

pub fn format_time(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return String::new() };
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    crate::core::time::format_local(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_sort_key_orders_digits_naturally_and_case_insensitively() {
        let mut names = ["file10.txt", "File2.txt", "file1.txt", "alpha.txt"];
        names.sort_by_key(|name| locale_sort_key(name));
        assert_eq!(names, ["alpha.txt", "file1.txt", "File2.txt", "file10.txt"]);

        // On zh-CN Windows, Chinese names sort by Pinyin rather than raw Unicode codepoints.
        // Note: Unicode order is 博(535A) < 张(5F20) < 钟(949F) < 阿(963F), whereas Pinyin is 阿(a) < 博(b) < 张(z).
        let mut cn = ["张.txt", "阿.txt", "博.txt"];
        cn.sort_by_key(|name| locale_sort_key(name));
        assert_eq!(cn, ["阿.txt", "博.txt", "张.txt"]);
    }
}
