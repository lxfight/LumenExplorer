//! Window chrome: tab strip, caption buttons, navigation bar and address bar.

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::{Command, LumenApp};
use crate::core::ViewMode;
use crate::platform;
use crate::shell::icons::IconKey;
use crate::theme::{self, Theme};
use crate::ui::{icons, widgets};

/// The caption buttons own the right-most 3 × 46 px; no chrome may be painted
/// underneath them.
const CAPTION_W: f32 = 3.0 * 46.0;
/// Tab width bounds. Tabs shrink to `MIN` before the strip starts scrolling.
const TAB_MIN_W: f32 = 96.0;
const TAB_MAX_W: f32 = 236.0;
const TAB_GAP: f32 = 3.0;

/// Which caption button a rect represents; the click action follows the button
/// identity, not its on-screen position (all three live in the window's right
/// corner, so a position heuristic cannot tell minimize from maximize).
enum CaptionAction {
    Close,
    Minimize,
    Maximize,
}

/// Presentation-only tab strip state: drag reorder and horizontal scroll belong
/// to the chrome, not to the persisted tab model.
#[derive(Clone, Copy)]
struct StripState {
    scroll: f32,
    drag: Option<usize>,
    last_active: usize,
}

impl Default for StripState {
    fn default() -> Self {
        Self { scroll: 0.0, drag: None, last_active: usize::MAX }
    }
}

fn strip_state_id() -> egui::Id {
    egui::Id::unique("tab-strip-state")
}

/// Icon button with hover fill, tooltip and the shared disabled treatment.
// Immediate-mode paint helper: each argument is distinct draw state.
#[allow(clippy::too_many_arguments)]
fn icon_button(
    ui: &mut egui::Ui,
    t: &Theme,
    rect: Rect,
    id: egui::Id,
    glyph: &str,
    size: f32,
    tip: &str,
    active: bool,
    enabled: bool,
) -> egui::Response {
    let ctx = ui.ctx().clone();
    let resp = ui.interact(rect, id, Sense::click());
    let hover = widgets::hover_t(&ctx, id, resp.hovered() && enabled);
    let target_scale = if enabled && resp.is_pointer_button_down_on() { 0.93 } else { 1.0 };
    let scale = crate::ui::spring::animate(&ctx, id.with("scale"), target_scale, 1.0, crate::ui::spring::MAGNETIC);
    let draw_rect = crate::ui::spring::scale_rect_around(rect, rect.center(), scale);
    if active {
        ui.painter().rect_filled(draw_rect, CornerRadius::same(5), t.hover_strong);
    } else if hover > 0.0 {
        ui.painter().rect_filled(draw_rect, CornerRadius::same(5), widgets::fade(t.hover_strong, hover));
    }
    widgets::icon_at(
        ui.painter(),
        draw_rect.center(),
        glyph,
        size * scale.clamp(0.85, 1.1),
        if enabled { t.text } else { t.text_disabled },
    );
    if enabled && resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.hovered() { resp.on_hover_text(tip) } else { resp }
}

impl LumenApp {
    pub(crate) fn shell_icon(
        &mut self,
        ctx: &egui::Context,
        path: Option<&std::path::Path>,
        ext: &str,
        is_folder: bool,
        size: u32,
        thumbnail: bool,
    ) -> Option<egui::TextureHandle> {
        let scale = ctx.pixels_per_point();
        let physical = (size as f32 * scale).round().clamp(16.0, 256.0) as u32;
        let key = IconKey {
            path: path.map(|p| p.to_path_buf()),
            pidl_bytes: None,
            ext: ext.to_string(),
            is_folder,
            size: physical,
            thumbnail,
            stamp: 0,
        };
        let id = self.icons.request(key);
        self.icons.get(id)
    }

    pub(crate) fn shell_icon_entry(
        &mut self,
        ctx: &egui::Context,
        entry: &crate::core::Entry,
        size: u32,
        thumbnail: bool,
    ) -> Option<egui::TextureHandle> {
        let scale = ctx.pixels_per_point();
        let physical = (size as f32 * scale).round().clamp(16.0, 256.0) as u32;
        // Virtual items without a filesystem path fetch their real shell icon via PIDL bytes.
        let pidl_bytes = if entry.path.is_none() {
            entry.pidl.as_ref().map(|p| std::sync::Arc::<[u8]>::from(p.to_bytes().into_boxed_slice()))
        } else {
            None
        };
        // Avoid triggering cloud hydration on OneDrive cloud-only placeholders when requesting thumbnails.
        let want_thumb = thumbnail && !entry.is_cloud_only();
        let key = IconKey {
            path: entry.path.clone(),
            pidl_bytes,
            ext: entry.ext.clone(),
            is_folder: entry.is_folder,
            size: physical,
            thumbnail: want_thumb,
            stamp: if want_thumb { entry.thumb_stamp() } else { 0 },
        };
        let id = self.icons.request(key);
        self.icons.get(id)
    }

    // ---- title bar -------------------------------------------------------

    pub(crate) fn paint_titlebar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let t = self.theme.clone();

