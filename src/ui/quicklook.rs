//! Spacebar Quick Look floating modal preview (high-res image, scrollable text/code,
//! and metadata summary) with live visible-order navigation and zero UI-thread blocking.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{Color32, CornerRadius, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::{Command, LumenApp};
use crate::core::{Entry, format_size, format_time};
use crate::theme;
use crate::ui::{details, icons, widgets};

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tif", "tiff"];
const QUICKLOOK_TEXT_CAP: u64 = 128 * 1024;
const QUICKLOOK_CHAR_CAP: usize = 24_000;
const MAX_IMAGE_DIM: u32 = 1600;
const FOOTER_H: f32 = 52.0;
const FOOTER_RIGHT_RESERVE: f32 = 112.0;
const FOOTER_HINTS: &str = "←/→ 切换  ·  Enter 打开  ·  Space/Esc 关闭";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum QuickLookLoadKind {
    Image,
    Text,
}

pub(crate) enum QuickLookPayload {
    Image { rgba: Vec<u8>, width: usize, height: usize, orig_w: u32, orig_h: u32 },
    Text(String),
    None,
}

struct QuickLookRequest {
    generation: u64,
    path: PathBuf,
    kind: QuickLookLoadKind,
    tx: Sender<QuickLookPayload>,
}

/// Single persistent background worker with generation-based cancellation so rapid
/// arrow-key stepping cancels stale decodes instead of spawning unbounded threads.
struct QuickLookWorker {
    tx: Sender<QuickLookRequest>,
    latest_gen: Arc<AtomicU64>,
}

