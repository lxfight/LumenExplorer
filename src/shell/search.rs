//! Background search supporting:
//! 1. Multi-threaded parallel recursive filesystem walk (always available, zero config)
//! 2. Everything IPC v2 auto-detection (instant indexed search when Everything is running)
//! 3. Optional Windows Search SystemIndex query (`Provider=Search.CollatorDSO`)

use std::collections::HashSet;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, bounded};

use crate::core::settings::SearchBackend;

const MAX_DEPTH: usize = 32;
const MAX_HITS: usize = 20_000;
const POLL_BATCH: usize = 500;
const CHANNEL_CAP: usize = 4096;
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
// WinSxS is a component store of 30k+ hard links; walking it takes minutes and nobody searches it.
// The other three are shell bookkeeping folders whose contents are never useful results.
const SKIP_DIRS: [&str; 4] = ["$Recycle.Bin", "System Volume Information", "Recovery", "WinSxS"];

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub path: PathBuf,
    pub name: String,
    pub is_folder: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizeFilter {
    pub greater: bool,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateFilter {
    pub greater: bool,
    pub unix: u64,
    pub year: u16,
    pub month: u16,
    pub day: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KindFilter {
    Files,
    Folders,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub terms: Vec<String>,
    pub exts: Vec<String>,
    pub size: Option<SizeFilter>,
    pub date: Option<DateFilter>,
    pub kind: Option<KindFilter>,
}

pub fn parse_query(input: &str) -> Query {
    let mut query = Query::default();
    for token in input.split_whitespace() {
        let lower = token.to_lowercase();
        if let Some(rest) = lower.strip_prefix("ext:") {
            for piece in rest.split([',', ';']) {
                let ext = piece.trim().trim_start_matches('.');
                let ext = ext.strip_prefix("ext:").unwrap_or(ext);
                if !ext.is_empty() {
                    query.exts.push(ext.to_string());
                }
            }
        } else if let Some(rest) = lower.strip_prefix("size:") {
            if let Some(filter) = parse_size(rest) {
                query.size = Some(filter);
            }
        } else if let Some(rest) = lower.strip_prefix("date:") {
            if let Some(filter) = parse_date(rest) {
                query.date = Some(filter);
            }
        } else if let Some(rest) = lower.strip_prefix("type:") {
            match rest {
                "file" | "files" => query.kind = Some(KindFilter::Files),
                "folder" | "folders" => query.kind = Some(KindFilter::Folders),
                _ => {}
            }
        } else if let Some(rest) = lower.strip_prefix("name:") {
            // Keep the prefix in the term so the matcher can tell "name:report" from "src\report".
            if !rest.is_empty() {
                query.terms.push(lower);
            }
        } else if lower.contains(['\\', '/']) {
            query.terms.push(lower.replace('/', "\\"));
        } else {
            query.terms.push(lower);
        }
    }
    query
}

impl Query {
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
            && self.exts.is_empty()
            && self.size.is_none()
            && self.date.is_none()
            && self.kind.is_none()
    }

    pub fn matches(&self, hit: &SearchHit) -> bool {
        match self.kind {
            Some(KindFilter::Files) if hit.is_folder => return false,
            Some(KindFilter::Folders) if !hit.is_folder => return false,
            _ => {}
        }
        if !self.exts.is_empty() {
            if hit.is_folder {
                return false;
            }
            let ext = crate::shell::enumerate::extract_ext(&hit.name);
            if !self.exts.contains(&ext) {
                return false;
            }
        }
        if let Some(size) = self.size {
            if hit.is_folder {
                return false;
            }
            if size.greater {
                if hit.size <= size.bytes {
                    return false;
                }
            } else if hit.size >= size.bytes {
                return false;
            }
        }
        if let Some(date) = self.date {
            let modified =
                hit.modified.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            if date.greater {
                if modified <= date.unix {
                    return false;
                }
            } else if modified >= date.unix {
                return false;
            }
        }
        if self.terms.is_empty() {
            return true;
        }
        let name = hit.name.to_lowercase();
        let path = if self.terms.iter().any(|t| !t.starts_with("name:") && t.contains('\\')) {
            hit.path.to_string_lossy().to_lowercase()
        } else {
            String::new()
        };
        self.terms.iter().all(|term| term_matches(term, &path, &name))
    }
}

/// `name:` forces a file-name match; a term carrying a path separator matches the full path instead.
fn term_matches(term: &str, path: &str, name: &str) -> bool {
    if let Some(pattern) = term.strip_prefix("name:") {
        return pattern_matches(pattern, name);
    }
    if term.contains('\\') {
        return pattern_matches(term, path);
    }
    pattern_matches(term, name)
}

fn pattern_matches(pattern: &str, name: &str) -> bool {
    if pattern.contains(['*', '?']) { wildcard_matches(pattern, name) } else { name.contains(pattern) }
}

fn wildcard_matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_text = 0usize;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            star_text = t;
            p += 1;
        } else if let Some(s) = star {
            p = s + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

fn parse_size(input: &str) -> Option<SizeFilter> {
    let (greater, rest) = match input.as_bytes().first()? {
        b'>' => (true, &input[1..]),
        b'<' => (false, &input[1..]),
        _ => (true, input),
    };
    let rest = rest.trim();
    let split = rest.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(rest.len());
    let value: f64 = rest[..split].parse().ok()?;
    let mult: u64 = match rest[split..].trim() {
        "" | "b" => 1,
        "kb" => 1024,
        "mb" => 1024 * 1024,
        "gb" => 1024 * 1024 * 1024,
        "tb" => 1024 * 1024 * 1024 * 1024,
        _ => return None,
    };
    Some(SizeFilter { greater, bytes: (value * mult as f64) as u64 })
}

fn parse_date(input: &str) -> Option<DateFilter> {
    let (greater, rest) = match input.as_bytes().first()? {
        b'>' => (true, &input[1..]),
        b'<' => (false, &input[1..]),
        _ => (true, input),
    };
    let (year, month, day) = parse_ymd(rest.trim())?;
    let unix = local_midnight_unix(year, month, day)?;
    Some(DateFilter { greater, unix, year, month, day })
}

fn parse_ymd(input: &str) -> Option<(u16, u16, u16)> {
    let mut parts = input.split(['-', '/']);
    let year: u16 = parts.next()?.parse().ok()?;
    let month: u16 = parts.next()?.parse().ok()?;
    let day: u16 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some((year, month, day))
}

fn local_midnight_unix(year: u16, month: u16, day: u16) -> Option<u64> {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime};

    let local = SYSTEMTIME { wYear: year, wMonth: month, wDay: day, ..Default::default() };
    let mut utc = SYSTEMTIME::default();
    let mut filetime = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    unsafe {
        TzSpecificLocalTimeToSystemTime(None, &local, &mut utc).ok()?;
        SystemTimeToFileTime(&utc, &mut filetime).ok()?;
    }
    let ticks = ((filetime.dwHighDateTime as u64) << 32) | filetime.dwLowDateTime as u64;
    Some((ticks / 10_000_000).saturating_sub(11_644_473_600))
}

