//! Window chrome: tab strip, caption buttons, navigation bar and address bar.

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::{Command, CrumbDropdown, LumenApp};
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
/// Dedicated horizontal slot reserved on each side of the tab strip when tabs overflow.
const TAB_SCROLL_INSET: f32 = 22.0;
const TAB_SCROLL_BTN_W: f32 = 18.0;
/// Width below which secondary right-side command bar buttons collapse into `更多`.
const COMMAND_COMPACT_W: f32 = 940.0;
/// Minimum horizontal gap enforced between address bar, search box, and adjacent controls.
const COMMAND_GAP: f32 = 8.0;

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

/// Pure geometric plan for the tab strip so overflow scroll arrows never overlap
/// visible tabs or the `+` new-tab button.
#[derive(Clone, Debug)]
struct TabStripLayout {
    tab_area: Rect,
    widths: Vec<f32>,
    total: f32,
    max_scroll: f32,
    overflows: bool,
    scroll_left_btn: Option<Rect>,
    scroll_right_btn: Option<Rect>,
    plus_rect: Rect,
}

fn compute_tab_strip_layout(titlebar: Rect, desired_widths: &[f32], scroll: f32) -> TabStripLayout {
    let tab_top = titlebar.top() + 5.0;
    let tab_h = theme::TAB_H;
    let plus_w = 28.0;
    let strip_left = titlebar.left() + 8.0;
    let strip_right = (titlebar.right() - CAPTION_W).max(strip_left + plus_w + 8.0);
    // The "+" button owns the right end of the strip; tabs and scroll buttons never enter it.
    let viewport_right = (strip_right - plus_w - 6.0).max(strip_left + 1.0);
    let base_avail = (viewport_right - strip_left).max(1.0);
    let count = desired_widths.len();
    let gaps = TAB_GAP * count.saturating_sub(1) as f32;

    let mut widths: Vec<f32> = desired_widths.iter().map(|w| w.clamp(TAB_MIN_W, TAB_MAX_W)).collect();
    let desired: f32 = widths.iter().sum::<f32>() + gaps;
    let min_total = TAB_MIN_W * count as f32 + gaps;
    let overflows = min_total > base_avail;

    let (tab_area, scroll_left_btn, scroll_right_btn) = if overflows {
        let inset_left = strip_left + TAB_SCROLL_INSET;
        let inset_right = (viewport_right - TAB_SCROLL_INSET).max(inset_left);
        let left_btn =
            Rect::from_min_size(Pos2::new(strip_left, tab_top + 4.0), Vec2::new(TAB_SCROLL_BTN_W, tab_h - 8.0));
        let right_btn = Rect::from_min_size(
            Pos2::new(viewport_right - TAB_SCROLL_BTN_W, tab_top + 4.0),
            Vec2::new(TAB_SCROLL_BTN_W, tab_h - 8.0),
        );
        (
            Rect::from_min_max(Pos2::new(inset_left, tab_top), Pos2::new(inset_right, tab_top + tab_h)),
            Some(left_btn),
            Some(right_btn),
        )
    } else {
        (Rect::from_min_max(Pos2::new(strip_left, tab_top), Pos2::new(viewport_right, tab_top + tab_h)), None, None)
    };

    let avail = tab_area.width().max(1.0);
    if overflows {
        widths.fill(TAB_MIN_W);
    } else if desired > avail {
        let uniform = ((avail - gaps) / count.max(1) as f32).clamp(TAB_MIN_W, TAB_MAX_W);
        widths.fill(uniform);
    }
    let total: f32 = widths.iter().sum::<f32>() + gaps;
    let max_scroll = (total - avail).max(0.0);
    let clamped_scroll = scroll.clamp(0.0, max_scroll);

    let plus_x = if overflows {
        (viewport_right + 4.0).min(strip_right - plus_w - 2.0)
    } else {
        let content_right = tab_area.left() - clamped_scroll + total;
        (content_right + 2.0).min(strip_right - plus_w - 2.0).max(strip_left)
    };
    let plus_rect = Rect::from_min_size(Pos2::new(plus_x, tab_top + 3.0), Vec2::new(plus_w, tab_h - 6.0));

    TabStripLayout { tab_area, widths, total, max_scroll, overflows, scroll_left_btn, scroll_right_btn, plus_rect }
}

/// Pure geometric layout for the command bar, enforcing non-overlapping rects at
/// every supported window width (including 820px minimum width).
#[derive(Clone, Debug)]
struct CommandBarLayout {
    compact: bool,
    nav_rects: [Rect; 4],
    addr: Rect,
    search: Rect,
    more_rect: Rect,
    settings_rect: Option<Rect>,
    details_rect: Option<Rect>,
    split_rect: Option<Rect>,
    view_rect: Option<Rect>,
    /// Right-cluster file action rects (5 icons in normal mode, 3 pills in Recycle Bin).
    action_rects: Vec<Rect>,
    new_rect: Option<Rect>,
    separators: Vec<f32>,
}

