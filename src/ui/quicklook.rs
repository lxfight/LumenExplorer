//! Spacebar Quick Look floating modal preview (high-res image, scrollable text/code,
//! and metadata summary) with live Up/Down navigation and zero UI-thread blocking.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crossbeam_channel::Receiver;
use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::{Command, LumenApp};
use crate::core::{Entry, format_size, format_time};
use crate::theme;
use crate::ui::{details, icons, widgets};

const IMAGE_EXTS: [&str; 9] = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tif", "tiff"];
const TEXT_EXTS: [&str; 32] = [
    "txt",
    "log",
    "md",
    "rs",
    "toml",
    "json",
    "yml",
    "yaml",
    "ini",
    "cfg",
    "cs",
    "js",
    "ts",
    "tsx",
    "jsx",
    "py",
    "c",
    "cpp",
    "h",
    "hpp",
    "ps1",
    "bat",
    "cmd",
    "sh",
    "xml",
    "html",
    "htm",
    "css",
    "sql",
    "csv",
    "gitignore",
    "env",
];
const QUICKLOOK_TEXT_CAP: u64 = 128 * 1024;
const MAX_IMAGE_DIM: u32 = 1600;

pub(crate) enum QuickLookPayload {
    Image { rgba: Vec<u8>, width: usize, height: usize, orig_w: u32, orig_h: u32 },
    Text(String),
    None,
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
        };
        if let Some(path) = entry.path.clone()
            && !entry.is_folder
            && !entry.is_cloud_only()
        {
            let ext = entry.ext.clone();
            if IMAGE_EXTS.contains(&ext.as_str()) || TEXT_EXTS.contains(&ext.as_str()) {
                state.loading = true;
                state.rx = Some(spawn_quicklook_loader(path, ext));
            }
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
                let tex = ctx.load_texture(format!("ql-{}", self.name), color, egui::TextureOptions::LINEAR);
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

fn spawn_quicklook_loader(path: PathBuf, ext: String) -> Receiver<QuickLookPayload> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .name("lumen-quicklook".to_string())
        .spawn(move || {
            let payload = load_payload(&path, &ext);
            let _ = tx.try_send(payload);
            crate::platform::wake();
        })
        .expect("spawn quicklook loader");
    rx
}

fn load_payload(path: &Path, ext: &str) -> QuickLookPayload {
    if IMAGE_EXTS.contains(&ext) {
        if let Ok(img) = image::open(path) {
            let orig_w = img.width();
            let orig_h = img.height();
            let scaled = if orig_w > MAX_IMAGE_DIM || orig_h > MAX_IMAGE_DIM {
                img.thumbnail(MAX_IMAGE_DIM, MAX_IMAGE_DIM)
            } else {
                img
            };
            let rgba = scaled.to_rgba8();
            let (w, h) = (rgba.width() as usize, rgba.height() as usize);
            return QuickLookPayload::Image { rgba: rgba.into_raw(), width: w, height: h, orig_w, orig_h };
        }
        return QuickLookPayload::None;
    }
    if TEXT_EXTS.contains(&ext)
        && let Ok(file) = std::fs::File::open(path)
    {
        let mut buf = Vec::with_capacity(8192);
        if file.take(QUICKLOOK_TEXT_CAP).read_to_end(&mut buf).is_ok() {
            let decoded = details::decode_text_bytes(&buf);
            let text: String = decoded.chars().take(24_000).collect();
            return QuickLookPayload::Text(text);
        }
    }
    QuickLookPayload::None
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

        let (close_key, step_delta) = ctx.input(|i| {
            let close = i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Space);
            let step = if i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowRight) {
                1isize
            } else if i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::ArrowLeft) {
                -1isize
            } else {
                0isize
            };
            (close, step)
        });
        if close_key || scrim.clicked() {
            crate::ui::spring::clear(&ctx, pop_id);
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
                Pos2::new(header.right() - 120.0, header.bottom()),
            ),
            &state.name,
            theme::semibold(14.0),
            t.text,
            0.0,
        );
        let close_btn = Rect::from_center_size(Pos2::new(header.right() - 22.0, header.center().y), Vec2::splat(28.0));
        let close_id = egui::Id::unique("ql-close");
        let close_resp = ui.interact(close_btn, close_id, Sense::click());
        let close_hover = widgets::hover_t(&ctx, close_id, close_resp.hovered());
        if close_hover > 0.0 {
            painter.rect_filled(close_btn, CornerRadius::same(5), widgets::fade(t.hover_strong, close_hover));
        }
        widgets::icon_at(&painter, close_btn.center(), icons::CANCEL, 10.5, t.text_secondary);
        if close_resp.clicked() {
            return;
        }
        painter.line_segment(
            [Pos2::new(card.left() + 12.0, header.bottom()), Pos2::new(card.right() - 12.0, header.bottom())],
            Stroke::new(1.0, t.divider),
        );

        // Footer bar.
        let footer = Rect::from_min_max(Pos2::new(card.left(), card.bottom() - 46.0), card.max);
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
        widgets::text_left(
            ui,
            &painter,
            Rect::from_min_max(
                Pos2::new(footer.left() + 16.0, footer.top()),
                Pos2::new(footer.right() - 190.0, footer.bottom()),
            ),
            &meta_parts.join("  ·  "),
            theme::font(11.5),
            t.text_secondary,
            0.0,
        );

        let mut open_clicked = false;
        let open_btn =
            Rect::from_center_size(Pos2::new(footer.right() - 56.0, footer.center().y), Vec2::new(84.0, 28.0));
        let open_id = egui::Id::unique("ql-open");
        let open_resp = ui.interact(open_btn, open_id, Sense::click());
        let open_hover = widgets::hover_t(&ctx, open_id, open_resp.hovered());
        let fill = if open_hover > 0.0 { widgets::fade(t.accent, 0.85) } else { t.accent };
        painter.rect_filled(open_btn, CornerRadius::same(5), fill);
        let og = widgets::galley(ui, "打开", theme::semibold(12.0), Color32::WHITE, 70.0);
        widgets::galley_at(&painter, open_btn.center() - og.size() * 0.5, og, Color32::WHITE);
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
            let sz = tex.size_vec2();
            let scale = ((body.width() - 16.0) / sz.x).min((body.height() - 16.0) / sz.y).min(1.0);
            let dst = Rect::from_center_size(body.center(), sz * scale);
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
            let g = widgets::galley(ui, &sub, theme::font(12.0), t.text_tertiary, body.width() - 40.0);
            widgets::galley_at(
                &painter,
                Pos2::new(body.center().x - g.size().x * 0.5, icon_box.bottom() + 18.0),
                g,
                t.text_tertiary,
            );
            let hint =
                widgets::galley(ui, "按 ↑/↓ 切换预览 · 空格/Esc 关闭", theme::font(11.0), t.text_disabled, 320.0);
            widgets::galley_at(
                &painter,
                Pos2::new(body.center().x - hint.size().x * 0.5, body.bottom() - 24.0),
                hint,
                t.text_disabled,
            );
        }

        if open_clicked {
            self.command(Command::Open);
            return;
        }

        self.quick_look = Some(state);

        if step_delta != 0 {
            let count = self.active().entries.len();
            if count > 0 {
                let current =
                    self.active().cursor.or_else(|| self.active().selection.iter().next().copied()).unwrap_or(0);
                let next = (current as isize + step_delta).clamp(0, count as isize - 1) as usize;
                if next != current {
                    self.active_mut().focus_row(next);
                    self.sync_quick_look_to_selection();
                }
            }
        }
    }
}
