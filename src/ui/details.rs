//! Details / preview pane on the right edge with async text reading and
//! automatic UTF-8 / UTF-16 / Windows ANSI (GBK) decoding.

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::{LumenApp, TextPreview};
use crate::core::{Entry, format_size, format_time};
use crate::theme::{self};
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

const TEXT_EXT: [&str; 24] = [
    "txt", "log", "md", "rs", "toml", "json", "yml", "yaml", "ini", "cfg", "cs", "js", "ts", "tsx", "jsx", "py", "c",
    "cpp", "h", "hpp", "ps1", "bat", "sh", "xml",
];
/// Read only the leading 64 KiB for preview so huge log files still preview
/// instantaneously without loading gigabytes into RAM or blocking the UI.
const PREVIEW_HEAD_BYTES: u64 = 64 * 1024;

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

/// Decodes leading bytes as UTF-8, UTF-16 LE/BE (with BOM), or Windows active
/// ANSI codepage (e.g. GBK / CP936 on Chinese Windows) via `MultiByteToWideChar`.
pub(crate) fn decode_text_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) && bytes.len() >= 2 {
        let u16s: Vec<u16> = bytes[2..].as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
        return String::from_utf16_lossy(&u16s);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) && bytes.len() >= 2 {
        let u16s: Vec<u16> = bytes[2..].as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c)).collect();
        return String::from_utf16_lossy(&u16s);
    }
    if let Ok(utf8) = std::str::from_utf8(bytes) {
        return utf8.to_owned();
    }
    // Trim up to 3 trailing bytes in case the 64 KiB boundary split a UTF-8 sequence.
    for trim in 1..=3 {
        if bytes.len() > trim
            && let Ok(utf8) = std::str::from_utf8(&bytes[..bytes.len() - trim])
        {
            return utf8.to_owned();
        }
    }
    // Fallback to active Windows ANSI codepage (GBK on zh-CN Windows).
    use windows::Win32::Globalization::{CP_ACP, MB_ERR_INVALID_CHARS, MultiByteToWideChar};
    let needed = unsafe { MultiByteToWideChar(CP_ACP, MB_ERR_INVALID_CHARS, bytes, None) };
    if needed > 0 {
        let mut wide = vec![0u16; needed as usize];
        let written = unsafe { MultiByteToWideChar(CP_ACP, MB_ERR_INVALID_CHARS, bytes, Some(&mut wide)) };
        if written > 0 {
            return String::from_utf16_lossy(&wide[..written as usize]);
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn read_text_head(path: &std::path::Path) -> String {
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut buf = Vec::with_capacity(4096);
    if file.take(PREVIEW_HEAD_BYTES).read_to_end(&mut buf).is_err() {
        return String::new();
    }
    decode_text_bytes(&buf).chars().take(6000).collect()
}

impl LumenApp {
    pub(crate) fn paint_details(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.theme.clone();
        let painter = ui.painter().clone();
        painter.rect_filled(rect, CornerRadius::same(theme::RADIUS_CARD), t.surface);
        widgets::stroke_rect(&painter, rect, theme::RADIUS_CARD, Stroke::new(1.0, t.border));

        let tab = self.active();
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

        let mut y = rect.top() + 14.0;
        let left = rect.left() + 16.0;
        let width = rect.width() - 32.0;
        widgets::text_left(
            ui,
            &painter,
            Rect::from_min_size(Pos2::new(left, y), Vec2::new(width, 22.0)),
            "详细信息",
            theme::semibold(12.5),
            t.text,
            0.0,
        );
        y += 30.0;

        match &entry {
            Some(entry) => {
                let is_image =
                    matches!(entry.ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "ico");
                let texture = self.shell_icon_entry(ui.ctx(), entry, if is_image { 256 } else { 64 }, is_image);
                let preview_h = if is_image { 200.0 } else { 84.0 };
                let preview = Rect::from_min_size(Pos2::new(left, y), Vec2::new(width, preview_h));
                painter.rect_filled(preview, CornerRadius::same(6), t.surface_alt);
                match texture {
                    Some(texture) => {
                        let size = texture.size_vec2();
                        let scale = (preview.width() - 16.0) / size.x;
                        let draw_size = size * scale.min((preview.height() - 16.0) / size.y).min(1.0);
                        let dst = Rect::from_center_size(preview.center(), draw_size);
                        painter.image(
                            texture.id(),
                            dst,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    None => {
                        let glyph = if entry.is_folder { icons::FOLDER } else { icons::FILE };
                        widgets::icon_at(&painter, preview.center(), glyph, 38.0, t.text_tertiary);
                    }
                }
                y += preview_h + 14.0;
                let name = entry.name.clone();
                let galley = widgets::galley(ui, &name, theme::bold(14.5), t.text, width);
                widgets::galley_at(&painter, Pos2::new(left, y), galley, t.text);
                y += 26.0;

                let mut rows: Vec<(&str, String)> = Vec::with_capacity(6);
                rows.push(("类型", entry.kind.clone()));
                let mut want_calc_size = false;
                if entry.is_folder {
                    let matched = self
                        .folder_size
                        .as_ref()
                        .and_then(|job| entry.path.as_ref().filter(|p| **p == job.path).and(job.latest.as_ref()));
                    if let Some(res) = matched {
                        let suffix = if res.done { "" } else { " …" };
                        rows.push((
                            "大小",
                            format!("{} ({} 个文件，{} 个文件夹){suffix}", format_size(res.bytes), res.files, res.dirs),
                        ));
                    } else if self
                        .folder_size
                        .as_ref()
                        .is_some_and(|j| entry.path.as_ref().is_some_and(|p| *p == j.path))
                    {
                        rows.push(("大小", "正在计算…".to_string()));
                    } else if entry.path.is_some() {
                        want_calc_size = true;
                    } else {
                        rows.push(("大小", "—".to_string()));
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
                    let label_galley = widgets::galley(ui, label, theme::font(11.0), t.text_tertiary, width);
                    widgets::galley_at(&painter, Pos2::new(left, y), label_galley, t.text_tertiary);
                    y += 16.0;
                    if label == "路径" {
                        let galley = widgets::galley_wrap(ui, &value, theme::font(12.0), t.text_secondary, width, 3);
                        let height = galley.size().y;
                        widgets::galley_at(&painter, Pos2::new(left, y), galley, t.text_secondary);
                        y += height + 8.0;
                    } else {
                        let value_galley = widgets::galley(ui, &value, theme::font(12.0), t.text_secondary, width);
                        widgets::galley_at(&painter, Pos2::new(left, y), value_galley, t.text_secondary);
                        y += 24.0;
                    }
                }
                if want_calc_size && let Some(path) = entry.path.clone() {
                    let btn = Rect::from_min_size(Pos2::new(left, y), Vec2::new(108.0, 26.0));
                    let bid = egui::Id::unique("details-calc-size");
                    let bresp = ui.interact(btn, bid, Sense::click());
                    let bhover = widgets::hover_t(ui.ctx(), bid, bresp.hovered());
                    painter.rect_filled(
                        btn,
                        CornerRadius::same(5),
                        if bhover > 0.0 { t.hover_strong } else { t.surface_alt },
                    );
                    widgets::stroke_rect(&painter, btn, 5, Stroke::new(1.0, t.border));
                    let bg = widgets::galley(ui, "计算文件夹大小", theme::font(11.5), t.accent, 100.0);
                    widgets::galley_at(&painter, btn.center() - bg.size() * 0.5, bg, t.accent);
                    if bresp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if bresp.clicked() {
                        self.folder_size = Some(spawn_folder_size(path));
                    }
                    y += 34.0;
                }

                // Queue async text preview off the UI thread (never stalls on SMB/OneDrive).
                self.refresh_text_preview(entry);
                if let Some(preview) = &self.text_preview
                    && !preview.text.is_empty()
                    && y < rect.bottom() - 80.0
                {
                    y += 6.0;
                    let text_rect =
                        Rect::from_min_max(Pos2::new(left, y), Pos2::new(left + width, rect.bottom() - 10.0));
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect));
                    child.set_clip_rect(text_rect);
                    egui::ScrollArea::vertical().id_salt("details-text-preview").auto_shrink([false, false]).show(
                        &mut child,
                        |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&preview.text).font(theme::mono(11.0)).color(t.text_secondary),
                                )
                                .wrap(),
                            );
                        },
                    );
                }
            }
            None => {
                if selected_count > 1 {
                    let label = format!("已选择 {selected_count} 个项目");
                    let galley = widgets::galley(ui, &label, theme::semibold(14.0), t.text, width);
                    widgets::galley_at(&painter, Pos2::new(left, y), galley, t.text);
                    y += 30.0;
                    let selected_files = selected_count.saturating_sub(selected_folders);
                    let mut rows: Vec<(&str, String)> =
                        vec![("文件夹", format!("{selected_folders} 个")), ("文件", format!("{selected_files} 个"))];
                    if selected_size > 0 {
                        rows.push(("总大小（文件）", format_size(selected_size)));
                    }
                    rows.push(("位置", dir.parsing_name()));
                    for (label, value) in rows {
                        let label_galley = widgets::galley(ui, label, theme::font(11.0), t.text_tertiary, width);
                        widgets::galley_at(&painter, Pos2::new(left, y), label_galley, t.text_tertiary);
                        y += 16.0;
                        let value_galley = widgets::galley(ui, &value, theme::font(12.0), t.text_secondary, width);
                        widgets::galley_at(&painter, Pos2::new(left, y), value_galley, t.text_secondary);
                        y += 24.0;
                    }
                } else {
                    let label = dir.display.clone();
                    let galley = widgets::galley(ui, &label, theme::semibold(14.0), t.text, width);
                    widgets::galley_at(&painter, Pos2::new(left, y), galley, t.text);
                    y += 30.0;
                    let rows: Vec<(&str, String)> = vec![
                        ("类型", "文件夹".to_string()),
                        ("项目数", format!("{total}")),
                        ("位置", dir.parsing_name()),
                    ];
                    for (label, value) in rows {
                        let label_galley = widgets::galley(ui, label, theme::font(11.0), t.text_tertiary, width);
                        widgets::galley_at(&painter, Pos2::new(left, y), label_galley, t.text_tertiary);
                        y += 16.0;
                        let value_galley = widgets::galley(ui, &value, theme::font(12.0), t.text_secondary, width);
                        widgets::galley_at(&painter, Pos2::new(left, y), value_galley, t.text_secondary);
                        y += 24.0;
                    }
                }
            }
        }

        let _ = ui.interact(rect, egui::Id::unique("details-pane"), Sense::hover());
    }

    fn refresh_text_preview(&mut self, entry: &Entry) {
        let Some(path) = entry.path.as_ref() else {
            self.text_preview = None;
            self.text_preview_rx = None;
            self.text_preview_pending = None;
            return;
        };
        if entry.is_folder || entry.is_cloud_only() || !TEXT_EXT.contains(&entry.ext.as_str()) || entry.size == 0 {
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