impl QuickLookWorker {
    fn spawn_with<F>(name: &str, loader: F) -> Self
    where
        F: Fn(&Path, QuickLookLoadKind, &AtomicU64, u64) -> QuickLookPayload + Send + 'static,
    {
        let (tx, rx) = unbounded::<QuickLookRequest>();
        let latest_gen = Arc::new(AtomicU64::new(0));
        let gen_bg = latest_gen.clone();
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                while let Ok(mut req) = rx.recv() {
                    // Drain queued bursts so only the most recent request is decoded.
                    while let Ok(newer) = rx.try_recv() {
                        req = newer;
                    }
                    if gen_bg.load(Ordering::Acquire) != req.generation {
                        continue;
                    }
                    let payload = loader(&req.path, req.kind, &gen_bg, req.generation);
                    if gen_bg.load(Ordering::Acquire) != req.generation {
                        continue;
                    }
                    let _ = req.tx.try_send(payload);
                    crate::platform::wake();
                }
            })
            .expect("spawn quicklook worker");
        Self { tx, latest_gen }
    }

    fn submit(&self, path: PathBuf, kind: QuickLookLoadKind) -> (u64, Receiver<QuickLookPayload>) {
        let generation = self.latest_gen.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        let (tx, rx) = crossbeam_channel::bounded(1);
        let _ = self.tx.send(QuickLookRequest { generation, path, kind, tx });
        (generation, rx)
    }

    fn cancel_if_current(&self, generation: u64) {
        if generation != 0 {
            let _ = self.latest_gen.compare_exchange(
                generation,
                generation.wrapping_add(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

static WORKER: OnceLock<QuickLookWorker> = OnceLock::new();

fn quicklook_worker() -> &'static QuickLookWorker {
    WORKER.get_or_init(|| QuickLookWorker::spawn_with("lumen-quicklook", load_payload))
}

fn classify_load_kind(ext: &str, path: Option<&Path>, name: &str) -> Option<QuickLookLoadKind> {
    let norm = details::effective_ext(ext, path, name);
    if IMAGE_EXTS.contains(&norm.as_str()) {
        return Some(QuickLookLoadKind::Image);
    }
    if details::is_text_file(ext, path, name) {
        return Some(QuickLookLoadKind::Text);
    }
    None
}

pub(crate) struct QuickLookState {
    pub path: Option<PathBuf>,
    pub name: String,
    pub is_folder: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub kind: String,
    pub ext: String,
    pub image: Option<egui::TextureHandle>,
    pub image_dims: Option<(u32, u32)>,
    pub text: Option<String>,
    pub loading: bool,
    pub rx: Option<Receiver<QuickLookPayload>>,
    generation: u64,
}

impl Drop for QuickLookState {
    fn drop(&mut self) {
        if let Some(worker) = WORKER.get() {
            worker.cancel_if_current(self.generation);
        }
    }
}

impl QuickLookState {
    pub(crate) fn open(entry: &Entry) -> Self {
        let mut state = Self {
            path: entry.path.clone(),
            name: entry.name.clone(),
            is_folder: entry.is_folder,
            size: entry.size,
            modified: entry.modified,
            kind: entry.kind.clone(),
            ext: entry.ext.clone(),
            image: None,
            image_dims: None,
            text: None,
            loading: false,
            rx: None,
            generation: 0,
        };
        if let Some(path) = entry.path.clone()
            && !entry.is_folder
            && !entry.is_cloud_only()
            && let Some(load_kind) = classify_load_kind(&entry.ext, Some(&path), &entry.name)
        {
            let (generation, rx) = quicklook_worker().submit(path, load_kind);
            state.generation = generation;
            state.loading = true;
            state.rx = Some(rx);
        }
        state
    }

    pub(crate) fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else { return };
        let Ok(payload) = rx.try_recv() else { return };
        self.rx = None;
        self.loading = false;
        match payload {
            QuickLookPayload::Image { rgba, width, height, orig_w, orig_h } => {
                let color = egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
                let tex = ctx.load_texture(
                    format!("ql-{}-{}", self.generation, self.name),
                    color,
                    egui::TextureOptions::LINEAR,
                );
                self.image = Some(tex);
                self.image_dims = Some((orig_w, orig_h));
            }
            QuickLookPayload::Text(text) => {
                self.text = Some(text);
            }
            QuickLookPayload::None => {}
        }
    }
}

fn oriented_dimensions(w: u32, h: u32, orientation: image::metadata::Orientation) -> (u32, u32) {
    use image::metadata::Orientation;
    match orientation {
        Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH => {
            (h, w)
        }
        _ => (w, h),
    }
}

/// Decodes an image file, applies its EXIF/TIFF orientation metadata, caps to
/// `MAX_IMAGE_DIM` for crisp preview performance, and returns the oriented image
/// alongside its true post-orientation pixel dimensions.
fn decode_oriented_image(
    path: &Path,
    latest_gen: &AtomicU64,
    generation: u64,
) -> Option<(image::DynamicImage, u32, u32)> {
    use image::{DynamicImage, ImageDecoder, ImageReader, Limits, metadata::Orientation};
    let reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let mut limits = Limits::default();
    let mut decoder = reader.into_decoder().ok()?;
    limits.reserve(decoder.total_bytes()).ok()?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    if latest_gen.load(Ordering::Acquire) != generation {
        return None;
    }
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    if latest_gen.load(Ordering::Acquire) != generation {
        return None;
    }
    let (raw_w, raw_h) = (img.width(), img.height());
    let (orig_w, orig_h) = oriented_dimensions(raw_w, raw_h, orientation);
    if raw_w > MAX_IMAGE_DIM || raw_h > MAX_IMAGE_DIM {
        img = img.thumbnail(MAX_IMAGE_DIM, MAX_IMAGE_DIM);
    }
    img.apply_orientation(orientation);
    Some((img, orig_w, orig_h))
}

fn load_payload(path: &Path, kind: QuickLookLoadKind, latest_gen: &AtomicU64, generation: u64) -> QuickLookPayload {
    match kind {
        QuickLookLoadKind::Image => {
            if let Some((img, orig_w, orig_h)) = decode_oriented_image(path, latest_gen, generation) {
                if latest_gen.load(Ordering::Acquire) != generation {
                    return QuickLookPayload::None;
                }
                let rgba = img.to_rgba8();
                let (w, h) = (rgba.width() as usize, rgba.height() as usize);
                return QuickLookPayload::Image { rgba: rgba.into_raw(), width: w, height: h, orig_w, orig_h };
            }
            QuickLookPayload::None
        }
        QuickLookLoadKind::Text => match details::read_text_preview(path, QUICKLOOK_TEXT_CAP, QUICKLOOK_CHAR_CAP) {
            Some(text) => QuickLookPayload::Text(text),
            None => QuickLookPayload::None,
        },
    }
}

/// Converts a decoded texture's pixel size into DPI-aware logical points and scales
/// down (never up) so it fits cleanly inside `viewport` with 8px padding on each side.
fn fit_image_logical_size(tex_px: Vec2, viewport: Vec2, pixels_per_point: f32) -> Vec2 {
    let ppp = pixels_per_point.max(0.5);
    let logical_sz = tex_px / ppp;
    if logical_sz.x <= 0.0 || logical_sz.y <= 0.0 {
        return Vec2::ZERO;
    }
    let avail = (viewport - Vec2::splat(16.0)).max(Vec2::splat(1.0));
    let scale = (avail.x / logical_sz.x).min(avail.y / logical_sz.y).min(1.0);
    logical_sz * scale
}

fn clear_quicklook_focus(ctx: &egui::Context, open_id: egui::Id, close_id: egui::Id) {
    ctx.memory_mut(|m| {
        if m.has_focus(open_id) {
            m.surrender_focus(open_id);
        }
        if m.has_focus(close_id) {
            m.surrender_focus(close_id);
        }
    });
}

impl LumenApp {
    pub(crate) fn toggle_quick_look(&mut self) {
        if self.quick_look.is_some() {
            self.quick_look = None;
            return;
        }
        self.sync_quick_look_to_selection();
    }

    pub(crate) fn sync_quick_look_to_selection(&mut self) {
        let entry = self
            .active()
            .cursor
            .or_else(|| self.active().selection.iter().next().copied())
            .and_then(|idx| self.active().entries.get(idx))
            .cloned();
        if let Some(entry) = entry {
            self.quick_look = Some(QuickLookState::open(&entry));
        }
    }

    pub(crate) fn paint_quick_look(&mut self, ui: &mut egui::Ui, full: Rect) {
        let ctx = ui.ctx().clone();
        let pop_id = egui::Id::unique("ql-modal-pop");
        let open_id = egui::Id::unique("ql-open");
        let close_id = egui::Id::unique("ql-close");
        let Some(mut state) = self.quick_look.take() else {
            crate::ui::spring::clear(&ctx, pop_id);
            return;
        };
        state.poll(&ctx);

        let t = self.theme.clone();
        let painter = ui.painter().clone();
        let pop = crate::ui::spring::animate(&ctx, pop_id, 1.0, 0.0, crate::ui::spring::MODAL);
        let alpha = pop.clamp(0.0, 1.0);
        let scale = 0.93 + 0.07 * pop;

        // Scrim + click-outside dismissal.
        painter.rect_filled(full, CornerRadius::ZERO, Color32::from_black_alpha((115.0 * alpha) as u8));
        let scrim = ui.interact(full, egui::Id::unique("ql-scrim"), Sense::click());

        // Capture & trap keyboard focus inside Quick Look so Tab and arrows never leak
        // into the background file list or sidebar while the modal is open.
        const FOCUS_LOCK: egui::EventFilter =
            egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };
        let mut focused = ctx.memory(|m| m.focused());
        if focused != Some(open_id) && focused != Some(close_id) {
            ctx.memory_mut(|m| {
                m.request_focus(open_id);
                m.move_focus(egui::FocusDirection::None);
            });
            focused = Some(open_id);
        } else {
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }

        let (close_key, enter_key, tab_key, step_delta) = ctx.input_mut(|i| {
            let close = i.consume_key(Modifiers::NONE, Key::Escape) || i.consume_key(Modifiers::NONE, Key::Space);
            let enter = i.consume_key(Modifiers::NONE, Key::Enter);
            let tab = i.consume_key(Modifiers::NONE, Key::Tab) || i.consume_key(Modifiers::SHIFT, Key::Tab);
            let step = if i.consume_key(Modifiers::NONE, Key::ArrowDown)
                || i.consume_key(Modifiers::NONE, Key::ArrowRight)
            {
                1isize
            } else if i.consume_key(Modifiers::NONE, Key::ArrowUp) || i.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                -1isize
            } else {
                0isize
            };
            (close, enter, tab, step)
        });

        if tab_key {
            let next_focus = if focused == Some(open_id) { close_id } else { open_id };
            ctx.memory_mut(|m| {
                m.request_focus(next_focus);
                m.move_focus(egui::FocusDirection::None);
            });
            focused = Some(next_focus);
        }
        if let Some(active_id) = focused {
            ctx.memory_mut(|m| m.set_focus_lock_filter(active_id, FOCUS_LOCK));
        }

        if close_key || scrim.clicked() || (enter_key && focused == Some(close_id)) {
            crate::ui::spring::clear(&ctx, pop_id);
            clear_quicklook_focus(&ctx, open_id, close_id);
            return;
        }

        let card_w = (full.width() * 0.72).clamp(440.0, 960.0);
        let card_h = (full.height() * 0.78).clamp(360.0, 720.0);
        let base_card = Rect::from_center_size(full.center(), Vec2::new(card_w, card_h));
        let card = crate::ui::spring::scale_rect_around(base_card, base_card.center(), scale);
        painter.rect_filled(card.translate(Vec2::new(0.0, 6.0)).expand(3.0), CornerRadius::same(12), t.shadow);
        painter.rect_filled(card, CornerRadius::same(10), t.popup);
        painter.rect_stroke(card, CornerRadius::same(10), Stroke::new(1.0, t.border_strong), egui::StrokeKind::Inside);
        let _ = ui.interact(card, egui::Id::unique("ql-card"), Sense::click());

        // Header bar.
        let header = Rect::from_min_size(card.min, Vec2::new(card.width(), 44.0));
        widgets::text_left(
            ui,
            &painter,
            Rect::from_min_max(
                Pos2::new(header.left() + 16.0, header.top()),
                Pos2::new(header.right() - 48.0, header.bottom()),
            ),
            &state.name,
            theme::semibold(14.0),
            t.text,
            0.0,
        );
        let close_btn = Rect::from_center_size(Pos2::new(header.right() - 22.0, header.center().y), Vec2::splat(28.0));
        let close_resp = ui.interact(close_btn, close_id, Sense::click());
        let close_hover = widgets::hover_t(&ctx, close_id, close_resp.hovered());
        if close_hover > 0.0 {
            painter.rect_filled(close_btn, CornerRadius::same(5), widgets::fade(t.hover_strong, close_hover));
        }
        if focused == Some(close_id) {
            widgets::stroke_rect(&painter, close_btn, 5, Stroke::new(1.5, t.accent_text));
        }
        widgets::icon_at(&painter, close_btn.center(), icons::CANCEL, 10.5, t.text_secondary);
        if close_resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if close_resp.clicked() {
            crate::ui::spring::clear(&ctx, pop_id);
            clear_quicklook_focus(&ctx, open_id, close_id);
            return;
        }
        painter.line_segment(
            [Pos2::new(card.left() + 12.0, header.bottom()), Pos2::new(card.right() - 12.0, header.bottom())],
            Stroke::new(1.0, t.divider),
        );

        // Footer bar.
        let footer = Rect::from_min_max(Pos2::new(card.left(), card.bottom() - FOOTER_H), card.max);
        painter.line_segment(
            [Pos2::new(card.left() + 12.0, footer.top()), Pos2::new(card.right() - 12.0, footer.top())],
            Stroke::new(1.0, t.divider),
        );

        let mut meta_parts = vec![state.kind.clone()];
        if !state.is_folder {
            meta_parts.push(format_size(state.size));
        }
        if let Some((w, h)) = state.image_dims {
            meta_parts.push(format!("{w} × {h} px"));
        }
        let mtime = format_time(state.modified);
        if !mtime.is_empty() {
            meta_parts.push(mtime);
        }
        let footer_text_left = footer.left() + 16.0;
        let footer_text_right = (footer.right() - FOOTER_RIGHT_RESERVE).max(footer_text_left + 40.0);
        widgets::text_left(
            ui,
            &painter,
            Rect::from_min_max(
                Pos2::new(footer_text_left, footer.top() + 7.0),
                Pos2::new(footer_text_right, footer.top() + 25.0),
            ),
            &meta_parts.join("  ·  "),
            theme::font(11.5),
            t.text_secondary,
            0.0,
        );
        widgets::text_left(
            ui,
            &painter,
            Rect::from_min_max(
                Pos2::new(footer_text_left, footer.top() + 27.0),
                Pos2::new(footer_text_right, footer.bottom() - 7.0),
            ),
            FOOTER_HINTS,
            theme::font(10.5),
            t.text_tertiary,
            0.0,
        );

        let mut open_clicked = enter_key;
        let open_btn =
            Rect::from_center_size(Pos2::new(footer.right() - 56.0, footer.center().y), Vec2::new(84.0, 28.0));
        let open_resp = ui.interact(open_btn, open_id, Sense::click());
        let open_hover = widgets::hover_t(&ctx, open_id, open_resp.hovered());
        let fill = if open_hover > 0.0 { widgets::fade(t.accent, 0.85) } else { t.accent };
        painter.rect_filled(open_btn, CornerRadius::same(5), fill);
        let og = widgets::galley(ui, "打开", theme::semibold(12.0), t.text_on_accent, 70.0);
        widgets::galley_at(&painter, open_btn.center() - og.size() * 0.5, og, t.text_on_accent);
        if open_resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if open_resp.clicked() {
            open_clicked = true;
        }

        // Body content area.
        let body = Rect::from_min_max(
            Pos2::new(card.left() + 16.0, header.bottom() + 12.0),
            Pos2::new(card.right() - 16.0, footer.top() - 12.0),
        );
        if state.loading {
            let g = widgets::galley(ui, "正在加载预览…", theme::font(13.0), t.text_tertiary, 200.0);
            widgets::galley_at(&painter, body.center() - g.size() * 0.5, g, t.text_tertiary);
        } else if let Some(tex) = &state.image {
            painter.rect_filled(body, CornerRadius::same(6), t.surface_alt);
            let draw_sz = fit_image_logical_size(tex.size_vec2(), body.size(), ctx.pixels_per_point());
            let dst = Rect::from_center_size(body.center(), draw_sz);
            painter.image(tex.id(), dst, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        } else if let Some(text) = &state.text {
            painter.rect_filled(body, CornerRadius::same(6), t.surface_alt);
            let text_rect = body.shrink(10.0);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect));
            child.set_clip_rect(text_rect);
            egui::ScrollArea::vertical().id_salt("ql-text-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(text).font(theme::mono(12.0)).color(t.text)).wrap());
            });
        } else {
            let icon = self.shell_icon(ui.ctx(), state.path.as_deref(), &state.ext, state.is_folder, 128, true);
            let icon_box =
                Rect::from_center_size(Pos2::new(body.center().x, body.center().y - 26.0), Vec2::splat(96.0));
            match icon {
                Some(tex) => {
                    painter.image(
                        tex.id(),
                        icon_box,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                None => {
                    widgets::icon_at(
                        &painter,
                        icon_box.center(),
                        if state.is_folder { icons::FOLDER } else { icons::FILE },
                        56.0,
                        t.accent,
                    );
                }
            }
            let sub =
                state.path.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| state.kind.clone());
            let g = widgets::galley_wrap(ui, &sub, theme::font(12.0), t.text_tertiary, body.width() - 40.0, 2);
            widgets::galley_at(
                &painter,
                Pos2::new(body.center().x - g.size().x * 0.5, icon_box.bottom() + 18.0),
                g,
                t.text_tertiary,
            );
        }

        if open_clicked {
            crate::ui::spring::clear(&ctx, pop_id);
            clear_quicklook_focus(&ctx, open_id, close_id);
            self.command(Command::Open);
            return;
        }

        self.quick_look = Some(state);

        if step_delta != 0 {
            let current = self.active().cursor.or_else(|| self.active().selection.iter().next().copied());
            if let Some(next) = self.active().step_visible_entry(current, step_delta)
                && Some(next) != current
            {
                self.active_mut().focus_row(next);
                self.sync_quick_look_to_selection();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    #[test]
    fn classify_recognizes_dotfiles_and_tiff() {
        assert_eq!(classify_load_kind("", Some(Path::new(".gitignore")), ".gitignore"), Some(QuickLookLoadKind::Text));
        assert_eq!(
            classify_load_kind("gitignore", Some(Path::new(".gitignore")), ".gitignore"),
            Some(QuickLookLoadKind::Text)
        );
        assert_eq!(classify_load_kind("", Some(Path::new(".env")), ".env"), Some(QuickLookLoadKind::Text));
        assert_eq!(classify_load_kind("", Some(Path::new(".env.local")), ".env.local"), Some(QuickLookLoadKind::Text));
        assert_eq!(
            classify_load_kind("", Some(Path::new(".editorconfig")), ".editorconfig"),
            Some(QuickLookLoadKind::Text)
        );
        assert_eq!(classify_load_kind("tif", Some(Path::new("a.tif")), "a.tif"), Some(QuickLookLoadKind::Image));
        assert_eq!(classify_load_kind("tiff", Some(Path::new("a.tiff")), "a.tiff"), Some(QuickLookLoadKind::Image));
        assert_eq!(classify_load_kind("bin", Some(Path::new("a.bin")), "a.bin"), None);
    }

    #[test]
    fn dpi_scaling_renders_small_images_at_logical_size_and_fits_large_images() {
        // 200x200 px image at 200% DPI -> 100x100 logical pt (1:1 physical pixels, no blur).
        let sz = fit_image_logical_size(Vec2::new(200.0, 200.0), Vec2::new(600.0, 500.0), 2.0);
        assert!((sz.x - 100.0).abs() < 1e-3 && (sz.y - 100.0).abs() < 1e-3);

        // 1600x1200 px image at 200% DPI (800x600 logical) inside 600x500 (avail 584x484) -> scales down cleanly.
        let big = fit_image_logical_size(Vec2::new(1600.0, 1200.0), Vec2::new(600.0, 500.0), 2.0);
        assert!(big.x <= 584.0 + 1e-3 && big.y <= 484.0 + 1e-3);
        assert!((big.x / big.y - (4.0 / 3.0)).abs() < 1e-3);

        // 200x200 px image at 100% DPI -> 200x200 logical pt.
        let sz1 = fit_image_logical_size(Vec2::new(200.0, 200.0), Vec2::new(600.0, 500.0), 1.0);
        assert!((sz1.x - 200.0).abs() < 1e-3 && (sz1.y - 200.0).abs() < 1e-3);
    }

    #[test]
    fn exif_orientation_rotates_jpeg_and_swaps_dimensions() {
        use image::metadata::Orientation;

        assert_eq!(oriented_dimensions(400, 200, Orientation::NoTransforms), (400, 200));
        assert_eq!(oriented_dimensions(400, 200, Orientation::Rotate180), (400, 200));
        assert_eq!(oriented_dimensions(400, 200, Orientation::Rotate90), (200, 400));
        assert_eq!(oriented_dimensions(400, 200, Orientation::Rotate270), (200, 400));
        assert_eq!(oriented_dimensions(400, 200, Orientation::Rotate90FlipH), (200, 400));
        assert_eq!(oriented_dimensions(400, 200, Orientation::Rotate270FlipH), (200, 400));

        // Build a 16x8 RGB JPEG where the top-left 8x8 block is bright red and the
        // right 8x8 block is bright blue, then inject an EXIF APP1 Orientation=6 (Rotate90)
        // segment right after the JPEG SOI marker (FF D8).
        let mut raw_img = image::RgbImage::new(16, 8);
        for y in 0..8 {
            for x in 0..8 {
                raw_img.put_pixel(x, y, image::Rgb([240, 20, 20]));
            }
            for x in 8..16 {
                raw_img.put_pixel(x, y, image::Rgb([20, 20, 240]));
            }
        }
        let mut jpeg_bytes = Vec::new();
        image::DynamicImage::ImageRgb8(raw_img)
            .write_to(&mut std::io::Cursor::new(&mut jpeg_bytes), image::ImageFormat::Jpeg)
            .expect("encode jpeg");
        assert_eq!(&jpeg_bytes[..2], &[0xFF, 0xD8]);

        // Minimal little-endian TIFF EXIF chunk with 1 IFD entry: tag 0x0112 (Orientation), type SHORT (3), count 1, value 6 (Rotate90).
        let mut exif_payload: Vec<u8> = Vec::new();
        exif_payload.extend_from_slice(b"Exif\0\0");
        exif_payload.extend_from_slice(&[b'I', b'I', 42, 0]); // Little endian TIFF header
        exif_payload.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset = 8
        exif_payload.extend_from_slice(&1u16.to_le_bytes()); // 1 IFD entry
        exif_payload.extend_from_slice(&0x0112u16.to_le_bytes()); // Tag: Orientation
        exif_payload.extend_from_slice(&3u16.to_le_bytes()); // Type: SHORT
        exif_payload.extend_from_slice(&1u32.to_le_bytes()); // Count: 1
        exif_payload.extend_from_slice(&6u16.to_le_bytes()); // Value: 6 (Rotate90 CW)
        exif_payload.extend_from_slice(&0u16.to_le_bytes()); // Padding
        exif_payload.extend_from_slice(&0u32.to_le_bytes()); // Next IFD offset = 0

        let mut with_exif = Vec::with_capacity(jpeg_bytes.len() + exif_payload.len() + 4);
        with_exif.extend_from_slice(&jpeg_bytes[..2]);
        with_exif.extend_from_slice(&[0xFF, 0xE1]); // APP1 marker
        with_exif.extend_from_slice(&((exif_payload.len() + 2) as u16).to_be_bytes());
        with_exif.extend_from_slice(&exif_payload);
        with_exif.extend_from_slice(&jpeg_bytes[2..]);

        let tmp = std::env::temp_dir().join(format!("lumen-ql-exif-{}.jpg", std::process::id()));
        std::fs::write(&tmp, &with_exif).expect("write test jpeg");

        let current_gen = AtomicU64::new(1);
        let (decoded, orig_w, orig_h) = decode_oriented_image(&tmp, &current_gen, 1).expect("decode jpeg");
        let _ = std::fs::remove_file(&tmp);

        // After 90-degree CW rotation, 16x8 becomes 8x16, red (left half) moves to top half,
        // and blue (right half) moves to bottom half.
        assert_eq!((orig_w, orig_h), (8, 16));
        assert_eq!((decoded.width(), decoded.height()), (8, 16));
        let rgba = decoded.to_rgba8();
        let top_px = rgba.get_pixel(4, 3);
        let bottom_px = rgba.get_pixel(4, 12);
        assert!(top_px[0] > 180 && top_px[2] < 80, "top half should be red after Rotate90, got {top_px:?}");
        assert!(
            bottom_px[2] > 180 && bottom_px[0] < 80,
            "bottom half should be blue after Rotate90, got {bottom_px:?}"
        );
    }

    #[test]
    fn worker_cancels_stale_generations_without_spawning_multiple_threads() {
        let (started_tx, started_rx) = crossbeam_channel::bounded::<()>(1);
        let (gate_tx, gate_rx) = crossbeam_channel::bounded::<()>(1);
        let decoded_count = Arc::new(AtomicUsize::new(0));
        let decoded_bg = decoded_count.clone();
        let worker_tid = Arc::new(std::sync::Mutex::new(None));
        let worker_tid_bg = worker_tid.clone();
        let worker = QuickLookWorker::spawn_with("lumen-ql-test", move |path, _, latest_gen, req_gen| {
            let tid = std::thread::current().id();
            let mut slot = worker_tid_bg.lock().unwrap();
            if let Some(prev) = *slot {
                assert_eq!(prev, tid, "all loads must reuse one persistent worker thread");
            } else {
                *slot = Some(tid);
            }
            drop(slot);

            if path.ends_with("first.txt") {
                let _ = started_tx.try_send(());
                // Hold the worker while a burst of newer requests arrives.
                let _ = gate_rx.recv_timeout(Duration::from_secs(2));
            }
            if latest_gen.load(Ordering::Acquire) != req_gen {
                return QuickLookPayload::None;
            }
            decoded_bg.fetch_add(1, Ordering::SeqCst);
            QuickLookPayload::Text(path.file_name().unwrap().to_string_lossy().into_owned())
        });

        let (_g1, rx1) = worker.submit(PathBuf::from("first.txt"), QuickLookLoadKind::Text);
        started_rx.recv_timeout(Duration::from_secs(2)).expect("first request must enter worker");

        // Submit a rapid burst while "first.txt" is blocked in the single worker.
        let mut stale_rxs = Vec::new();
        for i in 0..10 {
            let (_g, rx) = worker.submit(PathBuf::from(format!("mid_{i}.txt")), QuickLookLoadKind::Text);
            stale_rxs.push(rx);
        }
        let (g_last, rx_last) = worker.submit(PathBuf::from("last.txt"), QuickLookLoadKind::Text);
        // Release the blocked first request.
        let _ = gate_tx.try_send(());

        let payload = rx_last.recv_timeout(Duration::from_secs(2)).expect("latest request must finish");
        match payload {
            QuickLookPayload::Text(name) => assert_eq!(name, "last.txt"),
            _ => panic!("expected text payload"),
        }
        // Only the final request should have produced a live decoded payload.
        assert_eq!(decoded_count.load(Ordering::SeqCst), 1);
        assert!(rx1.try_recv().is_err(), "stale first request must be suppressed");
        for rx in stale_rxs {
            assert!(rx.try_recv().is_err(), "intermediate burst requests must be skipped");
        }

        // Submit another request after the burst and verify the exact same worker thread serves it.
        worker.cancel_if_current(g_last);
        let (_g_next, rx_next) = worker.submit(PathBuf::from("after.txt"), QuickLookLoadKind::Text);
        let next_payload = rx_next.recv_timeout(Duration::from_secs(2)).expect("follow-up request must finish");
        assert!(matches!(next_payload, QuickLookPayload::Text(ref s) if s == "after.txt"));
    }

    #[test]
    fn quicklook_steps_through_visible_grouped_entries_and_traps_focus() {
        use crate::core::SortKey;
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
        let mk = |name: &str| Entry {
            name: name.to_string(),
            path: None,
            is_folder: false,
            size: 10,
            modified: None,
            created: None,
            kind: "文本文档".to_string(),
            free_space: 0,
            ext: "txt".to_string(),
            file_attrs: 0,
            pidl: None,
        };
        let tab = app.active_mut();
        tab.entries = Arc::new(vec![mk("alpha.txt"), mk("beta.txt"), mk("charlie.txt")]);
        tab.group_by = Some(SortKey::Name);
        tab.collapsed_groups.insert("B".to_string());
        tab.rebuild_display();
        tab.focus_row(0);
        app.sync_quick_look_to_selection();
        assert_eq!(app.quick_look.as_ref().map(|q| q.name.as_str()), Some("alpha.txt"));

        let run_ql = |app: &mut LumenApp, events: Vec<egui::Event>| {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 700.0))),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| {
                crate::ui::spring::begin_frame(true);
                let full = ui.max_rect();
                app.paint_quick_look(ui, full);
            });
            out.textures_delta.clear();
        };

        // Frame 1: Quick Look opens and captures focus onto open_id.
        run_ql(&mut app, vec![]);
        let open_id = egui::Id::unique("ql-open");
        let close_id = egui::Id::unique("ql-close");
        let pop_id = egui::Id::unique("ql-modal-pop");
        assert_eq!(ctx.memory(|m| m.focused()), Some(open_id));

        // Tab cycles strictly between open_id and close_id (trapped inside Quick Look).
        run_ql(
            &mut app,
            vec![egui::Event::Key {
                key: Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert_eq!(ctx.memory(|m| m.focused()), Some(close_id));

        run_ql(
            &mut app,
            vec![egui::Event::Key {
                key: Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert_eq!(ctx.memory(|m| m.focused()), Some(open_id));

        // ArrowRight steps through visible grouped entries, skipping collapsed "B" (beta.txt) straight to charlie.txt!
        run_ql(
            &mut app,
            vec![egui::Event::Key {
                key: Key::ArrowRight,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert_eq!(app.quick_look.as_ref().map(|q| q.name.as_str()), Some("charlie.txt"));
        assert_eq!(app.active().cursor, Some(2));

        // Lock pop spring to 1.0, then press Enter to open & close Quick Look;
        // verify pop_id spring was cleared immediately (BUG-41).
        crate::ui::spring::set_immediate(&ctx, pop_id, 1.0);
        run_ql(
            &mut app,
            vec![egui::Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert!(app.quick_look.is_none(), "Enter must close Quick Look after opening");
        crate::ui::spring::begin_frame(true);
        let reopened_pop = crate::ui::spring::animate(&ctx, pop_id, 1.0, 0.0, crate::ui::spring::MODAL);
        assert!(reopened_pop < 0.5, "pop_id spring must have been cleared on Enter close, got {reopened_pop}");

        unsafe { CoUninitialize() };
    }
}