        // Caption buttons.
        let btn_w = 46.0;
        let close_btn = Rect::from_min_max(Pos2::new(rect.right() - btn_w, rect.top()), rect.right_bottom());
        let max_btn = Rect::from_min_max(
            Pos2::new(rect.right() - btn_w * 2.0, rect.top()),
            Pos2::new(rect.right() - btn_w, rect.bottom()),
        );
        let min_btn = Rect::from_min_max(
            Pos2::new(rect.right() - btn_w * 3.0, rect.top()),
            Pos2::new(rect.right() - btn_w * 2.0, rect.bottom()),
        );

        let drag_rect = Rect::from_min_max(rect.min, Pos2::new(rect.right() - CAPTION_W, rect.bottom()));
        let drag = ui.interact(drag_rect, egui::Id::unique("titlebar-drag"), Sense::click_and_drag());
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag.double_clicked() {
            platform::toggle_maximize();
        }

        let close_hover = self.caption_button(ui, close_btn, egui::Id::unique("cap-close"), CaptionAction::Close);
        widgets::icon_at(
            &painter,
            close_btn.center(),
            icons::CLOSE,
            11.5,
            if close_hover > 0.5 { Color32::WHITE } else { t.text },
        );

        let _min_hover = self.caption_button(ui, min_btn, egui::Id::unique("cap-min"), CaptionAction::Minimize);
        widgets::icon_at(&painter, min_btn.center(), icons::MINIMIZE, 12.0, t.text);
        let _max_hover = self.caption_button(ui, max_btn, egui::Id::unique("cap-max"), CaptionAction::Maximize);
        widgets::icon_at(
            &painter,
            max_btn.center(),
            if platform::is_maximized() { icons::RESTORE } else { icons::MAXIMIZE },
            11.0,
            t.text,
        );

        // ---- tab strip ---------------------------------------------------
        let tab_top = rect.top() + 5.0;
        let tab_h = theme::TAB_H;
        let plus_w = 28.0;
        let strip_left = rect.left() + 8.0;
        let strip_right = rect.right() - CAPTION_W;
        // The "+" button owns the right end of the strip; tabs never enter it.
        let viewport_right = strip_right - plus_w - 6.0;
        let avail = (viewport_right - strip_left).max(1.0);
        let count = self.tabs.len();
        let gaps = TAB_GAP * count.saturating_sub(1) as f32;

        let font = theme::font(12.5);
        let mut widths: Vec<f32> = Vec::with_capacity(count);
        for tab in &self.tabs {
            widths.push((widgets::ellipsis_size(ui, &tab.title, font.clone()).x + 68.0).clamp(TAB_MIN_W, TAB_MAX_W));
        }
        let desired: f32 = widths.iter().sum::<f32>() + gaps;
        if desired > avail {
            // Shrinking uniformly keeps the strip readable instead of letting the
            // last tab stab into the caption buttons.
            let uniform = ((avail - gaps) / count.max(1) as f32).clamp(TAB_MIN_W, TAB_MAX_W);
            widths.fill(uniform);
        }
        let total: f32 = widths.iter().sum::<f32>() + gaps;
        let max_scroll = (total - avail).max(0.0);
        let mut state = ctx.data_mut(|data| data.get_temp::<StripState>(strip_state_id()).unwrap_or_default());
        state.scroll = state.scroll.clamp(0.0, max_scroll);

        let view_start = strip_left;
        let view_end = viewport_right;
        if state.drag.is_none() && state.last_active != self.active {
            let mut left = strip_left - state.scroll;
            for width in widths.iter().take(self.active) {
                left += width + TAB_GAP;
            }
            let right = left + widths[self.active.min(count - 1)];
            if left < view_start {
                state.scroll -= view_start - left;
            } else if right > view_end {
                state.scroll += right - view_end;
            }
            state.scroll = state.scroll.clamp(0.0, max_scroll);
            state.last_active = self.active;
        }
        let tab_area = Rect::from_min_max(Pos2::new(strip_left, tab_top), Pos2::new(viewport_right, tab_top + tab_h));
        if max_scroll > 0.0 && ui.rect_contains_pointer(tab_area) {
            let delta = ui.input(|input| input.smooth_scroll_delta.x + input.smooth_scroll_delta.y);
            state.scroll = (state.scroll - delta).clamp(0.0, max_scroll);
        }

