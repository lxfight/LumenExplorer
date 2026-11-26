//! Explorer-compatible CLI parsing and single-instance Named Pipe IPC.
//!
//! When Windows, Chrome ("Show in Folder"), VS Code, or `Win+E` invokes
//! `lumen.exe <path>` or `lumen.exe /select,<file>`, a secondary process sends
//! the parsed `LaunchCommand` over `\\.\pipe\LumenExplorer.IPC.v1` to the
//! primary instance (opening a new tab in < 15ms) unless `--new-window` is set.

use std::ffi::c_void;
use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_INBOUND, ReadFile, WriteFile,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
    WaitNamedPipeW,
};
use windows::core::{HSTRING, PCWSTR};

const PIPE_NAME: &str = r"\\.\pipe\LumenExplorer.IPC.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaunchTarget {
    /// No path passed: restore/focus window (or open default home).
    Default,
    /// Open a filesystem folder.
    OpenDir(PathBuf),
    /// Open parent folder and highlight + scroll to the file (`/select,<path>`).
    SelectFile(PathBuf),
    /// Open a shell parsing name (`::{GUID}`, `shell:...`, or raw path string).
    OpenShell(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliOptions {
    pub target: LaunchTarget,
    pub new_window: bool,
    pub start_background: bool,
}

/// Parses Windows Explorer–compatible command-line flags:
/// - `<dir_or_file>`
/// - `/select,<path>` or `/select, <path>`
/// - `/e,<path>`, `/root,<path>`, `/n`
/// - `--new-window`, `--background`
pub fn parse_args<I, S>(args: I) -> CliOptions
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let raw: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    let mut new_window = false;
    let mut start_background = false;
    let mut select_next = false;
    let mut target = LaunchTarget::Default;

    for arg in raw {
        let trimmed = arg.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower == "--new-window" || lower == "/n" {
            new_window = true;
            continue;
        }
        if lower == "--background" || lower == "--tray" {
            start_background = true;
            continue;
        }
        if select_next {
            select_next = false;
            target = LaunchTarget::SelectFile(PathBuf::from(trimmed.trim_matches('"')));
            continue;
        }
        if lower == "/select" || lower == "/select," {
            select_next = true;
            continue;
        }
        if let Some(rest) = strip_prefix_ci(trimmed, "/select,") {
            let p = rest.trim().trim_matches('"');
            if !p.is_empty() {
                target = LaunchTarget::SelectFile(PathBuf::from(p));
            }
            continue;
        }
        if let Some(rest) = strip_prefix_ci(trimmed, "/e,")
            .or_else(|| strip_prefix_ci(trimmed, "/root,"))
            .or_else(|| strip_prefix_ci(trimmed, "/n,"))
        {
            if lower.starts_with("/n,") {
                new_window = true;
            }
            let p = rest.trim().trim_matches('"');
            if !p.is_empty() {
                target = classify_target(p);
            }
            continue;
        }
        if lower == "/e" || lower == "/root" {
            continue;
        }
        target = classify_target(trimmed.trim_matches('"'));
    }

    CliOptions { target, new_window, start_background }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn classify_target(raw: &str) -> LaunchTarget {
    let expanded = crate::shell::location::expand_env(raw);
    if expanded.starts_with("::{") || expanded.to_ascii_lowercase().starts_with("shell:") {
        return LaunchTarget::OpenShell(expanded);
    }
    let path = PathBuf::from(&expanded);
    if path.is_file() {
        LaunchTarget::SelectFile(path)
    } else if path.is_dir() {
        LaunchTarget::OpenDir(path)
    } else {
        LaunchTarget::OpenShell(expanded)
    }
}

/// Attempts to forward `target` to an already-running primary Lumen window.
/// Returns `true` when delivered so the caller can exit immediately.
pub fn try_send_to_existing(target: &LaunchTarget) -> bool {
    let pipe_wide = HSTRING::from(PIPE_NAME);
    // Quick non-blocking wait in case another client just disconnected.
    let _ = unsafe { WaitNamedPipeW(PCWSTR(pipe_wide.as_ptr()), 120) };
    let Ok(handle) = (unsafe {
        CreateFileW(
            PCWSTR(pipe_wide.as_ptr()),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }) else {
        return false;
    };
    if handle == INVALID_HANDLE_VALUE {
        return false;
    }
    let Ok(payload) = serde_json::to_vec(target) else {
        let _ = unsafe { CloseHandle(handle) };
        return false;
    };
    let mut written = 0u32;
    let ok = unsafe { WriteFile(handle, Some(&payload), Some(&mut written), None) }.is_ok()
        && written as usize == payload.len();
    let _ = unsafe { CloseHandle(handle) };
    ok
}

/// Starts the background Named Pipe server on the primary instance.
pub fn start_server() -> Receiver<LaunchTarget> {
    let (tx, rx) = unbounded::<LaunchTarget>();
    std::thread::Builder::new().name("lumen-ipc".to_string()).spawn(move || server_loop(tx)).expect("spawn ipc server");
    rx
}

fn server_loop(tx: Sender<LaunchTarget>) {
    let pipe_wide = HSTRING::from(PIPE_NAME);
    loop {
        let pipe = unsafe {
            CreateNamedPipeW(
                PCWSTR(pipe_wide.as_ptr()),
                PIPE_ACCESS_INBOUND,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                8,
                0,
                4096,
                0,
                None,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            // Another primary instance already owns the pipe or OS error.
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue;
        }
        let connected = match unsafe { ConnectNamedPipe(pipe, None) } {
            Ok(()) => true,
            Err(err) => err.code() == windows::core::HRESULT::from_win32(ERROR_PIPE_CONNECTED.0),
        };
        if connected {
            let mut buf = vec![0u8; 4096];
            let mut read = 0u32;
            if unsafe { ReadFile(pipe, Some(&mut buf), Some(&mut read), None) }.is_ok() && read > 0 {
                buf.truncate(read as usize);
                if let Ok(target) = serde_json::from_slice::<LaunchTarget>(&buf) {
                    let _ = tx.send(target);
                    crate::platform::restore_and_focus();
                    crate::platform::wake();
                }
            }
            let _ = unsafe { DisconnectNamedPipe(pipe) };
        }
        let _ = unsafe { CloseHandle(pipe) };
        let _ = (FILE_GENERIC_READ, FILE_FLAGS_AND_ATTRIBUTES(0), std::ptr::null::<c_void>() as *const HANDLE);
    }
}

/// Spawns a fresh independent Lumen window (`Ctrl+N`) optionally at `path`.
pub fn spawn_new_window(path: Option<&str>) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--new-window");
    if let Some(p) = path
        && !p.is_empty()
    {
        cmd.arg(p);
    }
    cmd.spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explorer_compatible_cli_flags() {
        let opts = parse_args(["/select,C:\\Windows\\notepad.exe"]);
        assert_eq!(opts.target, LaunchTarget::SelectFile(PathBuf::from("C:\\Windows\\notepad.exe")));
        assert!(!opts.new_window);

        let opts = parse_args(["/select,", "C:\\Windows\\notepad.exe"]);
        assert_eq!(opts.target, LaunchTarget::SelectFile(PathBuf::from("C:\\Windows\\notepad.exe")));

        let opts = parse_args(["--new-window", "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"]);
        assert!(opts.new_window);
        assert_eq!(opts.target, LaunchTarget::OpenShell("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string()));

        let opts = parse_args(["--background"]);
        assert!(opts.start_background);
        assert_eq!(opts.target, LaunchTarget::Default);

        let opts = parse_args(["/e,C:\\Windows"]);
        assert_eq!(opts.target, LaunchTarget::OpenDir(PathBuf::from("C:\\Windows")));
    }
}
