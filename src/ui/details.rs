//! Details / preview pane on the right edge with async text reading and
//! automatic UTF-8 / UTF-16 / Windows ANSI (GBK) decoding.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{Color32, CornerRadius, Margin, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::{LumenApp, TextPreview};
use crate::core::{Entry, format_size, format_time};
use crate::theme;
use crate::ui::{icons, widgets};

#[derive(Clone, Debug)]
pub(crate) struct FolderSizeResult {
    pub bytes: u64,
    pub files: usize,
    pub dirs: usize,
    pub done: bool,
}

pub(crate) struct FolderSizeJob {
    pub path: PathBuf,
    pub rx: Receiver<FolderSizeResult>,
    pub cancel: Arc<AtomicBool>,
    pub latest: Option<FolderSizeResult>,
}

impl Drop for FolderSizeJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub(crate) fn spawn_folder_size(root: PathBuf) -> FolderSizeJob {
    let (tx, rx) = crossbeam_channel::bounded(4);
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_bg = cancel.clone();
    let root_bg = root.clone();
    std::thread::Builder::new()
        .name("lumen-dirsize".to_string())
        .spawn(move || {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
            let mut stack = vec![root_bg.clone()];
            let mut bytes = 0u64;
            let mut files = 0usize;
            let mut dirs = 0usize;
            let mut visited = 0usize;
            while let Some(dir) = stack.pop() {
                if cancel_bg.load(Ordering::Relaxed) {
                    return;
                }
                let Ok(entries) = std::fs::read_dir(&dir) else { continue };
                for entry in entries.flatten() {
                    if cancel_bg.load(Ordering::Relaxed) {
                        return;
                    }
                    let Ok(meta) = entry.metadata() else { continue };
                    let attrs = meta.file_attributes();
                    if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
                        dirs += 1;
                        if attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                            stack.push(entry.path());
                        }
                    } else {
                        files += 1;
                        bytes = bytes.saturating_add(meta.len());
                    }
                    visited += 1;
                    if visited.is_multiple_of(512) {
                        let _ = tx.try_send(FolderSizeResult { bytes, files, dirs, done: false });
                        crate::platform::wake();
                    }
                }
            }
            let _ = tx.send(FolderSizeResult { bytes, files, dirs, done: true });
            crate::platform::wake();
        })
        .expect("spawn folder size worker");
    FolderSizeJob { path: root, rx, cancel, latest: None }
}

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "ico", "tif", "tiff"];

const TEXT_EXTS: &[&str] = &[
    "txt",
    "log",
    "md",
    "markdown",
    "rs",
    "toml",
    "json",
    "jsonc",
    "yml",
    "yaml",
    "ini",
    "cfg",
    "conf",
    "cs",
    "js",
    "mjs",
    "cjs",
    "ts",
    "mts",
    "cts",
    "tsx",
    "jsx",
    "py",
    "c",
    "cpp",
    "cc",
    "cxx",
    "h",
    "hpp",
    "hh",
    "go",
    "java",
    "kt",
    "swift",
    "rb",
    "php",
    "lua",
    "r",
    "ps1",
    "psm1",
    "bat",
    "cmd",
    "sh",
    "bash",
    "zsh",
    "fish",
    "xml",
    "html",
    "htm",
    "css",
    "scss",
    "less",
    "sql",
    "csv",
    "tsv",
    "gitignore",
    "gitattributes",
    "gitmodules",
    "env",
    "editorconfig",
    "dockerignore",
    "npmrc",
    "yarnrc",
    "nvmrc",
    "eslintrc",
    "prettierrc",
    "babelrc",
    "browserslistrc",
    "htaccess",
    "clang-format",
    "clang-tidy",
    "props",
    "targets",
    "csproj",
    "sln",
    "cmake",
    "iss",
    "reg",
];

const EXTENSIONLESS_TEXT_NAMES: &[&str] = &[
    "dockerfile",
    "makefile",
    "license",
    "readme",
    "changelog",
    "authors",
    "notice",
    "procfile",
    "gemfile",
    "rakefile",
    "justfile",
    "vagrantfile",
];

/// Read only the leading 64 KiB for preview so huge log files still preview
/// instantaneously without loading gigabytes into RAM or blocking the UI.
const PREVIEW_HEAD_BYTES: u64 = 64 * 1024;

