//! Shell clipboard interop: file lists (CF_HDROP + Preferred DropEffect), text.
//!
//! All entry points must be called from the UI thread: the clipboard is owned by
//! an STA and Windows hands it to the thread, not the process.

use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, bail};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, POINT};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT, DROPEFFECT_COPY, DROPEFFECT_MOVE};
use windows::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};
use windows::core::{BOOL, w};

const OPEN_ATTEMPTS: u32 = 3;
const OPEN_RETRY_DELAY: Duration = Duration::from_millis(10);

fn preferred_effect_format() -> u32 {
    unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) }
}

static CLIPBOARD_LOCK: Mutex<()> = Mutex::new(());

struct ClipboardGuard {
    _lock: MutexGuard<'static, ()>,
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn open_clipboard(owner: HWND) -> anyhow::Result<ClipboardGuard> {
    let lock = CLIPBOARD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for attempt in 0..OPEN_ATTEMPTS {
        if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
            return Ok(ClipboardGuard { _lock: lock });
        }
        if attempt + 1 < OPEN_ATTEMPTS {
            std::thread::sleep(OPEN_RETRY_DELAY);
        }
    }
    bail!("open clipboard failed: another process still holds it after {OPEN_ATTEMPTS} attempts")
}

/// Moveable global memory block handed over to the clipboard.
struct GlobalBuffer(Option<HGLOBAL>);

impl GlobalBuffer {
    fn copy(bytes: &[u8]) -> anyhow::Result<Self> {
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len())
                .with_context(|| format!("GlobalAlloc({} bytes) failed", bytes.len()))?;
            let ptr = GlobalLock(handle) as *mut u8;
            if ptr.is_null() {
                let _ = GlobalFree(Some(handle));
                bail!("GlobalLock failed for a {} byte clipboard block", bytes.len());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            let _ = GlobalUnlock(handle);
            Ok(Self(Some(handle)))
        }
    }

    /// Hands the block to the clipboard. Only a successful `SetClipboardData`
    /// transfers ownership; on failure `self` still frees the handle.
    fn put(&mut self, format: u32) -> anyhow::Result<()> {
        let handle = self.0.expect("clipboard block already handed over");
        unsafe { SetClipboardData(format, Some(HANDLE(handle.0))) }
            .with_context(|| format!("SetClipboardData for clipboard format {format} failed"))?;
        self.0 = None;
        Ok(())
    }
}

impl Drop for GlobalBuffer {
    fn drop(&mut self) {
        if let Some(handle) = self.0 {
            unsafe {
                let _ = GlobalFree(Some(handle));
            }
        }
    }
}

/// Replaces the clipboard contents with the given formats.
fn replace_clipboard(entries: &[(u32, Vec<u8>)]) -> anyhow::Result<()> {
    let mut blocks = entries.iter().map(|(_, bytes)| GlobalBuffer::copy(bytes)).collect::<anyhow::Result<Vec<_>>>()?;
    let _guard = open_clipboard(crate::platform::hwnd())?;
    unsafe { EmptyClipboard() }.context("EmptyClipboard failed")?;
    for ((format, _), block) in entries.iter().zip(blocks.iter_mut()) {
        block.put(*format)?;
    }
    Ok(())
}

fn hdrop_bytes(paths: &[PathBuf]) -> Vec<u8> {
    let mut units: Vec<u16> = Vec::new();
    for path in paths {
        units.extend(path.as_os_str().encode_wide());
        units.push(0);
    }
    units.push(0);
    let header = DROPFILES {
        pFiles: std::mem::size_of::<DROPFILES>() as u32,
        pt: POINT { x: 0, y: 0 },
        fNC: BOOL(0),
        fWide: BOOL(1),
    };
    let mut bytes = Vec::with_capacity(std::mem::size_of::<DROPFILES>() + units.len() * 2);
    bytes.extend_from_slice(unsafe {
        std::slice::from_raw_parts((&header as *const DROPFILES).cast::<u8>(), std::mem::size_of::<DROPFILES>())
    });
    bytes.extend_from_slice(unsafe { std::slice::from_raw_parts(units.as_ptr().cast::<u8>(), units.len() * 2) });
    bytes
}

