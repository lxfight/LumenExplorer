//! Explorer-compatible CLI parsing and single-instance Named Pipe IPC.
//!
//! When Windows, Chrome ("Show in Folder"), VS Code, or `Win+E` invokes
//! `lumen.exe <path>` or `lumen.exe /select,<file>`, a secondary process sends
//! the parsed `LaunchCommand` over `\\.\pipe\LumenExplorer.IPC.v1` to the
//! primary instance (opening a new tab in < 15ms) unless `--new-window` is set.

use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_INBOUND,
    ReadFile, WriteFile,
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
    /// Request the running primary instance to save state and exit cleanly (`--quit`).
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliOptions {
    pub target: LaunchTarget,
    pub new_window: bool,
    pub start_background: bool,
    pub quit: bool,
}

/// Parses Windows Explorer–compatible command-line flags:
/// - `<dir_or_file>`
/// - `/select,<path>` or `/select, <path>`
/// - `/e,<path>`, `/root,<path>`, `/n`
/// - `--new-window`, `--background`, `--quit`
pub fn parse_args<I, S>(args: I) -> CliOptions
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let raw: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    let mut new_window = false;
    let mut start_background = false;
    let mut quit = false;
    let mut select_next = false;
    let mut target = LaunchTarget::Default;

    for arg in raw {
        let trimmed = arg.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower == "--quit" || lower == "--exit" {
            quit = true;
            target = LaunchTarget::Quit;
            continue;
        }
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
            let cleaned = crate::shell::location::normalize_drive_spec(rest);
            if !cleaned.is_empty() {
                target = classify_target(&cleaned);
            }
            continue;
        }
        if lower == "/e" || lower == "/root" {
            continue;
        }
        let cleaned = crate::shell::location::normalize_drive_spec(trimmed);
        if !cleaned.is_empty() {
            target = classify_target(&cleaned);
        }
    }

    CliOptions { target, new_window, start_background, quit }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn classify_target(raw: &str) -> LaunchTarget {
    let normalized = crate::shell::location::normalize_drive_spec(raw);
    let expanded = crate::shell::location::normalize_drive_spec(&crate::shell::location::expand_env(&normalized));
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
    let extended_retry = crate::platform::is_primary_instance_running();
    try_send_to_pipe(PIPE_NAME, target, extended_retry)
}