/// Resolves a normalized lowercase extension, falling back to the filename/path
/// so single-dot dotfiles (`.gitignore`, `.env`, `.editorconfig`, ...) are
/// recognized even if `Entry::ext` was not populated.
pub(crate) fn effective_ext(ext: &str, path: Option<&Path>, name: &str) -> String {
    if !ext.is_empty() {
        return ext.to_ascii_lowercase();
    }
    let file_name = path.and_then(|p| p.file_name()).and_then(|n| n.to_str()).filter(|n| !n.is_empty()).unwrap_or(name);
    crate::shell::enumerate::extract_ext(file_name)
}

/// Returns true when the entry is an image supported by the details thumbnail pane.
pub(crate) fn is_image_file(ext: &str, path: Option<&Path>, name: &str) -> bool {
    let norm = effective_ext(ext, path, name);
    IMAGE_EXTS.contains(&norm.as_str())
}

/// Returns true when the entry should be previewed as plain text / source code
/// (shared by both the details pane and Quick Look), including dotfiles such as
/// `.gitignore`, `.env`, `.env.local`, `.editorconfig`, `.dockerignore`, etc.
pub(crate) fn is_text_file(ext: &str, path: Option<&Path>, name: &str) -> bool {
    let norm = effective_ext(ext, path, name);
    if !norm.is_empty() && TEXT_EXTS.contains(&norm.as_str()) {
        return true;
    }
    let base = path
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or(name)
        .to_ascii_lowercase();
    if base.is_empty() {
        return false;
    }
    if EXTENSIONLESS_TEXT_NAMES.contains(&base.as_str()) {
        return true;
    }
    if let Some(dot_rest) = base.strip_prefix('.') {
        if TEXT_EXTS.contains(&dot_rest) {
            return true;
        }
        // Match multi-segment dotfiles such as `.env.local`, `.env.example`, `.eslintrc.cjs`.
        if let Some((head, _)) = dot_rest.split_once('.')
            && matches!(head, "env" | "eslintrc" | "prettierrc" | "babelrc" | "stylelintrc")
        {
            return true;
        }
    }
    false
}

fn is_drive_root_path(path: &Path) -> bool {
    path.parent().is_none() && path.has_root()
}

/// Returns true for drives / `This PC` root entries whose capacity is already
/// reported by the OS and must never trigger a recursive folder-size walk.
pub(crate) fn is_drive_entry(entry: &Entry, in_this_pc: bool) -> bool {
    if !entry.is_folder {
        return false;
    }
    in_this_pc || entry.free_space > 0 || entry.path.as_deref().is_some_and(is_drive_root_path)
}

struct PreviewRequest {
    path: PathBuf,
    modified: Option<SystemTime>,
    tx: Sender<TextPreview>,
}

static PREVIEW_TX: OnceLock<Sender<PreviewRequest>> = OnceLock::new();

fn preview_worker() -> &'static Sender<PreviewRequest> {
    PREVIEW_TX.get_or_init(|| {
        let (tx, rx) = unbounded::<PreviewRequest>();
        std::thread::Builder::new()
            .name("lumen-preview".to_string())
            .spawn(move || {
                for req in rx {
                    let text = read_text_head(&req.path);
                    let _ = req.tx.try_send(TextPreview { path: req.path, modified: req.modified, text });
                    crate::platform::wake();
                }
            })
            .expect("spawn preview worker");
        tx
    })
}

pub(crate) fn spawn_text_preview(path: PathBuf, modified: Option<SystemTime>) -> Receiver<TextPreview> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let _ = preview_worker().send(PreviewRequest { path, modified, tx });
    rx
}

fn decode_ansi_codepage(bytes: &[u8], codepage: u32, max_trim: usize) -> Option<String> {
    use windows::Win32::Globalization::{MB_ERR_INVALID_CHARS, MultiByteToWideChar};
    for trim in 0..=max_trim {
        if bytes.len() <= trim {
            break;
        }
        let slice = &bytes[..bytes.len() - trim];
        let needed = unsafe { MultiByteToWideChar(codepage, MB_ERR_INVALID_CHARS, slice, None) };
        if needed > 0 {
            let mut wide = vec![0u16; needed as usize];
            let written = unsafe { MultiByteToWideChar(codepage, MB_ERR_INVALID_CHARS, slice, Some(&mut wide)) };
            if written > 0 {
                return Some(String::from_utf16_lossy(&wide[..written as usize]));
            }
        }
    }
    None
}

