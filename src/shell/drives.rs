//! Logical drives and quick-access known folders.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW, GetVolumeInformationW,
};
use windows::core::{HSTRING, PCWSTR};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    Cdrom,
    Ram,
    Unknown,
}

impl DriveKind {
    pub fn label(self) -> &'static str {
        match self {
            DriveKind::Fixed => "本地磁盘",
            DriveKind::Removable => "可移动磁盘",
            DriveKind::Network => "网络驱动器",
            DriveKind::Cdrom => "光盘驱动器",
            DriveKind::Ram => "RAM 磁盘",
            DriveKind::Unknown => "驱动器",
        }
    }
}

#[derive(Clone)]
pub struct Drive {
    pub root: String,
    pub letter: Option<char>,
    pub label: String,
    pub filesystem: String,
    pub total: u64,
    pub free: u64,
    pub kind: DriveKind,
}

impl Drive {
    pub fn display_name(&self) -> String {
        let letter = self.letter.map(|c| format!("{c}:")).unwrap_or_default();
        if self.label.is_empty() {
            format!("{} ({letter})", self.kind.label())
        } else {
            format!("{} ({letter})", self.label)
        }
    }

    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        ((self.total - self.free) as f64 / self.total as f64) as f32
    }

    pub fn path(&self) -> PathBuf {
        PathBuf::from(&self.root)
    }
}

/// Total and free bytes for a filesystem path; (0, 0) when unavailable.
pub fn disk_space(path: &Path) -> (u64, u64) {
    let wide = HSTRING::from(path.as_os_str().to_string_lossy().as_ref());
    let mut free = 0u64;
    let mut total = 0u64;
    let ok = unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut free), Some(&mut total), None) };
    if ok.is_err() {
        return (0, 0);
    }
    (total, free)
}

static DRIVE_CACHE: OnceLock<Mutex<Option<Vec<Drive>>>> = OnceLock::new();

fn cache() -> &'static Mutex<Option<Vec<Drive>>> {
    DRIVE_CACHE.get_or_init(|| Mutex::new(None))
}

/// Cached volume list; returns immediately without blocking on offline drives.
/// Call `spawn_refresh()` to probe volumes on a background thread.
pub fn cached() -> Vec<Drive> {
    cache().lock().unwrap().clone().unwrap_or_default()
}

/// Re-enumerates every volume on a background thread and notifies the UI when
/// ready so an offline network drive never stalls startup, F5, or USB hotplug.
pub fn spawn_refresh() -> crossbeam_channel::Receiver<Vec<Drive>> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .name("lumen-drives".to_string())
        .spawn(move || {
            let enumerated = enumerate_drives();
            *cache().lock().unwrap() = Some(enumerated.clone());
            let _ = tx.try_send(enumerated);
            crate::platform::wake();
        })
        .expect("spawn drives worker");
    rx
}

fn enumerate_drives() -> Vec<Drive> {
    let mut buffer = [0u16; 512];
    let len = unsafe { GetLogicalDriveStringsW(Some(&mut buffer)) } as usize;
    if len == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < len {
        let rest = &buffer[start..len];
        let Some(end) = rest.iter().position(|&c| c == 0) else { break };
        let root = String::from_utf16_lossy(&rest[..end]);
        start += end + 1;
        if root.is_empty() {
            continue;
        }
        out.push(probe_drive(&root));
    }
    out
}

fn probe_drive(root: &str) -> Drive {
    let wide = HSTRING::from(root);
    let kind = match unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) } {
        2 => DriveKind::Removable,
        3 => DriveKind::Fixed,
        4 => DriveKind::Network,
        5 => DriveKind::Cdrom,
        6 => DriveKind::Ram,
        _ => DriveKind::Unknown,
    };
    let mut label = [0u16; 256];
    let mut filesystem = [0u16; 64];
    let _ = unsafe {
        GetVolumeInformationW(PCWSTR(wide.as_ptr()), Some(&mut label), None, None, None, Some(&mut filesystem))
    };
    let mut free = 0u64;
    let mut total = 0u64;
    let _ = unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut free), Some(&mut total), None) };
    let trim = |buf: &[u16]| {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    };
    Drive {
        root: root.to_string(),
        letter: root.chars().next(),
        label: trim(&label),
        filesystem: trim(&filesystem),
        total,
        free,
        kind,
    }
}
