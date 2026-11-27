//! Shell icon and thumbnail service: viewport-prioritized four-thread COM worker
//! pool plus a true-LRU, byte-budgeted egui texture cache.
//!
//! Improvements over naive FIFO icon loading:
//! - LIFO viewport-first queue with folder generation eviction so rapid scrolling
//!   through 10k photos or navigating away immediately prioritizes visible items.
//! - True LRU eviction using lock-free atomic access stamps.
//! - Per-frame GPU upload budget (`MAX_UPLOADS_PER_FRAME`) for smooth 60/120fps.
//! - Thumbnail stamp (`mtime ^ size`) invalidates stale cache entries on edits.
//! - Supports `SHGFI_PIDL` icons for virtual items (Recycle Bin, MTP, Control Panel)
//!   and folder thumbnails via `IShellItemImageFactory`.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::{OsStr, c_void};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, ensure};
use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    GetDIBits, GetObjectW, HBITMAP, HDC, HGDIOBJ, RGBQUAD,
};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize, IBindCtx};
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Shell::{
    IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName, SHFILEINFOW, SHGFI_FLAGS, SHGFI_ICON,
    SHGFI_LARGEICON, SHGFI_PIDL, SHGFI_SMALLICON, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW,
    SHGetImageList, SHIL_EXTRALARGE, SHIL_JUMBO, SHIL_LARGE, SHIL_SMALL, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
    SIIGBF_THUMBNAILONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::core::{Interface, PCWSTR};

use super::location::Pidl;

const WORKER_THREADS: usize = 4;
const CACHE_BYTE_BUDGET: usize = 48 * 1024 * 1024;
const CACHE_ENTRY_LIMIT: usize = 4096;
const MAX_UPLOADS_PER_FRAME: usize = 12;
const PENDING_REPAINT: Duration = Duration::from_millis(16);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IconKey {
    pub path: Option<PathBuf>,
    /// Serialized PIDL bytes for virtual items without a filesystem path.
    pub pidl_bytes: Option<Arc<[u8]>>,
    pub ext: String,
    pub is_folder: bool,
    pub size: u32,
    pub thumbnail: bool,
    /// Cache-busting stamp (`mtime ^ size`) for thumbnails; 0 for plain icons.
    pub stamp: u64,
}

impl IconKey {
    pub fn folder(path: impl Into<PathBuf>, size: u32) -> Self {
        Self {
            path: Some(path.into()),
            pidl_bytes: None,
            ext: String::new(),
            is_folder: true,
            size,
            thumbnail: false,
            stamp: 0,
        }
    }

    pub fn file_type(ext: impl Into<String>, size: u32) -> Self {
        Self { path: None, pidl_bytes: None, ext: ext.into(), is_folder: false, size, thumbnail: false, stamp: 0 }
    }
}

struct Job {
    id: u64,
    generation: u64,
    key: IconKey,
}

struct Done {
    id: u64,
    image: Option<ColorImage>,
}

struct CachedIcon {
    key: IconKey,
    texture: Option<TextureHandle>,
    bytes: usize,
    completed: bool,
    last_used: AtomicU64,
}

struct JobQueue {
    stack: Vec<Job>,
    current_gen: u64,
    closed: bool,
}

struct SharedQueue {
    inner: Mutex<JobQueue>,
    cv: Condvar,
}

impl SharedQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(JobQueue { stack: Vec::new(), current_gen: 0, closed: false }),
            cv: Condvar::new(),
        })
    }

    fn push(&self, job: Job) -> Vec<u64> {
        let mut q = self.inner.lock().unwrap();
        // Bound pending stack size: drop oldest off-screen items when user scrolls fast.
        let dropped = if q.stack.len() >= 512 { q.stack.drain(0..128).map(|job| job.id).collect() } else { Vec::new() };
        q.stack.push(job);
        self.cv.notify_one();
        dropped
    }

    fn set_generation(&self, generation: u64) -> Vec<u64> {
        let mut q = self.inner.lock().unwrap();
        q.current_gen = generation;
        let mut dropped = Vec::new();
        let mut kept = Vec::new();
        for job in q.stack.drain(..) {
            if job.generation == generation {
                kept.push(job);
            } else {
                dropped.push(job.id);
            }
        }
        q.stack = kept;
        dropped
    }

    /// Pops the newest (most recently visible) job matching the current generation.
    fn pop(&self) -> Option<Job> {
        let mut q = self.inner.lock().unwrap();
        loop {
            if let Some(job) = q.stack.pop() {
                return Some(job);
            }
            if q.closed {
                return None;
            }
            q = self.cv.wait(q).unwrap();
        }
    }

    fn close(&self) {
        let mut q = self.inner.lock().unwrap();
        q.closed = true;
        self.cv.notify_all();
    }
}