/// Decodes leading bytes as UTF-8, UTF-16 LE/BE (with BOM), or the specified
/// Windows ANSI codepage. Returns `None` when non-UTF-16 data contains NUL
/// (`0x00`) bytes so binary files are not misidentified as text.
///
/// When `truncated` is true, incomplete trailing multi-byte sequences split by
/// the preview read cap (both UTF-8 and multi-byte ANSI/GBK) are trimmed before
/// falling back to UTF-8 lossy decoding.
pub(crate) fn decode_text_bytes_with_codepage(bytes: &[u8], codepage: u32, truncated: bool) -> Option<String> {
    if bytes.is_empty() {
        return Some(String::new());
    }
    if bytes.starts_with(&[0xFF, 0xFE]) && bytes.len() >= 2 {
        let mut u16s: Vec<u16> = bytes[2..].as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
        if truncated && u16s.last().is_some_and(|&u| (0xD800..=0xDBFF).contains(&u)) {
            u16s.pop();
        }
        return Some(String::from_utf16_lossy(&u16s));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) && bytes.len() >= 2 {
        let mut u16s: Vec<u16> = bytes[2..].as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c)).collect();
        if truncated && u16s.last().is_some_and(|&u| (0xD800..=0xDBFF).contains(&u)) {
            u16s.pop();
        }
        return Some(String::from_utf16_lossy(&u16s));
    }
    let (had_utf8_bom, slice) =
        if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) { (true, rest) } else { (false, bytes) };
    // Non-UTF-16 files containing NUL bytes are binary data, not plain text.
    if slice.contains(&0) {
        return None;
    }
    match std::str::from_utf8(slice) {
        Ok(utf8) => return Some(utf8.to_owned()),
        Err(err) if truncated && err.error_len().is_none() && (had_utf8_bom || err.valid_up_to() > 0) => {
            // Buffer cap cut a valid UTF-8 multi-byte character right at the end.
            let prefix = std::str::from_utf8(&slice[..err.valid_up_to()]).expect("valid_up_to is valid UTF-8");
            return Some(prefix.to_owned());
        }
        Err(_) if had_utf8_bom => {
            return Some(String::from_utf8_lossy(slice).into_owned());
        }
        Err(_) => {}
    }
    // Fallback to Windows ANSI codepage (e.g. GBK / CP936 on zh-CN Windows).
    // When truncated at the read cap, retry trimming 1..=3 trailing bytes in
    // case a multi-byte character was split in half at the end of the buffer.
    let max_trim = if truncated { 3 } else { 0 };
    if let Some(decoded) = decode_ansi_codepage(slice, codepage, max_trim) {
        return Some(decoded);
    }
    Some(String::from_utf8_lossy(slice).into_owned())
}

/// Decodes leading bytes as UTF-8, UTF-16 LE/BE (with BOM), or Windows active
/// ANSI codepage (e.g. GBK / CP936 on Chinese Windows) via `MultiByteToWideChar`.
#[allow(dead_code)] // Shared text-decoding utility kept public to sibling UI modules
pub(crate) fn decode_text_bytes(bytes: &[u8]) -> String {
    use windows::Win32::Globalization::CP_ACP;
    decode_text_bytes_with_codepage(bytes, CP_ACP, true).unwrap_or_default()
}

/// Reads up to `byte_cap` leading bytes from `path`, decodes them with binary
/// rejection and boundary-safe UTF-8/GBK handling, and caps output to `char_cap`.
/// Returns `None` if the file cannot be read or contains binary NUL bytes.
pub(crate) fn read_text_preview(path: &Path, byte_cap: u64, char_cap: usize) -> Option<String> {
    use windows::Win32::Globalization::CP_ACP;
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(4096);
    file.take(byte_cap.saturating_add(1)).read_to_end(&mut buf).ok()?;
    let truncated = buf.len() as u64 > byte_cap;
    if truncated {
        buf.truncate(byte_cap as usize);
    }
    let decoded = decode_text_bytes_with_codepage(&buf, CP_ACP, truncated)?;
    Some(decoded.chars().take(char_cap).collect())
}

fn read_text_head(path: &Path) -> String {
    read_text_preview(path, PREVIEW_HEAD_BYTES, 6000).unwrap_or_default()
}

