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

fn system_tar_exe() -> PathBuf {
    if let Some(sys_root) = std::env::var_os("SystemRoot") {
        let candidate = PathBuf::from(sys_root).join("System32").join("tar.exe");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("tar.exe")
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

    // Group items by parent directory so cross-folder selections (e.g. search results)
    // archive cleanly via `-C <parent>` rather than assuming direct children of `dest_dir`.
    let mut groups: Vec<(PathBuf, Vec<std::ffi::OsString>)> = Vec::new();
    for item in items {
        let Some(name) = item.file_name() else { continue };
        let parent = item.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(dest_dir);
        if let Some((_, list)) =
            groups.iter_mut().find(|(dir, _)| dir.as_os_str().eq_ignore_ascii_case(parent.as_os_str()))
        {
            list.push(name.to_os_string());
        } else {
            groups.push((parent.to_path_buf(), vec![name.to_os_string()]));
        }
    }
    if groups.is_empty() {
        return ArchiveOutcome {
            message: String::new(), select_name: None, error: Some("无法解析压缩路径".into())
        };
    }

    let mut cmd = std::process::Command::new(system_tar_exe());
    cmd.args(["-a", "-c", "-f"]).arg(&zip_path);
    let single_dir = groups.len() == 1;
    for (parent, names) in &groups {
        cmd.arg("-C").arg(parent);
        // For a single directory, pass `--` before member names to terminate option parsing.
        // For multiple directories, bsdtar requires `-C <parent>` between positional groups
        // (and rejects `--` after positional args), so we neutralize leading `-`/`@` via `./`.
        if single_dir {
            cmd.arg("--");
        }
        for name in names {
            let text = name.to_string_lossy();
            // Neutralize leading `-` (flag injection) and `@` (bsdtar archive-inclusion).
            if text.starts_with('-') || text.starts_with('@') {
                let mut safe = std::ffi::OsString::from("./");
                safe.push(name);
                cmd.arg(safe);
            } else {
                cmd.arg(name);
            }
        }
    }
    match cmd.creation_flags(CREATE_NO_WINDOW.0).output() {
        Ok(out) if out.status.success() => {
            ArchiveOutcome { message: format!("已压缩为 {zip_name}"), select_name: Some(zip_name), error: None }
        }
        Ok(out) => {
            let _ = std::fs::remove_file(&zip_path);
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            ArchiveOutcome {
                message: String::new(),
                select_name: None,
                error: Some(if stderr.is_empty() { "压缩失败".into() } else { format!("压缩失败: {stderr}") }),
            }
        }
        Err(err) => {
            let _ = std::fs::remove_file(&zip_path);
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
    let (target_dir, select_name, created_dir) = if into_subfolder {
        let folder_name = crate::app::unique_name(dest_dir, &stem, "");
        let target = dest_dir.join(&folder_name);
        if let Err(err) = std::fs::create_dir_all(&target) {
            return ArchiveOutcome {
                message: String::new(),
                select_name: None,
                error: Some(format!("创建解压目录失败: {err}")),
            };
        }
        (target, Some(folder_name), true)
    } else {
        let existed = dest_dir.exists();
        if !existed && let Err(err) = std::fs::create_dir_all(dest_dir) {
            return ArchiveOutcome {
                message: String::new(),
                select_name: None,
                error: Some(format!("创建解压目录失败: {err}")),
            };
        }
        (dest_dir.to_path_buf(), None, !existed)
    };

    let output = std::process::Command::new(system_tar_exe())
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
            if created_dir {
                let _ = std::fs::remove_dir(&target_dir);
            }
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
            if created_dir {
                let _ = std::fs::remove_dir(&target_dir);
            }
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

    #[test]
    fn compress_handles_cross_directory_and_special_leading_chars() {
        let root = std::env::temp_dir().join(format!("lumen-archive-multi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir_a = root.join("dir_a");
        let dir_b = root.join("dir_b");
        let out_dir = root.join("out");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        std::fs::create_dir_all(&out_dir).unwrap();

        let f_dash = dir_a.join("-dash.txt");
        let f_at = dir_a.join("@at.txt");
        let f_plain = dir_a.join("plain.txt");
        let f_sib = dir_b.join("sibling.txt");
        let f_sib_dash = dir_b.join("-other.txt");
        std::fs::write(&f_dash, b"dash-body").unwrap();
        std::fs::write(&f_at, b"at-body").unwrap();
        std::fs::write(&f_plain, b"plain-body").unwrap();
        std::fs::write(&f_sib, b"sib-body").unwrap();
        std::fs::write(&f_sib_dash, b"other-body").unwrap();

        let items = vec![f_dash, f_at, f_plain, f_sib, f_sib_dash];
        let outcome = compress_zip_sync(&items, &out_dir);
        assert!(outcome.error.is_none(), "compress failed: {:?}", outcome.error);
        let zip_name = outcome.select_name.expect("zip name");
        let zip_path = out_dir.join(&zip_name);
        assert!(zip_path.is_file());

        let extracted = extract_sync(&zip_path, &out_dir, true);
        assert!(extracted.error.is_none(), "extract failed: {:?}", extracted.error);
        let sub = out_dir.join(extracted.select_name.expect("subfolder name"));
        assert_eq!(std::fs::read(sub.join("-dash.txt")).unwrap(), b"dash-body");
        assert_eq!(std::fs::read(sub.join("@at.txt")).unwrap(), b"at-body");
        assert_eq!(std::fs::read(sub.join("plain.txt")).unwrap(), b"plain-body");
        assert_eq!(std::fs::read(sub.join("sibling.txt")).unwrap(), b"sib-body");
        assert_eq!(std::fs::read(sub.join("-other.txt")).unwrap(), b"other-body");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_extract_cleans_up_newly_created_empty_dir() {
        let root = std::env::temp_dir().join(format!("lumen-archive-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let bad = root.join("corrupt.zip");
        std::fs::write(&bad, b"definitely-not-a-valid-zip").unwrap();

        let sub_out = extract_sync(&bad, &root, true);
        assert!(sub_out.error.is_some());
        assert!(!root.join("corrupt").exists(), "failed subfolder extract must not leave empty dir");

        let fresh_dest = root.join("fresh_target");
        let flat_out = extract_sync(&bad, &fresh_dest, false);
        assert!(flat_out.error.is_some());
        assert!(!fresh_dest.exists(), "failed extract into newly created dest_dir must remove empty dir");

        let _ = std::fs::remove_dir_all(&root);
    }
}