        let mut x = strip_left - state.scroll;
        let mut rects: Vec<Rect> = Vec::with_capacity(count);
        let mut close_request: Option<usize> = None;
        let mut activate_request: Option<usize> = None;
        let mut menu_request: Option<(usize, Pos2)> = None;
        let tab_painter = painter.with_clip_rect(tab_area);
        let active_pill_bg = tab_painter.add(egui::Shape::Noop);
        let active_pill_bar = tab_painter.add(egui::Shape::Noop);
        let mut active_tab_rect: Option<Rect> = None;
        for idx in 0..count {
            let width = widths[idx];
            let tab_rect = Rect::from_min_size(Pos2::new(x, tab_top), Vec2::new(width, tab_h));
            x += width + TAB_GAP;
            rects.push(tab_rect);
            let active = idx == self.active;
            if active {
                active_tab_rect = Some(tab_rect);
            }
            if tab_rect.right() <= tab_area.left() || tab_rect.left() >= tab_area.right() {
                continue;
            }
            let id = egui::Id::unique(("tab", idx));
            let resp = ui.interact(tab_rect.intersect(tab_area), id, Sense::click_and_drag());
            let dragging = state.drag == Some(idx);
            let hover = widgets::hover_t(&ctx, id, resp.hovered() || dragging);
            if !active && hover > 0.0 {
                tab_painter.rect_filled(tab_rect, CornerRadius::same(6), widgets::fade(t.hover, hover));
            }
            let icon_color = if active { t.accent } else { t.text_tertiary };
            widgets::icon_at(
                &tab_painter,
                Pos2::new(tab_rect.left() + 16.0, tab_rect.center().y),
                icons::FOLDER,
                13.5,
                icon_color,
            );
            let text_rect = Rect::from_min_max(
                Pos2::new(tab_rect.left() + 28.0, tab_rect.top()),
                Pos2::new(tab_rect.right() - 28.0, tab_rect.bottom()),
            );
            widgets::text_left(
                ui,
                &tab_painter,
                text_rect,
                &self.tabs[idx].title,
                theme::font(12.5),
                if active { t.text } else { t.text_secondary },
                0.0,
            );

            let close_rect =
                Rect::from_center_size(Pos2::new(tab_rect.right() - 15.0, tab_rect.center().y), Vec2::splat(18.0));
            if tab_area.contains_rect(close_rect) {
                let close_id = egui::Id::unique(("tab-close", idx));
                let close_resp = ui.interact(close_rect, close_id, Sense::click());
                let close_h = widgets::hover_t(&ctx, close_id, close_resp.hovered());
                if active || hover > 0.0 || close_h > 0.0 {
                    if close_h > 0.0 {
                        tab_painter.rect_filled(
                            close_rect,
                            CornerRadius::same(4),
                            widgets::fade(t.hover_strong, close_h),
                        );
                    }
                    let alpha = if close_h > 0.0 { 1.0 } else { hover.max(0.4) };
                    widgets::icon_at(
                        &tab_painter,
                        close_rect.center(),
                        icons::CANCEL,
                        8.0,
                        widgets::fade(t.text_secondary, alpha),
                    );
                }
                if close_resp.clicked() || close_resp.middle_clicked() {
                    close_request = Some(idx);
                }
            }

            if resp.drag_started() {
                state.drag = Some(idx);
            }
            if resp.secondary_clicked() && state.drag.is_none() {
                let anchor = ctx.pointer_interact_pos().unwrap_or(tab_rect.center());
                menu_request = Some((idx, anchor));
            } else if resp.middle_clicked() && state.drag.is_none() {
                close_request = Some(idx);
            } else if resp.clicked() && state.drag.is_none() {
                activate_request = Some(idx);
            }
            if resp.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
        if let Some(target) = active_tab_rect {
            let x_id = egui::Id::unique("active-tab-pill-x");
            let w_id = egui::Id::unique("active-tab-pill-w");
            let (px, pw) = if state.drag.is_some() {
                crate::ui::spring::set_immediate(&ctx, x_id, target.left());
                crate::ui::spring::set_immediate(&ctx, w_id, target.width());
                (target.left(), target.width())
            } else {
                let x = crate::ui::spring::animate(&ctx, x_id, target.left(), target.left(), crate::ui::spring::PANEL);
                let w =
                    crate::ui::spring::animate(&ctx, w_id, target.width(), target.width(), crate::ui::spring::PANEL);
                (x, w.max(40.0))
            };
            let pill = Rect::from_min_size(Pos2::new(px, tab_top), Vec2::new(pw, tab_h));
            tab_painter.set(active_pill_bg, egui::Shape::rect_filled(pill, CornerRadius::same(6), t.surface_raised));
            tab_painter.set(
                active_pill_bar,
                egui::Shape::rect_stroke(
                    pill,
                    CornerRadius::same(6),
                    Stroke::new(1.0, t.border),
                    egui::StrokeKind::Inside,
                ),
            );
        }

        // Drag reorder: swap on the frame the pointer enters another tab. No
        // animation and no floating preview; the strip itself moves.
        if let Some(from) = state.drag {
            // A shortcut (Ctrl+W) can remove a tab while the pointer is still down.
            if from >= count {
                state.drag = None;
            }
        }
        if let Some(from) = state.drag {
            // Swap only in the direction the pointer is currently moving, and
            // only with a tab the pointer is over: position-only swapping would
            // oscillate when the dragged tab is narrower than its neighbour.
            if let Some(pos) = ctx.pointer_interact_pos() {
                let dx = ctx.input(|input| input.pointer.delta().x);
                let target = if dx > 0.0 {
                    (from + 1..count).find(|index| rects[*index].contains(pos))
                } else if dx < 0.0 {
                    (0..from).rev().find(|index| rects[*index].contains(pos))
                } else {
                    None
                };
                if let Some(to) = target {
                    self.tabs.swap(from, to);
                    self.active = match self.active {
                        value if value == from => to,
                        value if value == to => from,
                        value => value,
                    };
                    state.drag = Some(to);
                    ctx.request_repaint();
                }
            }
            if !ctx.input(|input| input.pointer.any_down()) {
                state.drag = None;
            }
        }

        // New tab button, pinned to the right end once the tabs fill the strip.
        let content_right = strip_left - state.scroll + widths.iter().sum::<f32>() + gaps;
        let plus_rect = Rect::from_min_size(
            Pos2::new((content_right + 2.0).min(strip_right - plus_w - 2.0).max(strip_left), tab_top + 3.0),
            Vec2::new(plus_w, tab_h - 6.0),
        );
        // Tell WM_NCHITTEST where the empty titlebar begins for native Aero Snap.
        platform::set_titlebar_drag_left(plus_rect.right() + 4.0);
        let plus_id = egui::Id::unique("tab-new");
        let plus_resp = ui.interact(plus_rect, plus_id, Sense::click());
        let plus_hover = widgets::hover_t(&ctx, plus_id, plus_resp.hovered());
        if plus_hover > 0.0 {
            painter.rect_filled(plus_rect, CornerRadius::same(4), widgets::fade(t.hover, plus_hover));
        }
        widgets::icon_at(&painter, plus_rect.center(), icons::ADD, 11.0, t.text_secondary);
        if plus_resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if plus_resp.clicked() {
            self.command(Command::NewTab);
        }

        // Horizontal scroll affordances; they only exist while the strip overflows.
        if max_scroll > 0.0 {
            let step = (avail * 0.6).max(TAB_MIN_W * 2.0);
            if state.scroll > 0.0 {
                let r = Rect::from_min_size(Pos2::new(tab_area.left(), tab_top + 4.0), Vec2::new(18.0, tab_h - 8.0));
                let id = egui::Id::unique("tab-scroll-left");
                let resp = ui.interact(r, id, Sense::click());
                let hover = widgets::hover_t(&ctx, id, resp.hovered());
                painter.rect_filled(
                    r,
                    CornerRadius::same(4),
                    if hover > 0.0 { t.hover_strong } else { t.surface_raised },
                );
                widgets::icon_at(&painter, r.center(), icons::CHEVRON_LEFT, 8.0, t.text_secondary);
                if resp.clicked() {
                    state.scroll = (state.scroll - step).max(0.0);
                }
            }
            if state.scroll < max_scroll {
                let r = Rect::from_min_size(
                    Pos2::new(tab_area.right() - 18.0, tab_top + 4.0),
                    Vec2::new(18.0, tab_h - 8.0),
                );
                let id = egui::Id::unique("tab-scroll-right");
                let resp = ui.interact(r, id, Sense::click());
                let hover = widgets::hover_t(&ctx, id, resp.hovered());
                painter.rect_filled(
                    r,
                    CornerRadius::same(4),
                    if hover > 0.0 { t.hover_strong } else { t.surface_raised },
                );
                widgets::icon_at(&painter, r.center(), icons::CHEVRON_RIGHT, 8.0, t.text_secondary);
                if resp.clicked() {
                    state.scroll = (state.scroll + step).min(max_scroll);
                }
            }
        }

        ctx.data_mut(|data| data.insert_temp(strip_state_id(), state));

        if let Some((idx, anchor)) = menu_request {
            self.menu_anchor = None;
            crate::ui::overlays::open_tab_menu(&ctx, idx, anchor);
        }
        if let Some(idx) = close_request {
            self.address.editing = false;
            self.close_tab(idx);
        } else if let Some(idx) = activate_request {
            self.address.editing = false;
            self.rename = None;
            self.activate_tab(idx);
        }
    }