pub struct IconService {
    queue: Arc<SharedQueue>,
    done_rx: Receiver<Done>,
    next_id: u64,
    generation: u64,
    clock: AtomicU64,
    by_key: HashMap<IconKey, u64>,
    cached: HashMap<u64, CachedIcon>,
    resident_bytes: usize,
    completed_count: usize,
    pending: usize,
    ctx: Option<egui::Context>,
}

impl IconService {
    pub fn new() -> Self {
        // Warm up the shell image list on the calling thread to avoid cold-init races.
        let _ = shell_file_info(&IconKey::file_type("", 32), SHGFI_SYSICONINDEX);
        let _ = shell_file_info(&IconKey::folder(std::env::temp_dir(), 32), SHGFI_SYSICONINDEX);
        let queue = SharedQueue::new();
        let (done_tx, done_rx) = unbounded::<Done>();
        for index in 0..WORKER_THREADS {
            let q = queue.clone();
            let done = done_tx.clone();
            std::thread::Builder::new()
                .name(format!("lumen-icon-{index}"))
                .spawn(move || worker(q, done))
                .expect("failed to spawn icon worker");
        }
        drop(done_tx);
        Self {
            queue,
            done_rx,
            next_id: 1,
            generation: 0,
            clock: AtomicU64::new(1),
            by_key: HashMap::new(),
            cached: HashMap::new(),
            resident_bytes: 0,
            completed_count: 0,
            pending: 0,
            ctx: None,
        }
    }

    /// Bumps the navigation generation so pending off-screen jobs from the
    /// previous folder are cancelled instead of starving the newly opened view.
    pub fn bump_generation(&mut self) {
        self.generation += 1;
        let dropped = self.queue.set_generation(self.generation);
        self.discard_pending(dropped);
    }

    fn discard_pending(&mut self, dropped: Vec<u64>) {
        for id in dropped {
            if let Some(entry) = self.cached.remove(&id) {
                self.by_key.remove(&entry.key);
                self.pending = self.pending.saturating_sub(1);
            }
        }
    }

    pub fn request(&mut self, key: IconKey) -> u64 {
        let tick = self.clock.fetch_add(1, Ordering::Relaxed);
        if let Some(&id) = self.by_key.get(&key) {
            if let Some(entry) = self.cached.get(&id) {
                entry.last_used.store(tick, Ordering::Relaxed);
            }
            return id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.pending += 1;
        self.by_key.insert(key.clone(), id);
        self.cached.insert(
            id,
            CachedIcon { key: key.clone(), texture: None, bytes: 0, completed: false, last_used: AtomicU64::new(tick) },
        );
        let dropped = self.queue.push(Job { id, generation: self.generation, key });
        self.discard_pending(dropped);
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
        id
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        if self.ctx.is_none() {
            self.ctx = Some(ctx.clone());
        }
        let mut uploaded = 0usize;
        let mut more_ready = false;
        while uploaded < MAX_UPLOADS_PER_FRAME {
            let Ok(done) = self.done_rx.try_recv() else {
                break;
            };
            self.pending = self.pending.saturating_sub(1);
            let Some(slot) = self.cached.get_mut(&done.id) else {
                continue;
            };
            slot.completed = true;
            self.completed_count += 1;
            if let Some(image) = done.image {
                let bytes = image.size[0] * image.size[1] * size_of::<Color32>();
                let name = format!("lumen-icon-{}", done.id);
                let texture = ctx.load_texture(name, image, TextureOptions::LINEAR);
                slot.texture = Some(texture);
                slot.bytes = bytes;
                self.resident_bytes += bytes;
                uploaded += 1;
            }
        }
        if !self.done_rx.is_empty() {
            more_ready = true;
        }

        if self.resident_bytes > CACHE_BYTE_BUDGET || self.completed_count > CACHE_ENTRY_LIMIT {
            self.evict_lru();
        }

        if uploaded > 0 || more_ready {
            ctx.request_repaint();
        } else if self.pending > 0 {
            ctx.request_repaint_after(PENDING_REPAINT);
        }
    }

    fn evict_lru(&mut self) {
        let mut candidates: Vec<(u64, u64, usize)> = self
            .cached
            .iter()
            .filter(|(_, entry)| entry.completed)
            .map(|(&id, entry)| (entry.last_used.load(Ordering::Relaxed), id, entry.bytes))
            .collect();
        candidates.sort_unstable_by_key(|&(used, _, _)| used);
        for (_, id, bytes) in candidates {
            if self.resident_bytes <= CACHE_BYTE_BUDGET && self.completed_count <= CACHE_ENTRY_LIMIT {
                break;
            }
            if let Some(removed) = self.cached.remove(&id) {
                self.by_key.remove(&removed.key);
                self.resident_bytes = self.resident_bytes.saturating_sub(bytes);
                self.completed_count = self.completed_count.saturating_sub(1);
            }
        }
    }

    pub fn get(&self, id: u64) -> Option<TextureHandle> {
        let entry = self.cached.get(&id)?;
        let tick = self.clock.fetch_add(1, Ordering::Relaxed);
        entry.last_used.store(tick, Ordering::Relaxed);
        entry.texture.clone()
    }

    pub fn is_busy(&self) -> bool {
        self.pending > 0 || !self.done_rx.is_empty()
    }
}

impl Drop for IconService {
    fn drop(&mut self) {
        self.queue.close();
    }
}

fn worker(queue: Arc<SharedQueue>, done: Sender<Done>) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let mut image_lists: HashMap<i32, IImageList> = HashMap::new();
    while let Some(job) = queue.pop() {
        let image = resolve(&job.key, &mut image_lists);
        let wake_needed = done.send(Done { id: job.id, image }).is_ok();
        if !wake_needed {
            break;
        }
        crate::platform::wake();
    }
    drop(image_lists);
    if com {
        unsafe { CoUninitialize() };
    }
}