fn compute_command_bar_layout(rect: Rect, in_recycle_bin: bool) -> CommandBarLayout {
    let cy = rect.center().y;
    let command_h = 30.0;
    let compact = rect.width() < COMMAND_COMPACT_W;

    // Left-hand navigation buttons.
    let mut x = rect.left() + 10.0;
    let btn = 34.0;
    let mut nav_rects = [Rect::NOTHING; 4];
    for slot in &mut nav_rects {
        *slot = Rect::from_center_size(Pos2::new(x + btn * 0.5, cy), Vec2::splat(command_h));
        x += btn;
    }

    // Right-hand command buttons, measured from the right edge.
    let mut right = rect.right() - 10.0;
    let more_rect = Rect::from_center_size(Pos2::new(right - 15.0, cy), Vec2::splat(command_h));
    let (settings_rect, details_rect, split_rect, view_rect) = if compact {
        // Secondary utility controls (View / Split / Details / Settings) live in
        // `更多` and the status bar, so collapsing them keeps the address bar and
        // search box readable at 820px–939px.
        right -= 38.0;
        (None, None, None, None)
    } else {
        right -= 34.0;
        let settings = Rect::from_center_size(Pos2::new(right - 15.0, cy), Vec2::splat(command_h));
        right -= 34.0;
        let details = Rect::from_center_size(Pos2::new(right - 15.0, cy), Vec2::splat(command_h));
        right -= 34.0;
        let split = Rect::from_center_size(Pos2::new(right - 15.0, cy), Vec2::splat(command_h));
        right -= 34.0;
        let view = Rect::from_center_size(Pos2::new(right - 15.0, cy), Vec2::splat(command_h));
        right -= 40.0;
        (Some(settings), Some(details), Some(split), Some(view))
    };

    let mut action_rects = Vec::with_capacity(5);
    let mut separators = Vec::with_capacity(2);

    let new_rect = if in_recycle_bin {
        let bin_widths = [96.0, 86.0, 64.0];
        for w in bin_widths {
            let r = Rect::from_center_size(Pos2::new(right - w * 0.5, cy), Vec2::new(w, command_h));
            action_rects.push(r);
            right -= w + 6.0;
        }
        separators.push(right - 2.0);
        right -= 10.0;
        None
    } else {
        for _ in 0..5 {
            let r = Rect::from_center_size(Pos2::new(right - 15.0, cy), Vec2::splat(command_h));
            action_rects.push(r);
            right -= 34.0;
        }
        separators.push(right - 3.0);
        right -= 10.0;

        let nr = Rect::from_center_size(Pos2::new(right - 33.0, cy), Vec2::new(62.0, command_h));
        right -= 70.0;
        separators.push(right - 4.0);
        right -= 10.0;
        Some(nr)
    };

    // Middle span: `[addr] [COMMAND_GAP] [search] [COMMAND_GAP] [right cluster]`.
    // Never let `addr.right()` exceed `search.left() - COMMAND_GAP`.
    let addr_left = x + 6.0;
    let middle_right = (right - COMMAND_GAP).max(addr_left);
    let avail = (middle_right - addr_left).max(0.0);
    let usable = (avail - COMMAND_GAP).max(0.0);
    let ideal_search = (rect.width() * 0.16).clamp(130.0, 240.0);
    let max_search = (usable * 0.42).max(0.0);
    let search_w = ideal_search.min(max_search);
    let addr_w = (usable - search_w).max(0.0);

    let addr = Rect::from_min_max(Pos2::new(addr_left, cy - 17.0), Pos2::new(addr_left + addr_w, cy + 17.0));
    let search_left = addr.right() + COMMAND_GAP;
    let search = Rect::from_min_max(Pos2::new(search_left, cy - 17.0), Pos2::new(search_left + search_w, cy + 17.0));

    CommandBarLayout {
        compact,
        nav_rects,
        addr,
        search,
        more_rect,
        settings_rect,
        details_rect,
        split_rect,
        view_rect,
        action_rects,
        new_rect,
        separators,
    }
}