impl LumenApp {
    pub(crate) fn paint_details(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.theme.clone();
        let outer_painter = ui.painter().clone();
        outer_painter.rect_filled(rect, CornerRadius::same(theme::RADIUS_CARD), t.surface);
        widgets::stroke_rect(&outer_painter, rect, theme::RADIUS_CARD, Stroke::new(1.0, t.border));

        let tab = self.active();
        let in_this_pc = tab.is_this_pc();
        let selected_count = tab.selection.len();
        let selected_size = tab.stats.selected_size;
        let selected_folders = tab.selection.iter().filter_map(|i| tab.entries.get(*i)).filter(|e| e.is_folder).count();
        let entry = if selected_count == 1 {
            tab.selection.iter().next().and_then(|index| tab.entries.get(*index)).cloned()
        } else {
            None
        };
        let dir = tab.location.clone();
        let total = tab.entries.len();

        // Cancel any in-flight folder size calculation once selection no longer
        // matches that folder (including multi-select, empty selection, files, or drives).
        let active_folder_path =
            entry.as_ref().filter(|e| e.is_folder && !is_drive_entry(e, in_this_pc)).and_then(|e| e.path.as_deref());
        if let Some(job) = &self.folder_size
            && active_folder_path != Some(job.path.as_path())
        {
            self.folder_size = None;
        }

        let (is_image, texture) = if let Some(entry) = &entry {
            let is_img = is_image_file(&entry.ext, entry.path.as_deref(), &entry.name);
            let tex = self.shell_icon_entry(ui.ctx(), entry, if is_img { 256 } else { 64 }, is_img);
            self.refresh_text_preview(entry);
            (is_img, tex)
        } else {
            (false, None)
        };

        let folder_size_matched = self.folder_size.as_ref().and_then(|job| {
            entry.as_ref().and_then(|e| e.path.as_ref()).filter(|p| **p == job.path).map(|_| job.latest.clone())
        });

        let _ = ui.interact(rect, egui::Id::unique("details-pane"), Sense::hover());

        let inner = rect.shrink(1.0);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        child.set_clip_rect(inner);

        let mut start_folder_size: Option<PathBuf> = None;
        egui::ScrollArea::vertical().id_salt("details-pane-scroll").auto_shrink([false, false]).show(
            &mut child,
            |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                let pad_x = 16.0;
                let avail_w = ui.available_width().max(64.0);
                let content_w = (avail_w - pad_x * 2.0).max(32.0);

                let paint_kv = |ui: &mut egui::Ui, label: &str, value: &str, max_rows: usize| {
                    let label_galley = widgets::galley(ui, label, theme::font(11.0), t.text_tertiary, content_w);
                    let value_galley =
                        widgets::galley_wrap(ui, value, theme::font(12.0), t.text_secondary, content_w, max_rows);
                    let block_h = 16.0 + value_galley.size().y.max(16.0) + 8.0;
                    let (strip, _) = ui.allocate_exact_size(Vec2::new(avail_w, block_h), Sense::hover());
                    let p = ui.painter();
                    let x = strip.left() + pad_x;
                    widgets::galley_at(p, Pos2::new(x, strip.top()), label_galley, t.text_tertiary);
                    widgets::galley_at(p, Pos2::new(x, strip.top() + 16.0), value_galley, t.text_secondary);
                };

                ui.add_space(14.0);
                let (hdr_strip, _) = ui.allocate_exact_size(Vec2::new(avail_w, 30.0), Sense::hover());
                widgets::text_left(
                    ui,
                    ui.painter(),
                    Rect::from_min_size(
                        Pos2::new(hdr_strip.left() + pad_x, hdr_strip.top()),
                        Vec2::new(content_w, 22.0),
                    ),
                    "详细信息",
                    theme::semibold(12.5),
                    t.text,
                    0.0,
                );

                match &entry {
                    Some(entry) => {
                        let preview_h = if is_image { 200.0 } else { 84.0 };
                        let (prev_strip, _) =
                            ui.allocate_exact_size(Vec2::new(avail_w, preview_h + 14.0), Sense::hover());
                        let preview = Rect::from_min_size(
                            Pos2::new(prev_strip.left() + pad_x, prev_strip.top()),
                            Vec2::new(content_w, preview_h),
                        );
                        let p = ui.painter();
                        p.rect_filled(preview, CornerRadius::same(6), t.surface_alt);
                        match &texture {
                            Some(texture) => {
                                let size = texture.size_vec2();
                                let scale = (preview.width() - 16.0) / size.x;
                                let draw_size = size * scale.min((preview.height() - 16.0) / size.y).min(1.0);
                                let dst = Rect::from_center_size(preview.center(), draw_size);
                                p.image(
                                    texture.id(),
                                    dst,
                                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            }
                            None => {
                                let glyph = if entry.is_folder { icons::FOLDER } else { icons::FILE };
                                widgets::icon_at(p, preview.center(), glyph, 38.0, t.text_tertiary);
                            }
                        }

                        let name_galley =
                            widgets::galley_wrap(ui, &entry.name, theme::bold(14.5), t.text, content_w, 3);
                        let name_h = name_galley.size().y.max(18.0) + 10.0;
                        let (name_strip, _) = ui.allocate_exact_size(Vec2::new(avail_w, name_h), Sense::hover());
                        widgets::galley_at(
                            ui.painter(),
                            Pos2::new(name_strip.left() + pad_x, name_strip.top()),
                            name_galley,
                            t.text,
                        );

                        let is_drive = is_drive_entry(entry, in_this_pc);
                        let mut rows: Vec<(&str, String)> = Vec::with_capacity(8);
                        rows.push(("类型", entry.kind.clone()));
                        let mut want_calc_size = false;
                        if is_drive {
                            if entry.size > 0 {
                                let used = entry.size.saturating_sub(entry.free_space);
                                rows.push(("总容量", format_size(entry.size)));
                                rows.push(("可用空间", format_size(entry.free_space)));
                                rows.push(("已用空间", format_size(used)));
                            } else {
                                rows.push(("总容量", "—".to_string()));
                            }
                        } else if entry.is_folder {
                            match &folder_size_matched {
                                Some(Some(res)) => {
                                    let suffix = if res.done { "" } else { " …" };
                                    rows.push((
                                        "大小",
                                        format!(
                                            "{} ({} 个文件，{} 个文件夹){suffix}",
                                            format_size(res.bytes),
                                            res.files,
                                            res.dirs
                                        ),
                                    ));
                                }
                                Some(None) => {
                                    rows.push(("大小", "正在计算…".to_string()));
                                }
                                None if entry.path.is_some() => {
                                    want_calc_size = true;
                                }
                                None => {
                                    rows.push(("大小", "—".to_string()));
                                }
                            }
                        } else {
                            rows.push(("大小", format_size(entry.size)));
                        }
                        rows.push(("修改日期", format_time(entry.modified)));
                        rows.push(("创建日期", format_time(entry.created)));
                        let mut flags = Vec::new();
                        if entry.is_readonly() {
                            flags.push("只读");
                        }
                        if entry.is_hidden() {
                            flags.push("隐藏");
                        }
                        if entry.is_system() {
                            flags.push("系统");
                        }
                        if entry.is_symlink_or_shortcut() {
                            flags.push("快捷方式/链接");
                        }
                        if entry.is_cloud_only() {
                            flags.push("仅联机 (云端)");
                        } else if entry.is_cloud_pinned() {
                            flags.push("始终保留在本地");
                        }
                        if !flags.is_empty() {
                            rows.push(("属性", flags.join(" · ")));
                        }
                        rows.retain(|(_, value)| !value.is_empty());
                        if let Some(path) = &entry.path {
                            rows.push(("路径", path.to_string_lossy().to_string()));
                        }
                        for (label, value) in rows {
                            let max_rows = if label == "路径" { 3 } else { 2 };
                            paint_kv(ui, label, &value, max_rows);
                        }

                        if want_calc_size && let Some(path) = entry.path.clone() {
                            let (btn_strip, _) = ui.allocate_exact_size(Vec2::new(avail_w, 34.0), Sense::hover());
                            let btn = Rect::from_min_size(
                                Pos2::new(btn_strip.left() + pad_x, btn_strip.top()),
                                Vec2::new(112.0, 26.0),
                            );
                            let bid = egui::Id::unique("details-calc-size");
                            let bresp = ui.interact(btn, bid, Sense::click());
                            let bhover = widgets::hover_t(ui.ctx(), bid, bresp.hovered());
                            let p = ui.painter();
                            p.rect_filled(
                                btn,
                                CornerRadius::same(5),
                                if bhover > 0.0 { t.hover_strong } else { t.surface_alt },
                            );
                            widgets::stroke_rect(p, btn, 5, Stroke::new(1.0, t.border));
                            let bg = widgets::galley(ui, "计算文件夹大小", theme::font(11.5), t.accent_text, 104.0);
                            widgets::galley_at(p, btn.center() - bg.size() * 0.5, bg, t.accent_text);
                            if bresp.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if bresp.clicked() {
                                start_folder_size = Some(path);
                            }
                        }

                        if let Some(preview) = &self.text_preview
                            && !preview.text.is_empty()
                        {
                            ui.add_space(4.0);
                            let lbl = widgets::galley(ui, "内容预览", theme::font(11.0), t.text_tertiary, content_w);
                            let (lbl_strip, _) = ui.allocate_exact_size(Vec2::new(avail_w, 18.0), Sense::hover());
                            widgets::galley_at(
                                ui.painter(),
                                Pos2::new(lbl_strip.left() + pad_x, lbl_strip.top()),
                                lbl,
                                t.text_tertiary,
                            );
                            egui::Frame::new()
                                .outer_margin(Margin::symmetric(16, 2))
                                .inner_margin(Margin::same(8))
                                .fill(t.surface_alt)
                                .corner_radius(CornerRadius::same(6))
                                .stroke(Stroke::new(1.0, t.border))
                                .show(ui, |ui| {
                                    ui.set_width((content_w - 16.0).max(32.0));
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&preview.text)
                                                .font(theme::mono(11.0))
                                                .color(t.text_secondary),
                                        )
                                        .wrap(),
                                    );
                                });
                        }
                    }
                    None => {
                        if selected_count > 1 {
                            let label = format!("已选择 {selected_count} 个项目");
                            let galley = widgets::galley_wrap(ui, &label, theme::semibold(14.0), t.text, content_w, 2);
                            let title_h = galley.size().y.max(18.0) + 12.0;
                            let (strip, _) = ui.allocate_exact_size(Vec2::new(avail_w, title_h), Sense::hover());
                            widgets::galley_at(
                                ui.painter(),
                                Pos2::new(strip.left() + pad_x, strip.top()),
                                galley,
                                t.text,
                            );

                            let selected_files = selected_count.saturating_sub(selected_folders);
                            let mut rows: Vec<(&str, String)> = vec![
                                ("文件夹", format!("{selected_folders} 个")),
                                ("文件", format!("{selected_files} 个")),
                            ];
                            if selected_size > 0 {
                                rows.push(("总大小（文件）", format_size(selected_size)));
                            }
                            rows.push(("位置", dir.parsing_name()));
                            for (label, value) in rows {
                                let max_rows = if label == "位置" { 3 } else { 2 };
                                paint_kv(ui, label, &value, max_rows);
                            }
                        } else {
                            let galley =
                                widgets::galley_wrap(ui, &dir.display, theme::semibold(14.0), t.text, content_w, 2);
                            let title_h = galley.size().y.max(18.0) + 12.0;
                            let (strip, _) = ui.allocate_exact_size(Vec2::new(avail_w, title_h), Sense::hover());
                            widgets::galley_at(
                                ui.painter(),
                                Pos2::new(strip.left() + pad_x, strip.top()),
                                galley,
                                t.text,
                            );

                            let rows: Vec<(&str, String)> = vec![
                                ("类型", "文件夹".to_string()),
                                ("项目数", format!("{total}")),
                                ("位置", dir.parsing_name()),
                            ];
                            for (label, value) in rows {
                                let max_rows = if label == "位置" { 3 } else { 2 };
                                paint_kv(ui, label, &value, max_rows);
                            }
                        }
                    }
                }

