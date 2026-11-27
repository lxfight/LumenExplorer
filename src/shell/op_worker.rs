//! Dedicated STA worker for non-blocking `IFileOperation` execution.
//!
//! Running shell file operations on their own STA apartment keeps the main UI
//! window completely responsive during large copies, moves, or deletes while
//! retaining native Windows conflict dialogs and accurate destination paths
//! via a hand-written `IFileOperationProgressSink` COM vtable.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender, unbounded};
use windows::Win32::Foundation::{E_NOINTERFACE, HWND, S_OK};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IBindCtx,
};
use windows::Win32::UI::Shell::{
    FILEOPERATION_FLAGS, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_RENAMEONCOLLISION, FOF_WANTNUKEWARNING,
    FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IFileOperationProgressSink, IFileOperationProgressSink_Vtbl,
    IShellItem, SHCreateItemFromParsingName, SHGetIDListFromObject,
};
use windows::core::{GUID, HRESULT, HSTRING, IUnknown, IUnknown_Vtbl, Interface, PCWSTR};

use crate::shell::location::item_path;

#[derive(Clone, Debug)]
pub enum OpRequest {
    Copy { items: Vec<PathBuf>, dest_dir: PathBuf },
    Move { items: Vec<PathBuf>, dest_dir: PathBuf, clear_clipboard: bool },
    Delete { items: Vec<PathBuf>, permanent: bool },
    DeleteDirect { items: Vec<PathBuf> },
    Restore { items: Vec<RecycledItem> },
    Rename { from: PathBuf, new_name: String, is_undo: bool },
    NewFolder { parent: PathBuf, name: String },
    CreateShortcut { items: Vec<PathBuf>, dest_dir: PathBuf },
}

#[derive(Clone, Debug)]
pub struct OpProgress {
    pub label: String,
    pub percent: u8,
    pub active_jobs: usize,
}

#[derive(Clone, Debug)]
pub enum OpResult {
    Copied { pairs: Vec<(PathBuf, PathBuf)> },
    Moved { pairs: Vec<(PathBuf, PathBuf)>, clear_clipboard: bool },
    Deleted { items: Vec<RecycledItem>, permanent: bool, count: usize },
    DeletedDirect { count: usize },
    Restored { items: Vec<RecycledItem>, result: Result<usize, String> },
    Renamed { from: PathBuf, to: PathBuf, is_undo: bool },
    CreatedFolder { path: PathBuf, name: String },
    CreatedShortcuts { paths: Vec<PathBuf> },
}

/// Exact Shell identity of a recycled item, serialized for use in another STA.
#[derive(Clone, Debug)]
pub struct RecycledItem {
    pub original: PathBuf,
    pub pidl: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct OpFinished {
    pub result: Result<OpResult, String>,
}

pub struct OpWorker {
    req_tx: Sender<OpRequest>,
    done_rx: Receiver<OpFinished>,
    progress: Arc<Mutex<OpProgress>>,
    queued: Arc<AtomicUsize>,
}

impl OpWorker {
    pub fn new() -> Self {
        let (req_tx, req_rx) = unbounded::<OpRequest>();
        let (done_tx, done_rx) = unbounded::<OpFinished>();
        let progress = Arc::new(Mutex::new(OpProgress { label: String::new(), percent: 0, active_jobs: 0 }));
        let queued = Arc::new(AtomicUsize::new(0));
        let worker_progress = progress.clone();
        let worker_queued = queued.clone();
        std::thread::Builder::new()
            .name("lumen-ops".to_string())
            .spawn(move || worker_loop(req_rx, done_tx, worker_progress, worker_queued))
            .expect("spawn file operation STA worker");
        Self { req_tx, done_rx, progress, queued }
    }

    pub fn submit(&self, req: OpRequest) {
        self.queued.fetch_add(1, Ordering::Relaxed);
        let _ = self.req_tx.send(req);
        crate::platform::wake();
    }

    pub fn poll(&self) -> Vec<OpFinished> {
        let mut out = Vec::new();
        while let Ok(item) = self.done_rx.try_recv() {
            out.push(item);
        }
        out
    }

    pub fn progress(&self) -> Option<OpProgress> {
        let active = self.queued.load(Ordering::Relaxed);
        if active == 0 {
            return None;
        }
        let mut snap = self.progress.lock().ok()?.clone();
        snap.active_jobs = active;
        Some(snap)
    }