    fn caption_button(&mut self, ui: &mut egui::Ui, rect: Rect, id: egui::Id, action: CaptionAction) -> f32 {
        let ctx = ui.ctx().clone();
        let t = self.theme.clone();
        let resp = ui.interact(rect, id, Sense::click());
        let native_max_hover = matches!(action, CaptionAction::Maximize) && platform::is_max_button_hovered();
        let hover = widgets::hover_t(&ctx, id, resp.hovered() || native_max_hover);
        if hover > 0.0 {
            let fill = if matches!(action, CaptionAction::Close) {
                Color32::from_rgb(0xC4, 0x2B, 0x1C).gamma_multiply(hover)
            } else {
                widgets::fade(t.hover_strong, hover)
            };
            ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
        }
        if resp.clicked() {
            match action {
                CaptionAction::Close => {
                    if self.settings.tray_resident {
                        platform::hide_to_tray();
                    } else {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                CaptionAction::Minimize => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                CaptionAction::Maximize => platform::toggle_maximize(),
            }
        }
        hover
    }

    // ---- command bar -----------------------------------------------------

    pub(crate) fn paint_command_bar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let t = self.theme.clone();
        let cy = rect.center().y;
        let mut x = rect.left() + 10.0;
        let btn = 34.0;

        let can_back = self.active().can_back();
        let can_forward = self.active().can_forward();
        let can_up = crate::core::tabs::parent_location(&self.active().location).is_some();
        let nav: [(&str, &str, bool, Command); 4] = [
            (icons::BACK, "后退  Alt+←", can_back, Command::Back),
            (icons::FORWARD, "前进  Alt+→", can_forward, Command::Forward),
            (icons::ARROW_UP, "上一级  Alt+↑", can_up, Command::Up),
            (icons::REFRESH, "刷新  F5", true, Command::Refresh),
        ];
        for (glyph, tip, enabled, command) in nav {
            let r = Rect::from_center_size(Pos2::new(x + btn * 0.5, cy), Vec2::splat(30.0));
            let resp = icon_button(ui, &t, r, egui::Id::unique(("navbtn", glyph)), glyph, 15.0, tip, false, enabled);
            if enabled && resp.clicked() {
                self.command(command);
            }
            x += btn;
        }

        // Right-hand command buttons, measured from the right edge.
        let command_h = 30.0;
        let mut right = rect.right() - 10.0;
        let more_rect = Rect::from_center_size(Pos2::new(right - 15.0, cy), Vec2::splat(30.0));
        right -= 34.0;
        let settings_rect = Rect::from_center_size(Pos2::new(right - 15.0, cy), Vec2::splat(30.0));
        right -= 34.0;
        let details_rect = Rect::from_center_size(Pos2::new(right - 15.0, cy), Vec2::splat(30.0));
        right -= 34.0;
        let split_rect = Rect::from_center_size(Pos2::new(right - 15.0, cy), Vec2::splat(30.0));
        right -= 34.0;
        let view_rect = Rect::from_center_size(Pos2::new(right - 15.0, cy), Vec2::splat(30.0));
        right -= 40.0;

        let selected = self.active().selection.len();
        let has_sel = selected > 0;
        let actions: [(&str, &str, bool, Command); 5] = [
            (icons::DELETE, "删除  Del / Ctrl+D", has_sel, Command::Delete),
            (icons::RENAME, "重命名  F2", selected == 1, Command::Rename),
            (icons::PASTE, "粘贴  Ctrl+V", self.clipboard_files, Command::Paste),
            (icons::COPY, "复制  Ctrl+C", has_sel, Command::Copy),
            (icons::CUT, "剪切  Ctrl+X", has_sel, Command::Cut),
        ];
        for (glyph, tip, enabled, command) in actions {
            let r = Rect::from_center_size(Pos2::new(right - 15.0, cy), Vec2::splat(command_h));
            right -= 34.0;
            let resp = icon_button(ui, &t, r, egui::Id::unique(("cmd", tip)), glyph, 15.0, tip, false, enabled);
            if enabled && resp.clicked() {
                self.command(command);
            }
        }
        widgets::separator_v(&painter, right - 3.0, cy, 20.0, t.divider);
        right -= 10.0;

        // Use the same document templates as the background context menu.
        let can_new = self.current_dir().is_some();
        let new_rect = Rect::from_center_size(Pos2::new(right - 40.0, cy), Vec2::new(76.0, command_h));
        right -= 84.0;
        let new_id = egui::Id::unique("cmd-new");
        let new_resp = ui.interact(new_rect, new_id, Sense::click());
        let new_hover = widgets::hover_t(&ctx, new_id, new_resp.hovered() && can_new);
        if new_hover > 0.0 {
            painter.rect_filled(new_rect, CornerRadius::same(5), widgets::fade(t.hover_strong, new_hover));
        }
        let new_color = if can_new { t.text } else { t.text_disabled };
        widgets::icon_at(&painter, Pos2::new(new_rect.left() + 15.0, new_rect.center().y), icons::ADD, 14.0, new_color);
        widgets::text_left(
            ui,
            &painter,
            Rect::from_min_max(Pos2::new(new_rect.left() + 26.0, new_rect.top()), new_rect.right_bottom()),
            "新建",
            theme::font(12.5),
            new_color,
            0.0,
        );
        if can_new && new_resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let new_resp = if new_resp.hovered() { new_resp.on_hover_text("新建文件夹或文档") } else { new_resp };
        widgets::icon_at(&painter, Pos2::new(new_rect.right() - 9.0, cy), icons::CHEVRON_DOWN, 8.0, new_color);
        if can_new && new_resp.clicked() {
            self.menu_anchor = None;
            self.crumb_menu = None;
            self.ctx_menu = Some(crate::ui::context_menu::ContextMenuState::open_new(self, new_rect.left_bottom()));
        }
        widgets::separator_v(&painter, right - 4.0, cy, 20.0, t.divider);
        right -= 10.0;

        // View / details / more.
        let view_resp =
            icon_button(ui, &t, view_rect, egui::Id::unique("cmd-view"), icons::VIEW, 15.0, "切换视图", false, true);
        if view_resp.clicked() {
            let next = match self.active().view {
                ViewMode::Details => ViewMode::List,
                ViewMode::List => ViewMode::Tiles,
                ViewMode::Tiles => ViewMode::Icons,
                ViewMode::Icons => ViewMode::Details,
            };
            self.set_view(next);
        }

        let split_active = self.secondary_tab.is_some();
        let split_resp = icon_button(
            ui,
            &t,
            split_rect,
            egui::Id::unique("cmd-split"),
            icons::GRID,
            14.5,
            "双栏分屏视图  Ctrl+Shift+E",
            split_active,
            true,
        );
        if split_resp.clicked() {
            self.command(Command::ToggleSplitView);
        }

        let details_active = self.details_visible;
        let details_resp = icon_button(
            ui,
            &t,
            details_rect,
            egui::Id::unique("cmd-details"),
            icons::INFO,
            14.5,
            "详细信息窗格",
            details_active,
            true,
        );
        if details_resp.clicked() {
            self.command(Command::ToggleDetails);
        }

        let settings_resp = icon_button(
            ui,
            &t,
            settings_rect,
            egui::Id::unique("cmd-settings"),
            icons::SETTINGS,
            14.5,
            "设置  Ctrl+,",
            self.settings_open,
            true,
        );
        if settings_resp.clicked() {
            self.command(Command::OpenSettings);
        }

        let more_active = self.menu_anchor.is_some();
        let more_resp =
            icon_button(ui, &t, more_rect, egui::Id::unique("cmd-more"), icons::MORE, 15.0, "更多", more_active, true);
        if more_resp.clicked() {
            crate::ui::overlays::close_tab_menu(&ctx);
            self.menu_anchor =
                Some(Pos2::new(more_rect.right() - crate::ui::overlays::MORE_MENU_W, more_rect.bottom() + 6.0));
        }

        // ---- address bar -------------------------------------------------
        let addr_left = x + 6.0;
        let search_w = ((rect.width() * 0.16).clamp(150.0, 240.0)).min((right - addr_left - 8.0 - 180.0).max(120.0));
        let search_left = right - 8.0 - search_w;
        let addr_right = search_left - 8.0;
        let addr = Rect::from_min_max(
            Pos2::new(addr_left, cy - 17.0),
            Pos2::new(addr_right.max(addr_left + 120.0), cy + 17.0),
        );
        let addr_id = egui::Id::unique("address");
        let addr_resp = ui.interact(addr, addr_id, Sense::click());
        let addr_hover = widgets::hover_t(&ctx, addr_id, addr_resp.hovered() && !self.address.editing);
        let base = if t.dark {
            Color32::from_rgba_unmultiplied(255, 255, 255, 12)
        } else {
            Color32::from_rgba_unmultiplied(255, 255, 255, 175)
        };
        painter.rect_filled(addr, CornerRadius::same(theme::RADIUS_CTRL), base);
        widgets::stroke_rect(&painter, addr, theme::RADIUS_CTRL, Stroke::new(1.0, t.border));
        if addr_hover > 0.0 {
            painter.rect_filled(addr, CornerRadius::same(theme::RADIUS_CTRL), widgets::fade(t.hover, addr_hover * 0.5));
        }

        self.address.pill = Some(addr);
        if self.address.editing {
            let go_rect = Rect::from_center_size(Pos2::new(addr.right() - 16.0, cy), Vec2::splat(24.0));
            let go_id = egui::Id::unique("address-go");
            let go_resp = ui.interact(go_rect, go_id, Sense::click());
            let go_hover = widgets::hover_t(&ctx, go_id, go_resp.hovered());
            if go_hover > 0.0 {
                painter.rect_filled(go_rect, CornerRadius::same(4), widgets::fade(t.hover_strong, go_hover));
            }
            widgets::icon_at(&painter, go_rect.center(), icons::CHEVRON_RIGHT, 10.0, t.text_secondary);
            let inner = Rect::from_min_max(
                Pos2::new(addr.left() + 10.0, addr.top() + 5.0),
                Pos2::new(go_rect.left() - 4.0, addr.bottom() - 5.0),
            );
            let edit_id = egui::Id::unique("address-input");
            let te = egui::TextEdit::singleline(&mut self.address.buf)
                .id(edit_id)
                .font(theme::font(12.5))
                .frame(egui::Frame::NONE)
                .desired_width(inner.width());
            let resp = ui.put(inner, te);
            if self.address.focus {
                resp.request_focus();
                self.address.focus = false;
            }
            if self.address.select_all {
                self.address.select_all = false;
                if let Some(mut state) = egui::widgets::text_edit::TextEditState::load(&ctx, edit_id) {
                    let text = self.address.buf.chars().count();
                    state.cursor.set_char_range(Some(egui::text_selection::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(text),
                    )));
                    state.store(&ctx, edit_id);
                }
            }
            let commit =
                go_resp.clicked() || (resp.lost_focus() && !ui.input(|input| input.key_pressed(egui::Key::Escape)));
            self.refresh_address_suggestions();
            if commit {
                self.address.editing = false;
                self.address.suggestions.clear();
                self.address.suggestion_index = None;
                let buf = self.address.buf.clone();
                if !buf.trim().is_empty() {
                    self.navigate_input(&buf);
                }
            }
        } else {
            self.address.suggestions.clear();
            self.address.suggestion_index = None;
            let crumbs = self.active().crumbs.clone();
            let font = theme::font(12.5);
            let inner = Rect::from_min_max(
                Pos2::new(addr.left() + 10.0, addr.top()),
                Pos2::new(addr.right() - 8.0, addr.bottom()),
            );
            let last = crumbs.len().saturating_sub(1);
            let galleys: Vec<_> = crumbs
                .iter()
                .enumerate()
                .map(|(index, crumb)| {
                    let color = if index == last { t.text } else { t.text_secondary };
                    widgets::galley(ui, &crumb.label, font.clone(), color, 220.0)
                })
                .collect();
            let widths: Vec<f32> = galleys.iter().map(|galley| galley.size().x + 14.0).collect();
            let crumb_gap = 18.0;
            let natural = widths.iter().sum::<f32>() + crumb_gap * last as f32;

            let mut plan: Vec<(usize, Rect)> = Vec::with_capacity(crumbs.len());
            let mut tail: Option<(f32, f32)> = None;
            let mut show_ellipsis = false;
            if natural <= inner.width() {
                let mut cx = inner.left();
                for (index, width) in widths.iter().enumerate() {
                    plan.push((index, Rect::from_min_size(Pos2::new(cx, cy - 14.0), Vec2::new(*width, 28.0))));
                    cx += *width + crumb_gap;
                }
            } else if let Some(&last_width) = widths.last() {
                let tail_w = if last == 0 { inner.width() } else { last_width.min(inner.width() * 0.45) };
                let prefix_budget = (inner.width() - tail_w - 44.0).max(0.0);
                let mut cx = inner.left();
                for (index, width) in widths.iter().enumerate().take(last) {
                    if cx - inner.left() + *width > prefix_budget {
                        break;
                    }
                    plan.push((index, Rect::from_min_size(Pos2::new(cx, cy - 14.0), Vec2::new(*width, 28.0))));
                    cx += *width + crumb_gap;
                }
                let prefix_end = plan.last().map_or(inner.left(), |(_, seg)| seg.right());
                show_ellipsis = plan.len() < last;
                let tail_x = if show_ellipsis {
                    prefix_end + 44.0
                } else if last > 0 {
                    prefix_end + crumb_gap
                } else {
                    inner.left()
                };
                tail = Some((tail_x, tail_w.min(inner.right() - tail_x)));
            }

            let mut navigate: Option<usize> = None;
            let mut chevron_open: Option<(usize, Pos2)> = None;
            for (position, (index, seg)) in plan.iter().enumerate() {
                if position > 0 {
                    let prev_idx = plan[position - 1].0;
                    let chev_rect =
                        Rect::from_center_size(Pos2::new(seg.left() - crumb_gap * 0.5, cy), Vec2::new(16.0, 24.0));
                    let cid = egui::Id::unique(("crumb-chev", prev_idx));
                    let cresp = ui.interact(chev_rect, cid, Sense::click());
                    let chover = widgets::hover_t(&ctx, cid, cresp.hovered());
                    if chover > 0.0 {
                        painter.rect_filled(chev_rect, CornerRadius::same(4), widgets::fade(t.hover_strong, chover));
                    }
                    widgets::icon_at(&painter, chev_rect.center(), icons::CHEVRON_RIGHT, 8.0, t.text_tertiary);
                    if cresp.hovered() {
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if cresp.clicked() {
                        chevron_open = Some((prev_idx, Pos2::new(chev_rect.left(), addr.bottom() + 4.0)));
                    }
                }
                let id = egui::Id::unique(("crumb", index));
                let resp = ui.interact(*seg, id, Sense::click());
                let hover = widgets::hover_t(&ctx, id, resp.hovered());
                if hover > 0.0 {
                    painter.rect_filled(*seg, CornerRadius::same(4), widgets::fade(t.hover_strong, hover));
                }
                let galley = galleys[*index].clone();
                widgets::galley_at(
                    &painter,
                    Pos2::new(seg.left() + 7.0, seg.center().y - galley.size().y * 0.5),
                    galley,
                    t.text_secondary,
                );
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    navigate = Some(*index);
                }
            }
            if let Some((tail_x, tail_w)) = tail {
                if show_ellipsis {
                    if !plan.is_empty() {
                        widgets::icon_at(
                            &painter,
                            Pos2::new(tail_x - 35.0, cy),
                            icons::CHEVRON_RIGHT,
                            8.0,
                            t.text_tertiary,
                        );
                    }
                    let ellipsis = widgets::galley(ui, "…", font.clone(), t.text_tertiary, 40.0);
                    widgets::galley_at(
                        &painter,
                        Pos2::new(tail_x - 27.0, cy - ellipsis.size().y * 0.5),
                        ellipsis,
                        t.text_tertiary,
                    );
                    if last > 0 {
                        let chev_rect = Rect::from_center_size(Pos2::new(tail_x - 9.0, cy), Vec2::new(16.0, 24.0));
                        let cid = egui::Id::unique(("crumb-chev-tail", last - 1));
                        let cresp = ui.interact(chev_rect, cid, Sense::click());
                        let chover = widgets::hover_t(&ctx, cid, cresp.hovered());
                        if chover > 0.0 {
                            painter.rect_filled(
                                chev_rect,
                                CornerRadius::same(4),
                                widgets::fade(t.hover_strong, chover),
                            );
                        }
                        widgets::icon_at(&painter, chev_rect.center(), icons::CHEVRON_RIGHT, 8.0, t.text_tertiary);
                        if cresp.clicked() {
                            chevron_open = Some((last - 1, Pos2::new(chev_rect.left(), addr.bottom() + 4.0)));
                        }
                    }
                } else if !plan.is_empty() {
                    let prev_idx = plan.last().map(|(i, _)| *i).unwrap_or(0);
                    let chev_rect =
                        Rect::from_center_size(Pos2::new(tail_x - crumb_gap * 0.5, cy), Vec2::new(16.0, 24.0));
                    let cid = egui::Id::unique(("crumb-chev-tail", prev_idx));
                    let cresp = ui.interact(chev_rect, cid, Sense::click());
                    let chover = widgets::hover_t(&ctx, cid, cresp.hovered());
                    if chover > 0.0 {
                        painter.rect_filled(chev_rect, CornerRadius::same(4), widgets::fade(t.hover_strong, chover));
                    }
                    widgets::icon_at(&painter, chev_rect.center(), icons::CHEVRON_RIGHT, 8.0, t.text_tertiary);
                    if cresp.clicked() {
                        chevron_open = Some((prev_idx, Pos2::new(chev_rect.left(), addr.bottom() + 4.0)));
                    }
                }
                let seg = Rect::from_min_size(Pos2::new(tail_x, cy - 14.0), Vec2::new(tail_w, 28.0));
                let id = egui::Id::unique(("crumb", last));
                let resp = ui.interact(seg, id, Sense::click());
                let hover = widgets::hover_t(&ctx, id, resp.hovered());
                if hover > 0.0 {
                    painter.rect_filled(seg, CornerRadius::same(4), widgets::fade(t.hover_strong, hover));
                }
                let galley = widgets::galley(ui, &crumbs[last].label, font.clone(), t.text, seg.width() - 14.0);
                widgets::galley_at(
                    &painter,
                    Pos2::new(seg.left() + 7.0, seg.center().y - galley.size().y * 0.5),
                    galley,
                    t.text,
                );
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    navigate = Some(last);
                }
            }
            if let Some((idx, anchor)) = chevron_open
                && let Some(crumb) = crumbs.get(idx)
            {
                self.open_crumb_dropdown(crumb, anchor);
            }
            if let Some(index) = navigate {
                let crumb = crumbs[index].clone();
                if crumb.is_this_pc {
                    if let Ok(loc) =
                        crate::shell::location::Location::known(&windows::Win32::UI::Shell::FOLDERID_ComputerFolder)
                    {
                        self.navigate_to(loc);
                    }
                } else if let Some(path) = crumb.path {
                    self.navigate_to_path(&path);
                } else if let Some(loc) = crumb.virtual_target {
                    self.navigate_to(loc);
                }
            }
            if addr_resp.secondary_clicked() {
                let path = self
                    .current_dir()
                    .map(|path| path.to_string_lossy().to_string())
                    .unwrap_or_else(|| self.active().location.parsing_name());
                match crate::shell::clipboard::set_text(&path) {
                    Ok(()) => self.toast("地址已复制"),
                    Err(err) => self.toast_error(format!("{err:#}")),
                }
            }
            if addr_resp.clicked() {
                self.command(Command::FocusAddress);
            }
        }