                ui.add_space(12.0);
            },
        );

        if let Some(path) = start_folder_size {
            self.folder_size = Some(spawn_folder_size(path));
        }
    }

    fn refresh_text_preview(&mut self, entry: &Entry) {
        let Some(path) = entry.path.as_ref() else {
            self.text_preview = None;
            self.text_preview_rx = None;
            self.text_preview_pending = None;
            return;
        };
        if entry.is_folder
            || entry.is_cloud_only()
            || !is_text_file(&entry.ext, entry.path.as_deref(), &entry.name)
            || entry.size == 0
        {
            self.text_preview = None;
            self.text_preview_rx = None;
            self.text_preview_pending = None;
            return;
        }
        let fresh = self
            .text_preview
            .as_ref()
            .is_some_and(|preview| preview.path == *path && preview.modified == entry.modified);
        if fresh {
            return;
        }
        let in_flight = self.text_preview_pending.as_ref().is_some_and(|(p, m)| p == path && *m == entry.modified);
        if in_flight {
            return;
        }
        self.text_preview = None;
        self.text_preview_pending = Some((path.clone(), entry.modified));
        self.text_preview_rx = Some(spawn_text_preview(path.clone(), entry.modified));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CP_GBK: u32 = 936;

    #[test]
    fn gbk_truncated_multibyte_boundary_decodes_cleanly() {
        // GBK bytes for "中文测试": 中=D6 D0, 文=CE C4, 测=B2 E2, 试=CA D4
        let full_gbk: &[u8] = &[0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2, 0xCA, 0xD4];
        assert_eq!(decode_text_bytes_with_codepage(full_gbk, CP_GBK, false).as_deref(), Some("中文测试"));
        assert_eq!(decode_text_bytes_with_codepage(full_gbk, CP_GBK, true).as_deref(), Some("中文测试"));

        // Truncated in the middle of the last 2-byte GBK character ('试' lead byte 0xCA only).
        let truncated_gbk = &full_gbk[..full_gbk.len() - 1];
        let decoded = decode_text_bytes_with_codepage(truncated_gbk, CP_GBK, true).expect("must decode");
        assert_eq!(decoded, "中文测");
        assert!(!decoded.contains('\u{FFFD}'), "must not produce UTF-8 replacement chars");
    }

    #[test]
    fn gbk_complete_ascii_plus_hanzi_does_not_drop_final_char() {
        // Regression guard: ASCII prefix + single GBK char "中" (D6 D0) must not be
        // mis-trimmed into pure ASCII "hello".
        let gbk = b"hello\xD6\xD0";
        assert_eq!(decode_text_bytes_with_codepage(gbk, CP_GBK, true).as_deref(), Some("hello中"));
        assert_eq!(decode_text_bytes_with_codepage(gbk, CP_GBK, false).as_deref(), Some("hello中"));
    }

    #[test]
    fn utf8_truncated_multibyte_boundary_decodes_cleanly_without_mojibake() {
        // "你好世界🚀" in UTF-8: each CJK char is 3 bytes, emoji is 4 bytes.
        let src = "你好世界🚀".as_bytes();
        // Cut 1, 2, and 3 bytes into the 4-byte emoji.
        for cut in 1..=3 {
            let sliced = &src[..src.len() - cut];
            let decoded = decode_text_bytes_with_codepage(sliced, CP_GBK, true).expect("must decode");
            assert_eq!(decoded, "你好世界", "cut={cut} should keep intact prefix without mojibake");
            assert!(!decoded.contains('\u{FFFD}'));
        }
        // Cut 1 and 2 bytes into a 3-byte CJK character with UTF-8 BOM.
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice("你好".as_bytes());
        let decoded_bom = decode_text_bytes_with_codepage(&with_bom[..with_bom.len() - 1], CP_GBK, true).unwrap();
        assert_eq!(decoded_bom, "你");
    }

    #[test]
    fn nul_byte_rejects_binary_but_allows_utf16_bom() {
        let binary = b"ELF\x00\x01\x02\x03hello";
        assert_eq!(decode_text_bytes_with_codepage(binary, CP_GBK, false), None);
        assert_eq!(decode_text_bytes_with_codepage(binary, CP_GBK, true), None);
        assert_eq!(decode_text_bytes(binary), "");

        // UTF-16 LE BOM contains NUL bytes for ASCII chars and must still decode cleanly.
        let utf16_le: Vec<u8> =
            [0xFFu8, 0xFE].into_iter().chain("Hi中".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        assert_eq!(decode_text_bytes_with_codepage(&utf16_le, CP_GBK, false).as_deref(), Some("Hi中"));

        // UTF-16 BE BOM must also decode cleanly.
        let utf16_be: Vec<u8> =
            [0xFEu8, 0xFF].into_iter().chain("Hi中".encode_utf16().flat_map(|u| u.to_be_bytes())).collect();
        assert_eq!(decode_text_bytes_with_codepage(&utf16_be, CP_GBK, false).as_deref(), Some("Hi中"));
    }

    #[test]
    fn dotfiles_and_tiff_extensions_are_recognized() {
        // Dotfiles with empty `ext` (e.g. from Path::extension() returning None) and populated `ext`.
        for name in [".gitignore", ".env", ".editorconfig", ".dockerignore", ".npmrc", ".eslintrc", ".env.local"] {
            assert!(is_text_file("", Some(Path::new(name)), name), "{name} should be text with empty ext");
            let extracted = crate::shell::enumerate::extract_ext(name);
            assert!(is_text_file(&extracted, Some(Path::new(name)), name), "{name} should be text with extracted ext");
        }
        assert!(is_text_file("", Some(Path::new("Makefile")), "Makefile"));
        assert!(!is_text_file("exe", Some(Path::new("app.exe")), "app.exe"));

        // Image extensions include tif/tiff in details pane.
        assert!(is_image_file("tif", Some(Path::new("scan.tif")), "scan.tif"));
        assert!(is_image_file("tiff", Some(Path::new("scan.TIFF")), "scan.TIFF"));
        assert!(is_image_file("", Some(Path::new("scan.TIFF")), "scan.TIFF"));
        assert!(!is_image_file("txt", Some(Path::new("note.txt")), "note.txt"));
    }

    #[test]
    fn drive_entry_detection_matches_this_pc_and_roots() {
        let drive = Entry {
            name: "本地磁盘 (C:)".to_string(),
            path: Some(PathBuf::from(r"C:\")),
            is_folder: true,
            size: 500_000_000_000,
            modified: None,
            created: None,
            kind: "本地磁盘".to_string(),
            free_space: 120_000_000_000,
            ext: String::new(),
            file_attrs: 0,
            pidl: None,
        };
        assert!(is_drive_entry(&drive, true));
        assert!(is_drive_entry(&drive, false));

        let regular_folder = Entry {
            name: "src".to_string(),
            path: Some(PathBuf::from(r"C:\repo\src")),
            is_folder: true,
            size: 0,
            modified: None,
            created: None,
            kind: "文件夹".to_string(),
            free_space: 0,
            ext: String::new(),
            file_attrs: 0,
            pidl: None,
        };
        assert!(!is_drive_entry(&regular_folder, false));
        assert!(is_drive_entry(&regular_folder, true));
    }

    #[test]
    fn paint_details_cancels_stale_folder_size_job_when_selection_changes() {
        use crate::core::settings::Settings;
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let ctx = egui::Context::default();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut app = LumenApp::with_context(
            &ctx,
            Settings { tabs: vec![dir.to_string_lossy().to_string()], ..Default::default() },
        );
        let folder_a = Entry {
            name: "folder_a".to_string(),
            path: Some(PathBuf::from(r"C:\test\folder_a")),
            is_folder: true,
            size: 0,
            modified: None,
            created: None,
            kind: "文件夹".to_string(),
            free_space: 0,
            ext: String::new(),
            file_attrs: 0,
            pidl: None,
        };
        let folder_b = Entry {
            name: "folder_b".to_string(),
            path: Some(PathBuf::from(r"C:\test\folder_b")),
            is_folder: true,
            size: 0,
            modified: None,
            created: None,
            kind: "文件夹".to_string(),
            free_space: 0,
            ext: String::new(),
            file_attrs: 0,
            pidl: None,
        };
        let tab = app.active_mut();
        tab.entries = Arc::new(vec![folder_a, folder_b]);
        tab.rebuild_display();
        tab.focus_row(1); // Select folder_b while folder_size job is for folder_a.

        let cancel = Arc::new(AtomicBool::new(false));
        let (_tx, rx) = crossbeam_channel::bounded(1);
        app.folder_size =
            Some(FolderSizeJob { path: PathBuf::from(r"C:\test\folder_a"), rx, cancel: cancel.clone(), latest: None });

        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 400.0))),
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            let rect = ui.max_rect();
            app.paint_details(ui, rect);
        });
        out.textures_delta.clear();

        assert!(app.folder_size.is_none(), "stale folder_size job must be cleared on selection change");
        assert!(cancel.load(Ordering::Relaxed), "dropped FolderSizeJob must signal cancel flag");

        unsafe { CoUninitialize() };
    }
}