fn resolve(key: &IconKey, image_lists: &mut HashMap<i32, IImageList>) -> Option<ColorImage> {
    let result = match key.path.as_deref() {
        Some(path) if key.thumbnail => thumbnail(path, key.size).or_else(|_| icon(key, image_lists)),
        _ => icon(key, image_lists),
    };
    result.ok()
}

fn icon(key: &IconKey, image_lists: &mut HashMap<i32, IImageList>) -> anyhow::Result<ColorImage> {
    let index = shell_file_info(key, SHGFI_SYSICONINDEX)?.iIcon;
    let hicon = match image_list_icon(image_lists, index, key.size) {
        Ok(hicon) => hicon,
        Err(_) => shell_file_info(key, icon_only_flags(key.size))?.hIcon,
    };
    ensure!(!hicon.is_invalid(), "shell has no HICON for {:?}", key.path);
    let image = icon_image(hicon);
    let _ = unsafe { DestroyIcon(hicon) };
    image
}

fn icon_only_flags(size: u32) -> SHGFI_FLAGS {
    let scale = if size <= 16 { SHGFI_SMALLICON } else { SHGFI_LARGEICON };
    SHGFI_ICON | scale
}

fn image_list_icon(image_lists: &mut HashMap<i32, IImageList>, index: i32, size: u32) -> anyhow::Result<HICON> {
    let shil = shell_image_size(size);
    let list = match image_lists.entry(shil) {
        Entry::Occupied(known) => known.into_mut(),
        Entry::Vacant(unknown) => {
            let list: IImageList =
                unsafe { SHGetImageList(shil) }.with_context(|| format!("SHGetImageList({shil})"))?;
            unknown.insert(list)
        }
    };
    unsafe { list.GetIcon(index, ILD_TRANSPARENT.0) }
        .with_context(|| format!("IImageList::GetIcon({index}) of image list {shil}"))
}

fn shell_image_size(size: u32) -> i32 {
    let shil = if size <= 16 {
        SHIL_SMALL
    } else if size <= 32 {
        SHIL_LARGE
    } else if size <= 48 {
        SHIL_EXTRALARGE
    } else {
        SHIL_JUMBO
    };
    shil as i32
}