fn hdrop_paths(handle: HANDLE) -> Vec<PathBuf> {
    let hdrop = HDROP(handle.0);
    let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
    let mut paths = Vec::with_capacity(count as usize);
    for index in 0..count {
        let len = unsafe { DragQueryFileW(hdrop, index, None) } as usize;
        let mut buffer = vec![0u16; len + 1];
        let written = unsafe { DragQueryFileW(hdrop, index, Some(&mut buffer)) } as usize;
        buffer.truncate(written);
        paths.push(PathBuf::from(std::ffi::OsString::from_wide(&buffer)));
    }
    paths
}

fn read_hglobal_u32(global: HGLOBAL) -> Option<u32> {
    if unsafe { GlobalSize(global) } < std::mem::size_of::<u32>() {
        return None;
    }
    let ptr = unsafe { GlobalLock(global) } as *const u32;
    if ptr.is_null() {
        return None;
    }
    let value = unsafe { ptr.read_unaligned() };
    let _ = unsafe { GlobalUnlock(global) };
    Some(value)
}

fn clipboard_preferred_effect() -> Option<u32> {
    let handle = unsafe { GetClipboardData(preferred_effect_format()) }.ok()?;
    read_hglobal_u32(HGLOBAL(handle.0))
}

/// Places the selected files on the clipboard, optionally as a cut (move) operation.
pub fn set_files(paths: &[PathBuf], cut: bool) -> anyhow::Result<()> {
    anyhow::ensure!(!paths.is_empty(), "set_files needs at least one path");
    let effect = if cut { DROPEFFECT_MOVE } else { DROPEFFECT_COPY };
    replace_clipboard(&[
        (CF_HDROP.0 as u32, hdrop_bytes(paths)),
        (preferred_effect_format(), effect.0.to_ne_bytes().to_vec()),
    ])
}

/// Reads a file list and whether the shell marked it as cut.
/// 剪贴板被其它进程占用时安静返回 None：本函数供 UI 轮询，不能抛错。
pub fn get_files() -> Option<(Vec<PathBuf>, bool)> {
    let _guard = open_clipboard(crate::platform::hwnd()).ok()?;
    let handle = unsafe { GetClipboardData(CF_HDROP.0 as u32) }.ok()?;
    let paths = hdrop_paths(handle);
    if paths.is_empty() {
        return None;
    }
    let cut = clipboard_preferred_effect().is_some_and(|effect| effect & DROPEFFECT_MOVE.0 != 0);
    Some((paths, cut))
}

/// Replaces the clipboard with plain text.
pub fn set_text(text: &str) -> anyhow::Result<()> {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    units.push(0);
    let bytes = unsafe { std::slice::from_raw_parts(units.as_ptr().cast::<u8>(), units.len() * 2) };
    replace_clipboard(&[(CF_UNICODETEXT.0 as u32, bytes.to_vec())])
}

/// Reads plain Unicode text from the clipboard (capped to 1024 chars for quick path sensing).
pub fn get_text() -> Option<String> {
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32) }.is_err() {
        return None;
    }
    let _guard = open_clipboard(crate::platform::hwnd()).ok()?;
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT.0 as u32) }.ok()?;
    let global = HGLOBAL(handle.0);
    let bytes = unsafe { GlobalSize(global) };
    if bytes < 2 {
        return None;
    }
    let ptr = unsafe { GlobalLock(global) } as *const u16;
    if ptr.is_null() {
        return None;
    }
    let max_units = (bytes / 2).min(1024);
    let slice = unsafe { std::slice::from_raw_parts(ptr, max_units) };
    let len = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
    let text = String::from_utf16_lossy(&slice[..len]);
    let _ = unsafe { GlobalUnlock(global) };
    Some(text)
}