fn skip_dir(name: &str, attrs: u32, show_hidden: bool) -> bool {
    if SKIP_DIRS.iter().any(|skip| name.eq_ignore_ascii_case(skip)) {
        return true;
    }
    // Never descend into a reparse point: junctions such as C:\Users\All Users -> ProgramData
    // create cycles and re-walk whole subtrees, and files behind a link are not what "search this
    // folder" means.
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return true;
    }
    !show_hidden && (attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0)
}

const BACKEND_PARALLEL: u8 = 0;
const BACKEND_EVERYTHING: u8 = 1;
const BACKEND_WINDOWS_SEARCH: u8 = 2;

pub struct SearchEngine {
    rx: Option<Receiver<SearchHit>>,
    cancel_flag: Arc<AtomicBool>,
    running_flag: Arc<AtomicBool>,
    active_backend: Arc<AtomicU8>,
}

impl SearchEngine {
    pub fn new() -> Self {
        Self {
            rx: None,
            cancel_flag: Arc::new(AtomicBool::new(true)),
            running_flag: Arc::new(AtomicBool::new(false)),
            active_backend: Arc::new(AtomicU8::new(BACKEND_PARALLEL)),
        }
    }

    #[cfg(test)]
    pub fn start(&mut self, root: &Path, query: &str, show_hidden: bool) {
        self.start_with_backend(root, query, show_hidden, SearchBackend::BuiltIn);
    }

    pub fn start_with_backend(&mut self, root: &Path, raw_query: &str, show_hidden: bool, backend: SearchBackend) {
        self.cancel();
        let query = parse_query(raw_query);
        if query.is_empty() {
            return;
        }
        let (tx, rx) = bounded(CHANNEL_CAP);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let running_flag = Arc::new(AtomicBool::new(true));
        let active_backend = Arc::new(AtomicU8::new(BACKEND_PARALLEL));
        let root = root.to_path_buf();
        let cancel = cancel_flag.clone();
        let running = running_flag.clone();
        let backend_tag = active_backend.clone();
        std::thread::Builder::new()
            .name("lumen-search".into())
            .spawn(move || {
                let handled = match backend {
                    SearchBackend::Auto => {
                        if is_everything_running() && try_everything_search(&root, &query, show_hidden, &tx, &cancel) {
                            backend_tag.store(BACKEND_EVERYTHING, Ordering::Relaxed);
                            true
                        } else {
                            false
                        }
                    }
                    SearchBackend::WindowsSearch => {
                        if try_windows_search(&root, &query, show_hidden, &tx, &cancel) {
                            backend_tag.store(BACKEND_WINDOWS_SEARCH, Ordering::Relaxed);
                            true
                        } else {
                            false
                        }
                    }
                    SearchBackend::BuiltIn => false,
                };
                if !handled && !cancel.load(Ordering::Relaxed) {
                    backend_tag.store(BACKEND_PARALLEL, Ordering::Relaxed);
                    walk_parallel(&root, &query, show_hidden, &tx, &cancel);
                }
                running.store(false, Ordering::SeqCst);
                crate::platform::wake();
            })
            .expect("spawn search thread");
        self.rx = Some(rx);
        self.cancel_flag = cancel_flag;
        self.running_flag = running_flag;
        self.active_backend = active_backend;
    }

    /// Human-readable label of the backend answering the current or last search.
    pub fn active_backend_label(&self) -> &'static str {
        match self.active_backend.load(Ordering::Relaxed) {
            BACKEND_EVERYTHING => "Everything",
            BACKEND_WINDOWS_SEARCH => "Windows 索引",
            _ => "并行搜索",
        }
    }

    pub fn cancel(&mut self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
        // Dropping the receiver also wakes a worker blocked on the full bounded channel, so the
        // thread stops right away instead of waiting for the next poll.
        self.rx = None;
        self.running_flag.store(false, Ordering::SeqCst);
    }

    /// True while the worker walks or while unfetched hits still sit in the channel; the UI must
    /// keep polling until this is false or it drops the last batch.
    pub fn is_running(&self) -> bool {
        self.running_flag.load(Ordering::SeqCst) || self.rx.as_ref().is_some_and(|rx| !rx.is_empty())
    }

    pub fn poll(&mut self) -> Vec<SearchHit> {
        let Some(rx) = &self.rx else { return Vec::new() };
        let mut hits = Vec::with_capacity(POLL_BATCH);
        while hits.len() < POLL_BATCH {
            match rx.try_recv() {
                Ok(hit) => hits.push(hit),
                Err(_) => break,
            }
        }
        hits
    }
}

impl Default for SearchEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SearchEngine {
    fn drop(&mut self) {
        self.cancel();
    }
}

// ---- Built-in parallel multi-threaded filesystem walk ----------------------

struct WalkQueue {
    stack: Vec<(PathBuf, usize)>,
    active: usize,
}