    pub fn is_busy(&self) -> bool {
        self.queued.load(Ordering::Relaxed) > 0
    }
}

fn worker_loop(
    rx: Receiver<OpRequest>,
    tx: Sender<OpFinished>,
    progress: Arc<Mutex<OpProgress>>,
    queued: Arc<AtomicUsize>,
) {
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    for req in rx {
        let label = req_label(&req);
        if let Ok(mut p) = progress.lock() {
            p.label = label;
            p.percent = 0;
        }
        crate::platform::wake();
        let result = run_request(req, &progress).map_err(|e| format!("{e:#}"));
        queued.fetch_sub(1, Ordering::Relaxed);
        if let Ok(mut p) = progress.lock() {
            p.label.clear();
            p.percent = 0;
        }
        let _ = tx.send(OpFinished { result });
        crate::platform::wake();
    }
    if com {
        unsafe { CoUninitialize() };
    }
}

fn req_label(req: &OpRequest) -> String {
    match req {
        OpRequest::Copy { items, .. } => format!("正在复制 {} 个项目…", items.len()),
        OpRequest::Move { items, .. } => format!("正在移动 {} 个项目…", items.len()),
        OpRequest::Delete { items, permanent } => {
            if *permanent {
                format!("正在永久删除 {} 个项目…", items.len())
            } else {
                format!("正在删除 {} 个项目…", items.len())
            }
        }
        OpRequest::DeleteDirect { items } => format!("正在撤销 {} 个项目…", items.len()),
        OpRequest::Restore { items } => format!("正在还原 {} 个项目…", items.len()),
        OpRequest::Rename { new_name, .. } => format!("正在重命名为 {new_name}…"),
        OpRequest::NewFolder { name, .. } => format!("正在新建 {name}…"),
        OpRequest::CreateShortcut { items, .. } => format!("正在创建 {} 个快捷方式…", items.len()),
    }
}

// ---- hand-written IFileOperationProgressSink COM vtable --------------------

struct SinkData {
    progress: Arc<Mutex<OpProgress>>,
    landed_pairs: Mutex<Vec<(PathBuf, PathBuf)>>,
    deleted: Mutex<Vec<PathBuf>>,
    deleting: Mutex<std::collections::HashMap<usize, PathBuf>>,
    recycled: Mutex<Vec<RecycledItem>>,
}

#[repr(C)]
struct SinkObject {
    vtable: *const IFileOperationProgressSink_Vtbl,
    refs: AtomicU32,
    data: Arc<SinkData>,
}

unsafe extern "system" fn sink_qi(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    let iid = unsafe { &*iid };
    if *iid == IUnknown::IID || *iid == IFileOperationProgressSink::IID {
        unsafe { *out = this };
        unsafe { sink_add_ref(this) };
        S_OK
    } else {
        unsafe { *out = std::ptr::null_mut() };
        E_NOINTERFACE
    }
}

unsafe extern "system" fn sink_add_ref(this: *mut c_void) -> u32 {
    let obj = unsafe { &*(this as *const SinkObject) };
    obj.refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn sink_release(this: *mut c_void) -> u32 {
    let obj = unsafe { &*(this as *const SinkObject) };
    let rem = obj.refs.fetch_sub(1, Ordering::Release) - 1;
    if rem == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        unsafe { drop(Box::from_raw(this as *mut SinkObject)) };
    }
    rem
}

unsafe extern "system" fn sink_noop(_this: *mut c_void) -> HRESULT {
    S_OK
}

unsafe extern "system" fn sink_finish(_this: *mut c_void, _hr: HRESULT) -> HRESULT {
    S_OK
}

unsafe extern "system" fn sink_pre_rename(
    _this: *mut c_void,
    _flags: u32,
    _item: *mut c_void,
    _new: PCWSTR,
) -> HRESULT {
    S_OK
}

unsafe extern "system" fn sink_post_rename(
    this: *mut c_void,
    _flags: u32,
    item: *mut c_void,
    _new_name: PCWSTR,
    hr: HRESULT,
    newly: *mut c_void,
) -> HRESULT {
    if hr.is_ok() {
        record_pair(this, item, newly);
    }
    S_OK
}

unsafe extern "system" fn sink_pre_move(
    _this: *mut c_void,
    _flags: u32,
    _item: *mut c_void,
    _dest: *mut c_void,
    _new_name: PCWSTR,
) -> HRESULT {
    S_OK
}

unsafe extern "system" fn sink_post_move(
    this: *mut c_void,
    _flags: u32,
    item: *mut c_void,
    _dest: *mut c_void,
    _new_name: PCWSTR,
    hr: HRESULT,
    newly: *mut c_void,
) -> HRESULT {
    if hr.is_ok() {
        record_pair(this, item, newly);
    }
    S_OK
}

unsafe extern "system" fn sink_pre_copy(
    _this: *mut c_void,
    _flags: u32,
    _item: *mut c_void,
    _dest: *mut c_void,
    _new_name: PCWSTR,
) -> HRESULT {
    S_OK
}

unsafe extern "system" fn sink_post_copy(
    this: *mut c_void,
    _flags: u32,
    item: *mut c_void,
    _dest: *mut c_void,
    _new_name: PCWSTR,
    hr: HRESULT,
    newly: *mut c_void,
) -> HRESULT {
    if hr.is_ok() {
        record_pair(this, item, newly);
    }
    S_OK
}

unsafe extern "system" fn sink_pre_delete(this: *mut c_void, _flags: u32, item: *mut c_void) -> HRESULT {
    let obj = unsafe { &*(this as *const SinkObject) };
    if let Some(path) = unsafe { IShellItem::from_raw_borrowed(&item) }.and_then(item_path)
        && let Ok(mut deleting) = obj.data.deleting.lock()
    {
        deleting.insert(item as usize, path);
    }
    S_OK
}

unsafe extern "system" fn sink_post_delete(
    this: *mut c_void,
    _flags: u32,
    item: *mut c_void,
    hr: HRESULT,
    newly: *mut c_void,
) -> HRESULT {
    let obj = unsafe { &*(this as *const SinkObject) };
    let original = obj.data.deleting.lock().ok().and_then(|mut items| items.remove(&(item as usize)));
    if hr.is_ok()
        && let Some(src) = original.or_else(|| unsafe { IShellItem::from_raw_borrowed(&item) }.and_then(item_path))
        && !src.exists()
        && let Ok(mut list) = obj.data.deleted.lock()
    {
        list.push(src.clone());
        if let Some(recycled) = unsafe { IShellItem::from_raw_borrowed(&newly) }
            && let Ok(raw) = unsafe { SHGetIDListFromObject(recycled) }
        {
            let pidl = crate::shell::Pidl::from_raw(raw);
            if let Ok(mut items) = obj.data.recycled.lock() {
                items.push(RecycledItem { original: src, pidl: pidl.to_bytes() });
            }
        }
    }
    S_OK
}

unsafe extern "system" fn sink_pre_new(
    _this: *mut c_void,
    _flags: u32,
    _dest: *mut c_void,
    _new_name: PCWSTR,
) -> HRESULT {
    S_OK
}

unsafe extern "system" fn sink_post_new(
    this: *mut c_void,
    _flags: u32,
    _dest: *mut c_void,
    _new_name: PCWSTR,
    _template: PCWSTR,
    _attrs: u32,
    hr: HRESULT,
    newly: *mut c_void,
) -> HRESULT {
    if hr.is_ok() {
        let obj = unsafe { &*(this as *const SinkObject) };
        if let Some(dst) = unsafe { IShellItem::from_raw_borrowed(&newly) }.and_then(item_path)
            && let Ok(mut pairs) = obj.data.landed_pairs.lock()
        {
            pairs.push((dst.clone(), dst));
        }
    }
    S_OK
}

unsafe extern "system" fn sink_update_progress(this: *mut c_void, work_total: u32, work_so_far: u32) -> HRESULT {
    let obj = unsafe { &*(this as *const SinkObject) };
    if work_total > 0
        && let Ok(mut p) = obj.data.progress.lock()
    {
        let pct = ((work_so_far as u64 * 100) / work_total as u64).min(100) as u8;
        if pct != p.percent {
            p.percent = pct;
            crate::platform::wake();
        }
    }
    S_OK
}

fn record_pair(this: *mut c_void, src_raw: *mut c_void, dst_raw: *mut c_void) {
    let obj = unsafe { &*(this as *const SinkObject) };
    let src = unsafe { IShellItem::from_raw_borrowed(&src_raw) }.and_then(item_path);
    let dst = unsafe { IShellItem::from_raw_borrowed(&dst_raw) }.and_then(item_path);
    if let (Some(from), Some(to)) = (src, dst)
        && let Ok(mut pairs) = obj.data.landed_pairs.lock()
    {
        pairs.push((from, to));
    }
}

static SINK_VTABLE: IFileOperationProgressSink_Vtbl = IFileOperationProgressSink_Vtbl {
    base__: IUnknown_Vtbl { QueryInterface: sink_qi, AddRef: sink_add_ref, Release: sink_release },
    StartOperations: sink_noop,
    FinishOperations: sink_finish,
    PreRenameItem: sink_pre_rename,
    PostRenameItem: sink_post_rename,
    PreMoveItem: sink_pre_move,
    PostMoveItem: sink_post_move,
    PreCopyItem: sink_pre_copy,
    PostCopyItem: sink_post_copy,
    PreDeleteItem: sink_pre_delete,
    PostDeleteItem: sink_post_delete,
    PreNewItem: sink_pre_new,
    PostNewItem: sink_post_new,
    UpdateProgress: sink_update_progress,
    ResetTimer: sink_noop,
    PauseTimer: sink_noop,
    ResumeTimer: sink_noop,
};

fn new_sink(progress: Arc<Mutex<OpProgress>>) -> (IFileOperationProgressSink, Arc<SinkData>) {
    let data = Arc::new(SinkData {
        progress,
        landed_pairs: Mutex::new(Vec::new()),
        deleted: Mutex::new(Vec::new()),
        deleting: Mutex::new(std::collections::HashMap::new()),
        recycled: Mutex::new(Vec::new()),
    });
    let obj = Box::new(SinkObject { vtable: &SINK_VTABLE, refs: AtomicU32::new(1), data: data.clone() });
    let sink = unsafe { IFileOperationProgressSink::from_raw(Box::into_raw(obj).cast::<c_void>()) };
    (sink, data)
}

fn create_op(flags: FILEOPERATION_FLAGS, sink: &IFileOperationProgressSink) -> anyhow::Result<(IFileOperation, u32)> {
    let op: IFileOperation = unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }
        .map_err(|e| anyhow::anyhow!("CoCreateInstance(FileOperation) failed: {e}"))?;
    unsafe { op.SetOperationFlags(flags) }.map_err(|e| anyhow::anyhow!("SetOperationFlags failed: {e}"))?;
    let hwnd: HWND = crate::platform::hwnd();
    if !hwnd.0.is_null() {
        let _ = unsafe { op.SetOwnerWindow(hwnd) };
    }
    let cookie = unsafe { op.Advise(sink) }.map_err(|e| anyhow::anyhow!("Advise failed: {e}"))?;
    Ok((op, cookie))
}