/// Extracts an existing local filesystem path from single-line clipboard text
/// (e.g. `"D:\Work\Project"` or `C:\Users\me\notes.txt`), ignoring URLs and multi-line text.
pub fn parse_clipboard_path(text: &str) -> Option<PathBuf> {
    let line = text.trim();
    if line.is_empty() || line.contains(['\r', '\n']) || line.len() > 400 {
        return None;
    }
    let stripped = line.trim_matches('"').trim();
    let looks_local = (stripped.len() >= 3
        && stripped.as_bytes()[0].is_ascii_alphabetic()
        && stripped.as_bytes()[1] == b':'
        && matches!(stripped.as_bytes()[2], b'\\' | b'/'))
        || stripped.starts_with(r"\\");
    if !looks_local {
        return None;
    }
    let path = PathBuf::from(stripped);
    path.exists().then_some(path)
}

/// Empties the system clipboard (used after a cut-paste succeeds so a second
/// Ctrl+V does not retry already-moved paths).
pub fn clear() -> anyhow::Result<()> {
    let _guard = open_clipboard(crate::platform::hwnd())?;
    unsafe { EmptyClipboard() }.context("EmptyClipboard failed")
}

/// True when the system clipboard currently holds a bitmap/screenshot (`CF_DIB`).
pub fn has_image() -> bool {
    unsafe { IsClipboardFormatAvailable(CF_DIB.0 as u32) }.is_ok()
}

/// Saves the clipboard's `CF_DIB` screenshot/bitmap directly into `dir` as a PNG
/// file (using the already-linked `image` crate), returning the created path and name.
pub fn save_image_to(dir: &std::path::Path) -> anyhow::Result<(PathBuf, String)> {
    let dib = {
        let _guard = open_clipboard(crate::platform::hwnd())?;
        let handle = unsafe { GetClipboardData(CF_DIB.0 as u32) }.context("剪贴板中没有图像")?;
        let global = HGLOBAL(handle.0);
        let size = unsafe { GlobalSize(global) };
        anyhow::ensure!(size >= 40, "剪贴板图像数据不完整");
        let ptr = unsafe { GlobalLock(global) } as *const u8;
        anyhow::ensure!(!ptr.is_null(), "GlobalLock 失败");
        let bytes = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
        let _ = unsafe { GlobalUnlock(global) };
        bytes
    };
    let bmp = dib_to_bmp_bytes(&dib).context("剪贴板图像头无效")?;
    let img = image::load_from_memory_with_format(&bmp, image::ImageFormat::Bmp).context("解码剪贴板图像失败")?;
    let name = crate::app::unique_name(dir, "剪贴板图片", "png");
    let dest = dir.join(&name);
    img.save_with_format(&dest, image::ImageFormat::Png).context("保存剪贴板 PNG 失败")?;
    Ok((dest, name))
}

