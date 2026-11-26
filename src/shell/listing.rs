//! Background directory listing for both filesystem folders (MTA) and virtual
//! shell namespaces (dedicated STA thread).
//!
//! Neither filesystem enumeration nor virtual namespace enumeration (Network,
//! Recycle Bin, This PC, MTP devices) ever blocks the UI thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use crossbeam_channel::{Receiver, Sender, TryRecvError, bounded, unbounded};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

use crate::core::{Entry, Sort};
use crate::shell::location::{Location, Pidl};

pub struct Listing {
    pub generation: u64,
    pub sort: Sort,
    /// Already sorted with the requested `Sort`.
    pub entries: Vec<Entry>,
    pub error: Option<String>,
}

pub struct Job {
    pub generation: u64,
    rx: Receiver<Listing>,
    cancel: Arc<AtomicBool>,
}

impl Job {
    /// Stops the worker from touching the disk once it reaches its next check.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Non-blocking; yields the finished listing exactly once.
    pub fn poll(&self) -> Option<Listing> {
        match self.rx.try_recv() {
            Ok(listing) => Some(listing),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Listing {
                generation: self.generation,
                sort: Sort::default(),
                entries: Vec::new(),
                error: Some("读取目录失败".to_string()),
            }),
        }
    }
}

pub fn spawn(path: PathBuf, show_hidden: bool, show_system: bool, sort: Sort, generation: u64) -> Job {
    let (tx, rx) = bounded(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    std::thread::Builder::new()
        .name("lumen-list".to_string())
        .spawn(move || {
            let (mut entries, error) =
                match crate::shell::enumerate::list_fs(&path, show_hidden, show_system, &worker_cancel) {
                    Ok(entries) => (entries, None),
                    Err(err) => (Vec::new(), Some(format!("{err:#}"))),
                };
            if worker_cancel.load(Ordering::Relaxed) {
                return;
            }
            crate::core::tabs::sort_entries(&mut entries, sort);
            let _ = tx.try_send(Listing { generation, sort, entries, error });
            crate::platform::wake();
        })
        .expect("spawn listing worker");
    Job { generation, rx, cancel }
}

struct ShellListRequest {
    pidl_bytes: Vec<u8>,
    show_hidden: bool,
    sort: Sort,
    generation: u64,
    cancel: Arc<AtomicBool>,
    tx: Sender<Listing>,
}

static SHELL_LIST_TX: OnceLock<Sender<ShellListRequest>> = OnceLock::new();

fn shell_worker() -> &'static Sender<ShellListRequest> {
    SHELL_LIST_TX.get_or_init(|| {
        let (tx, rx) = unbounded::<ShellListRequest>();
        std::thread::Builder::new()
            .name("lumen-shell-list".to_string())
            .spawn(move || {
                let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
                for req in rx {
                    if req.cancel.load(Ordering::Relaxed) {
                        continue;
                    }
                    let result = Pidl::from_bytes(&req.pidl_bytes)
                        .ok_or_else(|| anyhow::anyhow!("无法解析虚拟目录 PIDL"))
                        .and_then(|pidl| Location::from_absolute_pidl(Arc::new(pidl)))
                        .and_then(|loc| crate::shell::enumerate::list_shell(&loc, req.show_hidden));
                    if req.cancel.load(Ordering::Relaxed) {
                        continue;
                    }
                    let (mut entries, error) = match result {
                        Ok(entries) => (entries, None),
                        Err(err) => (Vec::new(), Some(format!("{err:#}"))),
                    };
                    crate::core::tabs::sort_entries(&mut entries, req.sort);
                    let _ = req.tx.try_send(Listing { generation: req.generation, sort: req.sort, entries, error });
                    crate::platform::wake();
                }
                if com {
                    unsafe { CoUninitialize() };
                }
            })
            .expect("spawn shell listing STA worker");
        tx
    })
}

/// Enumerates a virtual shell namespace (This PC, Network, Recycle Bin, MTP) on
/// a dedicated STA worker thread so slow shell namespaces never block the UI.
pub fn spawn_shell(pidl_bytes: Vec<u8>, show_hidden: bool, sort: Sort, generation: u64) -> Job {
    let (tx, rx) = bounded(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let _ =
        shell_worker().send(ShellListRequest { pidl_bytes, show_hidden, sort, generation, cancel: cancel.clone(), tx });
    Job { generation, rx, cancel }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn sandbox(name: &str) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/tmp")
            .join(format!("listing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn delivers_sorted_entries_off_thread() {
        let root = sandbox("sort");
        std::fs::write(root.join("file10.txt"), b"x").unwrap();
        std::fs::write(root.join("file2.txt"), b"x").unwrap();
        std::fs::create_dir(root.join("folder")).unwrap();
        let job = spawn(root.clone(), false, false, Sort::default(), 7);

        let deadline = Instant::now() + Duration::from_secs(10);
        let listing = loop {
            if let Some(listing) = job.poll() {
                break listing;
            }
            assert!(Instant::now() < deadline, "listing never finished");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(listing.generation, 7);
        assert!(listing.error.is_none());
        let names: Vec<&str> = listing.entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["folder", "file2.txt", "file10.txt"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cancellation_discards_the_work() {
        let root = sandbox("cancel");
        std::fs::write(root.join("a.txt"), b"x").unwrap();
        let job = spawn(root.clone(), false, false, Sort::default(), 1);
        job.cancel();
        let _ = job.poll();
        std::fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    #[test]
    #[ignore]
    fn listing_bench() {
        for path in ["C:\\Windows\\WinSxS", "C:\\Windows\\System32", "C:\\"] {
            let dir = std::path::Path::new(path);
            if !dir.is_dir() {
                continue;
            }
            let cancel = AtomicBool::new(false);
            let start = Instant::now();
            let mut entries = crate::shell::enumerate::list_fs(dir, false, false, &cancel).expect("list");
            let listed = start.elapsed();
            let count = entries.len();
            let sort_start = Instant::now();
            crate::core::tabs::sort_entries(&mut entries, crate::core::Sort::default());
            println!("{path}: {count} entries, list {listed:?}, sort {:?}", sort_start.elapsed());
        }
    }
}