fn walk_parallel(root: &Path, query: &Query, show_hidden: bool, tx: &Sender<SearchHit>, cancel: &Arc<AtomicBool>) {
    let workers = std::thread::available_parallelism().map(|n| n.get().clamp(2, 4)).unwrap_or(2);
    let queue = Arc::new((Mutex::new(WalkQueue { stack: vec![(root.to_path_buf(), 0)], active: 0 }), Condvar::new()));
    let seen = Arc::new(Mutex::new(HashSet::<String>::new()));
    let sent = Arc::new(AtomicUsize::new(0));
    let query = Arc::new(query.clone());

    let mut handles = Vec::with_capacity(workers);
    for idx in 0..workers {
        let queue = queue.clone();
        let seen = seen.clone();
        let sent = sent.clone();
        let query = query.clone();
        let tx = tx.clone();
        let cancel = cancel.clone();
        let handle = std::thread::Builder::new()
            .name(format!("lumen-search-{idx}"))
            .spawn(move || {
                loop {
                    if cancel.load(Ordering::Relaxed) || sent.load(Ordering::Relaxed) >= MAX_HITS {
                        break;
                    }
                    let next = {
                        let (lock, cv) = &*queue;
                        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if cancel.load(Ordering::Relaxed) || sent.load(Ordering::Relaxed) >= MAX_HITS {
                                cv.notify_all();
                                return;
                            }
                            if let Some(item) = guard.stack.pop() {
                                guard.active += 1;
                                break Some(item);
                            }
                            if guard.active == 0 {
                                cv.notify_all();
                                break None;
                            }
                            guard = cv.wait(guard).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    let Some((dir, depth)) = next else { break };

                    let mut subdirs = Vec::new();
                    if let Ok(entries) = std::fs::read_dir(&dir) {
                        for entry in entries.flatten() {
                            if cancel.load(Ordering::Relaxed) || sent.load(Ordering::Relaxed) >= MAX_HITS {
                                break;
                            }
                            let Ok(meta) = entry.metadata() else { continue };
                            let attrs = meta.file_attributes();
                            let name = entry.file_name().to_string_lossy().into_owned();
                            let is_folder = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
                            let path = entry.path();
                            if is_folder && depth < MAX_DEPTH && !skip_dir(&name, attrs, show_hidden) {
                                subdirs.push((path.clone(), depth + 1));
                            }
                            let hit = SearchHit {
                                path,
                                name,
                                is_folder,
                                size: if is_folder { 0 } else { meta.len() },
                                modified: meta.modified().ok(),
                            };
                            if !query.matches(&hit) {
                                continue;
                            }
                            let key = hit.path.to_string_lossy().to_lowercase();
                            let unique = seen.lock().unwrap_or_else(|e| e.into_inner()).insert(key);
                            if !unique {
                                continue;
                            }
                            let count = sent.fetch_add(1, Ordering::Relaxed) + 1;
                            if count > MAX_HITS {
                                break;
                            }
                            if tx.send(hit).is_err() {
                                cancel.store(true, Ordering::Relaxed);
                                break;
                            }
                            if count.is_multiple_of(64) {
                                crate::platform::wake();
                            }
                        }
                    }

                    let (lock, cv) = &*queue;
                    let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                    guard.active = guard.active.saturating_sub(1);
                    if !cancel.load(Ordering::Relaxed) && sent.load(Ordering::Relaxed) < MAX_HITS {
                        guard.stack.extend(subdirs);
                    }
                    cv.notify_all();
                }
            })
            .expect("spawn search worker");
        handles.push(handle);
    }
    for handle in handles {
        let _ = handle.join();
    }
}

// ---- Everything IPC v2 auto-detection & scoped query -----------------------

const EVERYTHING_IPC_COPYDATA_QUERY2W: usize = 18;
const EVERYTHING_IPC_QUERY2_REQUEST_FULL_PATH_AND_NAME: u32 = 0x0000_0004;
const EVERYTHING_IPC_QUERY2_REQUEST_SIZE: u32 = 0x0000_0010;
const EVERYTHING_IPC_QUERY2_REQUEST_DATE_MODIFIED: u32 = 0x0000_0040;
const EVERYTHING_IPC_QUERY2_REQUEST_ATTRIBUTES: u32 = 0x0000_0100;
const EVERYTHING_IPC_FOLDER: u32 = 0x0000_0001;
const EVERYTHING_REPLY_ID: u32 = 0x4C55_4D45; // 'LUME'

fn find_everything_hwnd() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    use windows::core::{PCWSTR, w};
    for class in [w!("EVERYTHING_TASKBAR_NOTIFICATION"), w!("EVERYTHING_TASKBAR_NOTIFICATION_(1.5a)")] {
        if let Ok(hwnd) = unsafe { FindWindowW(class, PCWSTR::null()) }
            && !hwnd.0.is_null()
        {
            return Some(hwnd);
        }
    }
    None
}

/// Fast non-blocking check whether voidtools Everything is currently running and
/// ready to answer IPC queries.
pub fn is_everything_running() -> bool {
    find_everything_hwnd().is_some()
}

/// Formats a scoped Everything query string for `root` and `query`.
fn build_everything_query_string(root: &Path, query: &Query) -> String {
    let mut root_str = root.to_string_lossy().replace('/', "\\");
    if !root_str.ends_with('\\') {
        root_str.push('\\');
    }
    let mut parts = vec![format!("\"{root_str}\"")];
    for term in &query.terms {
        let clean = term.strip_prefix("name:").unwrap_or(term);
        if clean.contains(' ') {
            parts.push(format!("\"{clean}\""));
        } else {
            parts.push(clean.to_string());
        }
    }
    if !query.exts.is_empty() {
        parts.push(format!("ext:{}", query.exts.join(";")));
    }
    if let Some(size) = query.size {
        let op = if size.greater { '>' } else { '<' };
        parts.push(format!("size:{op}{}", size.bytes));
    }
    if let Some(date) = query.date {
        // `date:>YYYY-MM-DD` matches timestamps after local midnight of that day, so query
        // Everything with `dm:>=YYYY-MM-DD` and let `Query::matches` enforce the exact boundary.
        let op = if date.greater { ">=" } else { "<" };
        parts.push(format!("dm:{op}{:04}-{:02}-{:02}", date.year, date.month, date.day));
    }
    match query.kind {
        Some(KindFilter::Files) => parts.push("file:".to_string()),
        Some(KindFilter::Folders) => parts.push("folder:".to_string()),
        None if query.size.is_some() => parts.push("file:".to_string()),
        None => {}
    }
    parts.join(" ")
}