fn shell_item(path: &Path) -> anyhow::Result<IShellItem> {
    let wide = HSTRING::from(path);
    unsafe { SHCreateItemFromParsingName(&wide, None::<&IBindCtx>) }
        .map_err(|e| anyhow::anyhow!("SHCreateItemFromParsingName({}) failed: {e}", path.display()))
}

fn run_request(req: OpRequest, progress: &Arc<Mutex<OpProgress>>) -> anyhow::Result<OpResult> {
    let (sink, data) = new_sink(progress.clone());
    match req {
        OpRequest::Restore { items } => {
            let result = crate::shell::ops::restore_from_recycle_bin(&items).map_err(|err| format!("{err:#}"));
            Ok(OpResult::Restored { items, result })
        }
        OpRequest::Copy { items, dest_dir } => {
            let same_dir = items
                .iter()
                .any(|item| item.parent().is_some_and(|p| p.as_os_str().eq_ignore_ascii_case(dest_dir.as_os_str())));
            let flags = if same_dir { FOF_ALLOWUNDO | FOF_RENAMEONCOLLISION } else { FOF_ALLOWUNDO };
            let (op, cookie) = create_op(flags, &sink)?;
            let dest = shell_item(&dest_dir)?;
            for item in &items {
                let src = shell_item(item)?;
                unsafe { op.CopyItem(&src, &dest, PCWSTR::null(), None::<&IFileOperationProgressSink>) }?;
            }
            let res = unsafe { op.PerformOperations() };
            let _ = unsafe { op.Unadvise(cookie) };
            res?;
            let pairs = data.landed_pairs.lock().unwrap().clone();
            Ok(OpResult::Copied { pairs })
        }
        OpRequest::Move { items, dest_dir, clear_clipboard } => {
            let (op, cookie) = create_op(FOF_ALLOWUNDO, &sink)?;
            let dest = shell_item(&dest_dir)?;
            for item in &items {
                let src = shell_item(item)?;
                unsafe { op.MoveItem(&src, &dest, PCWSTR::null(), None::<&IFileOperationProgressSink>) }?;
            }
            let res = unsafe { op.PerformOperations() };
            let _ = unsafe { op.Unadvise(cookie) };
            res?;
            let pairs = data.landed_pairs.lock().unwrap().clone();
            Ok(OpResult::Moved { pairs, clear_clipboard })
        }
        OpRequest::Delete { items, permanent } => {
            let flags = if permanent {
                FOF_WANTNUKEWARNING
            } else {
                FOF_ALLOWUNDO | FOF_WANTNUKEWARNING | FOFX_RECYCLEONDELETE
            };
            let (op, cookie) = create_op(flags, &sink)?;
            for item in &items {
                let shell = shell_item(item)?;
                unsafe { op.DeleteItem(&shell, None::<&IFileOperationProgressSink>) }?;
            }
            let res = unsafe { op.PerformOperations() };
            let _ = unsafe { op.Unadvise(cookie) };
            res?;
            let count = data.deleted.lock().unwrap().len();
            let items = data.recycled.lock().unwrap().clone();
            Ok(OpResult::Deleted { items, permanent, count })
        }
        OpRequest::DeleteDirect { items } => {
            let (op, cookie) = create_op(FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING, &sink)?;
            for item in &items {
                if !item.exists() {
                    continue;
                }
                let shell = shell_item(item)?;
                unsafe { op.DeleteItem(&shell, None::<&IFileOperationProgressSink>) }?;
            }
            let res = unsafe { op.PerformOperations() };
            let _ = unsafe { op.Unadvise(cookie) };
            res?;
            let count = data.deleted.lock().unwrap().len();
            Ok(OpResult::DeletedDirect { count })
        }
        OpRequest::Rename { from, new_name, is_undo } => {
            crate::shell::ops::validate_name(&new_name)?;
            let (op, cookie) = create_op(FILEOPERATION_FLAGS(0), &sink)?;
            let shell = shell_item(&from)?;
            let wide = HSTRING::from(new_name.as_str());
            unsafe { op.RenameItem(&shell, &wide, None::<&IFileOperationProgressSink>) }?;
            let res = unsafe { op.PerformOperations() };
            let _ = unsafe { op.Unadvise(cookie) };
            res?;
            let to = data
                .landed_pairs
                .lock()
                .unwrap()
                .first()
                .map(|(_, to)| to.clone())
                .unwrap_or_else(|| from.parent().map(|p| p.join(&new_name)).unwrap_or_else(|| from.clone()));
            Ok(OpResult::Renamed { from, to, is_undo })
        }
        OpRequest::NewFolder { parent, name } => {
            crate::shell::ops::validate_name(&name)?;
            let (op, cookie) = create_op(FILEOPERATION_FLAGS(0), &sink)?;
            let dir = shell_item(&parent)?;
            let wide = HSTRING::from(name.as_str());
            unsafe {
                op.NewItem(&dir, FILE_ATTRIBUTE_DIRECTORY.0, &wide, PCWSTR::null(), None::<&IFileOperationProgressSink>)
            }?;
            let res = unsafe { op.PerformOperations() };
            let _ = unsafe { op.Unadvise(cookie) };
            res?;
            let created = data
                .landed_pairs
                .lock()
                .unwrap()
                .first()
                .map(|(_, to)| to.clone())
                .unwrap_or_else(|| parent.join(&name));
            Ok(OpResult::CreatedFolder { path: created, name })
        }
        OpRequest::CreateShortcut { items, dest_dir } => {
            let mut paths = Vec::new();
            for item in &items {
                paths.push(crate::shell::ops::create_shortcut(item, &dest_dir)?);
            }
            Ok(OpResult::CreatedShortcuts { paths })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deleted_items_restore_to_their_exact_original_paths() {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok().expect("COM STA");
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("tmp").join(format!("undo-delete-{stamp}"));
        let first = root.join("first").join(format!("lumen-undo-{stamp}.txt"));
        let second = root.join("second").join(first.file_name().unwrap());
        for (path, contents) in [(&first, b"first".as_slice()), (&second, b"second".as_slice())] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        let progress = Arc::new(Mutex::new(OpProgress { label: String::new(), percent: 0, active_jobs: 0 }));
        let delete = |path: &PathBuf| {
            let result = run_request(OpRequest::Delete { items: vec![path.clone()], permanent: false }, &progress)
                .expect("delete through worker");
            let OpResult::Deleted { items, count, .. } = result else { panic!("expected delete result") };
            assert_eq!(count, 1);
            assert_eq!(items.len(), 1);
            assert!(!path.exists());
            items
        };
        let first_deleted = delete(&first);
        let second_deleted = delete(&second);
        assert_eq!(crate::shell::ops::restore_from_recycle_bin(&first_deleted).expect("restore first"), 1);
        assert_eq!(std::fs::read(&first).unwrap(), b"first");
        assert!(!second.exists(), "restoring one delete must leave the other same-name item in the bin");
        assert_eq!(crate::shell::ops::restore_from_recycle_bin(&second_deleted).expect("restore second"), 1);
        assert_eq!(std::fs::read(&second).unwrap(), b"second");
        std::fs::remove_dir_all(root).unwrap();
        unsafe { CoUninitialize() };
    }
}
