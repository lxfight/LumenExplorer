//! Event-driven directory change detection used to auto-refresh the view.
//!
//! One background thread keeps a single overlapped `ReadDirectoryChangesW` request
//! per watched folder and blocks until it completes: no polling, no wakeups while
//! nothing happens.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, ERROR_IO_PENDING, ERROR_NOTIFY_ENUM_DIR, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE,
    FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects};
use windows::core::{HRESULT, HSTRING, PCWSTR};

/// Everything that changes a row of the list: entries appearing/disappearing/renaming,
/// size and timestamps (writes update the latter two), attributes.
const NOTIFY: FILE_NOTIFY_CHANGE = FILE_NOTIFY_CHANGE(
    FILE_NOTIFY_CHANGE_FILE_NAME.0
        | FILE_NOTIFY_CHANGE_DIR_NAME.0
        | FILE_NOTIFY_CHANGE_SIZE.0
        | FILE_NOTIFY_CHANGE_LAST_WRITE.0
        | FILE_NOTIFY_CHANGE_CREATION.0
        | FILE_NOTIFY_CHANGE_ATTRIBUTES.0,
);

/// Bulk work queues hundreds of records at once; 64 KiB absorbs a whole burst instead
/// of overflowing into a blind rescan.
const BUFFER_BYTES: u32 = 64 * 1024;

/// A burst has to stay quiet this long before it is reported, so one copy becomes one
/// refresh instead of dozens.
const QUIET: Duration = Duration::from_millis(120);

/// A copy that never goes quiet still has to refresh now and then.
const MAX_LATENCY: Duration = Duration::from_millis(500);

pub struct Watcher {
    commands: Sender<Command>,
    wake: Event,
    stop: Event,
    dirty: Arc<AtomicU64>,
    /// Generation of the currently watched folder; changes are reported with the
    /// generation they were seen in.
    epoch: u64,
    /// Newest generation already acknowledged by `watch` or consumed by `poll`.
    ack: u64,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    pub fn new() -> Self {
        let stop = Event::create();
        let wake = Event::create();
        let dirty = Arc::new(AtomicU64::new(0));
        let (commands, rx) = channel();
        let thread = {
            let dirty = dirty.clone();
            std::thread::Builder::new()
                .name("lumen-watch".into())
                .spawn(move || watch_loop(rx, stop, wake, dirty))
                .expect("watch thread")
        };
        Self { commands, wake, stop, dirty, epoch: 0, ack: 0, thread: Some(thread) }
    }

    pub fn watch(&mut self, path: Option<PathBuf>) {
        self.epoch += 1;
        self.ack = self.epoch - 1;
        self.dirty.store(0, Ordering::Release);
        let _ = self.commands.send(Command { path, epoch: self.epoch });
        self.wake.set();
    }

    pub fn poll(&mut self) -> bool {
        self.dirty.swap(0, Ordering::AcqRel) > self.ack
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.set();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        unsafe {
            let _ = CloseHandle(self.stop.0);
            let _ = CloseHandle(self.wake.0);
        }
    }
}

struct Command {
    path: Option<PathBuf>,
    epoch: u64,
}

/// Pending burst. `since` bounds how long a busy folder may defer the refresh,
/// `until` is the current quiet deadline.
struct Coalesce {
    since: Instant,
    until: Instant,
    epoch: u64,
}

struct Target {
    dir: HANDLE,
    buffer: Box<[u8]>,
    overlapped: Box<OVERLAPPED>,
    epoch: u64,
}

/// Kernel event wrapper. `HANDLE` is a raw pointer, so the `windows` crate does not
/// mark it `Send`; the handle itself is valid from any thread.
#[derive(Clone, Copy)]
struct Event(HANDLE);

unsafe impl Send for Event {}

impl Event {
    fn create() -> Event {
        Event(unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.expect("watch event"))
    }

    fn set(self) {
        unsafe {
            let _ = SetEvent(self.0);
        }
    }

    fn reset(self) {
        unsafe {
            let _ = ResetEvent(self.0);
        }
    }
}

/// Wait handles: 0 = stop, 1 = a command is queued, 2 = the directory request completed.
fn watch_loop(rx: Receiver<Command>, stop: Event, wake: Event, dirty: Arc<AtomicU64>) {
    let io = Event::create();
    let handles = [stop.0, wake.0, io.0];
    let mut target: Option<Target> = None;
    let mut pending: Option<Coalesce> = None;

    loop {
        let timeout = match &pending {
            // Round the remainder up so a sub-millisecond deadline cannot spin the wait.
            Some(p) => p.until.saturating_duration_since(Instant::now()).as_millis().min(1_000) as u32 + 1,
            None => INFINITE,
        };
        let wait = unsafe { WaitForMultipleObjects(&handles, false, timeout) };

        if wait == WAIT_OBJECT_0 {
            break;
        }
        if wait.0 == WAIT_OBJECT_0.0 + 1 {
            // Reset before draining: a command that lands mid-drain re-arms the event.
            wake.reset();
            while let Ok(command) = rx.try_recv() {
                cancel(target.take());
                pending = None;
                // The cancelled request may have completed but never been reaped.
                io.reset();
                target = command.path.and_then(|path| open(&path, io, command.epoch));
            }
            continue;
        }
        if wait.0 == WAIT_OBJECT_0.0 + 2 {
            io.reset();
            let Some(current) = target.as_ref() else { continue };
            if finished(current) {
                let now = Instant::now();
                match &mut pending {
                    Some(burst) => burst.until = (burst.since + MAX_LATENCY).min(now + QUIET),
                    None => pending = Some(Coalesce { since: now, until: now + QUIET, epoch: current.epoch }),
                }
                let Some(current) = target.as_mut() else { continue };
                if !arm(current, io) {
                    cancel(target.take());
                }
            } else {
                // Folder gone, volume removed or handle revoked: just stop listening.
                cancel(target.take());
            }
            continue;
        }

        // Timeout: report the coalesced burst once it has been quiet long enough.
        if let Some(burst) = &pending
            && Instant::now() >= burst.until
        {
            dirty.store(burst.epoch, Ordering::Release);
            // The window may be asleep; the refresh is only useful if a
            // frame actually runs to pick the flag up.
            crate::platform::wake();
            pending = None;
        }
    }

    cancel(target.take());
    unsafe {
        let _ = CloseHandle(io.0);
    }
}