fn filetime_u64_to_system_time(ft: u64) -> Option<SystemTime> {
    if ft == u64::MAX || ft == 0 {
        return None;
    }
    let secs = ft / 10_000_000;
    let unix_secs = secs.checked_sub(11_644_473_600)?;
    let nanos = ((ft % 10_000_000) * 100) as u32;
    Some(UNIX_EPOCH + Duration::new(unix_secs, nanos))
}

/// Decodes a raw `EVERYTHING_IPC_LIST2` payload returned by Everything IPC v2.
fn parse_everything_list2(bytes: &[u8]) -> Option<Vec<(SearchHit, u32)>> {
    if bytes.len() < 20 {
        return None;
    }
    let read_u32 = |off: usize| -> Option<u32> {
        let s = bytes.get(off..off + 4)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    };
    let read_u64 = |off: usize| -> Option<u64> {
        let s = bytes.get(off..off + 8)?;
        Some(u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
    };

    let numitems = read_u32(4)? as usize;
    let request_flags = read_u32(12)?;
    let items_end = 20usize.checked_add(numitems.checked_mul(8)?)?;
    if items_end > bytes.len() {
        return None;
    }
    let mut results = Vec::with_capacity(numitems.min(MAX_HITS));
    for i in 0..numitems.min(MAX_HITS) {
        let item_off = 20 + i * 8;
        let flags = read_u32(item_off)?;
        let mut cursor = read_u32(item_off + 4)? as usize;
        let is_folder = flags & EVERYTHING_IPC_FOLDER != 0;

        let mut full_path: Option<String> = None;
        let mut size: u64 = 0;
        let mut modified: Option<SystemTime> = None;
        let mut attrs: u32 = if is_folder { FILE_ATTRIBUTE_DIRECTORY } else { 0 };

        // Read fields in ascending `EVERYTHING_IPC_QUERY2_REQUEST_*` bit order.
        let read_wstr = |pos: &mut usize| -> Option<String> {
            let char_len = read_u32(*pos)? as usize;
            *pos += 4;
            let byte_len = char_len.checked_mul(2)?;
            let slice = bytes.get(*pos..*pos + byte_len)?;
            let units: Vec<u16> = slice.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
            // Skip the null terminator WCHAR as well.
            *pos = pos.checked_add(byte_len + 2)?;
            Some(String::from_utf16_lossy(&units))
        };

        if request_flags & 0x01 != 0 {
            let _ = read_wstr(&mut cursor)?;
        }
        if request_flags & 0x02 != 0 {
            let _ = read_wstr(&mut cursor)?;
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_FULL_PATH_AND_NAME != 0 {
            full_path = Some(read_wstr(&mut cursor)?);
        }
        if request_flags & 0x08 != 0 {
            let _ = read_wstr(&mut cursor)?;
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_SIZE != 0 {
            let raw = read_u64(cursor)?;
            cursor += 8;
            if raw != u64::MAX && !is_folder {
                size = raw;
            }
        }
        if request_flags & 0x20 != 0 {
            cursor += 8;
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_DATE_MODIFIED != 0 {
            let raw = read_u64(cursor)?;
            cursor += 8;
            modified = filetime_u64_to_system_time(raw);
        }
        if request_flags & 0x80 != 0 {
            cursor += 8;
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_ATTRIBUTES != 0 {
            attrs = read_u32(cursor)?;
        }

        if let Some(p) = full_path {
            let path = PathBuf::from(p);
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !name.is_empty() {
                results.push((SearchHit { path, name, is_folder, size, modified }, attrs));
            }
        }
    }
    Some(results)
}

std::thread_local! {
    static EVERYTHING_REPLY: std::cell::RefCell<Option<Vec<(SearchHit, u32)>>> = const { std::cell::RefCell::new(None) };
}

unsafe extern "system" fn everything_reply_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::Win32::UI::WindowsAndMessaging::{DefWindowProcW, WM_COPYDATA};
    if msg == WM_COPYDATA && lparam.0 != 0 {
        let cds = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
        if cds.dwData == EVERYTHING_REPLY_ID as usize && !cds.lpData.is_null() && cds.cbData >= 20 {
            let bytes = unsafe { std::slice::from_raw_parts(cds.lpData.cast::<u8>(), cds.cbData as usize) };
            let parsed = parse_everything_list2(bytes);
            EVERYTHING_REPLY.with(|slot| *slot.borrow_mut() = parsed);
            return LRESULT(1);
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

static EVERYTHING_CLASS_REG: OnceLock<()> = OnceLock::new();

fn try_everything_search(
    root: &Path,
    query: &Query,
    show_hidden: bool,
    tx: &Sender<SearchHit>,
    cancel: &AtomicBool,
) -> bool {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, HWND_MESSAGE, MSG, PM_REMOVE, PeekMessageW, RegisterClassW,
        SMTO_ABORTIFHUNG, SendMessageTimeoutW, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COPYDATA, WNDCLASSW,
    };
    use windows::core::{PCWSTR, w};

    let Some(everything_hwnd) = find_everything_hwnd() else { return false };
    let class_name = w!("LumenEverythingIpcReply");
    let hinstance = unsafe { GetModuleHandleW(PCWSTR::null()) }.unwrap_or_default();
    EVERYTHING_CLASS_REG.get_or_init(|| {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(everything_reply_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        unsafe {
            let _ = RegisterClassW(&wc);
        }
    });

    let Ok(reply_hwnd) = (unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance.into()),
            None,
        )
    }) else {
        return false;
    };

    EVERYTHING_REPLY.with(|slot| *slot.borrow_mut() = None);

    let search_str = build_everything_query_string(root, query);
    let wide: Vec<u16> = std::ffi::OsStr::new(&search_str).encode_wide().chain(Some(0)).collect();
    let request_flags = EVERYTHING_IPC_QUERY2_REQUEST_FULL_PATH_AND_NAME
        | EVERYTHING_IPC_QUERY2_REQUEST_SIZE
        | EVERYTHING_IPC_QUERY2_REQUEST_DATE_MODIFIED
        | EVERYTHING_IPC_QUERY2_REQUEST_ATTRIBUTES;

    // Pack EVERYTHING_IPC_QUERY2 (7 x u32 = 28 bytes) + UTF-16LE search_string.
    let mut buf = Vec::with_capacity(28 + wide.len() * 2);
    buf.extend_from_slice(&(reply_hwnd.0 as usize as u32).to_le_bytes()); // reply_hwnd
    buf.extend_from_slice(&EVERYTHING_REPLY_ID.to_le_bytes()); // reply_copydata_message
    buf.extend_from_slice(&0u32.to_le_bytes()); // search_flags
    buf.extend_from_slice(&0u32.to_le_bytes()); // offset
    buf.extend_from_slice(&(MAX_HITS as u32).to_le_bytes()); // max_results
    buf.extend_from_slice(&request_flags.to_le_bytes()); // request_flags
    buf.extend_from_slice(&1u32.to_le_bytes()); // sort_type (NAME_ASCENDING)
    for ch in &wide {
        buf.extend_from_slice(&ch.to_le_bytes());
    }

    let cds = COPYDATASTRUCT {
        dwData: EVERYTHING_IPC_COPYDATA_QUERY2W,
        cbData: buf.len() as u32,
        lpData: buf.as_mut_ptr().cast(),
    };
    let mut send_res: usize = 0;
    let sent_ok = unsafe {
        SendMessageTimeoutW(
            everything_hwnd,
            WM_COPYDATA,
            WPARAM(reply_hwnd.0 as usize),
            LPARAM(&cds as *const COPYDATASTRUCT as isize),
            SMTO_ABORTIFHUNG,
            1500,
            Some(&mut send_res),
        )
    };

    if sent_ok.0 != 0 {
        let deadline = Instant::now() + Duration::from_millis(1200);
        while Instant::now() < deadline && !cancel.load(Ordering::Relaxed) {
            let ready = EVERYTHING_REPLY.with(|slot| slot.borrow().is_some());
            if ready {
                break;
            }
            let mut msg = MSG::default();
            while unsafe { PeekMessageW(&mut msg, Some(reply_hwnd), 0, 0, PM_REMOVE) }.as_bool() {
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if EVERYTHING_REPLY.with(|slot| slot.borrow().is_some()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
    }
    unsafe {
        let _ = DestroyWindow(reply_hwnd);
    }

    let Some(items) = EVERYTHING_REPLY.with(|slot| slot.borrow_mut().take()) else {
        return false;
    };
    // If Everything returned 0 items for an unindexed/removable path, let parallel walk verify.
    if items.is_empty() {
        return false;
    }

    let mut root_norm = root.to_string_lossy().to_lowercase();
    if !root_norm.ends_with('\\') {
        root_norm.push('\\');
    }
    let mut seen = HashSet::new();
    let mut emitted = 0usize;
    for (hit, attrs) in items {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        let path_low = hit.path.to_string_lossy().to_lowercase();
        if !path_low.starts_with(&root_norm) {
            continue;
        }
        if !show_hidden && (attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0) {
            continue;
        }
        if !query.matches(&hit) || !seen.insert(path_low) {
            continue;
        }
        if tx.send(hit).is_err() {
            return true;
        }
        emitted += 1;
        if emitted.is_multiple_of(64) {
            crate::platform::wake();
        }
    }
    emitted > 0
}

// ---- Optional Windows Search Index backend (SystemIndex via ADODB) ---------

// ---- Native COM Windows Search Index backend (ISearchFolderItemFactory) ----

fn build_windows_search_aqs(query: &Query) -> String {
    let mut parts = Vec::new();
    for term in &query.terms {
        if !term.starts_with("name:") && term.contains('\\') {
            // Subpath wildcard/segment matching cannot be expressed faithfully in AQS;
            // return empty so `try_windows_search` falls back to parallel walk.
            return String::new();
        }
        let clean = term.strip_prefix("name:").unwrap_or(term);
        if clean.is_empty() || clean == "*" {
            continue;
        }
        if clean.contains(' ') {
            parts.push(format!("\"{clean}\""));
        } else {
            parts.push(clean.to_string());
        }
    }
    match query.exts.as_slice() {
        [] => {}
        [single] => parts.push(format!("ext:.{single}")),
        many => {
            let joined = many.iter().map(|ext| format!("ext:.{ext}")).collect::<Vec<_>>().join(" OR ");
            parts.push(format!("({joined})"));
        }
    }
    if let Some(size) = query.size {
        let op = if size.greater { '>' } else { '<' };
        parts.push(format!("System.Size:{op}{}", size.bytes));
    }
    if let Some(date) = query.date {
        let op = if date.greater { ">=" } else { "<" };
        parts.push(format!("System.DateModified:{op}{:04}-{:02}-{:02}", date.year, date.month, date.day));
    }
    match query.kind {
        Some(KindFilter::Folders) => parts.push("kind:folder".to_string()),
        Some(KindFilter::Files) => {
            if parts.is_empty() {
                return String::new();
            }
            parts.push("NOT kind:folder".to_string());
        }
        None => {}
    }
    parts.join(" ")
}

fn try_windows_search(
    root: &Path,
    query: &Query,
    show_hidden: bool,
    tx: &Sender<SearchHit>,
    cancel: &AtomicBool,
) -> bool {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
    };
    use windows::Win32::System::Search::{ICondition, IQueryParser, IQueryParserManager, QueryParserManager};
    use windows::Win32::UI::Shell::{
        BHID_EnumItems, IEnumShellItems, ISearchFolderItemFactory, IShellItem, IShellItemArray,
        SHCreateItemFromParsingName, SHCreateShellItemArrayFromShellItem, SIGDN_FILESYSPATH, SearchFolderItemFactory,
    };
    use windows::core::{HSTRING, PCWSTR, w};

    let aqs = build_windows_search_aqs(query);
    if aqs.trim().is_empty() {
        return false;
    }

    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let need_uninit = hr.is_ok();
    let result = (|| -> windows::core::Result<bool> {
        let qpm: IQueryParserManager = unsafe { CoCreateInstance(&QueryParserManager, None, CLSCTX_INPROC_SERVER) }?;
        let parser: IQueryParser = unsafe { qpm.CreateLoadedParser(w!("SystemIndex"), 0x0409) }?;
        let aqs_w = HSTRING::from(&aqs);
        let solution = unsafe { parser.Parse(PCWSTR(aqs_w.as_ptr()), None) }?;
        let mut cond: Option<ICondition> = None;
        unsafe { solution.GetQuery(Some(&mut cond), None) }?;
        let Some(cond) = cond else {
            return Ok(false);
        };

        let root_w = HSTRING::from(root.as_os_str());
        let root_item: IShellItem = unsafe { SHCreateItemFromParsingName(PCWSTR(root_w.as_ptr()), None) }?;
        let scope: IShellItemArray = unsafe { SHCreateShellItemArrayFromShellItem(&root_item) }?;
        let factory: ISearchFolderItemFactory =
            unsafe { CoCreateInstance(&SearchFolderItemFactory, None, CLSCTX_INPROC_SERVER) }?;
        unsafe {
            factory.SetScope(&scope)?;
            factory.SetCondition(&cond)?;
        }
        let search_folder: IShellItem = unsafe { factory.GetShellItem() }?;
        let enumerator: IEnumShellItems = unsafe { search_folder.BindToHandler(None, &BHID_EnumItems) }?;

        let mut seen = HashSet::new();
        let mut emitted = 0usize;
        let mut batch: [Option<IShellItem>; 64] = std::array::from_fn(|_| None);
        loop {
            if cancel.load(Ordering::Relaxed) || emitted >= MAX_HITS {
                return Ok(true);
            }
            let mut fetched = 0u32;
            let status = unsafe { enumerator.Next(&mut batch, Some(&mut fetched)) };
            if fetched == 0 {
                break;
            }
            for item in batch[..fetched as usize].iter_mut().filter_map(Option::take) {
                if cancel.load(Ordering::Relaxed) || emitted >= MAX_HITS {
                    return Ok(true);
                }
                let Ok(pwstr) = (unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }) else {
                    continue;
                };
                let path_str = unsafe { pwstr.to_string() }.unwrap_or_default();
                unsafe { CoTaskMemFree(Some(pwstr.0.cast())) };
                if path_str.is_empty() {
                    continue;
                }
                let path = PathBuf::from(&path_str);
                let Ok(meta) = std::fs::metadata(&path) else { continue };
                let attrs = meta.file_attributes();
                if !show_hidden && (attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0) {
                    continue;
                }
                let is_folder = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
                let hit = SearchHit {
                    path,
                    name,
                    is_folder,
                    size: if is_folder { 0 } else { meta.len() },
                    modified: meta.modified().ok(),
                };
                if !query.matches(&hit) || !seen.insert(hit.path.to_string_lossy().to_lowercase()) {
                    continue;
                }
                if tx.send(hit).is_err() {
                    return Ok(true);
                }
                emitted += 1;
                if emitted.is_multiple_of(64) {
                    crate::platform::wake();
                }
            }
            if status.is_err() {
                break;
            }
        }
        // Only suppress fallback to parallel walk when Windows Search actually delivered
        // at least one hit that survived `query.matches(&hit)` filtering.
        Ok(emitted > 0)
    })();

    if need_uninit {
        unsafe { CoUninitialize() };
    }
    result.unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn hit(name: &str, is_folder: bool, size: u64, modified_secs: u64) -> SearchHit {
        SearchHit {
            path: PathBuf::from(format!("C:\\test\\{name}")),
            name: name.to_string(),
            is_folder,
            size,
            modified: Some(UNIX_EPOCH + Duration::from_secs(modified_secs)),
        }
    }

    fn drain(engine: &mut SearchEngine) -> Vec<SearchHit> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut hits = Vec::new();
        loop {
            hits.extend(engine.poll());
            if !engine.is_running() {
                return hits;
            }
            assert!(Instant::now() < deadline, "search did not finish, {} hits so far", hits.len());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn sorted_names(hits: &[SearchHit]) -> Vec<String> {
        let mut names: Vec<String> = hits.iter().map(|h| h.name.clone()).collect();
        names.sort();
        names
    }

    fn test_root(tag: &str) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("tmp")
            .join(format!("search-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn hide(path: &Path) {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, SetFileAttributesW};
        use windows::core::PCWSTR;

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe { SetFileAttributesW(PCWSTR(wide.as_ptr()), FILE_ATTRIBUTE_HIDDEN) }.unwrap();
    }

    #[test]
    fn parse_terms_and_filters() {
        let query = parse_query("Report *2024*  ext:RS,png size:>1MB date:<2024-06-01 type:file");
        assert_eq!(query.terms, vec!["report", "*2024*"]);
        assert_eq!(query.exts, vec!["rs", "png"]);
        assert_eq!(query.size, Some(SizeFilter { greater: true, bytes: 1024 * 1024 }));
        let date = query.date.unwrap();
        assert!(!date.greater);
        assert!((date.unix as i64 - 1_717_200_000).abs() < 86_400);
        assert_eq!(query.kind, Some(KindFilter::Files));
        assert_eq!(parse_query("ext:rs,ext:png").exts, vec!["rs", "png"]);
        assert_eq!(parse_query("type:folder").kind, Some(KindFilter::Folders));
        assert!(parse_query("  ").is_empty());
    }

    #[test]
    fn parse_size_units() {
        assert_eq!(parse_query("size:<10KB").size, Some(SizeFilter { greater: false, bytes: 10 * 1024 }));
        assert_eq!(parse_query("size:>1GB").size, Some(SizeFilter { greater: true, bytes: 1024 * 1024 * 1024 }));
        assert_eq!(parse_query("size:>500").size, Some(SizeFilter { greater: true, bytes: 500 }));
        assert_eq!(parse_query("size:1.5MB").size, Some(SizeFilter { greater: true, bytes: 1_572_864 }));
    }

    #[test]
    fn matching_rules() {
        let file = hit("report.csv", false, 2048, 1_700_000_000);
        assert!(parse_query("report").matches(&file));
        assert!(parse_query("REPORT").matches(&file));
        assert!(parse_query("*.csv").matches(&file));
        assert!(!parse_query("*.rs").matches(&file));
        assert!(parse_query("rep*t.csv").matches(&file));
        assert!(!parse_query("rep ?csv").matches(&file));
        assert!(parse_query("report *.csv").matches(&file));
        assert!(parse_query("ext:csv").matches(&file));
        assert!(!parse_query("ext:rs").matches(&file));
        assert!(parse_query("size:<10KB").matches(&file));
        assert!(!parse_query("size:>10KB").matches(&file));
        assert!(parse_query("date:>2023-01-01").matches(&file));
        assert!(!parse_query("date:<2023-01-01").matches(&file));
        assert!(parse_query("type:file").matches(&file));
        assert!(!parse_query("type:folder").matches(&file));

        let folder = hit("docs.csv", true, 0, 1_700_000_000);
        assert!(!parse_query("ext:csv").matches(&folder));
        assert!(!parse_query("size:<10KB").matches(&folder));
        assert!(parse_query("type:folder").matches(&folder));
        assert!(parse_query("docs").matches(&folder));
    }

    #[test]
    fn path_terms_match_full_path() {
        let mut file = hit("report.csv", false, 2048, 1_700_000_000);
        file.path = PathBuf::from("C:\\test\\sub\\report.csv");
        assert!(parse_query(r"*\sub\*.csv").matches(&file));
        assert!(parse_query(r"TEST\SUB").matches(&file));
        assert!(parse_query("test/sub/report.csv").matches(&file));
        assert!(parse_query(r"*\SUB\*.csv report").matches(&file));
        assert!(!parse_query(r"*\other\*.csv").matches(&file));
        assert!(!parse_query("sub").matches(&file), "a term without a separator still matches the name only");
        assert!(parse_query("name:report").matches(&file));
        assert!(!parse_query(r"name:*\sub\*").matches(&file), "name: never matches the path");
    }

    #[test]
    fn engine_finds_matching_files() {
        let root = test_root("basic");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::create_dir_all(root.join("$Recycle.Bin")).unwrap();
        std::fs::write(root.join("alpha.txt"), b"a").unwrap();
        std::fs::write(root.join("beta.rs"), b"bb").unwrap();
        std::fs::write(root.join("sub/gamma.rs"), b"ccc").unwrap();
        std::fs::write(root.join(".hidden/delta.rs"), b"d").unwrap();
        std::fs::write(root.join("$Recycle.Bin/junk.rs"), b"junk").unwrap();
        hide(&root.join(".hidden"));

        let mut engine = SearchEngine::new();
        engine.start(&root, "*.rs", false);
        assert!(engine.is_running());
        let hits = drain(&mut engine);
        assert!(!engine.is_running());
        assert_eq!(sorted_names(&hits), vec!["beta.rs", "gamma.rs"], "hits: {hits:?}");

        engine.start(&root, "txt", false);
        engine.cancel();
        assert!(!engine.is_running());
        assert!(engine.poll().is_empty());
        engine.start(&root, "   ", false);
        assert!(!engine.is_running());
        std::thread::sleep(Duration::from_millis(50));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn pending_hits_keep_engine_running() {
        let root = test_root("pending");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..POLL_BATCH + 100 {
            std::fs::write(root.join(format!("f{i}.txt")), b"x").unwrap();
        }

        let mut engine = SearchEngine::new();
        engine.start(&root, "*", false);
        // Wait until the worker has queued all hits so the first `poll()` reads
        // exactly `POLL_BATCH` and leaves the remaining 100 queued in the channel.
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.running_flag.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "engine did not finish walking");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(engine.is_running(), "queued hits must keep engine running after walk finishes");
        let first_batch = engine.poll();
        assert_eq!(first_batch.len(), POLL_BATCH, "poll returns at most one batch");
        assert!(engine.is_running(), "remaining queued hits must keep engine running");
        let mut hits = first_batch;
        hits.extend(drain(&mut engine));
        assert_eq!(hits.len(), POLL_BATCH + 100);
        assert!(!engine.is_running());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn does_not_descend_into_reparse_points() {
        let root = test_root("reparse");
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("target/inner.rs"), b"i").unwrap();
        std::fs::write(root.join("visible.rs"), b"v").unwrap();
        // Junction back to the search root: following it would re-walk the same tree.
        let out = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(root.join("link"))
            .arg(&root)
            .output()
            .unwrap();
        assert!(out.status.success(), "mklink /J failed: {}", String::from_utf8_lossy(&out.stderr));

        let mut engine = SearchEngine::new();
        engine.start(&root, "*.rs", false);
        assert_eq!(sorted_names(&drain(&mut engine)), vec!["inner.rs", "visible.rs"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn hidden_dirs_respect_show_hidden() {
        let root = test_root("hidden");
        std::fs::create_dir_all(root.join("visible")).unwrap();
        std::fs::create_dir_all(root.join(".dotdir")).unwrap();
        std::fs::create_dir_all(root.join("attrdir")).unwrap();
        std::fs::write(root.join("visible/visible.rs"), b"v").unwrap();
        std::fs::write(root.join(".dotdir/dot.rs"), b"d").unwrap();
        std::fs::write(root.join("attrdir/attr.rs"), b"a").unwrap();
        hide(&root.join("attrdir"));

        let mut engine = SearchEngine::new();
        engine.start(&root, "*.rs", false);
        // Dot-prefixed directories without FILE_ATTRIBUTE_HIDDEN are visible on Windows,
        // matching `enumerate.rs` and Windows Explorer.
        assert_eq!(sorted_names(&drain(&mut engine)), vec!["dot.rs", "visible.rs"]);

        engine.start(&root, "*.rs", true);
        assert_eq!(sorted_names(&drain(&mut engine)), vec!["attr.rs", "dot.rs", "visible.rs"]);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn pre_1970_date_filter_saturates_without_underflow() {
        let q = parse_query("date:<1960-01-01");
        let date = q.date.expect("pre-1970 date should parse safely");
        assert!(!date.greater);
        assert_eq!(date.unix, 0);
        assert_eq!((date.year, date.month, date.day), (1960, 1, 1));
        assert_eq!(local_midnight_unix(1969, 12, 31), Some(0));
    }

    #[test]
    fn everything_query_includes_size_and_date_filters() {
        let q = parse_query("report ext:rs,md size:>2MB date:<2024-06-01");
        let built = build_everything_query_string(Path::new(r"C:\Work"), &q);
        assert!(built.contains("size:>2097152"), "got: {built}");
        assert!(built.contains("dm:<2024-06-01"), "got: {built}");
        assert!(built.contains("file:"), "got: {built}");

        let q2 = parse_query("size:<500 date:>2025-01-15");
        let built2 = build_everything_query_string(Path::new(r"D:\Logs"), &q2);
        assert!(built2.contains("size:<500"), "got: {built2}");
        assert!(built2.contains("dm:>=2025-01-15"), "got: {built2}");
    }

    #[test]
    fn windows_search_aqs_covers_size_date_and_unrepresentable_fallback() {
        let q = parse_query("size:>1MB date:<2024-06-01");
        let aqs = build_windows_search_aqs(&q);
        assert!(aqs.contains("System.Size:>1048576"), "got: {aqs}");
        assert!(aqs.contains("System.DateModified:<2024-06-01"), "got: {aqs}");

        let q_gt = parse_query("date:>2024-06-01");
        let aqs_gt = build_windows_search_aqs(&q_gt);
        assert!(aqs_gt.contains("System.DateModified:>=2024-06-01"), "got: {aqs_gt}");

        // Unrepresentable queries return empty so `try_windows_search` falls back to parallel walk.
        assert!(build_windows_search_aqs(&parse_query("type:file")).is_empty());
        assert!(build_windows_search_aqs(&parse_query("*")).is_empty());
        assert!(build_windows_search_aqs(&parse_query(r"*\sub\*.rs")).is_empty());
        assert!(build_windows_search_aqs(&parse_query(r"src\report.rs report")).is_empty());

        // Verify Windows SystemIndex IQueryParser accepts our emitted AQS syntax.
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
        };
        use windows::Win32::System::Search::{ICondition, IQueryParser, IQueryParserManager, QueryParserManager};
        use windows::core::{HSTRING, PCWSTR, w};

        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let need_uninit = hr.is_ok();
        let qpm: windows::core::Result<IQueryParserManager> =
            unsafe { CoCreateInstance(&QueryParserManager, None, CLSCTX_INPROC_SERVER) };
        if let Ok(qpm) = qpm
            && let Ok(parser) = unsafe { qpm.CreateLoadedParser::<_, IQueryParser>(w!("SystemIndex"), 0x0409) }
        {
            for sample in [aqs.as_str(), aqs_gt.as_str(), "report (ext:.pdf OR ext:.md) kind:folder"] {
                let w = HSTRING::from(sample);
                let sol = unsafe { parser.Parse(PCWSTR(w.as_ptr()), None) }.expect("IQueryParser::Parse");
                let mut cond: Option<ICondition> = None;
                unsafe { sol.GetQuery(Some(&mut cond), None) }.expect("GetQuery");
                assert!(cond.is_some(), "expected valid ICondition for AQS {sample:?}");
            }
        }
        if need_uninit {
            unsafe { CoUninitialize() };
        }
    }

    #[test]
    fn everything_query_and_list2_parser_roundtrip() {
        let q = parse_query("report ext:rs,md type:file");
        let built = build_everything_query_string(Path::new(r"C:\Work"), &q);
        assert!(built.contains(r#""C:\Work\""#));
        assert!(built.contains("report"));
        assert!(built.contains("ext:rs;md"));
        assert!(built.contains("file:"));

        // Build a synthetic EVERYTHING_IPC_LIST2 binary payload with 1 item.
        let path_w: Vec<u16> = std::ffi::OsStr::new(r"C:\Work\report.rs").encode_wide().collect();
        let req_flags = EVERYTHING_IPC_QUERY2_REQUEST_FULL_PATH_AND_NAME
            | EVERYTHING_IPC_QUERY2_REQUEST_SIZE
            | EVERYTHING_IPC_QUERY2_REQUEST_DATE_MODIFIED
            | EVERYTHING_IPC_QUERY2_REQUEST_ATTRIBUTES;
        let data_offset = 28u32; // 20 header + 8 item entry
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u32.to_le_bytes()); // totitems
        buf.extend_from_slice(&1u32.to_le_bytes()); // numitems
        buf.extend_from_slice(&0u32.to_le_bytes()); // offset
        buf.extend_from_slice(&req_flags.to_le_bytes()); // request_flags
        buf.extend_from_slice(&1u32.to_le_bytes()); // sort_type
        buf.extend_from_slice(&0u32.to_le_bytes()); // item.flags (file)
        buf.extend_from_slice(&data_offset.to_le_bytes()); // item.data_offset
        buf.extend_from_slice(&(path_w.len() as u32).to_le_bytes());
        for ch in &path_w {
            buf.extend_from_slice(&ch.to_le_bytes());
        }
        buf.extend_from_slice(&0u16.to_le_bytes()); // null terminator
        buf.extend_from_slice(&4096u64.to_le_bytes()); // size
        buf.extend_from_slice(&133_500_000_000_000_000u64.to_le_bytes()); // filetime
        buf.extend_from_slice(&0x20u32.to_le_bytes()); // FILE_ATTRIBUTE_ARCHIVE

        let items = parse_everything_list2(&buf).expect("valid list2 payload");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].0.name, "report.rs");
        assert_eq!(items[0].0.size, 4096);
        assert!(!items[0].0.is_folder);
        assert!(items[0].0.modified.is_some());
        assert_eq!(items[0].1, 0x20);
    }

    #[test]
    fn windows_search_aqs_formats_terms_and_exts() {
        let q = parse_query("report ext:pdf,md type:folder");
        let aqs = build_windows_search_aqs(&q);
        assert!(aqs.contains("report"));
        assert!(aqs.contains("ext:.pdf"));
        assert!(aqs.contains("ext:.md"));
        assert!(aqs.contains("kind:folder"));
    }
}