fn shell_file_info(key: &IconKey, flags: SHGFI_FLAGS) -> anyhow::Result<SHFILEINFOW> {
    let mut info = SHFILEINFOW::default();
    let cb = size_of::<SHFILEINFOW>() as u32;
    if let Some(path) = &key.path {
        let wide = wide(path.as_os_str());
        let found = unsafe {
            SHGetFileInfoW(
                PCWSTR(wide.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(0),
                Some(&mut info as *mut SHFILEINFOW),
                cb,
                flags,
            )
        };
        if found != 0 {
            return Ok(info);
        }
    } else if let Some(bytes) = &key.pidl_bytes
        && let Some(pidl) = Pidl::from_bytes(bytes)
    {
        let found = unsafe {
            SHGetFileInfoW(
                PCWSTR(pidl.as_ptr().cast::<u16>()),
                FILE_FLAGS_AND_ATTRIBUTES(0),
                Some(&mut info as *mut SHFILEINFOW),
                cb,
                flags | SHGFI_PIDL,
            )
        };
        if found != 0 {
            return Ok(info);
        }
    }
    let (target, attributes) = file_type_target(key);
    let wide = wide(target.as_str());
    let found = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            attributes,
            Some(&mut info as *mut SHFILEINFOW),
            cb,
            flags | SHGFI_USEFILEATTRIBUTES,
        )
    };
    ensure!(found != 0, "SHGetFileInfoW found no icon for {:?} (extension {:?})", key.path, key.ext);
    Ok(info)
}

fn file_type_target(key: &IconKey) -> (String, FILE_FLAGS_AND_ATTRIBUTES) {
    let ext = key.ext.trim_start_matches('.');
    if key.is_folder {
        ("dummy".to_string(), FILE_ATTRIBUTE_DIRECTORY)
    } else if ext.is_empty() {
        ("dummy".to_string(), FILE_ATTRIBUTE_NORMAL)
    } else {
        (format!("dummy.{ext}"), FILE_ATTRIBUTE_NORMAL)
    }
}

fn thumbnail(path: &Path, size: u32) -> anyhow::Result<ColorImage> {
    let wide = wide(path.as_os_str());
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None::<&IBindCtx>) }
        .with_context(|| format!("SHCreateItemFromParsingName({})", path.display()))?;
    let factory: IShellItemImageFactory =
        item.cast().with_context(|| format!("IShellItemImageFactory for {}", path.display()))?;
    let extent = SIZE { cx: size as i32, cy: size as i32 };
    let bitmap = match unsafe { factory.GetImage(extent, SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK) } {
        Ok(bitmap) => bitmap,
        Err(thumbnail_err) => unsafe { factory.GetImage(extent, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK) }
            .with_context(|| format!("no thumbnail ({thumbnail_err}) and no icon for {}", path.display()))?,
    };
    let image = shell_bitmap_image(bitmap);
    let _ = unsafe { DeleteObject(HGDIOBJ(bitmap.0)) };
    image
}

fn shell_bitmap_image(bitmap: HBITMAP) -> anyhow::Result<ColorImage> {
    unsafe {
        let (width, height) = bitmap_extent(bitmap)?;
        let hdc = CreateCompatibleDC(None);
        ensure!(!hdc.is_invalid(), "CreateCompatibleDC failed for {width}x{height} shell bitmap");
        let pixels = dib_bits(hdc, bitmap, width, height, 32);
        let _ = DeleteDC(hdc);
        color_image_from_dib(width as usize, height as usize, &pixels?, None)
    }
}

fn icon_image(hicon: HICON) -> anyhow::Result<ColorImage> {
    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(hicon, &mut info).context("GetIconInfo")?;
        let image = icon_bitmaps_image(&info);
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
        image
    }
}

fn icon_bitmaps_image(info: &ICONINFO) -> anyhow::Result<ColorImage> {
    unsafe {
        if info.hbmColor.is_invalid() {
            let (width, mask_height) = bitmap_extent(info.hbmMask)?;
            ensure!(
                mask_height >= 2 && mask_height.is_multiple_of(2),
                "monochrome icon mask height {mask_height} must be a positive even number"
            );
            let height = mask_height / 2;
            let hdc = CreateCompatibleDC(None);
            ensure!(!hdc.is_invalid(), "CreateCompatibleDC failed for {width}x{height} monochrome icon");
            let mask = dib_bits(hdc, info.hbmMask, width, mask_height, 1);
            let _ = DeleteDC(hdc);
            return color_image_from_monochrome_mask(width as usize, height as usize, &mask?);
        }
        let (width, height) = bitmap_extent(info.hbmColor)?;
        let hdc = CreateCompatibleDC(None);
        ensure!(!hdc.is_invalid(), "CreateCompatibleDC failed for {width}x{height} icon");
        let colors = dib_bits(hdc, info.hbmColor, width, height, 32);
        let mask = if info.hbmMask.is_invalid() { None } else { dib_bits(hdc, info.hbmMask, width, height, 1).ok() };
        let _ = DeleteDC(hdc);
        color_image_from_dib(width as usize, height as usize, &colors?, mask.as_deref())
    }
}