/// Icon button with hover fill, tooltip and the shared disabled treatment.
// Immediate-mode paint helper: each argument is distinct draw state.
#[allow(clippy::too_many_arguments)]
fn cmd_pill_button(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    t: &Theme,
    rect: Rect,
    id: egui::Id,
    glyph: &str,
    label: &str,
    tip: &str,
    enabled: bool,
    danger: bool,
) -> bool {
    let ctx = ui.ctx().clone();
    let resp = ui.interact(rect, id, Sense::click());
    let hover = widgets::hover_t(&ctx, id, resp.hovered() && enabled);
    if hover > 0.0 {
        painter.rect_filled(rect, CornerRadius::same(5), widgets::fade(t.hover_strong, hover));
    }
    let text_color = if !enabled {
        t.text_disabled
    } else if danger {
        t.danger
    } else {
        t.text
    };
    let icon_color = if !enabled {
        t.text_disabled
    } else if danger {
        t.danger
    } else {
        t.text_secondary
    };
    widgets::icon_at(painter, Pos2::new(rect.left() + 14.0, rect.center().y), glyph, 14.0, icon_color);
    widgets::text_left(
        ui,
        painter,
        Rect::from_min_max(Pos2::new(rect.left() + 26.0, rect.top()), rect.right_bottom()),
        label,
        theme::font(12.5),
        text_color,
        0.0,
    );
    if enabled && resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let resp = if resp.hovered() { resp.on_hover_text(tip) } else { resp };
    enabled && resp.clicked()
}

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
        let overlay_blocking = self.has_blocking_overlay() || crate::ui::overlays::overlay_has_keyboard(&ctx);

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
        if !overlay_blocking && drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if !overlay_blocking && drag.double_clicked() {
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
        let count = self.tabs.len();
        let can_close = count > 1;
        let font = theme::font(12.5);
        let mut desired_widths: Vec<f32> = Vec::with_capacity(count);
        for tab in &self.tabs {
            desired_widths
                .push((widgets::ellipsis_size(ui, &tab.title, font.clone()).x + 68.0).clamp(TAB_MIN_W, TAB_MAX_W));
        }

        let mut state = ctx.data_mut(|data| data.get_temp::<StripState>(strip_state_id()).unwrap_or_default());
        let strip = compute_tab_strip_layout(rect, &desired_widths, state.scroll);
        let tab_area = strip.tab_area;
        let widths = &strip.widths;
        let max_scroll = strip.max_scroll;
        state.scroll = state.scroll.clamp(0.0, max_scroll);

        let view_start = tab_area.left();
        let view_end = tab_area.right();
        if state.drag.is_none() && state.last_active != self.active && count > 0 {
            let mut left = tab_area.left() - state.scroll;
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
        if max_scroll > 0.0 && !overlay_blocking && ui.rect_contains_pointer(tab_area) {
            let delta = ui.input(|input| input.smooth_scroll_delta.x + input.smooth_scroll_delta.y);
            state.scroll = (state.scroll - delta).clamp(0.0, max_scroll);
        }

        // Absorb pointer presses inside the occupied tab strip (including the 3px
        // gaps between tabs) so clicks/drags between tabs never hit `titlebar-drag`,
        // while keeping empty titlebar space to the right of `+` draggable.
        let occupied_right = if strip.overflows {
            tab_area.right()
        } else {
            (tab_area.left() - state.scroll + strip.total).min(tab_area.right()).max(tab_area.left())
        };
        if occupied_right > tab_area.left() {
            let occupied = Rect::from_min_max(tab_area.min, Pos2::new(occupied_right, tab_area.bottom()));
            let _ = ui.interact(occupied, egui::Id::unique("tab-strip-gap-catch"), Sense::click_and_drag());
        }

        let mut x = tab_area.left() - state.scroll;
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
            let visible_rect = tab_rect.intersect(tab_area);
            let id = egui::Id::unique(("tab", idx));
            let resp = ui.interact(visible_rect, id, Sense::click_and_drag());
            let dragging = state.drag == Some(idx);
            let close_rect =
                Rect::from_center_size(Pos2::new(tab_rect.right() - 15.0, tab_rect.center().y), Vec2::splat(18.0));
            let pointer_in_tab = !overlay_blocking && ctx.pointer_hover_pos().is_some_and(|p| visible_rect.contains(p));
            let prev_hover_id = id.with("hover-active");
            let was_hovered = ctx.data(|d| d.get_temp::<bool>(prev_hover_id).unwrap_or(false));
            // Keep inactive tab hovered while pointer moves onto its revealed `×` button,
            // without letting an unhovered inactive tab expose an invisible close hitbox.
            let tab_hovered = !overlay_blocking && (resp.hovered() || dragging || (pointer_in_tab && was_hovered));
            ctx.data_mut(|d| d.insert_temp(prev_hover_id, tab_hovered));

            let hover = widgets::hover_t(&ctx, id, tab_hovered);
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
            let text_right_pad = if can_close { 28.0 } else { 10.0 };
            let text_rect = Rect::from_min_max(
                Pos2::new(tab_rect.left() + 28.0, tab_rect.top()),
                Pos2::new((tab_rect.right() - text_right_pad).max(tab_rect.left() + 28.0), tab_rect.bottom()),
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

            if can_close && tab_area.contains_rect(close_rect) && (active || tab_hovered) {
                let close_id = egui::Id::unique(("tab-close", idx));
                let close_resp = ui.interact(close_rect, close_id, Sense::click());
                let close_h = widgets::hover_t(&ctx, close_id, close_resp.hovered());
                if close_h > 0.0 {
                    tab_painter.rect_filled(close_rect, CornerRadius::same(4), widgets::fade(t.hover_strong, close_h));
                }
                let alpha = if close_h > 0.0 { 1.0 } else { hover.max(0.45) };
                widgets::icon_at(
                    &tab_painter,
                    close_rect.center(),
                    icons::CANCEL,
                    8.0,
                    widgets::fade(t.text_secondary, alpha),
                );
                if close_resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
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
            } else if can_close && resp.middle_clicked() && state.drag.is_none() {
                close_request = Some(idx);
            } else if resp.clicked() && state.drag.is_none() {
                activate_request = Some(idx);
            }
            if tab_hovered {
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
        let plus_rect = strip.plus_rect;
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

        // Dedicated non-overlapping horizontal scroll buttons while the strip overflows.
        if strip.overflows {
            let step = (tab_area.width() * 0.6).max(TAB_MIN_W * 2.0);
            if let Some(r) = strip.scroll_left_btn {
                let can_left = state.scroll > 0.0;
                let id = egui::Id::unique("tab-scroll-left");
                let resp = ui.interact(r, id, Sense::click());
                let hover = widgets::hover_t(&ctx, id, resp.hovered() && can_left);
                painter.rect_filled(
                    r,
                    CornerRadius::same(4),
                    if hover > 0.0 {
                        widgets::fade(t.hover_strong, hover)
                    } else if can_left {
                        t.hover
                    } else {
                        Color32::TRANSPARENT
                    },
                );
                widgets::icon_at(
                    &painter,
                    r.center(),
                    icons::CHEVRON_LEFT,
                    8.5,
                    if can_left { t.text_secondary } else { t.text_disabled },
                );
                if can_left && resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if can_left && resp.clicked() {
                    state.scroll = (state.scroll - step).max(0.0);
                }
            }
            if let Some(r) = strip.scroll_right_btn {
                let can_right = state.scroll < max_scroll;
                let id = egui::Id::unique("tab-scroll-right");
                let resp = ui.interact(r, id, Sense::click());
                let hover = widgets::hover_t(&ctx, id, resp.hovered() && can_right);
                painter.rect_filled(
                    r,
                    CornerRadius::same(4),
                    if hover > 0.0 {
                        widgets::fade(t.hover_strong, hover)
                    } else if can_right {
                        t.hover
                    } else {
                        Color32::TRANSPARENT
                    },
                );
                widgets::icon_at(
                    &painter,
                    r.center(),
                    icons::CHEVRON_RIGHT,
                    8.5,
                    if can_right { t.text_secondary } else { t.text_disabled },
                );
                if can_right && resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if can_right && resp.clicked() {
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
        let in_recycle_bin = self.active().is_recycle_bin();
        let layout = compute_command_bar_layout(rect, in_recycle_bin);

        let can_back = self.active().can_back();
        let can_forward = self.active().can_forward();
        let can_up = crate::core::tabs::parent_location(&self.active().location).is_some();
        let nav: [(&str, &str, bool, Command); 4] = [
            (icons::BACK, "后退  Alt+←", can_back, Command::Back),
            (icons::FORWARD, "前进  Alt+→", can_forward, Command::Forward),
            (icons::ARROW_UP, "上一级  Alt+↑", can_up, Command::Up),
            (icons::REFRESH, "刷新  F5", true, Command::Refresh),
        ];
        for ((glyph, tip, enabled, command), r) in nav.into_iter().zip(layout.nav_rects) {
            let resp = icon_button(ui, &t, r, egui::Id::unique(("navbtn", glyph)), glyph, 15.0, tip, false, enabled);
            if enabled && resp.clicked() {
                self.command(command);
            }
        }

        let selected = self.active().selection.len();
        let has_sel = selected > 0;
        if in_recycle_bin {
            let has_items = !self.active().entries.is_empty();
            let bin_actions: [(&str, &str, &str, bool, bool, Command); 3] = [
                (icons::DELETE, "清空回收站", "清空回收站中的所有项目", has_items, true, Command::EmptyRecycleBin),
                (icons::DELETE, "永久删除", "永久删除所选项目  Del", has_sel, true, Command::PermanentDelete),
                (icons::HISTORY, "还原", "还原所选项目到原位置", has_sel, false, Command::RestoreSelected),
            ];
            for ((glyph, label, tip, enabled, danger, command), r) in
                bin_actions.into_iter().zip(layout.action_rects.iter().copied())
            {
                if cmd_pill_button(
                    ui,
                    &painter,
                    &t,
                    r,
                    egui::Id::unique(("bin-cmd", label)),
                    glyph,
                    label,
                    tip,
                    enabled,
                    danger,
                ) {
                    self.command(command);
                }
            }
        } else {
            let rename_tip = if selected > 1 { "批量重命名  F2" } else { "重命名  F2" };
            let actions: [(&str, &str, bool, Command); 5] = [
                (icons::DELETE, "删除  Del", has_sel, Command::Delete),
                (icons::RENAME, rename_tip, has_sel, Command::Rename),
                (icons::PASTE, "粘贴  Ctrl+V", self.clipboard_files, Command::Paste),
                (icons::COPY, "复制  Ctrl+C", has_sel, Command::Copy),
                (icons::CUT, "剪切  Ctrl+X", has_sel, Command::Cut),
            ];
            for ((glyph, tip, enabled, command), r) in actions.into_iter().zip(layout.action_rects.iter().copied()) {
                let resp = icon_button(ui, &t, r, egui::Id::unique(("cmd", glyph)), glyph, 15.0, tip, false, enabled);
                if enabled && resp.clicked() {
                    self.command(command);
                }
            }

            if let Some(new_rect) = layout.new_rect {
                let can_new = self.current_dir().is_some();
                if cmd_pill_button(
                    ui,
                    &painter,
                    &t,
                    new_rect,
                    egui::Id::unique("cmd-new"),
                    icons::ADD,
                    "新建",
                    "新建文件夹  Ctrl+Shift+N",
                    can_new,
                    false,
                ) {
                    self.command(Command::NewFolder);
                }
            }
        }

        for sep_x in &layout.separators {
            widgets::separator_v(&painter, *sep_x, cy, 20.0, t.divider);
        }

        // View / split / details / settings (collapsed into `更多` on narrow windows).
        if let Some(view_rect) = layout.view_rect {
            let view_resp = icon_button(
                ui,
                &t,
                view_rect,
                egui::Id::unique("cmd-view"),
                icons::VIEW,
                15.0,
                "切换视图",
                false,
                true,
            );
            if view_resp.clicked() {
                let next = match self.active().view {
                    ViewMode::Details => ViewMode::List,
                    ViewMode::List => ViewMode::Tiles,
                    ViewMode::Tiles => ViewMode::Icons,
                    ViewMode::Icons => ViewMode::Details,
                };
                self.set_view(next);
            }
        }

        if let Some(split_rect) = layout.split_rect {
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
        }

        if let Some(details_rect) = layout.details_rect {
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
        }

        if let Some(settings_rect) = layout.settings_rect {
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
        }

        let more_rect = layout.more_rect;
        let more_active = self.menu_anchor.is_some();
        let more_tip = if layout.compact { "更多（含视图、分屏与设置）" } else { "更多" };
        let more_resp = icon_button(
            ui,
            &t,
            more_rect,
            egui::Id::unique("cmd-more"),
            icons::MORE,
            15.0,
            more_tip,
            more_active,
            true,
        );
        if more_resp.clicked() {
            crate::ui::overlays::close_tab_menu(&ctx);
            self.menu_anchor =
                Some(Pos2::new(more_rect.right() - crate::ui::overlays::MORE_MENU_W, more_rect.bottom() + 6.0));
        }

        // ---- address bar -------------------------------------------------
        let addr = layout.addr;
        let addr_id = egui::Id::unique("address");
        let addr_resp = ui.interact(addr, addr_id, Sense::click());
        let addr_hover = widgets::hover_t(&ctx, addr_id, addr_resp.hovered() && !self.address.editing);
        // Translucent frosted pill over either the gradient chrome or DWM Mica backing.
        let base = if t.dark {
            Color32::from_rgba_unmultiplied(255, 255, 255, if self.settings.mica_backdrop { 18 } else { 12 })
        } else {
            Color32::from_rgba_unmultiplied(255, 255, 255, if self.settings.mica_backdrop { 205 } else { 175 })
        };
        painter.rect_filled(addr, CornerRadius::same(theme::RADIUS_CTRL), base);
        let addr_stroke = if self.address.editing { Stroke::new(1.5, t.accent) } else { Stroke::new(1.0, t.border) };
        widgets::stroke_rect(&painter, addr, theme::RADIUS_CTRL, addr_stroke);
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
            if go_resp.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let inner = Rect::from_min_max(
                Pos2::new(addr.left() + 10.0, addr.top() + 5.0),
                Pos2::new((go_rect.left() - 4.0).max(addr.left() + 10.0), addr.bottom() - 5.0),
            );
            let edit_id = egui::Id::unique("address-input");
            // Prevent egui's Tab focus traversal (including on the very first frame
            // after focus gain) and prevent stray tab characters in the address bar.
            if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                    || i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab)
            }) {
                ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
            }
            let te = egui::TextEdit::singleline(&mut self.address.buf)
                .id(edit_id)
                .font(theme::font(12.5))
                .frame(egui::Frame::NONE)
                .lock_focus(true)
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
            if self.address_cursor_end {
                self.address_cursor_end = false;
                if let Some(mut state) = egui::widgets::text_edit::TextEditState::load(&ctx, edit_id) {
                    let end = self.address.buf.chars().count();
                    state
                        .cursor
                        .set_char_range(Some(egui::text_selection::CCursorRange::one(egui::text::CCursor::new(end))));
                    state.store(&ctx, edit_id);
                }
                resp.request_focus();
            }
            // Clicking inside the suggestions popup causes `address-input` to fire
            // `lost_focus()` before `paint_address_suggestions` runs; suppress early
            // commit so the suggestion click can navigate cleanly.
            let clicking_suggestions = self.address_suggest_rect.is_some_and(|suggest_rect| {
                ctx.input(|i| {
                    let pos = i.pointer.interact_pos().or(i.pointer.hover_pos());
                    let in_rect = pos.is_some_and(|p| suggest_rect.contains(p));
                    let pointer_active = i.pointer.primary_down()
                        || i.pointer.primary_pressed()
                        || i.pointer.primary_clicked()
                        || i.pointer.primary_released();
                    in_rect && pointer_active
                })
            });
            let commit = go_resp.clicked()
                || (resp.lost_focus()
                    && !clicking_suggestions
                    && !ui.input(|input| input.key_pressed(egui::Key::Escape)));
            self.refresh_address_suggestions();
            if commit {
                self.address.editing = false;
                self.address.suggestions.clear();
                self.address.suggestion_index = None;
                self.address_suggest_rect = None;
                self.address_cursor_end = false;
                let buf = self.address.buf.clone();
                if !buf.trim().is_empty() {
                    self.navigate_input(&buf);
                }
            }
        } else {
            self.address.suggestions.clear();
            self.address.suggestion_index = None;
            self.address_suggest_rect = None;
            self.address_cursor_end = false;
            let crumbs = self.active().crumbs.clone();
            let font = theme::font(12.5);
            let inner = Rect::from_min_max(
                Pos2::new(addr.left() + 10.0, addr.top()),
                Pos2::new((addr.right() - 8.0).max(addr.left() + 10.0), addr.bottom()),
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
                let prefix_budget = (inner.width() - tail_w - 58.0).max(0.0);
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
                    if plan.is_empty() { prefix_end + 42.0 } else { prefix_end + 58.0 }
                } else if last > 0 {
                    prefix_end + crumb_gap
                } else {
                    inner.left()
                };
                tail = Some((tail_x, tail_w.min((inner.right() - tail_x).max(0.0))));
            }

            let mut navigate: Option<usize> = None;
            let mut chevron_open: Option<(usize, Pos2)> = None;
            let mut ellipsis_open: Option<Pos2> = None;
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
                    let prefix_end = plan.last().map_or(inner.left(), |(_, seg)| seg.right());
                    let ellipsis_rect = if !plan.is_empty() {
                        widgets::icon_at(
                            &painter,
                            Pos2::new(prefix_end + 9.0, cy),
                            icons::CHEVRON_RIGHT,
                            8.0,
                            t.text_tertiary,
                        );
                        Rect::from_min_size(Pos2::new(prefix_end + 18.0, cy - 12.0), Vec2::new(22.0, 24.0))
                    } else {
                        Rect::from_min_size(Pos2::new(inner.left(), cy - 12.0), Vec2::new(24.0, 24.0))
                    };
                    let eid = egui::Id::unique("crumb-ellipsis");
                    let eresp = ui.interact(ellipsis_rect, eid, Sense::click());
                    let ehover = widgets::hover_t(&ctx, eid, eresp.hovered());
                    if ehover > 0.0 {
                        painter.rect_filled(
                            ellipsis_rect,
                            CornerRadius::same(4),
                            widgets::fade(t.hover_strong, ehover),
                        );
                    }
                    let ellipsis_color = if ehover > 0.0 { t.text } else { t.text_secondary };
                    let ellipsis = widgets::galley(ui, "…", font.clone(), ellipsis_color, ellipsis_rect.width());
                    widgets::galley_at(
                        &painter,
                        Pos2::new(
                            ellipsis_rect.center().x - ellipsis.size().x * 0.5,
                            ellipsis_rect.center().y - ellipsis.size().y * 0.5,
                        ),
                        ellipsis,
                        ellipsis_color,
                    );
                    if eresp.hovered() {
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    let eresp =
                        if eresp.hovered() { eresp.on_hover_text("显示折叠的上级目录") } else { eresp };
                    if eresp.clicked() {
                        ellipsis_open = Some(Pos2::new(ellipsis_rect.left(), addr.bottom() + 4.0));
                    }

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
                        if cresp.hovered() {
                            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
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
                    if cresp.hovered() {
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
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
                let galley =
                    widgets::galley(ui, &crumbs[last].label, font.clone(), t.text, (seg.width() - 14.0).max(0.0));
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
            if let Some(anchor) = ellipsis_open {
                let collapsed: Vec<(String, std::path::PathBuf)> = crumbs[plan.len()..last]
                    .iter()
                    .filter_map(|c| {
                        if c.is_this_pc {
                            Some((c.label.clone(), std::path::PathBuf::from(crate::core::tabs::THIS_PC_PARSING)))
                        } else if let Some(path) = &c.path {
                            Some((c.label.clone(), path.clone()))
                        } else {
                            c.virtual_target
                                .as_ref()
                                .map(|loc| (c.label.clone(), std::path::PathBuf::from(loc.parsing_name())))
                        }
                    })
                    .collect();
                if !collapsed.is_empty() {
                    self.menu_anchor = None;
                    self.ctx_menu = None;
                    self.crumb_menu = Some(CrumbDropdown { anchor, items: collapsed });
                }
            } else if let Some((idx, anchor)) = chevron_open
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
            if addr_resp.clicked() && navigate.is_none() && chevron_open.is_none() && ellipsis_open.is_none() {
                self.command(Command::FocusAddress);
            }
        }

        // ---- search box --------------------------------------------------
        let search_rect = layout.search;
        painter.rect_filled(search_rect, CornerRadius::same(theme::RADIUS_CTRL), base);
        widgets::icon_at(&painter, Pos2::new(search_rect.left() + 15.0, cy), icons::SEARCH, 12.5, t.text_tertiary);

        let has_query = !self.search.query.is_empty();
        let clear_rect = Rect::from_center_size(Pos2::new(search_rect.right() - 14.0, cy), Vec2::splat(20.0));
        let show_clear = has_query && search_rect.width() >= 60.0;
        let search_text_right = if show_clear { clear_rect.left() - 2.0 } else { search_rect.right() - 8.0 };
        let search_inner = Rect::from_min_max(
            Pos2::new(search_rect.left() + 28.0, search_rect.top() + 5.0),
            Pos2::new(search_text_right.max(search_rect.left() + 28.0), search_rect.bottom() - 5.0),
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

        let mut clear_clicked = false;
        if show_clear {
            let clear_id = egui::Id::unique("search-clear");
            let clear_resp = ui.interact(clear_rect, clear_id, Sense::click());
            let clear_hover = widgets::hover_t(&ctx, clear_id, clear_resp.hovered());
            if clear_hover > 0.0 {
                painter.rect_filled(clear_rect, CornerRadius::same(4), widgets::fade(t.hover_strong, clear_hover));
            }
            widgets::icon_at(
                &painter,
                clear_rect.center(),
                icons::CANCEL,
                9.0,
                if clear_hover > 0.0 { t.text } else { t.text_tertiary },
            );
            if clear_resp.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let clear_resp = if clear_resp.hovered() { clear_resp.on_hover_text("清空搜索") } else { clear_resp };
            if clear_resp.clicked() {
                clear_clicked = true;
            }
        }

        if clear_clicked {
            if self.search_active() {
                self.cancel_search();
            } else {
                self.stop_search();
            }
            if search_resp.has_focus() {
                ctx.memory_mut(|m| m.surrender_focus(search_resp.id));
            }
            self.search_focused = false;
        }

        let search_stroke = if self.search_focused { Stroke::new(1.5, t.accent) } else { Stroke::new(1.0, t.border) };
        widgets::stroke_rect(&painter, search_rect, theme::RADIUS_CTRL, search_stroke);

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

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_command_rects(layout: &CommandBarLayout) -> Vec<(&'static str, Rect)> {
        let mut rects = Vec::new();
        for r in layout.nav_rects {
            rects.push(("nav", r));
        }
        rects.push(("addr", layout.addr));
        rects.push(("search", layout.search));
        if let Some(r) = layout.new_rect {
            rects.push(("new", r));
        }
        for &r in &layout.action_rects {
            rects.push(("action", r));
        }
        if let Some(r) = layout.view_rect {
            rects.push(("view", r));
        }
        if let Some(r) = layout.split_rect {
            rects.push(("split", r));
        }
        if let Some(r) = layout.details_rect {
            rects.push(("details", r));
        }
        if let Some(r) = layout.settings_rect {
            rects.push(("settings", r));
        }
        rects.push(("more", layout.more_rect));
        rects
    }

    /// BUG-31: command bar controls must never overlap at 820px, 920px, or 1280px,
    /// and `addr.right() + 8.0 <= search.left()` must always hold.
    #[test]
    fn command_bar_layout_never_overlaps_at_narrow_or_wide_widths() {
        for in_recycle_bin in [false, true] {
            for width in [820.0f32, 920.0, 940.0, 1280.0] {
                let bar = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, theme::COMMAND_H));
                let layout = compute_command_bar_layout(bar, in_recycle_bin);

                assert!(
                    layout.addr.right() + COMMAND_GAP <= layout.search.left() + 0.001,
                    "addr ({:?}) collided with search ({:?}) at {width}px (bin={in_recycle_bin})",
                    layout.addr,
                    layout.search
                );
                assert!(layout.addr.width() >= 150.0, "address bar too cramped at {width}px: {}", layout.addr.width());
                assert!(
                    layout.search.width() >= 120.0,
                    "search box too cramped at {width}px: {}",
                    layout.search.width()
                );

                if width < COMMAND_COMPACT_W {
                    assert!(layout.compact);
                    assert!(layout.view_rect.is_none());
                    assert!(layout.split_rect.is_none());
                    assert!(layout.details_rect.is_none());
                    assert!(layout.settings_rect.is_none());
                } else {
                    assert!(!layout.compact);
                    assert!(layout.view_rect.is_some());
                    assert!(layout.split_rect.is_some());
                    assert!(layout.details_rect.is_some());
                    assert!(layout.settings_rect.is_some());
                }

                let rects = collect_command_rects(&layout);
                for i in 0..rects.len() {
                    let (name_a, a) = rects[i];
                    assert!(a.left() >= bar.left() - 0.01 && a.right() <= bar.right() + 0.01);
                    for &(name_b, b) in &rects[i + 1..] {
                        assert!(
                            !a.intersects(b),
                            "command bar overlap between {name_a} ({a:?}) and {name_b} ({b:?}) at {width}px (bin={in_recycle_bin})"
                        );
                    }
                }
            }
        }
    }

    /// UI-03: when tabs overflow, `<` and `>` scroll buttons occupy dedicated slots
    /// outside `tab_area` and never overlap visible tabs or the `+` new-tab button.
    #[test]
    fn tab_strip_overflow_scroll_arrows_never_overlap_tab_area() {
        let titlebar = Rect::from_min_size(Pos2::ZERO, Vec2::new(820.0, theme::TITLEBAR_H));
        // 10 tabs at 140px desired each guarantees overflow on an 820px titlebar.
        let desired = vec![140.0; 10];
        let strip = compute_tab_strip_layout(titlebar, &desired, 0.0);

        assert!(strip.overflows);
        assert!(strip.max_scroll > 0.0);
        let left_btn = strip.scroll_left_btn.expect("left scroll button");
        let right_btn = strip.scroll_right_btn.expect("right scroll button");

        assert!(
            left_btn.right() <= strip.tab_area.left(),
            "left scroll arrow ({left_btn:?}) overlapped tab_area ({:?})",
            strip.tab_area
        );
        assert!(
            strip.tab_area.right() <= right_btn.left(),
            "right scroll arrow ({right_btn:?}) overlapped tab_area ({:?})",
            strip.tab_area
        );
        assert!(
            right_btn.right() <= strip.plus_rect.left(),
            "right scroll arrow ({right_btn:?}) overlapped plus button ({:?})",
            strip.plus_rect
        );
        assert!(!left_btn.intersects(strip.tab_area));
        assert!(!right_btn.intersects(strip.tab_area));
        assert!(!right_btn.intersects(strip.plus_rect));

        // Non-overflowing strip should not reserve scroll arrow slots.
        let few = compute_tab_strip_layout(titlebar, &[140.0, 140.0], 0.0);
        assert!(!few.overflows);
        assert!(few.scroll_left_btn.is_none());
        assert!(few.scroll_right_btn.is_none());
    }

    /// BUG-13: Tab completion keeps focus on the address input, places cursor at
    /// the end of `buf`, and clicking inside `address_suggest_rect` suppresses
    /// `lost_focus()` early commit so the suggestion click lands cleanly.
    #[test]
    fn address_tab_keeps_focus_at_end_and_suggestion_click_suppresses_blur() {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }

        let ctx = egui::Context::default();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let settings =
            crate::core::settings::Settings { tabs: vec![root.to_string_lossy().to_string()], ..Default::default() };
        let mut app = LumenApp::with_context(&ctx, settings);
        let src_dir = root.join("src");
        app.address.editing = true;
        app.address.focus = true;
        app.address.buf = format!("{}\\", src_dir.to_string_lossy());
        app.address.suggestions_source = app.address.buf.clone();
        app.address.suggestions = vec![("src".to_string(), src_dir.clone())];

        let full = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 600.0));
        let bar = Rect::from_min_size(Pos2::new(0.0, theme::TITLEBAR_H), Vec2::new(1000.0, theme::COMMAND_H));
        let run_frame = |app: &mut LumenApp, raw: egui::RawInput| {
            let mut out = ctx.run_ui(raw, |ui| {
                app.paint_command_bar(ui, bar);
                app.address_suggest_rect = None;
                app.paint_overlays(ui, full);
            });
            out.textures_delta.clear();
        };

        // Frame 1: focus address input & paint suggestion popup.
        run_frame(&mut app, egui::RawInput { screen_rect: Some(full), ..Default::default() });
        let edit_id = egui::Id::unique("address-input");
        assert_eq!(ctx.memory(|m| m.focused()), Some(edit_id));
        let suggest_rect = app.address_suggest_rect.expect("suggestion popup rect must be recorded");

        // Frame 2: simulate Tab completion flag + Tab key — must keep editing & move cursor to end.
        app.address_cursor_end = true;
        let mut tab_raw = egui::RawInput { screen_rect: Some(full), ..Default::default() };
        tab_raw.events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        tab_raw.events.push(egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        run_frame(&mut app, tab_raw);
        assert!(app.address.editing, "Tab must not blur/commit address editing");
        assert!(!app.address_cursor_end, "address_cursor_end flag must be cleared after applying");
        assert_eq!(ctx.memory(|m| m.focused()), Some(edit_id), "address input must keep focus after Tab");
        let state = egui::widgets::text_edit::TextEditState::load(&ctx, edit_id).expect("text edit state");
        let range = state.cursor.char_range().expect("cursor range");
        assert_eq!(
            range.primary.index,
            egui::text::CharIndex(app.address.buf.chars().count()),
            "cursor must be at end of buf"
        );

        // Frame 3: press pointer down on the first suggestion row (causes address-input lost_focus).
        let click_pos = Pos2::new(suggest_rect.center().x, suggest_rect.top() + 14.0);
        let mut down_raw = egui::RawInput { screen_rect: Some(full), ..Default::default() };
        down_raw.events.push(egui::Event::PointerMoved(click_pos));
        down_raw.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        run_frame(&mut app, down_raw);
        assert!(app.address.editing, "pressing inside suggestion popup must NOT abort editing before click completes");
        assert!(!app.address.suggestions.is_empty(), "suggestions must remain intact for click release");

        // Frame 4: release pointer on suggestion row — suggestion click navigates and closes editing.
        let mut up_raw = egui::RawInput { screen_rect: Some(full), ..Default::default() };
        up_raw.events.push(egui::Event::PointerMoved(click_pos));
        up_raw.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        run_frame(&mut app, up_raw);
        assert!(!app.address.editing, "clicking suggestion row must apply suggestion and exit editing");
        assert_eq!(
            app.current_dir().map(|p| p.to_string_lossy().to_ascii_lowercase()),
            Some(src_dir.to_string_lossy().to_ascii_lowercase()),
            "clicking suggestion row must navigate to the suggested directory"
        );

        unsafe { CoUninitialize() };
    }
}