fn try_send_to_pipe(pipe_name: &str, target: &LaunchTarget, extended_retry: bool) -> bool {
    let Ok(payload) = serde_json::to_vec(target) else {
        return false;
    };
    let pipe_wide = HSTRING::from(pipe_name);
    let not_found_delays: &[u64] = if extended_retry { &[15, 30, 50, 80, 120] } else { &[10, 20] };
    let busy_delays: &[u64] = &[10, 20, 40, 80, 120];
    let mut not_found_retries = 0usize;
    let mut busy_retries = 0usize;

    let handle = loop {
        // Brief wait in case another client is finishing up on the pipe.
        let _ = unsafe { WaitNamedPipeW(PCWSTR(pipe_wide.as_ptr()), 80) };
        match unsafe {
            CreateFileW(
                PCWSTR(pipe_wide.as_ptr()),
                FILE_GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        } {
            Ok(h) if h != INVALID_HANDLE_VALUE => break h,
            Ok(_) => return false,
            Err(err) => {
                let code = err.code();
                if code == windows::core::HRESULT::from_win32(ERROR_PIPE_BUSY.0) {
                    let Some(&delay_ms) = busy_delays.get(busy_retries) else {
                        return false;
                    };
                    busy_retries += 1;
                    std::thread::sleep(Duration::from_millis(delay_ms));
                } else if code == windows::core::HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) {
                    let Some(&delay_ms) = not_found_delays.get(not_found_retries) else {
                        return false;
                    };
                    not_found_retries += 1;
                    std::thread::sleep(Duration::from_millis(delay_ms));
                } else {
                    return false;
                }
            }
        }
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

fn create_pipe_instance(pipe_wide: &HSTRING) -> HANDLE {
    unsafe {
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
    }
}

fn server_loop(tx: Sender<LaunchTarget>) {
    serve_pipe_loop(PIPE_NAME, tx, false, crate::platform::request_force_quit);
}

fn serve_pipe_loop(pipe_name: &str, tx: Sender<LaunchTarget>, stop_on_quit: bool, on_quit: impl Fn()) {
    let pipe_wide = HSTRING::from(pipe_name);
    let mut pipe = create_pipe_instance(&pipe_wide);
    loop {
        if pipe == INVALID_HANDLE_VALUE {
            // Another primary instance already owns the pipe or OS error.
            std::thread::sleep(Duration::from_secs(2));
            pipe = create_pipe_instance(&pipe_wide);
            continue;
        }
        let connected = match unsafe { ConnectNamedPipe(pipe, None) } {
            Ok(()) => true,
            Err(err) => err.code() == windows::core::HRESULT::from_win32(ERROR_PIPE_CONNECTED.0),
        };
        // Pre-create the next listening instance before reading/disconnecting/closing
        // the current one so there is never a gap with zero listening pipe instances.
        let next_pipe = create_pipe_instance(&pipe_wide);
        let mut should_stop = false;
        if connected {
            let mut buf = vec![0u8; 4096];
            let mut read = 0u32;
            if unsafe { ReadFile(pipe, Some(&mut buf), Some(&mut read), None) }.is_ok() && read > 0 {
                buf.truncate(read as usize);
                if let Ok(target) = serde_json::from_slice::<LaunchTarget>(&buf) {
                    if target == LaunchTarget::Quit {
                        on_quit();
                        if stop_on_quit {
                            should_stop = true;
                        }
                    } else {
                        let _ = tx.send(target);
                        crate::platform::wake();
                    }
                }
            }
            let _ = unsafe { DisconnectNamedPipe(pipe) };
        }
        let _ = unsafe { CloseHandle(pipe) };
        if should_stop {
            if next_pipe != INVALID_HANDLE_VALUE {
                let _ = unsafe { CloseHandle(next_pipe) };
            }
            break;
        }
        pipe = next_pipe;
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
        assert!(!opts.quit);

        let opts = parse_args(["--quit"]);
        assert!(opts.quit);
        assert_eq!(opts.target, LaunchTarget::Quit);

        let opts = parse_args(["--exit"]);
        assert!(opts.quit);
        assert_eq!(opts.target, LaunchTarget::Quit);
    }

    #[test]
    fn normalizes_drive_specs_and_explorer_sentinel_paths() {
        for input in ["C:", r#"C:""#, r#""C:""#, r"C:\.", r"C:\\."] {
            assert_eq!(
                classify_target(input),
                LaunchTarget::OpenDir(PathBuf::from(r"C:\")),
                "classify_target failed for {input:?}"
            );
            let opts = parse_args([input]);
            assert_eq!(opts.target, LaunchTarget::OpenDir(PathBuf::from(r"C:\")), "parse_args failed for {input:?}");
        }
        assert_eq!(classify_target("c:"), LaunchTarget::OpenDir(PathBuf::from(r"c:\")));
        assert_eq!(parse_args([r#"/e,C:""#]).target, LaunchTarget::OpenDir(PathBuf::from(r"C:\")));
        assert_eq!(parse_args([r"C:\Windows\."]).target, LaunchTarget::OpenDir(PathBuf::from(r"C:\Windows")));
    }

    #[test]
    fn pipe_server_handles_rapid_consecutive_sends_without_gap() {
        let pipe = format!(r"\\.\pipe\LumenExplorer.IPC.Test.Rapid.{}", std::process::id());
        let pipe_srv = pipe.clone();
        let (tx, rx) = unbounded::<LaunchTarget>();
        let handle = std::thread::spawn(move || {
            serve_pipe_loop(&pipe_srv, tx, true, || {});
        });

        // Rapid consecutive invocations must all succeed without missing a pipe instance.
        for i in 0..15 {
            let target = LaunchTarget::OpenShell(format!("shell:Test{i}"));
            assert!(try_send_to_pipe(&pipe, &target, true), "send {i} failed");
        }
        assert!(try_send_to_pipe(&pipe, &LaunchTarget::Quit, true));
        handle.join().expect("server thread cleanly exits on Quit");

        let received: Vec<LaunchTarget> = rx.try_iter().collect();
        assert_eq!(received.len(), 15);
        for (i, target) in received.into_iter().enumerate() {
            assert_eq!(target, LaunchTarget::OpenShell(format!("shell:Test{i}")));
        }
    }

    #[test]
    fn client_retries_when_pipe_becomes_ready_shortly() {
        let pipe = format!(r"\\.\pipe\LumenExplorer.IPC.Test.Retry.{}", std::process::id());
        let pipe_srv = pipe.clone();
        let (tx, rx) = unbounded::<LaunchTarget>();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            serve_pipe_loop(&pipe_srv, tx, true, || {});
        });

        let target = LaunchTarget::OpenDir(PathBuf::from(r"C:\"));
        assert!(try_send_to_pipe(&pipe, &target, true));
        assert!(try_send_to_pipe(&pipe, &LaunchTarget::Quit, true));
        handle.join().expect("server thread exits cleanly");
        assert_eq!(rx.try_recv().ok(), Some(target));
    }
}