fn bitmap_extent(bitmap: HBITMAP) -> anyhow::Result<(u32, u32)> {
    ensure!(!bitmap.is_invalid(), "shell returned an invalid HBITMAP");
    let mut info = BITMAP::default();
    let read = unsafe {
        GetObjectW(HGDIOBJ(bitmap.0), size_of::<BITMAP>() as i32, Some(&mut info as *mut BITMAP as *mut c_void))
    };
    ensure!(read != 0, "GetObjectW(BITMAP) failed: {}", windows::core::Error::from_thread().message());
    ensure!(
        info.bmWidth > 0 && info.bmHeight != 0,
        "shell bitmap reports an empty extent {}x{}",
        info.bmWidth,
        info.bmHeight
    );
    Ok((info.bmWidth as u32, info.bmHeight.unsigned_abs()))
}

#[repr(C)]
struct BitmapInfo2 {
    header: BITMAPINFOHEADER,
    colors: [RGBQUAD; 2],
}

fn dib_bits(hdc: HDC, bitmap: HBITMAP, width: u32, height: u32, bit_count: u16) -> anyhow::Result<Vec<u8>> {
    let stride = (width as usize * bit_count as usize).div_ceil(32) * 4;
    let mut bits = vec![0u8; stride * height as usize];
    let mut info = BitmapInfo2 {
        header: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: bit_count,
            biCompression: BI_RGB.0,
            biSizeImage: bits.len() as u32,
            ..Default::default()
        },
        colors: [
            RGBQUAD { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbReserved: 0 },
            RGBQUAD { rgbBlue: 255, rgbGreen: 255, rgbRed: 255, rgbReserved: 0 },
        ],
    };
    let copied = unsafe {
        GetDIBits(
            hdc,
            bitmap,
            0,
            height,
            Some(bits.as_mut_ptr() as *mut c_void),
            (&mut info as *mut BitmapInfo2).cast::<BITMAPINFO>(),
            DIB_RGB_COLORS,
        )
    };
    ensure!(copied == height as i32, "GetDIBits copied {copied} of {height} scan lines");
    Ok(bits)
}

fn color_image_from_monochrome_mask(width: usize, height: usize, mask: &[u8]) -> anyhow::Result<ColorImage> {
    ensure!(width > 0 && height > 0, "empty monochrome icon {width}x{height}");
    let stride = width.div_ceil(32) * 4;
    let needed = stride * height * 2;
    ensure!(mask.len() >= needed, "monochrome icon mask holds {} bytes, {width}x{height} needs {needed}", mask.len());
    let mut pixels = Vec::with_capacity(width * height);
    for y in 0..height {
        let and_row = y * stride;
        let xor_row = (y + height) * stride;
        for x in 0..width {
            let shift = 7 - (x & 7);
            let byte_off = x / 8;
            let and_bit = (mask[and_row + byte_off] >> shift) & 1;
            let xor_bit = (mask[xor_row + byte_off] >> shift) & 1;
            let pixel = match (and_bit, xor_bit) {
                (1, 0) => Color32::TRANSPARENT,
                (0, 0) => Color32::from_rgba_unmultiplied(0, 0, 0, 255),
                (0, 1) => Color32::from_rgba_unmultiplied(255, 255, 255, 255),
                _ => Color32::from_rgba_unmultiplied(128, 128, 128, 255),
            };
            pixels.push(pixel);
        }
    }
    Ok(ColorImage::new([width, height], pixels))
}