fn open(path: &Path, io: Event, epoch: u64) -> Option<Target> {
    let wide = HSTRING::from(path.as_os_str());
    let dir = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        )
    }
    .ok()?;
    let mut target = Target {
        dir,
        buffer: vec![0u8; BUFFER_BYTES as usize].into_boxed_slice(),
        overlapped: Box::new(OVERLAPPED { hEvent: io.0, ..Default::default() }),
        epoch,
    };
    if arm(&mut target, io) {
        Some(target)
    } else {
        unsafe {
            let _ = CloseHandle(dir);
        }
        None
    }
}

fn arm(target: &mut Target, io: Event) -> bool {
    // Overlapped requests are one-shot: reuse needs a clean completion state.
    *target.overlapped = OVERLAPPED { hEvent: io.0, ..Default::default() };
    let mut transferred = 0u32;
    match unsafe {
        ReadDirectoryChangesW(
            target.dir,
            target.buffer.as_mut_ptr().cast(),
            BUFFER_BYTES,
            false,
            NOTIFY,
            Some(&mut transferred),
            Some(&mut *target.overlapped as *mut OVERLAPPED),
            None,
        )
    } {
        Ok(()) => true,
        // Overlapped requests normally return before completing; pending is success.
        Err(err) => err.code() == HRESULT::from_win32(ERROR_IO_PENDING.0),
    }
}

/// Whether the completed request carried changes. An empty completion or a dead handle
/// means there is nothing left to listen to.
fn finished(target: &Target) -> bool {
    let mut transferred = 0u32;
    match unsafe { GetOverlappedResult(target.dir, &*target.overlapped, &mut transferred, false) } {
        Ok(()) => transferred > 0,
        // Buffer overflow: the records are lost but a refresh covers it.
        Err(err) => err.code() == HRESULT::from_win32(ERROR_NOTIFY_ENUM_DIR.0),
    }
}

fn cancel(target: Option<Target>) {
    let Some(target) = target else { return };
    unsafe {
        // The driver still writes into OVERLAPPED and the buffer until the cancelled
        // request reports completion, so wait for that before dropping them.
        let _ = CancelIoEx(target.dir, Some(&*target.overlapped as *const OVERLAPPED));
        let mut transferred = 0u32;
        let _ = GetOverlappedResult(target.dir, &*target.overlapped, &mut transferred, true);
        let _ = CloseHandle(target.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    const TIMEOUT: Duration = Duration::from_secs(3);
    /// `watch` only queues the command; the worker arms the handle right after, so tests
    /// let it settle before changing anything.
    const SETTLE: Duration = Duration::from_millis(250);

    fn dirty_within(watcher: &mut Watcher, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if watcher.poll() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("lumen-watch-{tag}-{}-{stamp}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn watch_reports_create_rename_and_delete() {
        let dir = TempDir::new("basic");
        let mut watcher = Watcher::new();
        watcher.watch(Some(dir.0.clone()));
        std::thread::sleep(SETTLE);

        std::fs::write(dir.file("a.txt"), b"one").unwrap();
        assert!(dirty_within(&mut watcher, TIMEOUT), "create");

        std::fs::rename(dir.file("a.txt"), dir.file("b.txt")).unwrap();
        assert!(dirty_within(&mut watcher, TIMEOUT), "rename");

        std::fs::remove_file(dir.file("b.txt")).unwrap();
        assert!(dirty_within(&mut watcher, TIMEOUT), "delete");
    }

    #[test]
    fn watch_ignores_previous_directory_after_switch() {
        let first = TempDir::new("first");
        let second = TempDir::new("second");
        let mut watcher = Watcher::new();
        watcher.watch(Some(first.0.clone()));
        std::thread::sleep(SETTLE);
        std::fs::write(first.file("a.txt"), b"one").unwrap();
        assert!(dirty_within(&mut watcher, TIMEOUT), "first directory is watched");

        watcher.watch(Some(second.0.clone()));
        std::thread::sleep(SETTLE);
        let _ = watcher.poll();

        std::fs::write(first.file("b.txt"), b"two").unwrap();
        assert!(!dirty_within(&mut watcher, Duration::from_millis(700)), "old directory is no longer watched");

        std::fs::write(second.file("c.txt"), b"three").unwrap();
        assert!(dirty_within(&mut watcher, TIMEOUT), "new directory is watched");
    }

    #[test]
    fn watch_none_is_silent() {
        let mut watcher = Watcher::new();
        watcher.watch(None);
        assert!(!dirty_within(&mut watcher, Duration::from_millis(300)));
    }
}
