//! Virtualized file views: details, list and tiles/icons grids.
//!
//! Only visible rows are visited per frame; selection is held as entry indices so
//! every view and every command agree on what a row means.

use std::path::Path;
use std::sync::Arc;

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::{LumenApp, RenameState};
use crate::core::tabs::{DisplayRow, GROUP_H, ROW_H};
use crate::core::{Entry, SortKey, ViewMode, format_size, format_time};
use crate::theme::{self};
use crate::ui::{icons, widgets};

const HEADER_H: f32 = 32.0;
const LIST_ROW_H: f32 = 24.0;

/// Character count of the filename stem (excluding extension) for Explorer-style
/// smart rename selection. Folders and dotfiles with no extension select all.
fn stem_char_len(name: &str, is_folder: bool) -> usize {
    if is_folder {
        return name.chars().count();
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => name[..dot].chars().count(),
        _ => name.chars().count(),
    }
}

/// Renders an inline rename `TextEdit` inside `inner` and returns the committed
/// text when focus is lost (unless cancelled via Escape).
fn paint_inline_rename(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    t: &theme::Theme,
    editor: &mut RenameState,
    inner: Rect,
    font_size: f32,
    is_folder: bool,
) -> Option<String> {
    let ctx = ui.ctx().clone();
    let edit_id = egui::Id::unique(("inline-rename", editor.row));
    let text_edit = egui::TextEdit::singleline(&mut editor.buf)
        .id(edit_id)
        .font(theme::font(font_size))
        .frame(egui::Frame::NONE)
        .desired_width(inner.width().max(40.0));
    painter.rect_filled(inner, CornerRadius::same(3), t.surface_raised);
    let editor_response = ui.put(inner.shrink2(Vec2::new(3.0, 0.0)), text_edit);
    if editor.focus {
        editor_response.request_focus();
        editor.focus = false;
        if let Some(mut state) = egui::widgets::text_edit::TextEditState::load(&ctx, edit_id) {
            let end =
                if editor.select_all { editor.buf.chars().count() } else { stem_char_len(&editor.buf, is_folder) };
            state.cursor.set_char_range(Some(egui::text_selection::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(end),
            )));
            state.store(&ctx, edit_id);
        }
    }
    painter.rect_stroke(inner, CornerRadius::same(3), Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
    if editor_response.lost_focus() && !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        return Some(editor.buf.clone());
    }
    None
}

fn display_name(entry: &Entry, hide_extensions: bool) -> &str {
    if !hide_extensions || entry.is_folder {
        return &entry.name;
    }
    match entry.name.rfind('.') {
        Some(dot) if dot > 0 => &entry.name[..dot],
        _ => &entry.name,
    }
}

fn search_folder_label(entry: &Entry, search_root: Option<&Path>) -> String {
    let Some(parent) = entry.path.as_deref().and_then(Path::parent) else {
        return entry.kind.clone();
    };
    if let Some(root) = search_root
        && let Ok(rel) = parent.strip_prefix(root)
    {
        let s = rel.to_string_lossy();
        return if s.is_empty() { "当前文件夹".to_string() } else { s.into_owned() };
    }
    parent.to_string_lossy().into_owned()
}

fn is_drop_hovered(hover_target: Option<&Path>, path: Option<&Path>) -> bool {
    match (hover_target, path) {
        (Some(a), Some(b)) => a.as_os_str().eq_ignore_ascii_case(b.as_os_str()),
        _ => false,
    }
}

/// Draws a tiny Explorer-style status badge (OneDrive cloud / local pin / shortcut)
/// at the bottom-right corner of an item's icon.
fn paint_status_badge(painter: &egui::Painter, icon_rect: Rect, entry: &Entry, t: &theme::Theme) {
    let badge_pos = Pos2::new(icon_rect.right() - 2.0, icon_rect.bottom() - 2.0);
    if entry.is_cloud_only() {
        painter.circle_filled(badge_pos, 4.5, t.surface);
        painter.circle_stroke(badge_pos, 3.3, Stroke::new(1.2, t.accent));
    } else if entry.is_cloud_pinned() {
        painter.circle_filled(badge_pos, 4.2, Color32::from_rgb(0x10, 0x7C, 0x10));
        widgets::icon_at(painter, badge_pos, icons::CHECK, 6.0, Color32::WHITE);
    } else if entry.is_symlink_or_shortcut() {
        let box_rect = Rect::from_center_size(badge_pos, Vec2::splat(7.5));
        painter.rect_filled(box_rect, CornerRadius::same(1), t.surface);
        painter.rect_stroke(
            box_rect,
            CornerRadius::same(1),
            Stroke::new(0.8, t.border_strong),
            egui::StrokeKind::Inside,
        );
        widgets::icon_at(painter, badge_pos, icons::LINK, 6.0, t.accent);
    }
}