        // ---- search box --------------------------------------------------
        let search_rect =
            Rect::from_min_max(Pos2::new(search_left, cy - 17.0), Pos2::new(search_left + search_w, cy + 17.0));
        painter.rect_filled(search_rect, CornerRadius::same(theme::RADIUS_CTRL), base);
        widgets::stroke_rect(&painter, search_rect, theme::RADIUS_CTRL, Stroke::new(1.0, t.border));
        widgets::icon_at(&painter, Pos2::new(search_rect.left() + 15.0, cy), icons::SEARCH, 12.5, t.text_tertiary);
        let search_inner = Rect::from_min_max(
            Pos2::new(search_rect.left() + 28.0, search_rect.top() + 5.0),
            Pos2::new(search_rect.right() - 8.0, search_rect.bottom() - 5.0),
        );
        let te = egui::TextEdit::singleline(&mut self.search.query)
            .font(theme::font(12.5))
            .hint_text("搜索")
            .frame(egui::Frame::NONE)
            .desired_width(search_inner.width());
        let search_resp = ui.put(search_inner, te);
        if self.command_focus == Some(Command::FocusSearch) {
            self.command_focus = None;
            search_resp.request_focus();
        }
        self.search_focused = search_resp.has_focus();
        if self.search_focused && !platform::is_ime_composing() && ui.input(|input| input.key_pressed(egui::Key::Enter))
        {
            self.start_search();
        }
        if search_resp.has_focus() && self.search.query != self.search.typed_text {
            self.search.typed_text = self.search.query.clone();
            self.search.typed_at = crate::app::now();
        }
        // Leaving an empty search box behind while results are still shown is
        // confusing: an empty query always means "not searching".
        if !self.search_focused && self.search.query.trim().is_empty() && self.search_active() {
            self.cancel_search();
        }
    }
}