/// Wraps a raw Win32 `CF_DIB` payload with a 14-byte `BITMAPFILEHEADER` so
/// standard BMP decoders handle 24bpp, 32bpp, BI_BITFIELDS, and palette DIBs.
fn dib_to_bmp_bytes(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let header_size = u32::from_le_bytes([dib[0], dib[1], dib[2], dib[3]]);
    if header_size < 40 || header_size as usize > dib.len() {
        return None;
    }
    let bit_count = u16::from_le_bytes([dib[14], dib[15]]);
    let compression = u32::from_le_bytes([dib[16], dib[17], dib[18], dib[19]]);
    let colors_used = u32::from_le_bytes([dib[32], dib[33], dib[34], dib[35]]);
    let palette_bytes = if bit_count <= 8 {
        let entries = if colors_used == 0 { 1u32 << bit_count } else { colors_used };
        entries.saturating_mul(4)
    } else if compression == 3 && header_size == 40 {
        12
    } else {
        colors_used.saturating_mul(4)
    };
    let pixel_offset = 14u32.saturating_add(header_size).saturating_add(palette_bytes);
    let file_size = 14u32.saturating_add(dib.len() as u32);
    let mut bmp = Vec::with_capacity(14 + dib.len());
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&pixel_offset.to_le_bytes());
    bmp.extend_from_slice(dib);
    Some(bmp)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    fn fixture_dir() -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("tmp").join("clipboard-test");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A locked workstation (LogonUI in front) makes Windows deny every clipboard
    /// access, so a machine that cannot open the clipboard is skipped, not failed.
    fn clipboard_ready(files: &[PathBuf]) -> bool {
        for _ in 0..100 {
            if set_files(files, false).is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    // The clipboard is machine-global: a parallel test run in another process can
    // replace its contents between a set and the following read, so retry the pair.
    fn expect_files(files: &[PathBuf], cut: bool) {
        let expected = (files.to_vec(), cut);
        for _ in 0..50 {
            if set_files(files, cut).is_err() {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            if get_files() == Some(expected.clone()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("clipboard file round trip failed (cut={cut}): {:?}", get_files());
    }

    #[test]
    fn files_round_trip() {
        let dir = fixture_dir();
        let files = [dir.join("alpha.txt"), dir.join("测试 文件.txt")];
        for file in &files {
            std::fs::write(file, b"lumen").unwrap();
        }
        if clipboard_ready(&files) {
            expect_files(&files, true);
            expect_files(&files, false);
            set_text("Lumen clipboard 测试").expect("set_text");
        } else {
            eprintln!("clipboard unavailable (workstation locked?); skipping round trip");
        }
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn dib_wrapper_decodes_valid_2x2_bmp() {
        // Minimal 2x2 24bpp BITMAPINFOHEADER (40 bytes) + 2 rows of 8 bytes (padded to 4 bytes)
        let mut dib = vec![0u8; 40 + 16];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes()); // biSize
        dib[4..8].copy_from_slice(&2i32.to_le_bytes()); // biWidth
        dib[8..12].copy_from_slice(&2i32.to_le_bytes()); // biHeight
        dib[12..14].copy_from_slice(&1u16.to_le_bytes()); // biPlanes
        dib[14..16].copy_from_slice(&24u16.to_le_bytes()); // biBitCount
        let bmp = dib_to_bmp_bytes(&dib).expect("valid dib");
        assert_eq!(&bmp[0..2], b"BM");
        let img = image::load_from_memory_with_format(&bmp, image::ImageFormat::Bmp).expect("decode bmp");
        assert_eq!((img.width(), img.height()), (2, 2));
        assert!(dib_to_bmp_bytes(&[0u8; 10]).is_none());
    }

    #[test]
    fn parse_clipboard_path_recognizes_existing_paths_only() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let quoted = format!("\"{manifest_dir}\"");
        assert_eq!(parse_clipboard_path(&quoted), Some(PathBuf::from(manifest_dir)));
        assert_eq!(parse_clipboard_path("https://example.com"), None);
        assert_eq!(parse_clipboard_path("line1\nline2"), None);
        assert_eq!(parse_clipboard_path(r"Z:\NonExistent\LumenPath\12345"), None);
    }

    #[test]
    fn read_hglobal_u32_rejects_undersized_blocks_and_reads_valid_u32() {
        // 0-byte block (GlobalSize == 0) must return None instead of reading OOB.
        let empty = unsafe { GlobalAlloc(GMEM_MOVEABLE, 0) }.expect("alloc 0");
        assert_eq!(read_hglobal_u32(empty), None);
        let _ = unsafe { GlobalFree(Some(empty)) };

        // 2-byte block must return None when GlobalSize < 4.
        let tiny = GlobalBuffer::copy(&[0x01, 0x02]).expect("alloc 2");
        let tiny_h = tiny.0.unwrap();
        if unsafe { GlobalSize(tiny_h) } < std::mem::size_of::<u32>() {
            assert_eq!(read_hglobal_u32(tiny_h), None);
        }

        // Valid 4-byte block reads the exact u32.
        let valid = GlobalBuffer::copy(&DROPEFFECT_MOVE.0.to_ne_bytes()).expect("alloc 4");
        assert_eq!(read_hglobal_u32(valid.0.unwrap()), Some(DROPEFFECT_MOVE.0));
    }
}