impl LumenApp {
    pub(crate) fn paint_content(&mut self, ui: &mut egui::Ui, rect: Rect, secondary: bool) {
        let t = self.theme.clone();
        let painter = ui.painter().clone();
        let split_active = self.secondary_tab.is_some();
        let focused_pane = self.focus_secondary == secondary;
        painter.rect_filled(rect, CornerRadius::same(theme::RADIUS_CARD), t.surface);
        let border_col = if split_active && focused_pane { t.accent } else { t.border };
        widgets::stroke_rect(&painter, rect, theme::RADIUS_CARD, Stroke::new(1.0, border_col));

        let mut top = rect.top();
        // When split view is active, render a sleek 28px mini path header per pane
        // so the user always knows which folder each pane displays and can click to focus.
        if split_active {
            let hdr = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 28.0));
            let hdr_fill = if focused_pane { widgets::fade(t.accent_soft, 0.7) } else { t.surface_alt };
            painter.rect_filled(
                hdr,
                CornerRadius { nw: theme::RADIUS_CARD, ne: theme::RADIUS_CARD, sw: 0, se: 0 },
                hdr_fill,
            );
            let hdr_resp = ui.interact(hdr, egui::Id::unique(("pane-hdr", u8::from(secondary))), Sense::click());
            if hdr_resp.clicked() && self.focus_secondary != secondary {
                self.focus_secondary = secondary;
                self.sync_watch();
            }
            let title = self.tab_ref(secondary).location.parsing_name();
            widgets::icon_at(
                &painter,
                Pos2::new(hdr.left() + 14.0, hdr.center().y),
                icons::FOLDER,
                12.0,
                if focused_pane { t.accent } else { t.text_tertiary },
            );
            widgets::text_left(
                ui,
                &painter,
                Rect::from_min_max(
                    Pos2::new(hdr.left() + 26.0, hdr.top()),
                    Pos2::new(hdr.right() - 10.0, hdr.bottom()),
                ),
                &title,
                theme::font(11.5),
                if focused_pane { t.text } else { t.text_secondary },
                0.0,
            );
            top = hdr.bottom();
        }

        // Register pane-level drop zone before individual folder rows so dropping on
        // empty space in either pane targets that pane's directory.
        if let Some(dir) = self.tab_ref(secondary).location.path.clone() {
            self.drop_zones.push((rect, dir));
        }

        let entries = self.tab_ref(secondary).entries.clone();
        let view = self.tab_ref(secondary).view;
        let searching = self.tab_ref(secondary).is_search();
        let loading = self.tab_ref(secondary).loading;
        if searching && focused_pane {
            top = self.paint_search_banner(ui, rect, top);
        }
        let body =
            Rect::from_min_max(Pos2::new(rect.left() + 1.0, top), Pos2::new(rect.right() - 1.0, rect.bottom() - 1.0));

        if let Some(error) = self.tab_ref(secondary).error.clone() {
            self.paint_message(ui, body, icons::INFO, &error, true);
            return;
        }
        if entries.is_empty() {
            let text = if loading {
                "正在读取…"
            } else if searching {
                "没有匹配的项目"
            } else {
                "此文件夹为空"
            };
            let bg = ui.interact(body, egui::Id::unique(("empty-folder-bg", u8::from(secondary))), Sense::click());
            self.paint_message(ui, body, icons::FOLDER, text, false);
            if bg.clicked() && self.focus_secondary != secondary {
                self.focus_secondary = secondary;
                self.sync_watch();
            }
            if !loading && !searching && bg.secondary_clicked() {
                if self.focus_secondary != secondary {
                    self.focus_secondary = secondary;
                    self.sync_watch();
                }
                self.tab_mut(secondary).clear_selection();
                let extended = ui.input(|i| i.modifiers.shift);
                let anchor = ui.ctx().pointer_interact_pos().unwrap_or(body.center());
                self.open_context_menu(anchor, extended);
            }
            return;
        }
        if loading {
            self.paint_loading_bar(ui, body, &t);
        }

        match view {
            ViewMode::Details => self.paint_details_view(ui, body, entries, secondary),
            ViewMode::List => self.paint_list_view(ui, body, entries, secondary),
            ViewMode::Tiles | ViewMode::Icons => self.paint_grid_view(ui, body, entries, view, secondary),
        }
    }

    /// Thin indeterminate bar: the listing is on its way, keep the old rows visible.
    fn paint_loading_bar(&self, ui: &egui::Ui, area: Rect, t: &theme::Theme) {
        let time = ui.input(|input| input.time) as f32;
        let width = (area.width() * 0.22).max(120.0);
        let travel = area.width() - width;
        let x = area.left() + (travel * ((time * 0.9) % 1.0));
        let bar = Rect::from_min_size(Pos2::new(x, area.top()), Vec2::new(width, 2.0));
        ui.painter().rect_filled(bar, CornerRadius::same(1), t.accent);
    }

    fn paint_search_banner(&mut self, ui: &mut egui::Ui, rect: Rect, top: f32) -> f32 {
        let t = self.theme.clone();
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let banner = Rect::from_min_size(Pos2::new(rect.left(), top), Vec2::new(rect.width(), 34.0));
        painter.rect_filled(
            banner,
            CornerRadius { nw: theme::RADIUS_CARD, ne: theme::RADIUS_CARD, sw: 0, se: 0 },
            t.accent_soft,
        );
        let root = self.search.root.as_ref().map(|path| path.to_string_lossy().to_string()).unwrap_or_default();
        let count = self.active().entries.len();
        let backend = self.search.engine.active_backend_label();
        let text = if self.search.running {
            format!("正在使用 {backend} 搜索 “{}” … 已找到 {count} 个", self.search.query)
        } else {
            format!("“{root}” 中搜索 “{}” — {count} 个结果（{backend}）", self.search.query)
        };
        widgets::text_left(
            ui,
            &painter,
            banner.shrink2(Vec2::new(14.0, 0.0)),
            &text,
            theme::font(12.0),
            t.text_secondary,
            0.0,
        );
        let close = Rect::from_center_size(Pos2::new(banner.right() - 20.0, banner.center().y), Vec2::splat(30.0));
        let id = egui::Id::unique("cancel-search");
        let response = ui.interact(close, id, Sense::click());
        let hover = widgets::hover_t(&ctx, id, response.hovered());
        if hover > 0.0 {
            painter.rect_filled(close, CornerRadius::same(4), widgets::fade(t.hover_strong, hover));
        }
        widgets::icon_at(&painter, close.center(), icons::CANCEL, 9.0, t.text_secondary);
        if response.clicked() {
            self.cancel_search();
        }
        banner.bottom()
    }

    fn paint_message(&self, ui: &egui::Ui, rect: Rect, glyph: &str, text: &str, error: bool) {
        let t = &self.theme;
        let painter = ui.painter();
        let center = rect.center();
        widgets::icon_at(painter, center - Vec2::new(0.0, 22.0), glyph, 30.0, t.text_disabled);
        let color = if error { t.danger } else { t.text_tertiary };
        let galley = widgets::galley(ui, text, theme::font(12.5), color, rect.width() - 40.0);
        widgets::galley_at(painter, Pos2::new(center.x - galley.size().x * 0.5, center.y + 8.0), galley, color);
    }

    /// Paints a clickable collapsible group header; returns true when clicked.
    #[allow(clippy::too_many_arguments)]
    fn paint_group_header(
        &self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        rect: Rect,
        label: &str,
        count: usize,
        collapsed: bool,
        pane_tag: u8,
    ) -> bool {
        let t = &self.theme;
        let id = egui::Id::unique(("grp-hdr", pane_tag, label));
        let resp = ui.interact(rect, id, Sense::click());
        let hover = widgets::hover_t(ui.ctx(), id, resp.hovered());
        let fill = if hover > 0.0 { widgets::fade(t.hover_strong, hover) } else { widgets::fade(t.surface_alt, 0.7) };
        painter.rect_filled(rect, CornerRadius::same(theme::RADIUS_ROW), fill);
        widgets::icon_at(
            painter,
            Pos2::new(rect.left() + 14.0, rect.center().y),
            if collapsed { icons::CHEVRON_RIGHT } else { icons::CHEVRON_DOWN },
            8.5,
            t.text_tertiary,
        );
        widgets::text_left(
            ui,
            painter,
            Rect::from_min_max(
                Pos2::new(rect.left() + 28.0, rect.top()),
                Pos2::new(rect.right() - 80.0, rect.bottom()),
            ),
            label,
            theme::semibold(12.0),
            t.text_secondary,
            0.0,
        );
        widgets::text_right(
            ui,
            painter,
            Rect::from_min_max(
                Pos2::new(rect.right() - 76.0, rect.top()),
                Pos2::new(rect.right() - 10.0, rect.bottom()),
            ),
            &format!("{count} 个项目"),
            theme::font(10.5),
            t.text_tertiary,
        );
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp.clicked()
    }

    // ---- details view ----------------------------------------------------

    fn paint_details_view(&mut self, ui: &mut egui::Ui, area: Rect, entries: Arc<Vec<Entry>>, secondary: bool) {
        let t = self.theme.clone();
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let pane_tag = u8::from(secondary);
        let pane_focused = self.focus_secondary == secondary;
        let widths = self.tab_ref(secondary).col_widths;
        let header = Rect::from_min_size(area.min, Vec2::new(area.width(), HEADER_H));
        let inner_left = header.left() + 6.0;
        // Keep a gutter for the scrollbar so the last column never hugs the edge.
        let inner_width = header.width() - 12.0 - 16.0;
        let date_w = widths[0];
        let type_w = widths[1];
        let size_w = widths[2];
        let name_w = (inner_width - date_w - type_w - size_w).max(120.0);
        let sort = self.tab_ref(secondary).sort;

        let this_pc = self.tab_ref(secondary).is_this_pc();
        let searching = self.tab_ref(secondary).is_search();
        let columns: [(&str, SortKey, f32, bool); 4] = if this_pc {
            [
                ("名称", SortKey::Name, name_w, false),
                ("总大小", SortKey::Size, date_w, true),
                ("可用空间", SortKey::Size, type_w, true),
                ("类型", SortKey::Type, size_w, false),
            ]
        } else if searching {
            [
                ("名称", SortKey::Name, name_w, false),
                ("修改日期", SortKey::DateModified, date_w, false),
                ("所在文件夹", SortKey::Type, type_w, false),
                ("大小", SortKey::Size, size_w, true),
            ]
        } else {
            [
                ("名称", SortKey::Name, name_w, false),
                ("修改日期", SortKey::DateModified, date_w, false),
                ("类型", SortKey::Type, type_w, false),
                ("大小", SortKey::Size, size_w, true),
            ]
        };

        let rows_area = Rect::from_min_max(Pos2::new(area.left(), header.bottom()), area.max);
        // Registered before the rows so a press on empty space starts a marquee
        // and a press on a row still reaches the row.
        let background = ui.interact(rows_area, egui::Id::unique(("details-bg", pane_tag)), Sense::click_and_drag());

        let mut x = inner_left;
        let mut new_sort: Option<SortKey> = None;
        let mut boundaries: Vec<(f32, usize)> = Vec::new();
        for (index, (label, key, width, right_align)) in columns.iter().enumerate() {
            let cell = Rect::from_min_size(Pos2::new(x, header.top()), Vec2::new(*width, header.height()));
            let id = egui::Id::unique(("head", pane_tag, index));
            let response = ui.interact(cell, id, Sense::click());
            let hover = widgets::hover_t(&ctx, id, response.hovered());
            if hover > 0.0 {
                painter.rect_filled(
                    cell.shrink2(Vec2::new(1.0, 6.0)),
                    CornerRadius::same(3),
                    widgets::fade(t.hover, hover),
                );
            }
            let active = sort.key == *key;
            let text_color = if active { t.text } else { t.text_tertiary };
            if *right_align {
                widgets::text_right(ui, &painter, cell, label, theme::font(12.0), text_color);
            } else {
                widgets::text_left(
                    ui,
                    &painter,
                    Rect::from_min_max(
                        Pos2::new(cell.left() + if index == 0 { 26.0 } else { 7.0 }, cell.top()),
                        Pos2::new(cell.right() - 4.0, cell.bottom()),
                    ),
                    label,
                    theme::font(12.0),
                    text_color,
                    0.0,
                );
            }
            if active {
                widgets::icon_at(
                    &painter,
                    Pos2::new(cell.right() - 12.0, cell.center().y),
                    if sort.ascending { icons::CHEVRON_UP } else { icons::CHEVRON_DOWN },
                    8.0,
                    t.text_tertiary,
                );
            }
            if response.clicked() {
                new_sort = Some(*key);
            }
            x += *width;
            if index + 1 < columns.len() {
                boundaries.push((x, index));
            }
        }

        // Column resize: boundaries 0/1/2 separate Name|col0, col0|col1, col1|col2.
        let mut resized = false;
        const DEFAULTS: [f32; 3] = [168.0, 150.0, 92.0];
        for (boundary, index) in boundaries {
            let strip =
                Rect::from_min_max(Pos2::new(boundary - 3.0, header.top()), Pos2::new(boundary + 3.0, header.bottom()));
            let response =
                ui.interact(strip, egui::Id::unique(("col-resize", pane_tag, index)), Sense::click_and_drag());
            if response.hovered() || response.dragged() {
                ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if response.double_clicked() {
                let tab = self.tab_mut(secondary);
                tab.col_widths[index] = DEFAULTS[index];
                resized = true;
            } else if response.dragged() {
                let delta = response.drag_delta().x;
                let tab = self.tab_mut(secondary);
                match index {
                    0 => {
                        // Moving Name|Date separator right grows Name and shrinks Date.
                        tab.col_widths[0] = (tab.col_widths[0] - delta).clamp(80.0, 360.0);
                    }
                    1 => {
                        // Moving Date|Type separator right grows Date and shrinks Type.
                        let next_left = (tab.col_widths[0] + delta).clamp(80.0, 360.0);
                        let actual = next_left - tab.col_widths[0];
                        let next_right = (tab.col_widths[1] - actual).clamp(80.0, 360.0);
                        let applied = tab.col_widths[1] - next_right;
                        tab.col_widths[0] += applied;
                        tab.col_widths[1] = next_right;
                    }
                    2 => {
                        // Moving Type|Size separator right grows Type and shrinks Size.
                        let next_left = (tab.col_widths[1] + delta).clamp(80.0, 360.0);
                        let actual = next_left - tab.col_widths[1];
                        let next_right = (tab.col_widths[2] - actual).clamp(64.0, 280.0);
                        let applied = tab.col_widths[2] - next_right;
                        tab.col_widths[1] += applied;
                        tab.col_widths[2] = next_right;
                    }
                    _ => {}
                }
                resized = true;
            }
        }
        if resized && pane_focused {
            self.record_folder_view();
        }

        if let Some(key) = new_sort {
            self.tab_mut(secondary).set_sort(key);
            if pane_focused {
                self.record_folder_view();
            }
        }

        painter.line_segment(
            [Pos2::new(area.left() + 8.0, header.bottom()), Pos2::new(area.right() - 8.0, header.bottom())],
            Stroke::new(1.0, t.divider),
        );

        let scale = if self.settings.compact_mode { 26.0 / ROW_H } else { 1.0 };
        let row_h = ROW_H * scale;
        let group_h = GROUP_H * scale;
        let show_checks = self.settings.show_checkboxes;
        let hide_ext = self.settings.hide_extensions;
        let search_root = self.search.root.clone();

        let display = self.tab_ref(secondary).display.clone();
        let offsets = self.tab_ref(secondary).row_offsets.clone();
        let total = display.len();
        let content_h = offsets.last().copied().unwrap_or(0.0) * scale;
        if pane_focused {
            self.page_rows = (rows_area.height() / row_h).max(1.0) as usize;
        }

        let target_y = self
            .tab_mut(secondary)
            .scroll_to
            .take()
            .map(|index| self.tab_ref(secondary).offset_for_entry(index, rows_area.height() / scale) * scale);
        let offset = self.scroll_offset(ui, rows_area, content_h, target_y, secondary);
        let drag_hover = crate::shell::dnd::drag_hover();
        let hover_target = drag_hover.as_ref().and_then(|h| h.target.as_deref());

        let mut rename = if pane_focused { self.rename.take() } else { None };
        let mut rename_commit: Option<(usize, String)> = None;
        let mut open_index: Option<usize> = None;
        let mut context_index: Option<usize> = None;
        let mut drag_index: Option<usize> = None;
        let mut select_request: Option<(usize, egui::Modifiers)> = None;
        let mut toggle_group: Option<String> = None;
        let mut row_rects: Vec<(usize, Rect)> = Vec::new();

        let generation = self.tab_ref(secondary).generation;
        // Crisp 120ms cubic ease-out micro-glide on folder entry (zero overshoot).
        let enter_offset = (1.0
            - crate::ui::spring::ease_out_reveal(&ctx, egui::Id::unique(("pane-enter", pane_tag, generation)), 0.12))
            * 5.0;
        let single_selected = self.tab_ref(secondary).selection.len() == 1;
        let mut single_sel_row_y: Option<f32> = None;
        let mut single_sel_row_left: f32 = rows_area.left() + 5.0;
        let unscaled_offset = offset / scale;
        let first =
            offsets.partition_point(|&value| value <= unscaled_offset).saturating_sub(1).min(total.saturating_sub(1));
        for position in first..total {
            let row_top = rows_area.top() + offsets[position] * scale - offset + enter_offset;
            if row_top > rows_area.bottom() + group_h {
                break;
            }
            match &display[position] {
                DisplayRow::Group { label, count, collapsed } => {
                    let group = Rect::from_min_size(
                        Pos2::new(rows_area.left() + 4.0, row_top),
                        Vec2::new(rows_area.width() - 16.0, group_h),
                    );
                    if !rows_area.intersects(group) {
                        continue;
                    }
                    if self.paint_group_header(ui, &painter, group, label, *count, *collapsed, pane_tag) {
                        toggle_group = Some(label.clone());
                    }
                }
                DisplayRow::Entry(index) => {
                    let index = *index;
                    let Some(entry) = entries.get(index) else { continue };
                    let row = Rect::from_min_size(
                        Pos2::new(rows_area.left() + 4.0, row_top),
                        Vec2::new(rows_area.width() - 16.0, row_h),
                    );
                    if !rows_area.intersects(row) {
                        continue;
                    }
                    row_rects.push((index, row));
                    if entry.is_folder
                        && let Some(path) = &entry.path
                    {
                        self.drop_zones.push((row, path.clone()));
                    }
                    let id = egui::Id::unique(("row", pane_tag, index));
                    let response = ui.interact(row, id, Sense::click_and_drag());
                    let hover = widgets::hover_t(&ctx, id, response.hovered());
                    let selected = self.tab_ref(secondary).selection.contains(&index);
                    let drop_hi = entry.is_folder && is_drop_hovered(hover_target, entry.path.as_deref());
                    if drop_hi {
                        painter.rect_filled(row, CornerRadius::same(theme::RADIUS_ROW), t.accent_soft);
                        painter.rect_stroke(
                            row,
                            CornerRadius::same(theme::RADIUS_ROW),
                            Stroke::new(1.5, t.accent),
                            egui::StrokeKind::Inside,
                        );
                    } else if selected {
                        painter.rect_filled(row, CornerRadius::same(theme::RADIUS_ROW), t.selected);
                        if hover > 0.0 {
                            painter.rect_filled(
                                row,
                                CornerRadius::same(theme::RADIUS_ROW),
                                widgets::fade(t.selected_hover, hover),
                            );
                        }
                        if single_selected {
                            single_sel_row_y = Some(row.center().y);
                            single_sel_row_left = row.left() + 1.0;
                        } else {
                            painter.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(row.left() + 1.0, row.center().y - 8.0),
                                    Vec2::new(3.0, 16.0),
                                ),
                                CornerRadius::same(2),
                                t.selected_bar,
                            );
                        }
                    } else if hover > 0.0 {
                        painter.rect_filled(row, CornerRadius::same(theme::RADIUS_ROW), widgets::fade(t.hover, hover));
                    }

                    let cut = self.is_cut(entry.path.as_deref());
                    let dimmed = cut || entry.is_hidden();
                    let tint = if dimmed { Color32::from_white_alpha(135) } else { Color32::WHITE };
                    let text_color = if cut {
                        widgets::fade(t.text, 0.55)
                    } else if entry.is_hidden() {
                        t.text_secondary
                    } else {
                        t.text
                    };

                    let check_offset = if show_checks { 22.0 } else { 0.0 };
                    let mut checkbox_clicked = false;
                    if show_checks && (selected || hover > 0.0) {
                        let box_rect =
                            Rect::from_center_size(Pos2::new(row.left() + 14.0, row.center().y), Vec2::splat(14.0));
                        let box_resp = ui.interact(
                            box_rect.expand(3.0),
                            egui::Id::unique(("chk", pane_tag, index)),
                            Sense::click(),
                        );
                        if selected {
                            painter.rect_filled(box_rect, CornerRadius::same(3), t.accent);
                            widgets::icon_at(&painter, box_rect.center(), icons::CHECK, 9.0, Color32::WHITE);
                        } else {
                            painter.rect_stroke(
                                box_rect,
                                CornerRadius::same(3),
                                Stroke::new(1.2, t.border_strong),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if box_resp.clicked() {
                            checkbox_clicked = true;
                        }
                    }

                    let icon = self.shell_icon_entry(ui.ctx(), entry, 16, false);
                    let icon_rect = Rect::from_center_size(
                        Pos2::new(row.left() + 18.0 + check_offset, row.center().y),
                        Vec2::splat(16.0),
                    );
                    match &icon {
                        Some(texture) => {
                            painter.image(
                                texture.id(),
                                icon_rect,
                                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                tint,
                            );
                        }
                        None => {
                            let glyph = if entry.is_folder { icons::FOLDER } else { icons::FILE };
                            let color = if entry.is_folder { t.accent } else { t.text_secondary };
                            widgets::icon_at(
                                &painter,
                                icon_rect.center(),
                                glyph,
                                14.0,
                                if cut { widgets::fade(color, 0.5) } else { color },
                            );
                        }
                    }
                    paint_status_badge(&painter, icon_rect, entry, &t);

                    let name_rect = Rect::from_min_max(
                        Pos2::new(row.left() + 32.0 + check_offset, row.top()),
                        Pos2::new(row.left() + 4.0 + name_w - 6.0, row.bottom()),
                    );
                    let editing = rename.as_ref().map(|state| state.row == index).unwrap_or(false);
                    if editing {
                        let editor = rename.as_mut().expect("checked above");
                        if let Some(buf) = paint_inline_rename(
                            ui,
                            &painter,
                            &t,
                            editor,
                            name_rect.shrink2(Vec2::new(0.0, 3.0)),
                            13.0,
                            entry.is_folder,
                        ) {
                            rename_commit = Some((index, buf));
                        }
                    } else {
                        widgets::text_left(
                            ui,
                            &painter,
                            name_rect,
                            display_name(entry, hide_ext),
                            theme::font(13.0),
                            text_color,
                            0.0,
                        );
                    }

                    let date_rect =
                        Rect::from_min_size(Pos2::new(row.left() + 4.0 + name_w, row.top()), Vec2::new(date_w, row_h));
                    let type_rect =
                        Rect::from_min_size(Pos2::new(date_rect.right(), row.top()), Vec2::new(type_w, row_h));
                    let size_rect =
                        Rect::from_min_size(Pos2::new(type_rect.right(), row.top()), Vec2::new(size_w, row_h));
                    if this_pc {
                        let total_text = if entry.size > 0 { format_size(entry.size) } else { "—".to_string() };
                        let free_text =
                            if entry.free_space > 0 { format_size(entry.free_space) } else { "—".to_string() };
                        widgets::text_right(
                            ui,
                            &painter,
                            date_rect.shrink2(Vec2::new(2.0, 0.0)),
                            &total_text,
                            theme::font(12.5),
                            t.text_secondary,
                        );
                        widgets::text_right(
                            ui,
                            &painter,
                            type_rect.shrink2(Vec2::new(2.0, 0.0)),
                            &free_text,
                            theme::font(12.5),
                            t.text_secondary,
                        );
                        if entry.size > 0 && entry.free_space <= entry.size {
                            let used = ((entry.size - entry.free_space) as f64 / entry.size as f64) as f32;
                            let bar = Rect::from_min_size(
                                Pos2::new(type_rect.left() + 8.0, type_rect.bottom() - 4.5),
                                Vec2::new((type_rect.width() - 16.0).max(12.0), 2.5),
                            );
                            painter.rect_filled(bar, CornerRadius::same(1), widgets::fade(t.border_strong, 0.45));
                            let bar_col = if used > 0.9 { t.danger } else { t.accent };
                            painter.rect_filled(
                                Rect::from_min_size(
                                    bar.min,
                                    Vec2::new(bar.width() * used.clamp(0.0, 1.0), bar.height()),
                                ),
                                CornerRadius::same(1),
                                bar_col,
                            );
                        }
                        widgets::text_left(
                            ui,
                            &painter,
                            size_rect,
                            &entry.kind,
                            theme::font(12.5),
                            t.text_secondary,
                            7.0,
                        );
                    } else {
                        widgets::text_left(
                            ui,
                            &painter,
                            date_rect,
                            &format_time(entry.modified),
                            theme::font(12.5),
                            t.text_secondary,
                            7.0,
                        );
                        let col3 = if searching {
                            search_folder_label(entry, search_root.as_deref())
                        } else {
                            entry.kind.clone()
                        };
                        widgets::text_left(ui, &painter, type_rect, &col3, theme::font(12.5), t.text_secondary, 7.0);
                        let size_text = if entry.is_folder { String::new() } else { format_size(entry.size) };
                        widgets::text_right(
                            ui,
                            &painter,
                            size_rect.shrink2(Vec2::new(2.0, 0.0)),
                            &size_text,
                            theme::font(12.5),
                            t.text_secondary,
                        );
                    }

                    if checkbox_clicked {
                        select_request = Some((index, egui::Modifiers::CTRL));
                    } else if response.clicked() {
                        select_request = Some((index, ui.input(|input| input.modifiers)));
                    }
                    if response.double_clicked() {
                        open_index = Some(index);
                    }
                    if response.secondary_clicked() {
                        context_index = Some(index);
                    }
                    if response.drag_started() && !editing {
                        drag_index = Some(index);
                    }
                }
            }
        }

        if let Some(target_cy) = single_sel_row_y {
            let (cy, vy) = crate::ui::spring::animate_vel(
                &ctx,
                egui::Id::unique(("details-sel-bar-y", pane_tag, generation)),
                target_cy,
                target_cy,
                crate::ui::spring::MAGNETIC,
                0.15,
            );
            let stretch = (vy.abs() * 0.008).min(8.0);
            let bar = Rect::from_center_size(Pos2::new(single_sel_row_left + 1.5, cy), Vec2::new(3.0, 16.0 + stretch));
            if rows_area.intersects(bar) {
                painter.rect_filled(bar, CornerRadius::same(2), t.selected_bar);
            }
        }
        if let Some(label) = toggle_group {
            self.tab_mut(secondary).toggle_group(&label);
        }
        self.apply_marquee(ui, &painter, &background, &row_rects, &t, secondary);
        self.finish_view_interactions(
            ui,
            secondary,
            rename,
            rename_commit,
            select_request,
            context_index,
            open_index,
            drag_index,
            &background,
        );
    }

    // ---- list view -------------------------------------------------------

    fn paint_list_view(&mut self, ui: &mut egui::Ui, area: Rect, entries: Arc<Vec<Entry>>, secondary: bool) {
        let t = self.theme.clone();
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let pane_tag = u8::from(secondary);
        let pane_focused = self.focus_secondary == secondary;
        let display = self.tab_ref(secondary).display.clone();
        let total = display.len();
        let content_h = total as f32 * LIST_ROW_H;
        let background = ui.interact(area, egui::Id::unique(("list-bg", pane_tag)), Sense::click_and_drag());
        if pane_focused {
            self.page_rows = (area.height() / LIST_ROW_H).max(1.0) as usize;
        }

        let target_y = self.tab_mut(secondary).scroll_to.take().map(|index| {
            let pos = display.iter().position(|r| matches!(r, DisplayRow::Entry(e) if *e == index)).unwrap_or(index);
            pos as f32 * LIST_ROW_H
        });
        let offset = self.scroll_offset(ui, area, content_h, target_y, secondary);
        let drag_hover = crate::shell::dnd::drag_hover();
        let hover_target = drag_hover.as_ref().and_then(|h| h.target.as_deref());

        let mut rename = if pane_focused { self.rename.take() } else { None };
        let mut rename_commit: Option<(usize, String)> = None;
        let mut select_request: Option<(usize, egui::Modifiers)> = None;
        let mut open_index: Option<usize> = None;
        let mut context_index: Option<usize> = None;
        let mut drag_index: Option<usize> = None;
        let mut toggle_group: Option<String> = None;
        let mut row_rects: Vec<(usize, Rect)> = Vec::new();
        let first = (offset / LIST_ROW_H).floor().max(0.0) as usize;
        let visible = (area.height() / LIST_ROW_H).ceil() as usize + 1;
        let last = (first + visible).min(total);
        for position in first..last {
            let row_top = area.top() + position as f32 * LIST_ROW_H - offset;
            let row =
                Rect::from_min_size(Pos2::new(area.left() + 4.0, row_top), Vec2::new(area.width() - 16.0, LIST_ROW_H));
            if !area.intersects(row) {
                continue;
            }
            match &display[position] {
                DisplayRow::Group { label, count, collapsed } => {
                    if self.paint_group_header(ui, &painter, row, label, *count, *collapsed, pane_tag) {
                        toggle_group = Some(label.clone());
                    }
                }
                DisplayRow::Entry(index) => {
                    let index = *index;
                    let Some(entry) = entries.get(index) else { continue };
                    row_rects.push((index, row));
                    if entry.is_folder
                        && let Some(path) = &entry.path
                    {
                        self.drop_zones.push((row, path.clone()));
                    }
                    let id = egui::Id::unique(("lrow", pane_tag, index));
                    let response = ui.interact(row, id, Sense::click_and_drag());
                    let hover = widgets::hover_t(&ctx, id, response.hovered());
                    let drop_hi = entry.is_folder && is_drop_hovered(hover_target, entry.path.as_deref());
                    if drop_hi {
                        painter.rect_filled(row, CornerRadius::same(theme::RADIUS_ROW), t.accent_soft);
                        painter.rect_stroke(
                            row,
                            CornerRadius::same(theme::RADIUS_ROW),
                            Stroke::new(1.5, t.accent),
                            egui::StrokeKind::Inside,
                        );
                    } else if self.tab_ref(secondary).selection.contains(&index) {
                        painter.rect_filled(row, CornerRadius::same(theme::RADIUS_ROW), t.selected);
                    } else if hover > 0.0 {
                        painter.rect_filled(row, CornerRadius::same(theme::RADIUS_ROW), widgets::fade(t.hover, hover));
                    }

                    let cut = self.is_cut(entry.path.as_deref());
                    let dimmed = cut || entry.is_hidden();
                    let tint = if dimmed { Color32::from_white_alpha(135) } else { Color32::WHITE };
                    let text_color = if cut {
                        widgets::fade(t.text, 0.55)
                    } else if entry.is_hidden() {
                        t.text_secondary
                    } else {
                        t.text
                    };

                    let icon = self.shell_icon_entry(ui.ctx(), entry, 16, false);
                    let icon_rect =
                        Rect::from_center_size(Pos2::new(row.left() + 14.0, row.center().y), Vec2::splat(16.0));
                    match &icon {
                        Some(texture) => {
                            painter.image(
                                texture.id(),
                                icon_rect,
                                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                tint,
                            );
                        }
                        None => {
                            widgets::icon_at(
                                &painter,
                                icon_rect.center(),
                                if entry.is_folder { icons::FOLDER } else { icons::FILE },
                                14.0,
                                if cut { widgets::fade(t.text_secondary, 0.5) } else { t.text_secondary },
                            );
                        }
                    }
                    paint_status_badge(&painter, icon_rect, entry, &t);

                    let name_rect = Rect::from_min_max(
                        Pos2::new(row.left() + 28.0, row.top()),
                        Pos2::new(row.right() - 100.0, row.bottom()),
                    );
                    let editing = rename.as_ref().map(|state| state.row == index).unwrap_or(false);
                    if editing {
                        let editor = rename.as_mut().expect("checked above");
                        if let Some(buf) = paint_inline_rename(
                            ui,
                            &painter,
                            &t,
                            editor,
                            name_rect.shrink2(Vec2::new(0.0, 2.0)),
                            12.5,
                            entry.is_folder,
                        ) {
                            rename_commit = Some((index, buf));
                        }
                    } else {
                        widgets::text_left(
                            ui,
                            &painter,
                            name_rect,
                            display_name(entry, self.settings.hide_extensions),
                            theme::font(12.5),
                            text_color,
                            0.0,
                        );
                    }
                    if !entry.is_folder {
                        widgets::text_right(
                            ui,
                            &painter,
                            Rect::from_min_max(
                                Pos2::new(row.right() - 96.0, row.top()),
                                Pos2::new(row.right() - 6.0, row.bottom()),
                            ),
                            &format_size(entry.size),
                            theme::font(12.0),
                            t.text_tertiary,
                        );
                    }
                    if response.clicked() {
                        select_request = Some((index, ui.input(|input| input.modifiers)));
                    }
                    if response.double_clicked() {
                        open_index = Some(index);
                    }
                    if response.secondary_clicked() {
                        context_index = Some(index);
                    }
                    if response.drag_started() && !editing {
                        drag_index = Some(index);
                    }
                }
            }
        }

        if let Some(label) = toggle_group {
            self.tab_mut(secondary).toggle_group(&label);
        }
        self.apply_marquee(ui, &painter, &background, &row_rects, &t, secondary);
        self.finish_view_interactions(
            ui,
            secondary,
            rename,
            rename_commit,
            select_request,
            context_index,
            open_index,
            drag_index,
            &background,
        );
    }

    // ---- tiles / icons ---------------------------------------------------

    fn paint_grid_view(
        &mut self,
        ui: &mut egui::Ui,
        area: Rect,
        entries: Arc<Vec<Entry>>,
        view: ViewMode,
        secondary: bool,
    ) {
        let t = self.theme.clone();
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let pane_tag = u8::from(secondary);
        let pane_focused = self.focus_secondary == secondary;
        let zoom_delta = if ui.rect_contains_pointer(area) { ui.input(|input| input.zoom_delta()) } else { 1.0 };
        if zoom_delta != 1.0 {
            let tab = self.tab_mut(secondary);
            tab.grid_zoom = (tab.grid_zoom * zoom_delta).clamp(0.7, 2.4);
        }
        let zoom = self.tab_ref(secondary).grid_zoom;
        let (cell_w, cell_h, icon_px) = if view == ViewMode::Tiles {
            (108.0 * zoom, 98.0 * zoom, (44.0 * zoom).round() as u32)
        } else {
            (136.0 * zoom, 116.0 * zoom, (64.0 * zoom).round() as u32)
        };
        let columns = ((area.width() - 16.0) / cell_w).floor().max(1.0) as usize;
        if pane_focused {
            self.grid_columns = columns;
            self.page_rows = ((area.height() / cell_h).max(1.0) as usize) * columns;
        }
        let rows = entries.len().div_ceil(columns);
        let content_h = rows as f32 * cell_h;
        let background = ui.interact(area, egui::Id::unique(("grid-bg", pane_tag)), Sense::click_and_drag());

        let target_y = self.tab_mut(secondary).scroll_to.take().map(|index| (index / columns) as f32 * cell_h);
        let offset = self.scroll_offset(ui, area, content_h, target_y, secondary);
        let drag_hover = crate::shell::dnd::drag_hover();
        let hover_target = drag_hover.as_ref().and_then(|h| h.target.as_deref());

        let mut rename = if pane_focused { self.rename.take() } else { None };
        let mut rename_commit: Option<(usize, String)> = None;
        let mut select_request: Option<(usize, egui::Modifiers)> = None;
        let mut open_index: Option<usize> = None;
        let mut context_index: Option<usize> = None;
        let mut drag_index: Option<usize> = None;
        let mut row_rects: Vec<(usize, Rect)> = Vec::new();
        let first_row = (offset / cell_h).floor().max(0.0) as usize;
        let visible_rows = (area.height() / cell_h).ceil() as usize + 1;
        for row in first_row..(first_row + visible_rows).min(rows) {
            for column in 0..columns {
                let index = row * columns + column;
                let Some(entry) = entries.get(index) else { continue };
                let cell = Rect::from_min_size(
                    Pos2::new(area.left() + 4.0 + column as f32 * cell_w, area.top() + row as f32 * cell_h - offset),
                    Vec2::new(cell_w - 4.0, cell_h - 4.0),
                );
                if !area.intersects(cell) {
                    continue;
                }
                row_rects.push((index, cell));
                if entry.is_folder
                    && let Some(path) = &entry.path
                {
                    self.drop_zones.push((cell, path.clone()));
                }
                let id = egui::Id::unique(("cell", pane_tag, index));
                let response = ui.interact(cell, id, Sense::click_and_drag());
                let hover = widgets::hover_t(&ctx, id, response.hovered());
                let selected = self.tab_ref(secondary).selection.contains(&index);
                let drop_hi = entry.is_folder && is_drop_hovered(hover_target, entry.path.as_deref());
                if drop_hi {
                    painter.rect_filled(cell, CornerRadius::same(6), t.accent_soft);
                    painter.rect_stroke(
                        cell,
                        CornerRadius::same(6),
                        Stroke::new(1.5, t.accent),
                        egui::StrokeKind::Inside,
                    );
                } else if selected {
                    painter.rect_filled(cell, CornerRadius::same(6), t.selected);
                    painter.rect_stroke(
                        cell,
                        CornerRadius::same(6),
                        Stroke::new(1.0, t.selected_bar),
                        egui::StrokeKind::Inside,
                    );
                } else if hover > 0.0 {
                    painter.rect_filled(cell, CornerRadius::same(6), widgets::fade(t.hover, hover));
                }

                let cut = self.is_cut(entry.path.as_deref());
                let dimmed = cut || entry.is_hidden();
                let tint = if dimmed { Color32::from_white_alpha(135) } else { Color32::WHITE };
                let text_color = if cut {
                    widgets::fade(t.text, 0.55)
                } else if entry.is_hidden() {
                    t.text_secondary
                } else {
                    t.text
                };

                let icon = self.shell_icon_entry(ui.ctx(), entry, icon_px, true);
                let icon_size = icon_px as f32;
                let icon_rect = Rect::from_center_size(
                    Pos2::new(cell.center().x, cell.top() + 12.0 + icon_size * 0.5),
                    Vec2::splat(icon_size),
                );
                match &icon {
                    Some(texture) => {
                        painter.image(
                            texture.id(),
                            icon_rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            tint,
                        );
                    }
                    None => {
                        let glyph = if entry.is_folder { icons::FOLDER } else { icons::FILE };
                        let color = if entry.is_folder { t.accent } else { t.text_secondary };
                        widgets::icon_at(
                            &painter,
                            icon_rect.center(),
                            glyph,
                            icon_size * 0.6,
                            if cut { widgets::fade(color, 0.5) } else { color },
                        );
                    }
                }
                paint_status_badge(&painter, icon_rect, entry, &t);
                let name_rect = Rect::from_min_max(
                    Pos2::new(cell.left() + 4.0, icon_rect.bottom() + 4.0),
                    Pos2::new(cell.right() - 4.0, cell.bottom() - 2.0),
                );
                let label_size = (12.0 * zoom).clamp(11.0, 17.0);
                let editing = rename.as_ref().map(|state| state.row == index).unwrap_or(false);
                if editing {
                    let editor = rename.as_mut().expect("checked above");
                    let edit_rect = Rect::from_min_size(
                        Pos2::new(name_rect.left(), name_rect.top() + 2.0),
                        Vec2::new(name_rect.width(), (label_size + 8.0).min(name_rect.height().max(20.0))),
                    );
                    if let Some(buf) =
                        paint_inline_rename(ui, &painter, &t, editor, edit_rect, label_size, entry.is_folder)
                    {
                        rename_commit = Some((index, buf));
                    }
                } else {
                    let galley = widgets::galley(
                        ui,
                        display_name(entry, self.settings.hide_extensions),
                        theme::font(label_size),
                        text_color,
                        name_rect.width(),
                    );
                    widgets::galley_at(
                        &painter,
                        Pos2::new(name_rect.center().x - galley.size().x * 0.5, name_rect.top() + 4.0),
                        galley,
                        text_color,
                    );
                }
                if response.clicked() {
                    select_request = Some((index, ui.input(|input| input.modifiers)));
                }
                if response.double_clicked() {
                    open_index = Some(index);
                }
                if response.secondary_clicked() {
                    context_index = Some(index);
                }
                if response.drag_started() && !editing {
                    drag_index = Some(index);
                }
            }
        }

        self.apply_marquee(ui, &painter, &background, &row_rects, &t, secondary);
        self.finish_view_interactions(
            ui,
            secondary,
            rename,
            rename_commit,
            select_request,
            context_index,
            open_index,
            drag_index,
            &background,
        );
    }

    /// Unified post-frame interaction dispatch shared by Details, List, Tiles,
    /// and Icons views so every view has identical rename, drag-out, and
    /// background/item context menu behavior.
    #[allow(clippy::too_many_arguments)]
    fn finish_view_interactions(
        &mut self,
        ui: &egui::Ui,
        secondary: bool,
        mut rename: Option<RenameState>,
        rename_commit: Option<(usize, String)>,
        select_request: Option<(usize, egui::Modifiers)>,
        context_index: Option<usize>,
        open_index: Option<usize>,
        drag_index: Option<usize>,
        background: &egui::Response,
    ) {
        let pane_focused = self.focus_secondary == secondary;
        if pane_focused {
            if let Some((index, buf)) = rename_commit {
                if let Some(mut state) = rename.take() {
                    state.buf = buf;
                    state.row = index;
                    self.rename = Some(state);
                    self.finish_rename();
                }
            } else {
                self.rename = rename;
            }
        }

        let interacted = select_request.is_some()
            || context_index.is_some()
            || open_index.is_some()
            || drag_index.is_some()
            || background.clicked()
            || background.secondary_clicked();
        if interacted && self.focus_secondary != secondary {
            self.focus_secondary = secondary;
            self.last_selected_click = None;
            self.sync_watch();
        }

        if let Some((index, modifiers)) = select_request {
            let now_t = crate::app::now();
            let plain_click = !modifiers.ctrl && !modifiers.shift && !modifiers.alt;
            let already_sole = plain_click
                && self.tab_ref(secondary).selection.len() == 1
                && self.tab_ref(secondary).selection.contains(&index);
            let slow_second_click = already_sole
                && self
                    .last_selected_click
                    .is_some_and(|(prev, at)| prev == index && (now_t - at) >= 0.42 && (now_t - at) <= 2.2);
            if slow_second_click && open_index.is_none() {
                self.last_selected_click = None;
                self.command(crate::app::Command::Rename);
            } else {
                self.tab_mut(secondary).select_row(index, modifiers);
                self.last_selected_click = if plain_click { Some((index, now_t)) } else { None };
            }
        }
        if let Some(index) = context_index {
            self.last_selected_click = None;
            if !self.tab_ref(secondary).selection.contains(&index) {
                self.tab_mut(secondary).select_row(index, egui::Modifiers::default());
            }
            let extended = ui.input(|input| input.modifiers.shift);
            let anchor = ui.ctx().pointer_interact_pos().unwrap_or(Pos2::new(240.0, 180.0));
            self.open_context_menu(anchor, extended);
        } else if let Some(index) = open_index {
            self.last_selected_click = None;
            self.tab_mut(secondary).focus_row(index);
            self.open_selected();
        } else if let Some(index) = drag_index {
            self.last_selected_click = None;
            // Dragging an unselected row immediately selects and drags it (Explorer behavior).
            if !self.tab_ref(secondary).selection.contains(&index) {
                self.tab_mut(secondary).select_row(index, egui::Modifiers::default());
            }
            let paths = self.tab_ref(secondary).selected_paths();
            if !paths.is_empty()
                && let Err(err) = crate::shell::dnd::drag_out(&paths)
            {
                self.toast_error(format!("{err:#}"));
            }
        } else if background.secondary_clicked() {
            self.last_selected_click = None;
            self.tab_mut(secondary).clear_selection();
            let extended = ui.input(|input| input.modifiers.shift);
            let anchor = ui.ctx().pointer_interact_pos().unwrap_or(Pos2::new(240.0, 180.0));
            self.open_context_menu(anchor, extended);
        } else if background.clicked() {
            self.last_selected_click = None;
            self.tab_mut(secondary).clear_selection();
        }
    }

    // ---- marquee ---------------------------------------------------------

    /// Rubber-band selection: the drag starts on the view background, so pressing
    /// a row still initiates drag and drop.
    fn apply_marquee(
        &mut self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        background: &egui::Response,
        rows: &[(usize, Rect)],
        t: &theme::Theme,
        secondary: bool,
    ) {
        if background.drag_started() {
            if self.focus_secondary != secondary {
                self.focus_secondary = secondary;
                self.sync_watch();
            }
            self.marquee = ui.input(|input| input.pointer.press_origin());
        }
        if !background.dragged() {
            if background.drag_stopped() {
                self.marquee = None;
            }
            return;
        }
        let Some(start) = self.marquee else { return };
        let current = ui.input(|input| input.pointer.interact_pos()).unwrap_or(start);
        let band = Rect::from_two_pos(start, current);
        painter.rect_filled(band, CornerRadius::ZERO, t.accent_soft);
        painter.rect_stroke(band, CornerRadius::ZERO, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
        let hit: Vec<usize> = rows.iter().filter(|(_, rect)| rect.intersects(band)).map(|(index, _)| *index).collect();
        self.tab_mut(secondary).set_selection(hit);
    }

    // ---- scrolling -------------------------------------------------------

    fn scroll_offset(
        &mut self,
        ui: &mut egui::Ui,
        area: Rect,
        content_h: f32,
        target_y: Option<f32>,
        secondary: bool,
    ) -> f32 {
        let t = self.theme.clone();
        let ctx = ui.ctx().clone();
        let pane_tag = u8::from(secondary);
        let generation = self.tab_ref(secondary).generation;
        let scroll_spring_id = egui::Id::unique(("list-scroll-spring", pane_tag, generation));
        let mut immediate_scroll = false;
        let max_scroll = (content_h - area.height()).max(0.0);
        let mut target = self.tab_ref(secondary).scroll_offset.clamp(0.0, max_scroll);
        if ui.rect_contains_pointer(area) {
            let zooming = matches!(self.tab_ref(secondary).view, ViewMode::Tiles | ViewMode::Icons);
            let ctrl = ui.input(|input| input.modifiers.ctrl) && zooming;
            let delta = ui.input(|input| input.smooth_scroll_delta.y);
            if !ctrl && delta != 0.0 && max_scroll > 0.0 {
                target = (target - delta).clamp(0.0, max_scroll);
            }
        }
        if let Some(target_y) = target_y {
            target = (target_y - area.height() * 0.4).clamp(0.0, max_scroll);
            self.tab_mut(secondary).scroll_to = None;
        }
        self.tab_mut(secondary).scroll_offset = target;

        let mut offset = crate::ui::spring::animate(&ctx, scroll_spring_id, target, target, crate::ui::spring::SCROLL)
            .clamp(0.0, max_scroll);

        if max_scroll > 0.0 {
            let bar_w = 8.0;
            let track = Rect::from_min_max(
                Pos2::new(area.right() - bar_w - 2.0, area.top() + 2.0),
                Pos2::new(area.right() - 2.0, area.bottom() - 2.0),
            );
            let thumb_h = (track.height() * (area.height() / content_h)).clamp(28.0, track.height());
            let travel = track.height() - thumb_h;
            let thumb_y = track.top() + if max_scroll > 0.0 { offset / max_scroll * travel } else { 0.0 };
            let thumb = Rect::from_min_size(Pos2::new(track.left(), thumb_y), Vec2::new(bar_w, thumb_h));
            let id = egui::Id::unique(("list-scrollbar", pane_tag));
            let response = ui.interact(track, id, Sense::click_and_drag());
            let hover = widgets::hover_t(&ctx, id, response.hovered() || response.dragged());
            // Keep a subtle resting thumb visible whenever content overflows,
            // thickening and brightening on hover/drag.
            let alpha = 0.35 + hover * 0.65;
            let inset = (1.0 - hover) * 1.5;
            let paintable = Rect::from_min_max(
                Pos2::new(thumb.left() + inset, thumb.top()),
                Pos2::new(thumb.right() - inset, thumb.bottom()),
            );
            ui.painter().rect_filled(paintable, CornerRadius::same(3), widgets::fade(t.scrollbar_hover, alpha));
            if response.dragged() && travel > 0.0 {
                target = (target + response.drag_delta().y / travel * max_scroll).clamp(0.0, max_scroll);
                self.tab_mut(secondary).scroll_offset = target;
                immediate_scroll = true;
            } else if response.clicked()
                && travel > 0.0
                && let Some(pos) = response.interact_pointer_pos()
                && !thumb.contains(pos)
            {
                let ratio = ((pos.y - track.top() - thumb_h * 0.5) / travel).clamp(0.0, 1.0);
                target = ratio * max_scroll;
                self.tab_mut(secondary).scroll_offset = target;
            }
        }
        if immediate_scroll || max_scroll <= 0.0 {
            offset = target;
            crate::ui::spring::set_immediate(&ctx, scroll_spring_id, target);
        }
        offset
    }
}
