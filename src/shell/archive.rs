//! Asynchronous archive compression & extraction backed by Windows 11's
//! built-in `tar.exe` (libarchive: supports .zip, .7z, .rar, .tar, .gz, .tgz).

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

use crossbeam_channel::Receiver;
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

pub fn is_archive_ext(ext: &str) -> bool {
    matches!(ext, "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst")
}

pub struct ArchiveOutcome {
    pub message: String,
    pub select_name: Option<String>,
    pub error: Option<String>,
}

/// Compresses `items` (all residing in `dest_dir`) into a uniquely named `.zip`
/// file in `dest_dir` on a background worker thread.
pub fn spawn_compress_zip(items: Vec<PathBuf>, dest_dir: PathBuf) -> Receiver<ArchiveOutcome> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .name("lumen-archive".to_string())
        .spawn(move || {
            let outcome = compress_zip_sync(&items, &dest_dir);
            let _ = tx.try_send(outcome);
            crate::platform::wake();
        })
        .expect("spawn archive compress worker");
    rx
}

/// Extracts `archive` into `dest_dir` (or a new subfolder named after the
/// archive stem when `into_subfolder` is true) on a background worker thread.
pub fn spawn_extract(archive: PathBuf, dest_dir: PathBuf, into_subfolder: bool) -> Receiver<ArchiveOutcome> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .name("lumen-extract".to_string())
        .spawn(move || {
            let outcome = extract_sync(&archive, &dest_dir, into_subfolder);
            let _ = tx.try_send(outcome);
            crate::platform::wake();
        })
        .expect("spawn archive extract worker");
    rx
}

fn compress_zip_sync(items: &[PathBuf], dest_dir: &Path) -> ArchiveOutcome {
    if items.is_empty() {
        return ArchiveOutcome {
            message: String::new(), select_name: None, error: Some("没有可压缩的项目".into())
        };
    }
    let base = if items.len() == 1 {
        items[0]
            .file_stem()
            .or_else(|| items[0].file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "归档".to_string())
    } else {
        "归档".to_string()
    };
    let zip_name = crate::app::unique_name(dest_dir, &base, "zip");
    let zip_path = dest_dir.join(&zip_name);

    let mut cmd = std::process::Command::new("tar.exe");
    cmd.args(["-a", "-c", "-f"]).arg(&zip_path).arg("-C").arg(dest_dir);
    let mut added = 0usize;
    for item in items {
        if let Some(name) = item.file_name() {
            cmd.arg(name);
            added += 1;
        }
    }
    if added == 0 {
        return ArchiveOutcome {
            message: String::new(), select_name: None, error: Some("无法解析压缩路径".into())
        };
    }
    match cmd.creation_flags(CREATE_NO_WINDOW.0).output() {
        Ok(out) if out.status.success() => {
            ArchiveOutcome { message: format!("已压缩为 {zip_name}"), select_name: Some(zip_name), error: None }
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            ArchiveOutcome {
                message: String::new(),
                select_name: None,
                error: Some(if stderr.is_empty() { "压缩失败".into() } else { format!("压缩失败: {stderr}") }),
            }
        }
        Err(err) => {
            ArchiveOutcome { message: String::new(), select_name: None, error: Some(format!("压缩失败: {err}")) }
        }
    }
}

fn extract_sync(archive: &Path, dest_dir: &Path, into_subfolder: bool) -> ArchiveOutcome {
    let stem = archive
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "已解压".to_string());
    let (target_dir, select_name) = if into_subfolder {
        let folder_name = crate::app::unique_name(dest_dir, &stem, "");
        let target = dest_dir.join(&folder_name);
        if let Err(err) = std::fs::create_dir_all(&target) {
            return ArchiveOutcome {
                message: String::new(),
                select_name: None,
                error: Some(format!("创建解压目录失败: {err}")),
            };
        }
        (target, Some(folder_name))
    } else {
        (dest_dir.to_path_buf(), None)
    };

    let output = std::process::Command::new("tar.exe")
        .args(["-x", "-f"])
        .arg(archive)
        .arg("-C")
        .arg(&target_dir)
        .creation_flags(CREATE_NO_WINDOW.0)
        .output();
    match output {
        Ok(out) if out.status.success() => ArchiveOutcome {
            message: format!("已解压到 {}", target_dir.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()),
            select_name,
            error: None,
        },
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            ArchiveOutcome {
                message: String::new(),
                select_name: None,
                error: Some(if stderr.is_empty() {
                    "解压失败（可能需要第三方解压工具）".into()
                } else {
                    format!("解压失败: {stderr}")
                }),
            }
        }
        Err(err) => {
            ArchiveOutcome { message: String::new(), select_name: None, error: Some(format!("解压失败: {err}")) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_supported_archive_extensions() {
        for ext in ["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "zst"] {
            assert!(is_archive_ext(ext), "expected {ext} to be recognized");
        }
        assert!(!is_archive_ext("txt"));
        assert!(!is_archive_ext("exe"));
        assert!(!is_archive_ext(""));
    }

    #[test]
    fn empty_compress_returns_error() {
        let outcome = compress_zip_sync(&[], Path::new("."));
        assert!(outcome.error.is_some());
        assert!(outcome.select_name.is_none());
    }
}
