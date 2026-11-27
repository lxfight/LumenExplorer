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
    IShellItem, SHCreateItemFromIDList, SHCreateItemFromParsingName,
};
use windows::core::{GUID, HRESULT, HSTRING, IUnknown, IUnknown_Vtbl, Interface, PCWSTR};

use crate::shell::location::{Pidl, is_in_recycle_bin_path, item_path};

#[derive(Clone, Debug)]
pub enum OpRequest {
    Copy {
        items: Vec<PathBuf>,
        dest_dir: PathBuf,
    },
    Move {
        items: Vec<PathBuf>,
        dest_dir: PathBuf,
        clear_clipboard: bool,
    },
    Delete {
        items: Vec<PathBuf>,
        permanent: bool,
    },
    /// Permanently deletes virtual shell items (e.g. Recycle Bin entries) via
    /// their serialized absolute PIDLs so Windows cleans up `$R`/`$I` pairs
    /// atomically and prompts with the original filenames for permanent deletion.
    DeletePidls {
        pidl_bytes: Vec<Vec<u8>>,
    },
    DeleteDirect {
        items: Vec<PathBuf>,
    },
    Rename {
        from: PathBuf,
        new_name: String,
        is_undo: bool,
    },
    NewFolder {
        parent: PathBuf,
        name: String,
    },
    CreateShortcut {
        items: Vec<PathBuf>,
        dest_dir: PathBuf,
    },
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
    Deleted { items: Vec<PathBuf>, permanent: bool, count: usize },
    DeletedDirect { count: usize },
    Renamed { from: PathBuf, to: PathBuf, is_undo: bool },
    CreatedFolder { path: PathBuf, name: String },
    CreatedShortcuts { paths: Vec<PathBuf> },
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
        OpRequest::DeletePidls { pidl_bytes } => format!("正在永久删除 {} 个项目…", pidl_bytes.len()),
        OpRequest::DeleteDirect { items } => format!("正在撤销 {} 个项目…", items.len()),
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
    deleted_count: AtomicUsize,
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

unsafe extern "system" fn sink_pre_delete(_this: *mut c_void, _flags: u32, _item: *mut c_void) -> HRESULT {
    S_OK
}

unsafe extern "system" fn sink_post_delete(
    this: *mut c_void,
    _flags: u32,
    item: *mut c_void,
    hr: HRESULT,
    _newly: *mut c_void,
) -> HRESULT {
    if hr.is_ok() {
        let obj = unsafe { &*(this as *const SinkObject) };
        obj.data.deleted_count.fetch_add(1, Ordering::Relaxed);
        if let Some(src) = unsafe { IShellItem::from_raw_borrowed(&item) }.and_then(item_path)
            && let Ok(mut list) = obj.data.deleted.lock()
        {
            list.push(src);
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
        deleted_count: AtomicUsize::new(0),
    });
    let obj = Box::new(SinkObject { vtable: &SINK_VTABLE, refs: AtomicU32::new(1), data: data.clone() });
    let sink = unsafe { IFileOperationProgressSink::from_raw(Box::into_raw(obj).cast::<c_void>()) };
    (sink, data)
}

fn finish_allow_cancel(res: windows::core::Result<()>) -> anyhow::Result<()> {
    match res {
        Ok(()) => Ok(()),
        Err(e) if crate::shell::ops::is_user_cancelled_hr(e.code().0 as u32) => Ok(()),
        Err(e) => Err(e.into()),
    }
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
            // Never attempt to recycle items already inside a physical `$Recycle.Bin` folder.
            let permanent = permanent || (!items.is_empty() && items.iter().all(|p| is_in_recycle_bin_path(p)));
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
            finish_allow_cancel(res)?;
            let deleted = data.deleted.lock().unwrap().clone();
            let count = data.deleted_count.load(Ordering::Relaxed);
            Ok(OpResult::Deleted { items: deleted, permanent, count })
        }
        OpRequest::DeletePidls { pidl_bytes } => {
            // Permanent delete via Recycle Bin PIDLs: omitting FOF_ALLOWUNDO and
            // FOFX_RECYCLEONDELETE makes Windows show the native permanent-delete
            // confirmation with original filenames and remove `$R`/`$I` pairs cleanly.
            let (op, cookie) = create_op(FOF_WANTNUKEWARNING, &sink)?;
            let mut queued_any = false;
            for bytes in &pidl_bytes {
                let Some(pidl) = Pidl::from_bytes(bytes) else { continue };
                let shell: IShellItem = unsafe { SHCreateItemFromIDList(pidl.as_ptr()) }
                    .map_err(|e| anyhow::anyhow!("SHCreateItemFromIDList failed: {e}"))?;
                unsafe { op.DeleteItem(&shell, None::<&IFileOperationProgressSink>) }?;
                queued_any = true;
            }
            if !queued_any {
                let _ = unsafe { op.Unadvise(cookie) };
                return Ok(OpResult::Deleted { items: Vec::new(), permanent: true, count: 0 });
            }
            let res = unsafe { op.PerformOperations() };
            let _ = unsafe { op.Unadvise(cookie) };
            finish_allow_cancel(res)?;
            let count = data.deleted_count.load(Ordering::Relaxed);
            Ok(OpResult::Deleted { items: Vec::new(), permanent: true, count })
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
    fn treats_shell_cancel_hresults_as_benign() {
        let cancel = windows::core::Error::from_hresult(HRESULT(0x8027_0000u32 as i32));
        assert!(finish_allow_cancel(Err(cancel)).is_ok());
        let win32_cancel = windows::core::Error::from_hresult(HRESULT(0x8007_04C7u32 as i32));
        assert!(finish_allow_cancel(Err(win32_cancel)).is_ok());
        let access_denied = windows::core::Error::from_hresult(HRESULT(0x8007_0005u32 as i32));
        assert!(finish_allow_cancel(Err(access_denied)).is_err());
    }
}