fn color_image_from_dib(width: usize, height: usize, bgra: &[u8], mask: Option<&[u8]>) -> anyhow::Result<ColorImage> {
    ensure!(width > 0 && height > 0, "empty shell image {width}x{height}");
    ensure!(
        bgra.len() >= width * height * 4,
        "shell DIB holds {} bytes, {width}x{height} needs {}",
        bgra.len(),
        width * height * 4
    );
    let mask_stride = width.div_ceil(32) * 4;
    if let Some(mask) = mask {
        ensure!(
            mask.len() >= mask_stride * height,
            "shell AND mask holds {} bytes, {width}x{height} needs {}",
            mask.len(),
            mask_stride * height
        );
    }
    let alpha_missing = bgra[..width * height * 4].as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0);
    let mut pixels = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let offset = (y * width + x) * 4;
            let [blue, green, red, alpha] = [bgra[offset], bgra[offset + 1], bgra[offset + 2], bgra[offset + 3]];
            let alpha = if alpha_missing {
                match mask {
                    Some(mask) => {
                        let bit = (mask[y * mask_stride + x / 8] >> (7 - (x & 7))) & 1;
                        if bit == 1 { 0 } else { 255 }
                    }
                    None => 255,
                }
            } else {
                alpha
            };
            pixels.push(Color32::from_rgba_unmultiplied(red, green, blue, alpha));
        }
    }
    Ok(ColorImage::new([width, height], pixels))
}

fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_pressure_discards_bookkeeping_and_eventually_becomes_idle() {
        // No workers: keep the queue full deterministically, as if Shell icon
        // extraction were stalled while the user rapidly scrolls a large folder.
        let (done_tx, done_rx) = unbounded();
        let mut service = IconService {
            queue: SharedQueue::new(),
            done_rx,
            next_id: 1,
            generation: 0,
            clock: AtomicU64::new(1),
            by_key: HashMap::new(),
            cached: HashMap::new(),
            resident_bytes: 0,
            completed_count: 0,
            pending: 0,
            ctx: None,
        };
        for index in 0..20_000 {
            service.request(IconKey::file_type(format!("stress-{index}"), 16));
            assert!(service.pending <= 512);
            assert_eq!(service.pending, service.cached.len());
            assert_eq!(service.pending, service.by_key.len());
        }
        assert!(!service.by_key.contains_key(&IconKey::file_type("stress-0", 16)));
        let id = service.request(IconKey::file_type("stress-0", 16));
        assert_eq!(service.by_key[&IconKey::file_type("stress-0", 16)], id, "dropped icons can be retried");
        while let Some(job) = service.queue.inner.lock().unwrap().stack.pop() {
            done_tx.send(Done { id: job.id, image: None }).unwrap();
        }
        service.poll(&egui::Context::default());
        assert_eq!(service.pending, 0);
        assert!(!service.is_busy(), "discarded tasks must not keep requesting redraws forever");
        service.request(IconKey::file_type("next-folder", 16));
        service.bump_generation();
        assert!(!service.is_busy());
        assert_eq!(service.cached.len(), service.completed_count);
    }

    #[test]
    fn keeps_alpha_and_channel_order() {
        let bits = [10u8, 20, 30, 255, 1, 2, 3, 128];
        let image = color_image_from_dib(2, 1, &bits, None).unwrap();
        assert_eq!(image.pixels[0], Color32::from_rgba_unmultiplied(30, 20, 10, 255));
        assert_eq!(image.pixels[1], Color32::from_rgba_unmultiplied(3, 2, 1, 128));
    }

    #[test]
    fn dead_alpha_falls_back_to_opaque() {
        let image = color_image_from_dib(2, 1, &[1, 2, 3, 0, 4, 5, 6, 0], None).unwrap();
        assert_eq!(image.pixels[0].a(), 255);
        assert_eq!(image.pixels[1].a(), 255);
    }

    #[test]
    fn dead_alpha_uses_and_mask_msb_first() {
        let bits = [0u8; 8 * 4];
        let mask = [0b1100_0000u8, 0, 0, 0];
        let image = color_image_from_dib(8, 1, &bits, Some(&mask)).unwrap();
        let alphas: Vec<u8> = image.pixels.iter().map(|pixel| pixel.a()).collect();
        assert_eq!(alphas, [0, 0, 255, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn image_list_size_never_upscales() {
        assert_eq!(shell_image_size(1), SHIL_SMALL as i32);
        assert_eq!(shell_image_size(16), SHIL_SMALL as i32);
        assert_eq!(shell_image_size(17), SHIL_LARGE as i32);
        assert_eq!(shell_image_size(32), SHIL_LARGE as i32);
        assert_eq!(shell_image_size(33), SHIL_EXTRALARGE as i32);
        assert_eq!(shell_image_size(48), SHIL_EXTRALARGE as i32);
        assert_eq!(shell_image_size(49), SHIL_JUMBO as i32);
        assert_eq!(shell_image_size(256), SHIL_JUMBO as i32);
    }

    #[test]
    fn lifo_queue_prioritizes_latest_viewport_item_and_drops_stale_generations() {
        let q = SharedQueue::new();
        assert!(q.push(Job { id: 1, generation: 0, key: IconKey::file_type("a", 16) }).is_empty());
        assert!(q.push(Job { id: 2, generation: 0, key: IconKey::file_type("b", 16) }).is_empty());
        assert_eq!(q.pop().unwrap().id, 2, "newest visible item pops first");
        let dropped = q.set_generation(1);
        assert_eq!(dropped, vec![1], "stale folder items are dropped immediately");
    }

    #[test]
    fn queue_overflow_reclaims_pending_and_cache_entries() {
        let queue = SharedQueue::new();
        let (_done_tx, done_rx) = unbounded::<Done>();
        let mut svc = IconService {
            queue,
            done_rx,
            next_id: 1,
            generation: 0,
            clock: AtomicU64::new(1),
            by_key: HashMap::new(),
            cached: HashMap::new(),
            resident_bytes: 0,
            completed_count: 0,
            pending: 0,
            ctx: None,
        };
        for i in 0..600u32 {
            svc.request(IconKey::file_type(format!("ext{i}"), 32));
        }
        // Triggered one drain of 128 items when reaching 512, leaving 600 - 128 = 472 live jobs.
        assert_eq!(svc.pending, 472);
        assert_eq!(svc.cached.len(), 472);
        assert_eq!(svc.by_key.len(), 472);
        // Evicted key 0 is cleanly removed so re-requesting it schedules a fresh job.
        assert!(!svc.by_key.contains_key(&IconKey::file_type("ext0", 32)));
        let new_id = svc.request(IconKey::file_type("ext0", 32));
        assert!(new_id > 600);
        assert_eq!(svc.pending, 473);
    }

    #[test]
    fn monochrome_mask_and_hicon_decode_all_four_and_xor_states() {
        // 4x1 row: pixel 0 = (AND=1,XOR=0 transparent), pixel 1 = (AND=0,XOR=0 black),
        // pixel 2 = (AND=0,XOR=1 white), pixel 3 = (AND=1,XOR=1 inverted/gray).
        let mask = [
            0b1001_0000u8,
            0,
            0,
            0, // AND row (top half)
            0b0011_0000u8,
            0,
            0,
            0, // XOR row (bottom half)
        ];
        let img = color_image_from_monochrome_mask(4, 1, &mask).unwrap();
        assert_eq!(img.size, [4, 1]);
        assert_eq!(img.pixels[0], Color32::TRANSPARENT);
        assert_eq!(img.pixels[1], Color32::from_rgba_unmultiplied(0, 0, 0, 255));
        assert_eq!(img.pixels[2], Color32::from_rgba_unmultiplied(255, 255, 255, 255));
        assert_eq!(img.pixels[3], Color32::from_rgba_unmultiplied(128, 128, 128, 255));

        // End-to-end Win32 monochrome HICON via CreateIcon (hbmColor is NULL).
        use windows::Win32::UI::WindowsAndMessaging::CreateIcon;
        // 8x2 monochrome icon (1 byte per row, WORD-aligned -> 2 bytes per row in CreateIcon).
        let and_bits: [u8; 4] = [0b1001_0000, 0, 0b1111_1111, 0];
        let xor_bits: [u8; 4] = [0b0011_0000, 0, 0b0000_0000, 0];
        let hicon = unsafe { CreateIcon(None, 8, 2, 1, 1, and_bits.as_ptr(), xor_bits.as_ptr()) }
            .expect("CreateIcon monochrome");
        let decoded = icon_image(hicon).expect("monochrome icon_image");
        let _ = unsafe { DestroyIcon(hicon) };
        assert_eq!(decoded.size, [8, 2]);
        assert_eq!(decoded.pixels[0], Color32::TRANSPARENT);
        assert_eq!(decoded.pixels[1], Color32::from_rgba_unmultiplied(0, 0, 0, 255));
        assert_eq!(decoded.pixels[2], Color32::from_rgba_unmultiplied(255, 255, 255, 255));
        assert_eq!(decoded.pixels[3], Color32::from_rgba_unmultiplied(128, 128, 128, 255));
        // Second row is all AND=1, XOR=0 -> transparent.
        assert!(decoded.pixels[8..16].iter().all(|&p| p == Color32::TRANSPARENT));
    }
}
