//! Virtualized file views: details, multi-column list, horizontal tiles and icons grids.
//!
//! Only visible rows/cards are visited per frame; selection is held as entry indices so
//! every view and every command agree on what a row means.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::{LumenApp, RenameState};
use crate::core::tabs::{DisplayRow, GROUP_H, ROW_H};
use crate::core::{Entry, SortKey, ViewMode, format_size, format_time};
use crate::theme::{self};
use crate::ui::{icons, widgets};

const HEADER_H: f32 = 32.0;
const LIST_ROW_H_NORMAL: f32 = 26.0;
const LIST_ROW_H_COMPACT: f32 = 22.0;
const LIST_COL_W: f32 = 232.0;
const HSCROLL_H: f32 = 10.0;
const SORT_ARROW_PAD: f32 = 22.0;
const SORT_ARROW_OFFSET: f32 = 12.0;

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
#[allow(clippy::too_many_arguments)]
fn paint_inline_rename(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    t: &theme::Theme,
    editor: &mut RenameState,
    inner: Rect,
    font_size: f32,
    is_folder: bool,
    pane_tag: u8,
) -> Option<String> {
    let ctx = ui.ctx().clone();
    let edit_id = egui::Id::unique(("inline-rename", pane_tag, editor.row));
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

fn tile_metadata_text(entry: &Entry, this_pc: bool, searching: bool, search_root: Option<&Path>) -> String {
    if this_pc {
        if entry.size > 0 {
            return format!("{} 可用，共 {}", format_size(entry.free_space), format_size(entry.size));
        }
        return entry.kind.clone();
    }
    if searching {
        let loc = search_folder_label(entry, search_root);
        if entry.is_folder {
            return loc;
        }
        return format!("{} · {}", format_size(entry.size), loc);
    }
    if entry.is_folder {
        let modified = format_time(entry.modified);
        if modified.is_empty() {
            return entry.kind.clone();
        }
        return format!("{} · {}", entry.kind, modified);
    }
    let size = format_size(entry.size);
    if entry.kind.is_empty() { size } else { format!("{} · {size}", entry.kind) }
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

/// Paints an Explorer item selection checkbox using WCAG-contrasted `t.text_on_accent`.
#[allow(clippy::too_many_arguments)]
fn paint_item_checkbox(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    box_rect: Rect,
    id: egui::Id,
    selected: bool,
    t: &theme::Theme,
) -> bool {
    let box_resp = ui.interact(box_rect.expand(3.0), id, Sense::click());
    if selected {
        painter.rect_filled(box_rect, CornerRadius::same(3), t.accent);
        widgets::icon_at(painter, box_rect.center(), icons::CHECK, 9.0, t.text_on_accent);
    } else {
        painter.rect_filled(box_rect, CornerRadius::same(3), widgets::fade(t.surface_raised, 0.85));
        painter.rect_stroke(
            box_rect,
            CornerRadius::same(3),
            Stroke::new(1.2, t.border_strong),
            egui::StrokeKind::Inside,
        );
    }
    box_resp.clicked()
}

// ---- pure layout & marquee helpers -----------------------------------------

/// Computes the 4 details column widths `[name, col1, col2, col3]` and their total
/// width. Proportions shrink cleanly on narrow panes (split view / details pane open)
/// down to hard readable floors, beyond which horizontal scroll takes over cleanly.
fn compute_details_columns(avail_w: f32, preferred: [f32; 3]) -> ([f32; 4], f32) {
    const MIN_NAME_COMFORT: f32 = 120.0;
    const MIN_TAIL_COMFORT: [f32; 3] = [80.0, 72.0, 60.0];
    const FLOOR_COLS: [f32; 4] = [88.0, 64.0, 56.0, 48.0];
    const FLOOR_SUM: f32 = 88.0 + 64.0 + 56.0 + 48.0;

    let avail = avail_w.max(0.0);
    let pref = [preferred[0].max(48.0), preferred[1].max(48.0), preferred[2].max(48.0)];
    let pref_tail_sum = pref[0] + pref[1] + pref[2];

    if avail >= MIN_NAME_COMFORT + pref_tail_sum {
        let name_w = avail - pref_tail_sum;
        return ([name_w, pref[0], pref[1], pref[2]], avail);
    }

    let eff_min_tail =
        [MIN_TAIL_COMFORT[0].min(pref[0]), MIN_TAIL_COMFORT[1].min(pref[1]), MIN_TAIL_COMFORT[2].min(pref[2])];
    let min_tail_sum = eff_min_tail[0] + eff_min_tail[1] + eff_min_tail[2];

    if avail >= MIN_NAME_COMFORT + min_tail_sum {
        let tail_budget = avail - MIN_NAME_COMFORT;
        let slack = pref_tail_sum - min_tail_sum;
        let t = if slack > 0.001 { ((tail_budget - min_tail_sum) / slack).clamp(0.0, 1.0) } else { 0.0 };
        let c1 = eff_min_tail[0] + (pref[0] - eff_min_tail[0]) * t;
        let c2 = eff_min_tail[1] + (pref[1] - eff_min_tail[1]) * t;
        let c3 = (tail_budget - c1 - c2).max(eff_min_tail[2]);
        let widths = [MIN_NAME_COMFORT, c1, c2, c3];
        let total = widths.iter().sum();
        return (widths, total);
    }

    if avail >= FLOOR_SUM {
        let comfort = [MIN_NAME_COMFORT, eff_min_tail[0], eff_min_tail[1], eff_min_tail[2]];
        let comfort_sum: f32 = comfort.iter().sum();
        let slack = comfort_sum - FLOOR_SUM;
        let t = if slack > 0.001 { ((avail - FLOOR_SUM) / slack).clamp(0.0, 1.0) } else { 0.0 };
        let w0 = FLOOR_COLS[0] + (comfort[0] - FLOOR_COLS[0]).max(0.0) * t;
        let w1 = FLOOR_COLS[1] + (comfort[1] - FLOOR_COLS[1]).max(0.0) * t;
        let w2 = FLOOR_COLS[2] + (comfort[2] - FLOOR_COLS[2]).max(0.0) * t;
        let w3 = (avail - w0 - w1 - w2).max(FLOOR_COLS[3]);
        let widths = [w0, w1, w2, w3];
        let total = widths.iter().sum();
        return (widths, total);
    }

    (FLOOR_COLS, FLOOR_SUM)
}

/// Column descriptors `(label, sort_key, width, right_align)` for Details view.
fn details_column_specs(this_pc: bool, searching: bool, widths: [f32; 4]) -> [(&'static str, SortKey, f32, bool); 4] {
    if this_pc {
        [
            ("名称", SortKey::Name, widths[0], false),
            ("总大小", SortKey::Size, widths[1], true),
            ("可用空间", SortKey::FreeSpace, widths[2], true),
            ("类型", SortKey::Type, widths[3], false),
        ]
    } else if searching {
        [
            ("名称", SortKey::Name, widths[0], false),
            ("修改日期", SortKey::DateModified, widths[1], false),
            ("所在文件夹", SortKey::Location, widths[2], false),
            ("大小", SortKey::Size, widths[3], true),
        ]
    } else {
        [
            ("名称", SortKey::Name, widths[0], false),
            ("修改日期", SortKey::DateModified, widths[1], false),
            ("类型", SortKey::Type, widths[2], false),
            ("大小", SortKey::Size, widths[3], true),
        ]
    }
}

/// Computes the label rectangle for a Details column header cell.
/// Reserves `22px` right padding when `active` so the sort arrow at
/// `cell.right() - 12.0` never overlaps right-aligned (or long left-aligned) labels,
/// and aligns `"名称"` with data row checkbox + icon offset.
fn details_header_label_rect(cell: Rect, index: usize, right_align: bool, active: bool, check_offset: f32) -> Rect {
    let right_pad = if active { SORT_ARROW_PAD } else { 4.0 };
    let left_pad = if right_align {
        4.0
    } else if index == 0 {
        32.0 + check_offset
    } else {
        7.0
    };
    let left = cell.left() + left_pad;
    let right = (cell.right() - right_pad).max(left);
    Rect::from_min_max(Pos2::new(left, cell.top()), Pos2::new(right, cell.bottom()))
}

fn details_sort_arrow_x(cell: Rect) -> f32 {
    cell.right() - SORT_ARROW_OFFSET
}

#[derive(Clone, Debug, Default)]
struct MarqueeDrag {
    origin_logical: Pos2,
    base_selection: BTreeSet<usize>,
}

/// Combines rubber-band `hits` with the drag-start `base` selection according to
/// Explorer modifiers (`Ctrl` toggles / XORs, `Shift` unions, plain replaces).
fn combine_marquee_selection(base: &BTreeSet<usize>, hits: &[usize], ctrl: bool, shift: bool) -> Vec<usize> {
    if ctrl {
        let mut out = base.clone();
        for &idx in hits {
            if !out.remove(&idx) {
                out.insert(idx);
            }
        }
        out.into_iter().collect()
    } else if shift {
        let mut out = base.clone();
        out.extend(hits.iter().copied());
        out.into_iter().collect()
    } else {
        hits.to_vec()
    }
}

/// Computes all entry indices intersecting `band_logical` in Details view,
/// including rows currently scrolled outside the viewport.
fn details_marquee_hits(
    display: &[DisplayRow],
    offsets: &[f32],
    scale: f32,
    row_h: f32,
    row_width: f32,
    band_logical: Rect,
) -> Vec<usize> {
    if display.is_empty() || row_width <= 0.0 || band_logical.right() < 4.0 || band_logical.left() > 4.0 + row_width {
        return Vec::new();
    }
    let start = offsets.partition_point(|&v| v * scale <= band_logical.top()).saturating_sub(1).min(display.len());
    let mut hits = Vec::new();
    for (pos, row) in display.iter().enumerate().skip(start) {
        let top = offsets.get(pos).copied().unwrap_or(0.0) * scale;
        if top > band_logical.bottom() {
            break;
        }
        if let DisplayRow::Entry(idx) = *row {
            let rect = Rect::from_min_size(Pos2::new(4.0, top), Vec2::new(row_width, row_h));
            if rect.intersects(band_logical) {
                hits.push(idx);
            }
        }
    }
    hits
}

#[derive(Clone, Debug)]
struct GroupHeaderSpec {
    label: String,
    count: usize,
    collapsed: bool,
}

#[derive(Clone, Copy, Debug)]
struct GridMetrics {
    cell_w: f32,
    cell_h: f32,
    cell_gap_x: f32,
    icon_px: u32,
    columns: usize,
}

fn compute_grid_metrics(view: ViewMode, area_width: f32, zoom: f32) -> GridMetrics {
    let zoom = zoom.clamp(0.7, 2.4);
    let (cell_w, cell_h, cell_gap_x, icon_px) = if view == ViewMode::Tiles {
        (
            (248.0 * zoom).clamp(180.0, 420.0),
            (66.0 * zoom).clamp(54.0, 118.0),
            6.0,
            (42.0 * zoom).round().clamp(32.0, 72.0) as u32,
        )
    } else {
        (
            (128.0 * zoom).clamp(96.0, 260.0),
            (124.0 * zoom).clamp(96.0, 260.0),
            4.0,
            (56.0 * zoom).round().clamp(36.0, 128.0) as u32,
        )
    };
    let avail = (area_width - 16.0).max(cell_w);
    let columns = (avail / cell_w).floor().max(1.0) as usize;
    GridMetrics { cell_w, cell_h, cell_gap_x, icon_px, columns }
}

#[derive(Clone, Debug)]
struct GridSection {
    header: Option<GroupHeaderSpec>,
    entries_start: usize,
    entry_count: usize,
    top_y: f32,
    entries_top_y: f32,
    bottom_y: f32,
}

impl GridSection {
    fn cell_logical_rect(&self, local_idx: usize, m: GridMetrics) -> Rect {
        let row = local_idx / m.columns;
        let col = local_idx % m.columns;
        Rect::from_min_size(
            Pos2::new(4.0 + col as f32 * m.cell_w, self.entries_top_y + row as f32 * m.cell_h),
            Vec2::new((m.cell_w - m.cell_gap_x).max(16.0), (m.cell_h - 4.0).max(16.0)),
        )
    }
}

fn build_grid_sections(
    display: &[DisplayRow],
    columns: usize,
    cell_h: f32,
    group_h: f32,
    has_groups: bool,
) -> (Vec<GridSection>, f32) {
    let cols = columns.max(1);
    if !has_groups {
        let count = display.len();
        if count == 0 {
            return (Vec::new(), 0.0);
        }
        let rows = count.div_ceil(cols);
        let bottom = rows as f32 * cell_h;
        return (
            vec![GridSection {
                header: None,
                entries_start: 0,
                entry_count: count,
                top_y: 0.0,
                entries_top_y: 0.0,
                bottom_y: bottom,
            }],
            bottom,
        );
    }

    let mut sections: Vec<GridSection> = Vec::new();
    let mut y = 0.0f32;
    let mut i = 0usize;
    while i < display.len() {
        match &display[i] {
            DisplayRow::Group { label, count, collapsed } => {
                if !sections.is_empty() {
                    y += 4.0;
                }
                let top_y = y;
                let entries_top_y = top_y + group_h + 4.0;
                i += 1;
                let entries_start = i;
                while i < display.len() && matches!(display[i], DisplayRow::Entry(_)) {
                    i += 1;
                }
                let entry_count = i - entries_start;
                let rows = entry_count.div_ceil(cols);
                let bottom_y = if entry_count > 0 { entries_top_y + rows as f32 * cell_h } else { top_y + group_h };
                sections.push(GridSection {
                    header: Some(GroupHeaderSpec { label: label.clone(), count: *count, collapsed: *collapsed }),
                    entries_start,
                    entry_count,
                    top_y,
                    entries_top_y,
                    bottom_y,
                });
                y = bottom_y;
            }
            DisplayRow::Entry(_) => {
                let entries_start = i;
                while i < display.len() && matches!(display[i], DisplayRow::Entry(_)) {
                    i += 1;
                }
                let entry_count = i - entries_start;
                let rows = entry_count.div_ceil(cols);
                let bottom_y = y + rows as f32 * cell_h;
                sections.push(GridSection {
                    header: None,
                    entries_start,
                    entry_count,
                    top_y: y,
                    entries_top_y: y,
                    bottom_y,
                });
                y = bottom_y;
            }
        }
    }
    (sections, y)
}

fn grid_entry_target_y(
    display: &[DisplayRow],
    sections: &[GridSection],
    m: GridMetrics,
    entry_idx: usize,
) -> Option<f32> {
    for section in sections {
        let slice = &display[section.entries_start..section.entries_start + section.entry_count];
        if let Some(local) = slice.iter().position(|row| matches!(row, DisplayRow::Entry(e) if *e == entry_idx)) {
            return Some(section.entries_top_y + (local / m.columns) as f32 * m.cell_h);
        }
    }
    None
}

fn grid_marquee_hits(
    display: &[DisplayRow],
    sections: &[GridSection],
    m: GridMetrics,
    band_logical: Rect,
) -> Vec<usize> {
    if band_logical.right() < 4.0 {
        return Vec::new();
    }
    let col_lo = ((band_logical.left() - 4.0) / m.cell_w).floor().max(0.0) as usize;
    let col_hi = (((band_logical.right() - 4.0) / m.cell_w).floor().max(0.0) as usize).min(m.columns.saturating_sub(1));
    if col_lo > col_hi {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for section in sections {
        if section.entry_count == 0
            || section.bottom_y < band_logical.top()
            || section.entries_top_y > band_logical.bottom()
        {
            continue;
        }
        let total_rows = section.entry_count.div_ceil(m.columns);
        let row_lo = ((band_logical.top() - section.entries_top_y) / m.cell_h).floor().max(0.0) as usize;
        let row_hi = (((band_logical.bottom() - section.entries_top_y) / m.cell_h).floor().max(0.0) as usize)
            .min(total_rows.saturating_sub(1));
        for row in row_lo..=row_hi {
            for col in col_lo..=col_hi {
                let local = row * m.columns + col;
                if local >= section.entry_count {
                    continue;
                }
                if section.cell_logical_rect(local, m).intersects(band_logical)
                    && let DisplayRow::Entry(idx) = display[section.entries_start + local]
                {
                    hits.push(idx);
                }
            }
        }
    }
    hits
}

#[derive(Clone, Debug)]
struct ListGroupSection {
    header: Option<GroupHeaderSpec>,
    entries_start: usize,
    entry_count: usize,
    rows_per_col: usize,
    top_y: f32,
    entries_top_y: f32,
    bottom_y: f32,
}

impl ListGroupSection {
    fn cell_logical_rect(&self, local_idx: usize, col_w: f32, row_h: f32) -> Rect {
        let rows = self.rows_per_col.max(1);
        let col = local_idx / rows;
        let row = local_idx % rows;
        Rect::from_min_size(
            Pos2::new(4.0 + col as f32 * col_w, self.entries_top_y + row as f32 * row_h),
            Vec2::new((col_w - 6.0).max(40.0), row_h),
        )
    }
}

#[derive(Clone, Debug)]
enum ListLayout {
    /// True Windows Explorer multi-column horizontal-flowing list (ungrouped).
    HorizontalColumns { col_w: f32, row_h: f32, rows_per_col: usize, num_cols: usize, content_w: f32, has_hbar: bool },
    /// Grouped multi-column list: full-width `GROUP_H` headers + top-to-bottom column blocks.
    GroupedColumns {
        col_w: f32,
        row_h: f32,
        group_h: f32,
        cols: usize,
        sections: Vec<ListGroupSection>,
        content_h: f32,
    },
}

fn build_list_layout(display: &[DisplayRow], area: Rect, compact_mode: bool, has_groups: bool) -> ListLayout {
    let row_h = if compact_mode { LIST_ROW_H_COMPACT } else { LIST_ROW_H_NORMAL };
    let group_h = if compact_mode { GROUP_H * (26.0 / ROW_H) } else { GROUP_H };
    if !has_groups {
        let avail_w = (area.width() - 8.0).max(120.0);
        let col_w = LIST_COL_W.min(avail_w);
        let total = display.len();
        let inner_h_no_bar = (area.height() - 6.0).max(row_h);
        let rows_no_bar = (inner_h_no_bar / row_h).floor().max(1.0) as usize;
        let cols_no_bar = total.div_ceil(rows_no_bar);
        let content_w_no_bar = cols_no_bar as f32 * col_w;
        let has_hbar = content_w_no_bar > avail_w;
        let (rows_per_col, num_cols, content_w) = if has_hbar {
            let inner_h = (area.height() - 6.0 - HSCROLL_H).max(row_h);
            let rows = (inner_h / row_h).floor().max(1.0) as usize;
            let cols = total.div_ceil(rows);
            (rows, cols, cols as f32 * col_w)
        } else {
            (rows_no_bar, cols_no_bar, content_w_no_bar)
        };
        return ListLayout::HorizontalColumns { col_w, row_h, rows_per_col, num_cols, content_w, has_hbar };
    }

    let avail_w = (area.width() - 16.0).max(120.0);
    let cols = (avail_w / LIST_COL_W).floor().max(1.0) as usize;
    let col_w = avail_w / cols as f32;
    let mut sections: Vec<ListGroupSection> = Vec::new();
    let mut y = 0.0f32;
    let mut i = 0usize;
    while i < display.len() {
        match &display[i] {
            DisplayRow::Group { label, count, collapsed } => {
                if !sections.is_empty() {
                    y += 4.0;
                }
                let top_y = y;
                let entries_top_y = top_y + group_h + 2.0;
                i += 1;
                let entries_start = i;
                while i < display.len() && matches!(display[i], DisplayRow::Entry(_)) {
                    i += 1;
                }
                let entry_count = i - entries_start;
                let rows_per_col = entry_count.div_ceil(cols);
                let bottom_y =
                    if entry_count > 0 { entries_top_y + rows_per_col as f32 * row_h } else { top_y + group_h };
                sections.push(ListGroupSection {
                    header: Some(GroupHeaderSpec { label: label.clone(), count: *count, collapsed: *collapsed }),
                    entries_start,
                    entry_count,
                    rows_per_col,
                    top_y,
                    entries_top_y,
                    bottom_y,
                });
                y = bottom_y;
            }
            DisplayRow::Entry(_) => {
                let entries_start = i;
                while i < display.len() && matches!(display[i], DisplayRow::Entry(_)) {
                    i += 1;
                }
                let entry_count = i - entries_start;
                let rows_per_col = entry_count.div_ceil(cols);
                let bottom_y = y + rows_per_col as f32 * row_h;
                sections.push(ListGroupSection {
                    header: None,
                    entries_start,
                    entry_count,
                    rows_per_col,
                    top_y: y,
                    entries_top_y: y,
                    bottom_y,
                });
                y = bottom_y;
            }
        }
    }
    ListLayout::GroupedColumns { col_w, row_h, group_h, cols, sections, content_h: y }
}

fn list_marquee_hits(display: &[DisplayRow], layout: &ListLayout, band_logical: Rect) -> Vec<usize> {
    if band_logical.right() < 4.0 {
        return Vec::new();
    }
    let mut hits = Vec::new();
    match layout {
        ListLayout::HorizontalColumns { col_w, row_h, rows_per_col, num_cols, .. } => {
            if *num_cols == 0 || *rows_per_col == 0 {
                return hits;
            }
            let col_lo = ((band_logical.left() - 4.0) / col_w).floor().max(0.0) as usize;
            let col_hi =
                (((band_logical.right() - 4.0) / col_w).floor().max(0.0) as usize).min(num_cols.saturating_sub(1));
            let row_lo = (band_logical.top() / row_h).floor().max(0.0) as usize;
            let row_hi =
                ((band_logical.bottom() / row_h).floor().max(0.0) as usize).min(rows_per_col.saturating_sub(1));
            for col in col_lo..=col_hi {
                for row in row_lo..=row_hi {
                    let pos = col * rows_per_col + row;
                    let Some(DisplayRow::Entry(idx)) = display.get(pos) else { continue };
                    let rect = Rect::from_min_size(
                        Pos2::new(4.0 + col as f32 * col_w, row as f32 * row_h),
                        Vec2::new((col_w - 6.0).max(40.0), *row_h),
                    );
                    if rect.intersects(band_logical) {
                        hits.push(*idx);
                    }
                }
            }
        }
        ListLayout::GroupedColumns { col_w, row_h, cols, sections, .. } => {
            let col_lo = ((band_logical.left() - 4.0) / col_w).floor().max(0.0) as usize;
            let col_hi = (((band_logical.right() - 4.0) / col_w).floor().max(0.0) as usize).min(cols.saturating_sub(1));
            if col_lo > col_hi {
                return hits;
            }
            for section in sections {
                if section.entry_count == 0
                    || section.bottom_y < band_logical.top()
                    || section.entries_top_y > band_logical.bottom()
                {
                    continue;
                }
                let rows = section.rows_per_col.max(1);
                let row_lo = ((band_logical.top() - section.entries_top_y) / row_h).floor().max(0.0) as usize;
                let row_hi = (((band_logical.bottom() - section.entries_top_y) / row_h).floor().max(0.0) as usize)
                    .min(rows.saturating_sub(1));
                for col in col_lo..=col_hi {
                    for row in row_lo..=row_hi {
                        let local = col * rows + row;
                        if local >= section.entry_count {
                            continue;
                        }
                        if section.cell_logical_rect(local, *col_w, *row_h).intersects(band_logical)
                            && let DisplayRow::Entry(idx) = display[section.entries_start + local]
                        {
                            hits.push(idx);
                        }
                    }
                }
            }
        }
    }
    hits
}

/// Resets per-tab pane springs whenever `tab.nav_generation` advances (folder change,
/// back/forward, search transition), while preserving springs across `F5`/watcher reloads.
fn sync_pane_nav_generation(ctx: &egui::Context, pane_tag: u8, tab_uid: u64, nav_gen: u64) {
    let nav_id = egui::Id::unique(("pane-nav-gen", pane_tag, tab_uid));
    let prev = ctx.data(|d| d.get_temp::<u64>(nav_id));
    if prev != Some(nav_gen) {
        if prev.is_some() {
            for name in ["pane-enter", "list-scroll-spring", "list-hscroll-spring", "details-sel-bar-y"] {
                crate::ui::spring::generation_id(ctx, egui::Id::unique((name, pane_tag, tab_uid)), nav_gen);
            }
            ctx.data_mut(|d| {
                d.remove_temp::<f32>(egui::Id::unique(("list-hscroll-target", pane_tag, tab_uid)));
                d.remove_temp::<f32>(egui::Id::unique(("details-sel-bar-last", pane_tag, tab_uid)));
                d.remove_temp::<MarqueeDrag>(egui::Id::unique(("marquee-drag", pane_tag, tab_uid)));
            });
        }
        ctx.data_mut(|d| d.insert_temp(nav_id, nav_gen));
    }
}

/// Smooth Fluent-style rotating circular arc painted in the center of an empty loading view.
fn paint_loading_spinner(ui: &egui::Ui, painter: &egui::Painter, center: Pos2, radius: f32, t: &theme::Theme) {
    let time = ui.input(|i| i.time) as f32;
    painter.circle_stroke(center, radius, Stroke::new(2.0, widgets::fade(t.border_strong, 0.45)));
    let start_angle = time * 4.2;
    let sweep = std::f32::consts::PI * 0.65;
    let segments = 18usize;
    let points: Vec<Pos2> = (0..=segments)
        .map(|i| {
            let a = start_angle + sweep * (i as f32 / segments as f32);
            Pos2::new(center.x + a.cos() * radius, center.y + a.sin() * radius)
        })
        .collect();
    painter.add(egui::Shape::line(points, Stroke::new(2.2, t.accent)));
}

impl LumenApp {
    /// Returns keyboard focus to the file list by clearing `sidebar_focused` and
    /// surrendering any focused non-rename widget in egui memory (BUG-37).
    fn focus_filelist(&mut self, ui: &egui::Ui) {
        self.sidebar_focused = false;
        if self.rename.is_none() {
            ui.ctx().memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
    }

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
            if hdr_resp.clicked() {
                self.focus_filelist(ui);
                self.set_focus_secondary(secondary);
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
            let bg = ui.interact(body, egui::Id::unique(("err-folder-bg", u8::from(secondary))), Sense::click());
            if bg.clicked() {
                self.focus_filelist(ui);
                self.set_focus_secondary(secondary);
            }
            self.paint_message(ui, body, icons::INFO, &error, true);
            return;
        }
        if entries.is_empty() {
            let bg = ui.interact(body, egui::Id::unique(("empty-folder-bg", u8::from(secondary))), Sense::click());
            if loading {
                // UI-08: Paint BOTH the top indeterminate loading bar and a subtle centered
                // loading indicator when entering a slow/large folder so it never looks frozen.
                self.paint_loading_bar(ui, body, &t);
                let center = body.center();
                paint_loading_spinner(ui, &painter, center - Vec2::new(0.0, 18.0), 13.0, &t);
                let galley = widgets::galley(
                    ui,
                    "正在加载…",
                    theme::font(12.5),
                    t.text_tertiary,
                    (body.width() - 40.0).max(0.0),
                );
                widgets::galley_at(
                    &painter,
                    Pos2::new(center.x - galley.size().x * 0.5, center.y + 8.0),
                    galley,
                    t.text_tertiary,
                );
            } else {
                let text = if searching { "没有匹配的项目" } else { "此文件夹为空" };
                self.paint_message(ui, body, icons::FOLDER, text, false);
            }
            if bg.clicked() {
                self.focus_filelist(ui);
                self.set_focus_secondary(secondary);
            }
            if !loading && !searching && bg.secondary_clicked() {
                self.focus_filelist(ui);
                self.set_focus_secondary(secondary);
                self.tab_mut(secondary).clear_selection();
                let extended = ui.input(|i| i.modifiers.shift);
                let anchor = ui.ctx().pointer_interact_pos().unwrap_or(body.center());
                self.open_context_menu(anchor, extended);
            }
            return;
        }

        match view {
            ViewMode::Details => self.paint_details_view(ui, body, entries, secondary),
            ViewMode::List => self.paint_list_view(ui, body, entries, secondary),
            ViewMode::Tiles | ViewMode::Icons => self.paint_grid_view(ui, body, entries, view, secondary),
        }
        if loading {
            self.paint_loading_bar(ui, body, &t);
        }
    }

    /// Thin indeterminate bar: the listing is on its way, keep the old rows visible.
    fn paint_loading_bar(&self, ui: &egui::Ui, area: Rect, t: &theme::Theme) {
        let time = ui.input(|input| input.time) as f32;
        let width = (area.width() * 0.22).max(120.0).min(area.width());
        let travel = (area.width() - width).max(0.0);
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
        let galley = widgets::galley(ui, text, theme::font(12.5), color, (rect.width() - 40.0).max(0.0));
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
        tab_uid: u64,
    ) -> bool {
        let t = &self.theme;
        let id = egui::Id::unique(("grp-hdr", pane_tag, tab_uid, label));
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
                Pos2::new((rect.right() - 80.0).max(rect.left() + 28.0), rect.bottom()),
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
                Pos2::new((rect.right() - 76.0).max(rect.left() + 28.0), rect.top()),
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
        let (tab_uid, nav_gen, widths, sort, this_pc, searching) = {
            let tab = self.tab_ref(secondary);
            (tab.uid, tab.nav_generation, tab.col_widths, tab.sort, tab.is_this_pc(), tab.is_search())
        };
        sync_pane_nav_generation(&ctx, pane_tag, tab_uid, nav_gen);

        let header = Rect::from_min_size(area.min, Vec2::new(area.width(), HEADER_H));
        // Align header and data rows to the exact same origin (`area.left() + 4.0`) and
        // keep a 12px right gutter for the vertical scrollbar (4 + 12 = 16px).
        let avail_w = (area.width() - 16.0).max(0.0);
        let ([name_w, date_w, type_w, size_w], total_col_w) = compute_details_columns(avail_w, widths);
        let has_hbar = total_col_w > avail_w + 0.5;
        let rows_bottom = if has_hbar { (area.bottom() - HSCROLL_H).max(header.bottom()) } else { area.bottom() };
        let rows_area =
            Rect::from_min_max(Pos2::new(area.left(), header.bottom()), Pos2::new(area.right(), rows_bottom));
        let hbar_area = Rect::from_min_max(
            Pos2::new(area.left() + 4.0, rows_bottom),
            Pos2::new(area.right() - 12.0, area.bottom()),
        );
        let h_offset = if has_hbar {
            self.h_scroll_offset(ui, area, hbar_area, total_col_w, avail_w, None, false, secondary)
        } else {
            self.reset_h_scroll(&ctx, pane_tag, tab_uid);
            0.0
        };

        let show_checks = self.settings.show_checkboxes;
        let check_offset = if show_checks { 22.0 } else { 0.0 };
        let columns = details_column_specs(this_pc, searching, [name_w, date_w, type_w, size_w]);

        // Registered before the rows so a press on empty space starts a marquee
        // and a press on a row still reaches the row.
        let background = ui.interact(rows_area, egui::Id::unique(("details-bg", pane_tag)), Sense::click_and_drag());

        let header_clip = header.intersect(ui.clip_rect());
        let header_painter = painter.with_clip_rect(header_clip);
        let mut x = area.left() + 4.0 - h_offset;
        let mut new_sort: Option<SortKey> = None;
        let mut header_interacted = false;
        let mut boundaries: Vec<(f32, usize)> = Vec::new();
        for (index, (label, key, width, right_align)) in columns.iter().enumerate() {
            let cell = Rect::from_min_size(Pos2::new(x, header.top()), Vec2::new(*width, header.height()));
            let vis_cell = cell.intersect(header_clip);
            if vis_cell.width() > 2.0 {
                let id = egui::Id::unique(("head", pane_tag, index));
                let response = ui.interact(vis_cell, id, Sense::click());
                let hover = widgets::hover_t(&ctx, id, response.hovered());
                if hover > 0.0 {
                    header_painter.rect_filled(
                        cell.shrink2(Vec2::new(1.0, 6.0)),
                        CornerRadius::same(3),
                        widgets::fade(t.hover, hover),
                    );
                }
                let active = sort.key == *key;
                let text_color = if active { t.text } else { t.text_tertiary };
                let label_rect = details_header_label_rect(cell, index, *right_align, active, check_offset);
                if *right_align {
                    widgets::text_right(ui, &header_painter, label_rect, label, theme::font(12.0), text_color);
                } else {
                    widgets::text_left(ui, &header_painter, label_rect, label, theme::font(12.0), text_color, 0.0);
                }
                if active {
                    widgets::icon_at(
                        &header_painter,
                        Pos2::new(details_sort_arrow_x(cell), cell.center().y),
                        if sort.ascending { icons::CHEVRON_UP } else { icons::CHEVRON_DOWN },
                        8.0,
                        t.text_tertiary,
                    );
                }
                if response.clicked() {
                    new_sort = Some(*key);
                    header_interacted = true;
                }
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
            if boundary < header_clip.left() - 3.0 || boundary > header_clip.right() + 3.0 {
                continue;
            }
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
                header_interacted = true;
            } else if response.dragged() {
                let delta = response.drag_delta().x;
                let tab = self.tab_mut(secondary);
                match index {
                    0 => {
                        tab.col_widths[0] = (tab.col_widths[0] - delta).clamp(80.0, 360.0);
                    }
                    1 => {
                        let next_left = (tab.col_widths[0] + delta).clamp(80.0, 360.0);
                        let actual = next_left - tab.col_widths[0];
                        let next_right = (tab.col_widths[1] - actual).clamp(80.0, 360.0);
                        let applied = tab.col_widths[1] - next_right;
                        tab.col_widths[0] += applied;
                        tab.col_widths[1] = next_right;
                    }
                    2 => {
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
                header_interacted = true;
            }
        }
        if header_interacted {
            self.focus_filelist(ui);
            self.set_focus_secondary(secondary);
        }
        if resized && self.focus_secondary == secondary {
            self.record_folder_view();
        }
        if let Some(key) = new_sort {
            self.tab_mut(secondary).set_sort(key);
            if self.focus_secondary == secondary {
                self.reconcile_rename_row();
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
        let hide_ext = self.settings.hide_extensions;
        let search_root = self.search.root.clone();

        let display = self.tab_ref(secondary).display.clone();
        let offsets = self.tab_ref(secondary).row_offsets.clone();
        let total = display.len();
        let content_h = offsets.last().copied().unwrap_or(0.0) * scale;
        let row_width = total_col_w.max(avail_w);
        if pane_focused {
            self.page_rows = (rows_area.height() / row_h).max(1.0) as usize;
        }

        let target_y = self
            .tab_mut(secondary)
            .scroll_to
            .take()
            .map(|index| self.tab_ref(secondary).offset_for_entry(index, 0.0) * scale);
        let offset = self.scroll_offset(ui, rows_area, content_h, target_y, secondary);
        let drag_hover = crate::shell::dnd::drag_hover();
        let hover_target = drag_hover.as_ref().and_then(|h| h.target.as_deref());

        let mut rename = if pane_focused { self.rename.take() } else { None };
        let rename_was_initial_focus = rename.as_ref().is_some_and(|r| r.focus);
        let mut rename_painted = false;
        let mut rename_commit: Option<(usize, String)> = None;
        let mut open_index: Option<usize> = None;
        let mut context_index: Option<usize> = None;
        let mut drag_index: Option<usize> = None;
        let mut select_request: Option<(usize, egui::Modifiers)> = None;
        let mut toggle_group: Option<String> = None;

        // Crisp 120ms cubic ease-out micro-glide on real folder navigation (never on F5/watcher reloads).
        let enter_offset = (1.0
            - crate::ui::spring::ease_out_reveal(&ctx, egui::Id::unique(("pane-enter", pane_tag, tab_uid)), 0.12))
            * 5.0;
        let single_selected = self.tab_ref(secondary).selection.len() == 1;
        let mut single_sel_content_y: Option<f32> = None;
        let rows_clip = rows_area.intersect(ui.clip_rect());
        let rows_painter = painter.with_clip_rect(rows_clip);

        let unscaled_offset = offset / scale;
        let first =
            offsets.partition_point(|&value| value <= unscaled_offset).saturating_sub(1).min(total.saturating_sub(1));
        for position in first..total {
            let content_y = offsets[position] * scale;
            let row_top = rows_area.top() + content_y - offset + enter_offset;
            if row_top > rows_area.bottom() + group_h {
                break;
            }
            match &display[position] {
                DisplayRow::Group { label, count, collapsed } => {
                    let group =
                        Rect::from_min_size(Pos2::new(rows_area.left() + 4.0, row_top), Vec2::new(avail_w, group_h));
                    if !rows_area.intersects(group) {
                        continue;
                    }
                    if self.paint_group_header(ui, &rows_painter, group, label, *count, *collapsed, pane_tag, tab_uid) {
                        toggle_group = Some(label.clone());
                    }
                }
                DisplayRow::Entry(index) => {
                    let index = *index;
                    let Some(entry) = entries.get(index) else { continue };
                    let row = Rect::from_min_size(
                        Pos2::new(rows_area.left() + 4.0 - h_offset, row_top),
                        Vec2::new(row_width, row_h),
                    );
                    if !rows_area.intersects(row) {
                        continue;
                    }
                    let hit_row = row.intersect(rows_clip);
                    if entry.is_folder
                        && let Some(path) = &entry.path
                        && hit_row.width() > 0.0
                        && hit_row.height() > 0.0
                    {
                        self.drop_zones.push((hit_row, path.clone()));
                    }
                    let id = egui::Id::unique(("row", pane_tag, index));
                    let response = ui.interact(hit_row, id, Sense::click_and_drag());
                    let hover = widgets::hover_t(&ctx, id, response.hovered());
                    let selected = self.tab_ref(secondary).selection.contains(&index);
                    let drop_hi = entry.is_folder && is_drop_hovered(hover_target, entry.path.as_deref());
                    if drop_hi {
                        rows_painter.rect_filled(row, CornerRadius::same(theme::RADIUS_ROW), t.accent_soft);
                        rows_painter.rect_stroke(
                            row,
                            CornerRadius::same(theme::RADIUS_ROW),
                            Stroke::new(1.5, t.accent),
                            egui::StrokeKind::Inside,
                        );
                    } else if selected {
                        rows_painter.rect_filled(row, CornerRadius::same(theme::RADIUS_ROW), t.selected);
                        if hover > 0.0 {
                            rows_painter.rect_filled(
                                row,
                                CornerRadius::same(theme::RADIUS_ROW),
                                widgets::fade(t.selected_hover, hover),
                            );
                        }
                        if single_selected {
                            single_sel_content_y = Some(content_y + row_h * 0.5);
                        } else {
                            rows_painter.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(row.left() + 1.0, row.center().y - 8.0),
                                    Vec2::new(3.0, 16.0),
                                ),
                                CornerRadius::same(2),
                                t.selected_bar,
                            );
                        }
                    } else if hover > 0.0 {
                        rows_painter.rect_filled(
                            row,
                            CornerRadius::same(theme::RADIUS_ROW),
                            widgets::fade(t.hover, hover),
                        );
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

                    let mut checkbox_clicked = false;
                    if show_checks && (selected || hover > 0.0) {
                        let box_rect =
                            Rect::from_center_size(Pos2::new(row.left() + 14.0, row.center().y), Vec2::splat(14.0));
                        if rows_clip.intersects(box_rect) {
                            checkbox_clicked = paint_item_checkbox(
                                ui,
                                &rows_painter,
                                box_rect,
                                egui::Id::unique(("chk", pane_tag, index)),
                                selected,
                                &t,
                            );
                        }
                    }

                    let icon = self.shell_icon_entry(ui.ctx(), entry, 16, false);
                    let icon_rect = Rect::from_center_size(
                        Pos2::new(row.left() + 18.0 + check_offset, row.center().y),
                        Vec2::splat(16.0),
                    );
                    match &icon {
                        Some(texture) => {
                            rows_painter.image(
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
                                &rows_painter,
                                icon_rect.center(),
                                glyph,
                                14.0,
                                if cut { widgets::fade(color, 0.5) } else { color },
                            );
                        }
                    }
                    paint_status_badge(&rows_painter, icon_rect, entry, &t);

                    let name_left = row.left() + 32.0 + check_offset;
                    let name_right = (row.left() + name_w - 6.0).max(name_left);
                    let name_rect =
                        Rect::from_min_max(Pos2::new(name_left, row.top()), Pos2::new(name_right, row.bottom()));
                    let editing = rename.as_ref().map(|state| state.row == index).unwrap_or(false);
                    if editing {
                        rename_painted = true;
                        let editor = rename.as_mut().expect("checked above");
                        let edit_rect = name_rect.shrink2(Vec2::new(0.0, 3.0)).intersect(rows_clip);
                        if let Some(buf) = paint_inline_rename(
                            ui,
                            &rows_painter,
                            &t,
                            editor,
                            edit_rect,
                            13.0,
                            entry.is_folder,
                            pane_tag,
                        ) {
                            rename_commit = Some((index, buf));
                        }
                    } else {
                        widgets::text_left(
                            ui,
                            &rows_painter,
                            name_rect,
                            display_name(entry, hide_ext),
                            theme::font(13.0),
                            text_color,
                            0.0,
                        );
                    }

                    let date_rect =
                        Rect::from_min_size(Pos2::new(row.left() + name_w, row.top()), Vec2::new(date_w, row_h));
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
                            &rows_painter,
                            date_rect.shrink2(Vec2::new(4.0, 0.0)),
                            &total_text,
                            theme::font(12.5),
                            t.text_secondary,
                        );
                        widgets::text_right(
                            ui,
                            &rows_painter,
                            type_rect.shrink2(Vec2::new(4.0, 0.0)),
                            &free_text,
                            theme::font(12.5),
                            t.text_secondary,
                        );
                        if entry.size > 0 && entry.free_space <= entry.size {
                            let used = (entry.size.saturating_sub(entry.free_space) as f64 / entry.size as f64) as f32;
                            let bar = Rect::from_min_size(
                                Pos2::new(type_rect.left() + 8.0, type_rect.bottom() - 4.5),
                                Vec2::new((type_rect.width() - 16.0).max(12.0), 2.5),
                            );
                            rows_painter.rect_filled(bar, CornerRadius::same(1), widgets::fade(t.border_strong, 0.45));
                            let bar_col = if used > 0.9 { t.danger } else { t.accent };
                            rows_painter.rect_filled(
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
                            &rows_painter,
                            size_rect,
                            &entry.kind,
                            theme::font(12.5),
                            t.text_secondary,
                            7.0,
                        );
                    } else {
                        widgets::text_left(
                            ui,
                            &rows_painter,
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
                        widgets::text_left(
                            ui,
                            &rows_painter,
                            type_rect,
                            &col3,
                            theme::font(12.5),
                            t.text_secondary,
                            7.0,
                        );
                        let size_text = if entry.is_folder { String::new() } else { format_size(entry.size) };
                        widgets::text_right(
                            ui,
                            &rows_painter,
                            size_rect.shrink2(Vec2::new(4.0, 0.0)),
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

        // UI-08: Animate the selection accent pill in content-relative Y coordinates so
        // scrolling keeps the pill locked 1:1 to its row with zero lag or ghosting.
        let sel_bar_id = egui::Id::unique(("details-sel-bar-y", pane_tag, tab_uid));
        let sel_bar_last_id = egui::Id::unique(("details-sel-bar-last", pane_tag, tab_uid));
        if single_selected {
            if let Some(target_content_y) = single_sel_content_y {
                let prev_target = ctx.data(|d| d.get_temp::<f32>(sel_bar_last_id));
                if prev_target.is_some_and(|prev| (target_content_y - prev).abs() > rows_area.height().max(row_h)) {
                    crate::ui::spring::set_immediate(&ctx, sel_bar_id, target_content_y);
                }
                ctx.data_mut(|d| d.insert_temp(sel_bar_last_id, target_content_y));
                let (content_cy, vy) = crate::ui::spring::animate_vel(
                    &ctx,
                    sel_bar_id,
                    target_content_y,
                    target_content_y,
                    crate::ui::spring::MAGNETIC,
                    0.15,
                );
                let screen_cy = rows_area.top() + content_cy - offset + enter_offset;
                let stretch = (vy.abs() * 0.008).min(8.0);
                let bar = Rect::from_center_size(
                    Pos2::new(rows_area.left() + 4.0 - h_offset + 2.5, screen_cy),
                    Vec2::new(3.0, 16.0 + stretch),
                );
                if rows_area.intersects(bar) {
                    rows_painter.rect_filled(bar, CornerRadius::same(2), t.selected_bar);
                }
            }
        } else {
            crate::ui::spring::clear(&ctx, sel_bar_id);
            ctx.data_mut(|d| d.remove_temp::<f32>(sel_bar_last_id));
        }

        if let Some(label) = toggle_group {
            self.focus_filelist(ui);
            self.set_focus_secondary(secondary);
            self.tab_mut(secondary).toggle_group(&label);
        }
        self.apply_marquee(
            ui,
            &rows_painter,
            rows_area,
            Vec2::new(h_offset, offset),
            &background,
            &t,
            secondary,
            |band| details_marquee_hits(&display, &offsets, scale, row_h, row_width, band),
        );
        self.finish_view_interactions(
            ui,
            secondary,
            rename,
            rename_was_initial_focus,
            rename_painted,
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
        let (tab_uid, nav_gen, has_groups) = {
            let tab = self.tab_ref(secondary);
            (tab.uid, tab.nav_generation, tab.group_by.is_some())
        };
        sync_pane_nav_generation(&ctx, pane_tag, tab_uid, nav_gen);

        let display = self.tab_ref(secondary).display.clone();
        let layout = build_list_layout(&display, area, self.settings.compact_mode, has_groups);
        let show_checks = self.settings.show_checkboxes;
        let check_offset = if show_checks { 22.0 } else { 0.0 };
        let hide_ext = self.settings.hide_extensions;

        let enter_offset = (1.0
            - crate::ui::spring::ease_out_reveal(&ctx, egui::Id::unique(("pane-enter", pane_tag, tab_uid)), 0.12))
            * 5.0;

        let (viewport, scroll_vec) = match &layout {
            ListLayout::HorizontalColumns { col_w, rows_per_col, content_w, has_hbar, .. } => {
                if pane_focused {
                    self.page_rows = (*rows_per_col).max(1);
                }
                let avail_w = (area.width() - 8.0).max(1.0);
                let vp_bottom = if *has_hbar { (area.bottom() - HSCROLL_H).max(area.top()) } else { area.bottom() };
                let viewport = Rect::from_min_max(area.min, Pos2::new(area.right(), vp_bottom));
                let hbar_area = Rect::from_min_max(
                    Pos2::new(area.left() + 4.0, vp_bottom),
                    Pos2::new(area.right() - 4.0, area.bottom()),
                );
                let target_x = self.tab_mut(secondary).scroll_to.take().and_then(|index| {
                    let pos = display.iter().position(|r| matches!(r, DisplayRow::Entry(e) if *e == index))?;
                    let col = pos / (*rows_per_col).max(1);
                    Some((col as f32 * col_w, *col_w))
                });
                let h_offset = if *has_hbar {
                    self.h_scroll_offset(ui, area, hbar_area, *content_w, avail_w, target_x, true, secondary)
                } else {
                    self.reset_h_scroll(&ctx, pane_tag, tab_uid);
                    0.0
                };
                (viewport, Vec2::new(h_offset, 0.0))
            }
            ListLayout::GroupedColumns { row_h, sections, content_h, .. } => {
                self.reset_h_scroll(&ctx, pane_tag, tab_uid);
                if pane_focused {
                    self.page_rows = (area.height() / row_h).floor().max(1.0) as usize;
                }
                let target_y = self.tab_mut(secondary).scroll_to.take().and_then(|index| {
                    for sec in sections {
                        let slice = &display[sec.entries_start..sec.entries_start + sec.entry_count];
                        if let Some(local) = slice.iter().position(|r| matches!(r, DisplayRow::Entry(e) if *e == index))
                        {
                            let row = local % sec.rows_per_col.max(1);
                            return Some(sec.entries_top_y + row as f32 * row_h);
                        }
                    }
                    None
                });
                let v_offset = self.scroll_offset(ui, area, *content_h, target_y, secondary);
                (area, Vec2::new(0.0, v_offset))
            }
        };

        let background = ui.interact(viewport, egui::Id::unique(("list-bg", pane_tag)), Sense::click_and_drag());
        let vp_clip = viewport.intersect(ui.clip_rect());
        let vp_painter = painter.with_clip_rect(vp_clip);
        let drag_hover = crate::shell::dnd::drag_hover();
        let hover_target = drag_hover.as_ref().and_then(|h| h.target.as_deref());

        let mut rename = if pane_focused { self.rename.take() } else { None };
        let rename_was_initial_focus = rename.as_ref().is_some_and(|r| r.focus);
        let mut rename_painted = false;
        let mut rename_commit: Option<(usize, String)> = None;
        let mut select_request: Option<(usize, egui::Modifiers)> = None;
        let mut open_index: Option<usize> = None;
        let mut context_index: Option<usize> = None;
        let mut drag_index: Option<usize> = None;
        let mut toggle_group: Option<String> = None;

        let mut paint_list_cell = |app: &mut Self, ui: &mut egui::Ui, index: usize, cell: Rect| {
            let Some(entry) = entries.get(index) else { return };
            if !viewport.intersects(cell) {
                return;
            }
            let hit_cell = cell.intersect(vp_clip);
            if entry.is_folder
                && let Some(path) = &entry.path
                && hit_cell.width() > 0.0
                && hit_cell.height() > 0.0
            {
                app.drop_zones.push((hit_cell, path.clone()));
            }
            let id = egui::Id::unique(("lrow", pane_tag, index));
            let response = ui.interact(hit_cell, id, Sense::click_and_drag());
            let hover = widgets::hover_t(&ctx, id, response.hovered());
            let selected = app.tab_ref(secondary).selection.contains(&index);
            let drop_hi = entry.is_folder && is_drop_hovered(hover_target, entry.path.as_deref());
            if drop_hi {
                vp_painter.rect_filled(cell, CornerRadius::same(theme::RADIUS_ROW), t.accent_soft);
                vp_painter.rect_stroke(
                    cell,
                    CornerRadius::same(theme::RADIUS_ROW),
                    Stroke::new(1.5, t.accent),
                    egui::StrokeKind::Inside,
                );
            } else if selected {
                vp_painter.rect_filled(cell, CornerRadius::same(theme::RADIUS_ROW), t.selected);
                if hover > 0.0 {
                    vp_painter.rect_filled(
                        cell,
                        CornerRadius::same(theme::RADIUS_ROW),
                        widgets::fade(t.selected_hover, hover),
                    );
                }
            } else if hover > 0.0 {
                vp_painter.rect_filled(cell, CornerRadius::same(theme::RADIUS_ROW), widgets::fade(t.hover, hover));
            }

            let cut = app.is_cut(entry.path.as_deref());
            let dimmed = cut || entry.is_hidden();
            let tint = if dimmed { Color32::from_white_alpha(135) } else { Color32::WHITE };
            let text_color = if cut {
                widgets::fade(t.text, 0.55)
            } else if entry.is_hidden() {
                t.text_secondary
            } else {
                t.text
            };

            let mut checkbox_clicked = false;
            if show_checks && (selected || hover > 0.0) {
                let box_rect =
                    Rect::from_center_size(Pos2::new(cell.left() + 12.0, cell.center().y), Vec2::splat(14.0));
                if vp_clip.intersects(box_rect) {
                    checkbox_clicked = paint_item_checkbox(
                        ui,
                        &vp_painter,
                        box_rect,
                        egui::Id::unique(("lchk", pane_tag, index)),
                        selected,
                        &t,
                    );
                }
            }

            let icon = app.shell_icon_entry(ui.ctx(), entry, 16, false);
            let icon_rect = Rect::from_center_size(
                Pos2::new(cell.left() + 14.0 + check_offset, cell.center().y),
                Vec2::splat(16.0),
            );
            match &icon {
                Some(texture) => {
                    vp_painter.image(
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
                        &vp_painter,
                        icon_rect.center(),
                        glyph,
                        14.0,
                        if cut { widgets::fade(color, 0.5) } else { color },
                    );
                }
            }
            paint_status_badge(&vp_painter, icon_rect, entry, &t);

            let name_left = cell.left() + 28.0 + check_offset;
            let name_right = (cell.right() - 6.0).max(name_left);
            let name_rect = Rect::from_min_max(Pos2::new(name_left, cell.top()), Pos2::new(name_right, cell.bottom()));
            let editing = rename.as_ref().map(|state| state.row == index).unwrap_or(false);
            if editing {
                rename_painted = true;
                let editor = rename.as_mut().expect("checked above");
                let edit_rect = name_rect.shrink2(Vec2::new(0.0, 2.0)).intersect(vp_clip);
                if let Some(buf) =
                    paint_inline_rename(ui, &vp_painter, &t, editor, edit_rect, 12.5, entry.is_folder, pane_tag)
                {
                    rename_commit = Some((index, buf));
                }
            } else {
                widgets::text_left(
                    ui,
                    &vp_painter,
                    name_rect,
                    display_name(entry, hide_ext),
                    theme::font(12.5),
                    text_color,
                    0.0,
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
        };

        match &layout {
            ListLayout::HorizontalColumns { col_w, row_h, rows_per_col, num_cols, .. } => {
                let first_col = (scroll_vec.x / col_w).floor().max(0.0) as usize;
                let vis_cols = (viewport.width() / col_w).ceil() as usize + 2;
                let last_col = (first_col + vis_cols).min(*num_cols);
                for col in first_col..last_col {
                    for row in 0..*rows_per_col {
                        let pos = col * rows_per_col + row;
                        let Some(DisplayRow::Entry(index)) = display.get(pos) else { continue };
                        let cell = Rect::from_min_size(
                            Pos2::new(
                                viewport.left() + 4.0 + col as f32 * col_w - scroll_vec.x,
                                viewport.top() + row as f32 * row_h + enter_offset,
                            ),
                            Vec2::new((col_w - 6.0).max(40.0), *row_h),
                        );
                        paint_list_cell(self, ui, *index, cell);
                    }
                }
            }
            ListLayout::GroupedColumns { col_w, row_h, group_h, cols, sections, .. } => {
                let offset = scroll_vec.y;
                let group_w = (viewport.width() - 16.0).max(80.0);
                for section in sections {
                    if section.bottom_y < offset - group_h {
                        continue;
                    }
                    if section.top_y > offset + viewport.height() + group_h {
                        break;
                    }
                    if let Some(hdr) = &section.header {
                        let rect = Rect::from_min_size(
                            Pos2::new(viewport.left() + 4.0, viewport.top() + section.top_y - offset + enter_offset),
                            Vec2::new(group_w, *group_h),
                        );
                        if viewport.intersects(rect)
                            && self.paint_group_header(
                                ui,
                                &vp_painter,
                                rect,
                                &hdr.label,
                                hdr.count,
                                hdr.collapsed,
                                pane_tag,
                                tab_uid,
                            )
                        {
                            toggle_group = Some(hdr.label.clone());
                        }
                    }
                    if section.entry_count == 0 {
                        continue;
                    }
                    let rows = section.rows_per_col.max(1);
                    let first_row = ((offset - section.entries_top_y) / row_h).floor().max(0.0) as usize;
                    let vis_rows = (viewport.height() / row_h).ceil() as usize + 2;
                    let last_row = (first_row + vis_rows).min(rows);
                    for col in 0..*cols {
                        for row in first_row..last_row {
                            let local = col * rows + row;
                            if local >= section.entry_count {
                                continue;
                            }
                            let Some(DisplayRow::Entry(index)) = display.get(section.entries_start + local) else {
                                continue;
                            };
                            let cell = section
                                .cell_logical_rect(local, *col_w, *row_h)
                                .translate(Vec2::new(viewport.left(), viewport.top() - offset + enter_offset));
                            paint_list_cell(self, ui, *index, cell);
                        }
                    }
                }
            }
        }

        if let Some(label) = toggle_group {
            self.focus_filelist(ui);
            self.set_focus_secondary(secondary);
            self.tab_mut(secondary).toggle_group(&label);
        }
        self.apply_marquee(ui, &vp_painter, viewport, scroll_vec, &background, &t, secondary, |band| {
            list_marquee_hits(&display, &layout, band)
        });
        self.finish_view_interactions(
            ui,
            secondary,
            rename,
            rename_was_initial_focus,
            rename_painted,
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
        let (tab_uid, nav_gen, has_groups, this_pc, searching) = {
            let tab = self.tab_ref(secondary);
            (tab.uid, tab.nav_generation, tab.group_by.is_some(), tab.is_this_pc(), tab.is_search())
        };
        sync_pane_nav_generation(&ctx, pane_tag, tab_uid, nav_gen);
        self.reset_h_scroll(&ctx, pane_tag, tab_uid);

        // BUG-15: Never zoom the underlying file list while a modal/menu/popup is open.
        let zoom_delta = if !self.has_blocking_overlay() && ui.rect_contains_pointer(area) {
            ui.input(|input| input.zoom_delta())
        } else {
            1.0
        };
        if zoom_delta != 1.0 {
            let tab = self.tab_mut(secondary);
            tab.grid_zoom = (tab.grid_zoom * zoom_delta).clamp(0.7, 2.4);
        }
        let zoom = self.tab_ref(secondary).grid_zoom;
        let m = compute_grid_metrics(view, area.width(), zoom);
        if pane_focused {
            self.grid_columns = m.columns;
            self.page_rows = ((area.height() / m.cell_h).floor().max(1.0) as usize) * m.columns;
        }

        let display = self.tab_ref(secondary).display.clone();
        let group_h = if self.settings.compact_mode { GROUP_H * (26.0 / ROW_H) } else { GROUP_H };
        let (sections, content_h) = build_grid_sections(&display, m.columns, m.cell_h, group_h, has_groups);
        let background = ui.interact(area, egui::Id::unique(("grid-bg", pane_tag)), Sense::click_and_drag());

        let target_y = self
            .tab_mut(secondary)
            .scroll_to
            .take()
            .and_then(|index| grid_entry_target_y(&display, &sections, m, index));
        let offset = self.scroll_offset(ui, area, content_h, target_y, secondary);
        let drag_hover = crate::shell::dnd::drag_hover();
        let hover_target = drag_hover.as_ref().and_then(|h| h.target.as_deref());
        let search_root = self.search.root.clone();
        let show_checks = self.settings.show_checkboxes;
        let hide_ext = self.settings.hide_extensions;

        let enter_offset = (1.0
            - crate::ui::spring::ease_out_reveal(&ctx, egui::Id::unique(("pane-enter", pane_tag, tab_uid)), 0.12))
            * 5.0;

        let area_clip = area.intersect(ui.clip_rect());
        let grid_painter = painter.with_clip_rect(area_clip);

        let mut rename = if pane_focused { self.rename.take() } else { None };
        let rename_was_initial_focus = rename.as_ref().is_some_and(|r| r.focus);
        let mut rename_painted = false;
        let mut rename_commit: Option<(usize, String)> = None;
        let mut select_request: Option<(usize, egui::Modifiers)> = None;
        let mut open_index: Option<usize> = None;
        let mut context_index: Option<usize> = None;
        let mut drag_index: Option<usize> = None;
        let mut toggle_group: Option<String> = None;

        let group_w = (area.width() - 16.0).max(80.0);
        for section in &sections {
            if section.bottom_y < offset - group_h {
                continue;
            }
            if section.top_y > offset + area.height() + group_h {
                break;
            }
            if let Some(hdr) = &section.header {
                let hdr_rect = Rect::from_min_size(
                    Pos2::new(area.left() + 4.0, area.top() + section.top_y - offset + enter_offset),
                    Vec2::new(group_w, group_h),
                );
                if area.intersects(hdr_rect)
                    && self.paint_group_header(
                        ui,
                        &grid_painter,
                        hdr_rect,
                        &hdr.label,
                        hdr.count,
                        hdr.collapsed,
                        pane_tag,
                        tab_uid,
                    )
                {
                    toggle_group = Some(hdr.label.clone());
                }
            }
            if section.entry_count == 0 {
                continue;
            }
            let total_rows = section.entry_count.div_ceil(m.columns);
            let first_row = ((offset - section.entries_top_y) / m.cell_h).floor().max(0.0) as usize;
            let visible_rows = (area.height() / m.cell_h).ceil() as usize + 2;
            let last_row = (first_row + visible_rows).min(total_rows);
            for row in first_row..last_row {
                for column in 0..m.columns {
                    let local = row * m.columns + column;
                    if local >= section.entry_count {
                        continue;
                    }
                    let Some(DisplayRow::Entry(index)) = display.get(section.entries_start + local) else {
                        continue;
                    };
                    let index = *index;
                    let Some(entry) = entries.get(index) else { continue };
                    let cell = section
                        .cell_logical_rect(local, m)
                        .translate(Vec2::new(area.left(), area.top() - offset + enter_offset));
                    if !area.intersects(cell) {
                        continue;
                    }
                    let hit_cell = cell.intersect(area_clip);
                    if entry.is_folder
                        && let Some(path) = &entry.path
                        && hit_cell.width() > 0.0
                        && hit_cell.height() > 0.0
                    {
                        self.drop_zones.push((hit_cell, path.clone()));
                    }
                    let id = egui::Id::unique(("cell", pane_tag, index));
                    let response = ui.interact(hit_cell, id, Sense::click_and_drag());
                    let hover = widgets::hover_t(&ctx, id, response.hovered());
                    let selected = self.tab_ref(secondary).selection.contains(&index);
                    let drop_hi = entry.is_folder && is_drop_hovered(hover_target, entry.path.as_deref());
                    if drop_hi {
                        grid_painter.rect_filled(cell, CornerRadius::same(6), t.accent_soft);
                        grid_painter.rect_stroke(
                            cell,
                            CornerRadius::same(6),
                            Stroke::new(1.5, t.accent),
                            egui::StrokeKind::Inside,
                        );
                    } else if selected {
                        grid_painter.rect_filled(cell, CornerRadius::same(6), t.selected);
                        grid_painter.rect_stroke(
                            cell,
                            CornerRadius::same(6),
                            Stroke::new(1.0, t.selected_bar),
                            egui::StrokeKind::Inside,
                        );
                    } else if hover > 0.0 {
                        grid_painter.rect_filled(cell, CornerRadius::same(6), widgets::fade(t.hover, hover));
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

                    let mut checkbox_clicked = false;
                    let icon = self.shell_icon_entry(ui.ctx(), entry, m.icon_px, true);
                    let icon_size = m.icon_px as f32;
                    let editing = rename.as_ref().map(|state| state.row == index).unwrap_or(false);

                    if view == ViewMode::Tiles {
                        // True Windows Explorer horizontal tile: left 40-48px icon, right stacked
                        // 1-2 line filename + secondary metadata (type & size/date).
                        let check_offset = if show_checks { 20.0 } else { 0.0 };
                        if show_checks && (selected || hover > 0.0) {
                            let box_rect = Rect::from_center_size(
                                Pos2::new(cell.left() + 13.0, cell.center().y),
                                Vec2::splat(14.0),
                            );
                            if area_clip.intersects(box_rect) {
                                checkbox_clicked = paint_item_checkbox(
                                    ui,
                                    &grid_painter,
                                    box_rect,
                                    egui::Id::unique(("gchk", pane_tag, index)),
                                    selected,
                                    &t,
                                );
                            }
                        }
                        let icon_rect = Rect::from_center_size(
                            Pos2::new(cell.left() + 10.0 + check_offset + icon_size * 0.5, cell.center().y),
                            Vec2::splat(icon_size),
                        );
                        match &icon {
                            Some(texture) => {
                                grid_painter.image(
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
                                    &grid_painter,
                                    icon_rect.center(),
                                    glyph,
                                    icon_size * 0.62,
                                    if cut { widgets::fade(color, 0.5) } else { color },
                                );
                            }
                        }
                        paint_status_badge(&grid_painter, icon_rect, entry, &t);

                        let text_left = icon_rect.right() + 10.0;
                        let text_right = (cell.right() - 8.0).max(text_left);
                        let text_w = (text_right - text_left).max(0.0);
                        let title_size = (12.5 * zoom).clamp(11.0, 16.0);
                        let meta_size = (11.0 * zoom).clamp(10.0, 13.5);
                        if editing {
                            rename_painted = true;
                            let editor = rename.as_mut().expect("checked above");
                            let edit_h = (title_size + 8.0).min((cell.height() - 10.0).max(20.0));
                            let edit_rect = Rect::from_min_size(
                                Pos2::new(text_left, cell.center().y - edit_h * 0.5),
                                Vec2::new(text_w, edit_h),
                            )
                            .intersect(area_clip);
                            if let Some(buf) = paint_inline_rename(
                                ui,
                                &grid_painter,
                                &t,
                                editor,
                                edit_rect,
                                title_size,
                                entry.is_folder,
                                pane_tag,
                            ) {
                                rename_commit = Some((index, buf));
                            }
                        } else {
                            let title_galley = widgets::galley_wrap(
                                ui,
                                display_name(entry, hide_ext),
                                theme::semibold(title_size),
                                text_color,
                                text_w,
                                2,
                            );
                            let meta_text = tile_metadata_text(entry, this_pc, searching, search_root.as_deref());
                            let meta_galley =
                                widgets::galley(ui, &meta_text, theme::font(meta_size), t.text_tertiary, text_w);
                            let has_bar = this_pc && entry.size > 0 && entry.free_space <= entry.size;
                            let bar_extra = if has_bar { 5.0 } else { 0.0 };
                            let gap = 2.0;
                            let block_h = title_galley.size().y + gap + meta_galley.size().y + bar_extra;
                            let block_top = (cell.center().y - block_h * 0.5).max(cell.top() + 4.0);
                            widgets::galley_at(
                                &grid_painter,
                                Pos2::new(text_left, block_top),
                                title_galley.clone(),
                                text_color,
                            );
                            let meta_top = block_top + title_galley.size().y + gap;
                            widgets::galley_at(
                                &grid_painter,
                                Pos2::new(text_left, meta_top),
                                meta_galley.clone(),
                                t.text_tertiary,
                            );
                            if has_bar {
                                let used =
                                    (entry.size.saturating_sub(entry.free_space) as f64 / entry.size as f64) as f32;
                                let bar = Rect::from_min_size(
                                    Pos2::new(text_left, meta_top + meta_galley.size().y + 2.5),
                                    Vec2::new(text_w.max(12.0), 2.5),
                                );
                                grid_painter.rect_filled(
                                    bar,
                                    CornerRadius::same(1),
                                    widgets::fade(t.border_strong, 0.45),
                                );
                                let bar_col = if used > 0.9 { t.danger } else { t.accent };
                                grid_painter.rect_filled(
                                    Rect::from_min_size(
                                        bar.min,
                                        Vec2::new(bar.width() * used.clamp(0.0, 1.0), bar.height()),
                                    ),
                                    CornerRadius::same(1),
                                    bar_col,
                                );
                            }
                        }
                    } else {
                        // Icons view: spacious vertical icon + 2-line wrapped centered filename.
                        if show_checks && (selected || hover > 0.0) {
                            let box_rect = Rect::from_center_size(
                                Pos2::new(cell.left() + 13.0, cell.top() + 13.0),
                                Vec2::splat(14.0),
                            );
                            if area_clip.intersects(box_rect) {
                                checkbox_clicked = paint_item_checkbox(
                                    ui,
                                    &grid_painter,
                                    box_rect,
                                    egui::Id::unique(("gchk", pane_tag, index)),
                                    selected,
                                    &t,
                                );
                            }
                        }
                        let icon_rect = Rect::from_center_size(
                            Pos2::new(cell.center().x, cell.top() + 10.0 * zoom + icon_size * 0.5),
                            Vec2::splat(icon_size),
                        );
                        match &icon {
                            Some(texture) => {
                                grid_painter.image(
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
                                    &grid_painter,
                                    icon_rect.center(),
                                    glyph,
                                    icon_size * 0.6,
                                    if cut { widgets::fade(color, 0.5) } else { color },
                                );
                            }
                        }
                        paint_status_badge(&grid_painter, icon_rect, entry, &t);

                        let name_rect = Rect::from_min_max(
                            Pos2::new(cell.left() + 6.0, icon_rect.bottom() + 6.0),
                            Pos2::new(cell.right() - 6.0, cell.bottom() - 4.0),
                        );
                        let label_size = (12.0 * zoom).clamp(11.0, 16.0);
                        if editing {
                            rename_painted = true;
                            let editor = rename.as_mut().expect("checked above");
                            let edit_rect = Rect::from_min_size(
                                Pos2::new(name_rect.left(), name_rect.top() + 2.0),
                                Vec2::new(name_rect.width(), (label_size + 8.0).min(name_rect.height().max(20.0))),
                            )
                            .intersect(area_clip);
                            if let Some(buf) = paint_inline_rename(
                                ui,
                                &grid_painter,
                                &t,
                                editor,
                                edit_rect,
                                label_size,
                                entry.is_folder,
                                pane_tag,
                            ) {
                                rename_commit = Some((index, buf));
                            }
                        } else {
                            let galley = widgets::galley_wrap(
                                ui,
                                display_name(entry, hide_ext),
                                theme::font(label_size),
                                text_color,
                                name_rect.width(),
                                2,
                            );
                            let label_painter = grid_painter.with_clip_rect(name_rect.intersect(area_clip));
                            widgets::galley_at(
                                &label_painter,
                                Pos2::new(name_rect.center().x - galley.size().x * 0.5, name_rect.top() + 2.0),
                                galley,
                                text_color,
                            );
                        }
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

        if let Some(label) = toggle_group {
            self.focus_filelist(ui);
            self.set_focus_secondary(secondary);
            self.tab_mut(secondary).toggle_group(&label);
        }
        self.apply_marquee(ui, &grid_painter, area, Vec2::new(0.0, offset), &background, &t, secondary, |band| {
            grid_marquee_hits(&display, &sections, m, band)
        });
        self.finish_view_interactions(
            ui,
            secondary,
            rename,
            rename_was_initial_focus,
            rename_painted,
            rename_commit,
            select_request,
            context_index,
            open_index,
            drag_index,
            &background,
        );
    }

    /// Unified post-frame interaction dispatch shared by Details, List, Tiles,
    /// and Icons views so every view has identical rename, offscreen-rename cleanup,
    /// sidebar focus surrender, drag-out, and background/item context menu behavior.
    #[allow(clippy::too_many_arguments)]
    fn finish_view_interactions(
        &mut self,
        ui: &egui::Ui,
        secondary: bool,
        mut rename: Option<RenameState>,
        rename_was_initial_focus: bool,
        rename_painted: bool,
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
            } else if let Some(state) = rename {
                if !rename_painted {
                    let row_visible_in_display = self.tab_ref(secondary).visible_entry_indices().contains(&state.row);
                    if !row_visible_in_display {
                        // Row disappeared into a collapsed group: finish/cancel cleanly.
                        self.rename = Some(state);
                        self.finish_rename();
                    } else if rename_was_initial_focus {
                        // Freshly opened rename hasn't scrolled into view yet: scroll toward it.
                        self.tab_mut(secondary).scroll_to = Some(state.row);
                        self.rename = Some(state);
                    } else {
                        // BUG-35: User scrolled an active inline rename completely out of the viewport.
                        // Automatically commit (or cancel if unchanged) so keyboard & watcher never lock.
                        self.rename = Some(state);
                        self.finish_rename();
                    }
                } else {
                    self.rename = Some(state);
                }
            }
        }

        let interacted = select_request.is_some()
            || context_index.is_some()
            || open_index.is_some()
            || drag_index.is_some()
            || background.clicked()
            || background.secondary_clicked();
        if interacted {
            self.focus_filelist(ui);
            if self.focus_secondary != secondary {
                self.last_selected_click = None;
                self.set_focus_secondary(secondary);
            }
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

    /// Rubber-band selection in logical content coordinates: intersects ALL visible
    /// entries (even entries scrolled out of the viewport during dragging) and respects
    /// `Ctrl` (symmetric toggle / XOR) and `Shift` (union) modifiers (BUG-35).
    #[allow(clippy::too_many_arguments)]
    fn apply_marquee<F>(
        &mut self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        viewport: Rect,
        scroll: Vec2,
        background: &egui::Response,
        t: &theme::Theme,
        secondary: bool,
        compute_hits: F,
    ) where
        F: FnOnce(Rect) -> Vec<usize>,
    {
        let pane_tag = u8::from(secondary);
        let tab_uid = self.tab_ref(secondary).uid;
        let drag_id = egui::Id::unique(("marquee-drag", pane_tag, tab_uid));

        if background.drag_started() && !self.has_blocking_overlay() {
            self.focus_filelist(ui);
            self.set_focus_secondary(secondary);
            if let Some(origin_screen) = ui.input(|input| input.pointer.press_origin()) {
                let origin_logical =
                    Pos2::new(origin_screen.x - viewport.min.x + scroll.x, origin_screen.y - viewport.min.y + scroll.y);
                let base_selection = self.tab_ref(secondary).selection.clone();
                ui.ctx().data_mut(|d| {
                    d.insert_temp(drag_id, MarqueeDrag { origin_logical, base_selection });
                });
                self.marquee = Some(origin_screen);
            }
        }
        if !background.dragged() {
            if background.drag_stopped() {
                ui.ctx().data_mut(|d| d.remove_temp::<MarqueeDrag>(drag_id));
                self.marquee = None;
            }
            return;
        }
        let Some(drag) = ui.ctx().data(|d| d.get_temp::<MarqueeDrag>(drag_id)) else {
            return;
        };
        let start_screen = Pos2::new(
            drag.origin_logical.x + viewport.min.x - scroll.x,
            drag.origin_logical.y + viewport.min.y - scroll.y,
        );
        self.marquee = Some(start_screen);
        let current_screen = ui.input(|input| input.pointer.interact_pos()).unwrap_or(start_screen);
        let current_logical =
            Pos2::new(current_screen.x - viewport.min.x + scroll.x, current_screen.y - viewport.min.y + scroll.y);
        let band_screen = Rect::from_two_pos(start_screen, current_screen);
        let band_logical = Rect::from_two_pos(drag.origin_logical, current_logical);

        let clipped = painter.with_clip_rect(viewport.intersect(ui.clip_rect()));
        clipped.rect_filled(band_screen, CornerRadius::ZERO, t.accent_soft);
        clipped.rect_stroke(band_screen, CornerRadius::ZERO, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);

        let modifiers = ui.input(|input| input.modifiers);
        let hits = compute_hits(band_logical);
        let combined = combine_marquee_selection(
            &drag.base_selection,
            &hits,
            modifiers.ctrl || modifiers.command,
            modifiers.shift,
        );
        self.tab_mut(secondary).set_selection(combined);
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
        let tab_uid = self.tab_ref(secondary).uid;
        let scroll_spring_id = egui::Id::unique(("list-scroll-spring", pane_tag, tab_uid));
        let mut immediate_scroll = false;
        let max_scroll = (content_h - area.height()).max(0.0);
        let mut target = self.tab_ref(secondary).scroll_offset.clamp(0.0, max_scroll);
        // BUG-15: Never scroll the file list while a blocking modal/menu/popup is open.
        if !self.has_blocking_overlay() && ui.rect_contains_pointer(area) {
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
            let alpha = 0.35 + hover * 0.65;
            let inset = (1.0 - hover) * 1.5;
            let paintable = Rect::from_min_max(
                Pos2::new(thumb.left() + inset, thumb.top()),
                Pos2::new(thumb.right() - inset, thumb.bottom()),
            );
            ui.painter().rect_filled(paintable, CornerRadius::same(3), widgets::fade(t.scrollbar_hover, alpha));
            if response.dragged() && travel > 0.0 {
                self.focus_filelist(ui);
                target = (target + response.drag_delta().y / travel * max_scroll).clamp(0.0, max_scroll);
                self.tab_mut(secondary).scroll_offset = target;
                immediate_scroll = true;
            } else if response.clicked()
                && travel > 0.0
                && let Some(pos) = response.interact_pointer_pos()
                && !thumb.contains(pos)
            {
                self.focus_filelist(ui);
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

    fn reset_h_scroll(&self, ctx: &egui::Context, pane_tag: u8, tab_uid: u64) {
        let target_id = egui::Id::unique(("list-hscroll-target", pane_tag, tab_uid));
        let spring_id = egui::Id::unique(("list-hscroll-spring", pane_tag, tab_uid));
        ctx.data_mut(|d| d.remove_temp::<f32>(target_id));
        crate::ui::spring::clear(ctx, spring_id);
    }

    #[allow(clippy::too_many_arguments)]
    fn h_scroll_offset(
        &mut self,
        ui: &mut egui::Ui,
        wheel_area: Rect,
        bar_area: Rect,
        content_w: f32,
        viewport_w: f32,
        reveal_range: Option<(f32, f32)>,
        map_vertical_wheel: bool,
        secondary: bool,
    ) -> f32 {
        let t = self.theme.clone();
        let ctx = ui.ctx().clone();
        let pane_tag = u8::from(secondary);
        let tab_uid = self.tab_ref(secondary).uid;
        let target_id = egui::Id::unique(("list-hscroll-target", pane_tag, tab_uid));
        let spring_id = egui::Id::unique(("list-hscroll-spring", pane_tag, tab_uid));
        let max_scroll = (content_w - viewport_w).max(0.0);
        let mut target = ctx.data(|d| d.get_temp::<f32>(target_id).unwrap_or(0.0)).clamp(0.0, max_scroll);
        let mut immediate_scroll = false;

        // BUG-15: Guard horizontal wheel scrolling against blocking overlays.
        if !self.has_blocking_overlay() && ui.rect_contains_pointer(wheel_area) && max_scroll > 0.0 {
            let (dx, dy, shift) = ui.input(|i| (i.smooth_scroll_delta.x, i.smooth_scroll_delta.y, i.modifiers.shift));
            let delta = if dx != 0.0 {
                dx
            } else if map_vertical_wheel || shift {
                dy
            } else {
                0.0
            };
            if delta != 0.0 {
                target = (target - delta).clamp(0.0, max_scroll);
            }
        }
        if let Some((item_left, item_w)) = reveal_range {
            if item_left < target {
                target = item_left.clamp(0.0, max_scroll);
            } else if item_left + item_w > target + viewport_w {
                target = (item_left + item_w - viewport_w).clamp(0.0, max_scroll);
            }
        }
        ctx.data_mut(|d| d.insert_temp(target_id, target));

        let mut offset = crate::ui::spring::animate(&ctx, spring_id, target, target, crate::ui::spring::SCROLL)
            .clamp(0.0, max_scroll);

        if max_scroll > 0.0 && bar_area.width() > 24.0 && bar_area.height() >= 6.0 {
            let bar_h = 8.0f32.min(bar_area.height());
            let track = Rect::from_min_max(
                Pos2::new(bar_area.left(), bar_area.bottom() - bar_h - 1.0),
                Pos2::new(bar_area.right(), bar_area.bottom() - 1.0),
            );
            let thumb_w = (track.width() * (viewport_w / content_w)).clamp(28.0, track.width());
            let travel = track.width() - thumb_w;
            let thumb_x = track.left() + if max_scroll > 0.0 { offset / max_scroll * travel } else { 0.0 };
            let thumb = Rect::from_min_size(Pos2::new(thumb_x, track.top()), Vec2::new(thumb_w, bar_h));
            let id = egui::Id::unique(("list-hscrollbar", pane_tag));
            let response = ui.interact(track, id, Sense::click_and_drag());
            let hover = widgets::hover_t(&ctx, id, response.hovered() || response.dragged());
            let alpha = 0.35 + hover * 0.65;
            let inset = (1.0 - hover) * 1.5;
            let paintable = Rect::from_min_max(
                Pos2::new(thumb.left(), thumb.top() + inset),
                Pos2::new(thumb.right(), thumb.bottom() - inset),
            );
            ui.painter().rect_filled(paintable, CornerRadius::same(3), widgets::fade(t.scrollbar_hover, alpha));
            if response.dragged() && travel > 0.0 {
                self.focus_filelist(ui);
                target = (target + response.drag_delta().x / travel * max_scroll).clamp(0.0, max_scroll);
                ctx.data_mut(|d| d.insert_temp(target_id, target));
                immediate_scroll = true;
            } else if response.clicked()
                && travel > 0.0
                && let Some(pos) = response.interact_pointer_pos()
                && !thumb.contains(pos)
            {
                self.focus_filelist(ui);
                let ratio = ((pos.x - track.left() - thumb_w * 0.5) / travel).clamp(0.0, 1.0);
                target = ratio * max_scroll;
                ctx.data_mut(|d| d.insert_temp(target_id, target));
            }
        }
        if immediate_scroll || max_scroll <= 0.0 {
            offset = target;
            crate::ui::spring::set_immediate(&ctx, spring_id, target);
        }
        offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_right_aligned_active_header_reserves_padding_before_sort_arrow() {
        let cell = Rect::from_min_size(Pos2::new(300.0, 0.0), Vec2::new(100.0, HEADER_H));
        let arrow_x = details_sort_arrow_x(cell);
        let arrow_left = arrow_x - 4.0; // 8px glyph centered at arrow_x

        let active_right = details_header_label_rect(cell, 3, true, true, 0.0);
        assert!(
            active_right.right() <= arrow_left - 4.0,
            "active right-aligned header ({}) must clear sort arrow left edge ({arrow_left})",
            active_right.right()
        );
        assert!((cell.right() - active_right.right() - SORT_ARROW_PAD).abs() < 0.01);

        let inactive_right = details_header_label_rect(cell, 3, true, false, 0.0);
        assert!((cell.right() - inactive_right.right() - 4.0).abs() < 0.01);

        // Name header shifts by check_offset when checkboxes are enabled.
        let name_cell = Rect::from_min_size(Pos2::new(4.0, 0.0), Vec2::new(200.0, HEADER_H));
        let no_chk = details_header_label_rect(name_cell, 0, false, false, 0.0);
        let with_chk = details_header_label_rect(name_cell, 0, false, false, 22.0);
        assert!((with_chk.left() - no_chk.left() - 22.0).abs() < 0.01);
    }

    #[test]
    fn details_sort_keys_match_this_pc_and_search_columns() {
        let widths = [200.0, 140.0, 140.0, 90.0];

        let pc_cols = details_column_specs(true, false, widths);
        assert_eq!(pc_cols[1].0, "总大小");
        assert_eq!(pc_cols[1].1, SortKey::Size);
        assert!(pc_cols[1].3);
        assert_eq!(pc_cols[2].0, "可用空间");
        assert_eq!(pc_cols[2].1, SortKey::FreeSpace);
        assert!(pc_cols[2].3);

        let search_cols = details_column_specs(false, true, widths);
        assert_eq!(search_cols[2].0, "所在文件夹");
        assert_eq!(search_cols[2].1, SortKey::Location);

        let normal_cols = details_column_specs(false, false, widths);
        assert_eq!(normal_cols[2].0, "类型");
        assert_eq!(normal_cols[2].1, SortKey::Type);
    }

    #[test]
    fn details_columns_scale_cleanly_on_narrow_panes() {
        let pref = [168.0, 150.0, 92.0];
        // Wide pane keeps preferred widths exactly.
        let (wide, wide_total) = compute_details_columns(640.0, pref);
        assert!((wide_total - 640.0).abs() < 0.01);
        assert_eq!(wide[1], 168.0);
        assert_eq!(wide[2], 150.0);
        assert_eq!(wide[3], 92.0);

        // Moderately narrow pane scales columns proportionally to fit with zero overflow.
        let (narrow, narrow_total) = compute_details_columns(360.0, pref);
        assert!((narrow_total - 360.0).abs() < 0.5);
        assert!(narrow.iter().all(|&w| w >= 48.0));

        // Ultra-narrow pane clamps to hard floor sum so horizontal scroll handles the rest cleanly.
        let (tiny, tiny_total) = compute_details_columns(180.0, pref);
        assert!(tiny_total >= 256.0);
        assert_eq!(tiny, [88.0, 64.0, 56.0, 48.0]);
    }

    #[test]
    fn logical_marquee_hits_off_viewport_entries_and_respects_ctrl_and_shift() {
        // 50 rows of 30px = 1500px tall (far beyond a typical 300px viewport).
        let display: Vec<DisplayRow> = (0..50).map(DisplayRow::Entry).collect();
        let offsets: Vec<f32> = (0..=50).map(|i| i as f32 * ROW_H).collect();

        // Band spans from row 2 (y=65) down to row 45 (y=1360) — crossing offscreen rows!
        let band = Rect::from_min_max(Pos2::new(20.0, 65.0), Pos2::new(140.0, 1360.0));
        let hits = details_marquee_hits(&display, &offsets, 1.0, ROW_H, 500.0, band);
        assert_eq!(hits, (2..=45).collect::<Vec<_>>());

        // Plain drag replaces selection.
        let base: BTreeSet<usize> = [0, 1, 2, 3].into_iter().collect();
        let small_hits = vec![2, 3, 4, 5];
        assert_eq!(combine_marquee_selection(&base, &small_hits, false, false), vec![2, 3, 4, 5]);

        // Shift drag unions base and hits.
        assert_eq!(combine_marquee_selection(&base, &small_hits, false, true), vec![0, 1, 2, 3, 4, 5]);

        // Ctrl drag XOR-toggles hits against base (2 & 3 toggled off, 4 & 5 toggled on).
        assert_eq!(combine_marquee_selection(&base, &small_hits, true, false), vec![0, 1, 4, 5]);
    }

    #[test]
    fn multi_column_list_layout_flows_top_to_bottom_and_handles_groups() {
        // Ungrouped fitting cleanly without horizontal scrollbar: 25 entries in 710x266
        // (avail_w = 702 >= 3 * 232 = 696, inner_h = 260 => 10 rows/col, 3 columns).
        let ungrouped: Vec<DisplayRow> = (0..25).map(DisplayRow::Entry).collect();
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(710.0, 266.0));
        let layout = build_list_layout(&ungrouped, area, false, false);
        match &layout {
            ListLayout::HorizontalColumns { rows_per_col, num_cols, row_h, has_hbar, .. } => {
                assert_eq!(*row_h, LIST_ROW_H_NORMAL);
                assert!(!*has_hbar);
                assert_eq!(*rows_per_col, 10);
                assert_eq!(*num_cols, 3);
            }
            _ => panic!("expected ungrouped HorizontalColumns layout"),
        }

        // Narrower viewport triggers horizontal scrollbar and reserves HSCROLL_H cleanly.
        let narrow_area = Rect::from_min_size(Pos2::ZERO, Vec2::new(500.0, 266.0));
        let hbar_layout = build_list_layout(&ungrouped, narrow_area, false, false);
        match &hbar_layout {
            ListLayout::HorizontalColumns { rows_per_col, num_cols, has_hbar, .. } => {
                assert!(*has_hbar);
                assert_eq!(*rows_per_col, 9);
                assert_eq!(*num_cols, 3);
            }
            _ => panic!("expected ungrouped HorizontalColumns layout with scrollbar"),
        }

        // Compact mode shrinks row height and fits more rows per column.
        let compact_layout = build_list_layout(&ungrouped, area, true, false);
        match &compact_layout {
            ListLayout::HorizontalColumns { rows_per_col, row_h, .. } => {
                assert_eq!(*row_h, LIST_ROW_H_COMPACT);
                assert!(*rows_per_col > 10);
            }
            _ => panic!("expected compact HorizontalColumns layout"),
        }

        // Marquee hitting column 1 rows 1..=2 selects entries 11 and 12 (top-to-bottom order).
        let band =
            Rect::from_min_max(Pos2::new(4.0 + LIST_COL_W + 10.0, 30.0), Pos2::new(4.0 + LIST_COL_W + 40.0, 60.0));
        assert_eq!(list_marquee_hits(&ungrouped, &layout, band), vec![11, 12]);

        // Grouped list: 2 groups (first expanded with 5 items, second collapsed with 0 visible).
        let grouped = vec![
            DisplayRow::Group { label: "A".to_string(), count: 5, collapsed: false },
            DisplayRow::Entry(0),
            DisplayRow::Entry(1),
            DisplayRow::Entry(2),
            DisplayRow::Entry(3),
            DisplayRow::Entry(4),
            DisplayRow::Group { label: "B".to_string(), count: 3, collapsed: true },
        ];
        let g_layout = build_list_layout(&grouped, area, false, true);
        match &g_layout {
            ListLayout::GroupedColumns { cols, sections, group_h, .. } => {
                assert!(*cols >= 2, "wide area must lay out grouped list in multiple columns");
                assert_eq!(*group_h, GROUP_H);
                assert_eq!(sections.len(), 2);
                assert_eq!(sections[0].entry_count, 5);
                assert_eq!(sections[1].entry_count, 0);
                assert!(sections[1].top_y >= sections[0].bottom_y);
            }
            _ => panic!("expected GroupedColumns layout"),
        }
    }

    #[test]
    fn grouped_grid_layout_respects_groups_collapsed_state_and_marquee() {
        let display = vec![
            DisplayRow::Group { label: "A".to_string(), count: 3, collapsed: false },
            DisplayRow::Entry(0),
            DisplayRow::Entry(1),
            DisplayRow::Entry(2),
            DisplayRow::Group { label: "B".to_string(), count: 4, collapsed: true },
            DisplayRow::Group { label: "C".to_string(), count: 2, collapsed: false },
            DisplayRow::Entry(7),
            DisplayRow::Entry(8),
        ];

        // Tiles metrics should produce wide horizontal cards (~248x66 at zoom 1.0).
        let tiles = compute_grid_metrics(ViewMode::Tiles, 800.0, 1.0);
        assert!(tiles.cell_w >= 220.0 && tiles.cell_w <= 260.0);
        assert!(tiles.cell_h >= 58.0 && tiles.cell_h <= 70.0);
        assert!(tiles.icon_px >= 40 && tiles.icon_px <= 48);
        assert_eq!(tiles.columns, 3);

        let (sections, total_h) = build_grid_sections(&display, tiles.columns, tiles.cell_h, GROUP_H, true);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].entry_count, 3);
        assert_eq!(sections[1].entry_count, 0); // collapsed group B
        assert_eq!(sections[2].entry_count, 2);
        assert!(total_h > sections[2].entries_top_y);

        // Target Y for entry 7 in group C must sit below group A and collapsed group B.
        let y0 = grid_entry_target_y(&display, &sections, tiles, 0).expect("entry 0");
        let y7 = grid_entry_target_y(&display, &sections, tiles, 7).expect("entry 7");
        assert!(y7 > y0 + tiles.cell_h + GROUP_H);

        // Full-height marquee collects only visible entries across expanded groups (skips collapsed B).
        let band = Rect::from_min_max(Pos2::new(10.0, 0.0), Pos2::new(760.0, total_h));
        assert_eq!(grid_marquee_hits(&display, &sections, tiles, band), vec![0, 1, 2, 7, 8]);
    }

    mod headless {
        use super::*;
        use std::path::PathBuf;
        use std::time::{Duration, UNIX_EPOCH};
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

        struct Com;

        impl Com {
            fn init() -> Self {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                }
                Self
            }
        }

        impl Drop for Com {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }

        fn make_entry(name: &str) -> Entry {
            Entry {
                name: name.to_string(),
                path: Some(PathBuf::from(format!("C:\\test\\{name}"))),
                is_folder: false,
                size: 1024,
                modified: Some(UNIX_EPOCH + Duration::from_secs(1)),
                created: None,
                kind: "文本文档".to_string(),
                free_space: 0,
                ext: "txt".to_string(),
                file_attrs: 0,
                pidl: None,
            }
        }

        fn make_app(count: usize) -> (LumenApp, egui::Context) {
            let ctx = egui::Context::default();
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            let settings =
                crate::core::settings::Settings { tabs: vec![dir.to_string_lossy().to_string()], ..Default::default() };
            let mut app = LumenApp::with_context(&ctx, settings);
            let entries: Vec<Entry> = (0..count).map(|i| make_entry(&format!("file_{i:03}.txt"))).collect();
            let tab = app.active_mut();
            tab.entries = Arc::new(entries);
            tab.loading = false;
            tab.stats.total = count;
            tab.rebuild_display();
            (app, ctx)
        }

        #[test]
        fn blocking_overlay_prevents_wheel_scroll_and_grid_zoom() {
            let _com = Com::init();
            let (mut app, ctx) = make_app(80);
            let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(720.0, 320.0));

            // Open a blocking modal (settings) and send wheel scroll while pointer is inside filelist.
            app.settings_open = true;
            let raw = egui::RawInput {
                screen_rect: Some(rect),
                events: vec![
                    egui::Event::PointerMoved(Pos2::new(200.0, 160.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: Vec2::new(0.0, -120.0),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Unknown,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| app.paint_content(ui, rect, false));
            out.textures_delta.clear();
            assert_eq!(app.active().scroll_offset, 0.0, "wheel scroll must be blocked while overlay is open");

            // Switch to Icons view and send Ctrl+wheel zoom while modal is still open.
            app.active_mut().view = ViewMode::Icons;
            let raw_zoom = egui::RawInput {
                screen_rect: Some(rect),
                events: vec![
                    egui::Event::PointerMoved(Pos2::new(200.0, 160.0)),
                    egui::Event::ModifiersChanged(egui::Modifiers::CTRL),
                    egui::Event::Zoom(1.5),
                ],
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw_zoom, |ui| app.paint_content(ui, rect, false));
            out.textures_delta.clear();
            assert!((app.active().grid_zoom - 1.0).abs() < 0.001, "grid zoom must be blocked while overlay is open");

            // Close modal and verify wheel scrolling works normally again.
            app.settings_open = false;
            app.active_mut().view = ViewMode::Details;
            let raw_scroll = egui::RawInput {
                screen_rect: Some(rect),
                events: vec![
                    egui::Event::PointerMoved(Pos2::new(200.0, 160.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: Vec2::new(0.0, -120.0),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Unknown,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw_scroll, |ui| app.paint_content(ui, rect, false));
            out.textures_delta.clear();
            assert!(app.active().scroll_offset > 0.0, "wheel scroll must work once overlay closes");
        }

        #[test]
        fn offscreen_inline_rename_auto_finishes_after_initial_focus() {
            let _com = Com::init();
            let (mut app, ctx) = make_app(60);
            let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(720.0, 300.0));
            let row0_path = app.active().entries[0].path.clone().unwrap();

            // Start inline rename on row 0 (visible at scroll_offset = 0).
            app.rename = Some(RenameState {
                row: 0,
                target_path: row0_path,
                buf: "file_000.txt".to_string(),
                focus: true,
                select_all: false,
            });
            let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(rect), ..Default::default() }, |ui| {
                app.paint_content(ui, rect, false)
            });
            out.textures_delta.clear();
            assert!(app.rename.is_some(), "visible rename stays open");
            assert!(!app.rename.as_ref().unwrap().focus, "initial focus flag cleared after first paint");

            // Scroll far down so row 0 is completely outside rows_area (disable springs for instant jump).
            crate::ui::spring::begin_frame(false);
            app.active_mut().scroll_offset = 900.0;
            let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(rect), ..Default::default() }, |ui| {
                app.paint_content(ui, rect, false)
            });
            out.textures_delta.clear();
            assert!(app.rename.is_none(), "scrolling active rename completely offscreen must auto-finish/cancel it");
        }

        #[test]
        fn clicking_filelist_clears_sidebar_focused() {
            let _com = Com::init();
            let (mut app, ctx) = make_app(5);
            let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(720.0, 300.0));
            app.sidebar_focused = true;

            // First pass lays out hit rects, second pass clicks row 0.
            let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(rect), ..Default::default() }, |ui| {
                app.paint_content(ui, rect, false)
            });
            out.textures_delta.clear();

            let click_pos = Pos2::new(80.0, HEADER_H + 14.0);
            let raw_down = egui::RawInput {
                screen_rect: Some(rect),
                events: vec![
                    egui::Event::PointerMoved(click_pos),
                    egui::Event::PointerButton {
                        pos: click_pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw_down, |ui| app.paint_content(ui, rect, false));
            out.textures_delta.clear();

            let raw_up = egui::RawInput {
                screen_rect: Some(rect),
                events: vec![
                    egui::Event::PointerMoved(click_pos),
                    egui::Event::PointerButton {
                        pos: click_pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw_up, |ui| app.paint_content(ui, rect, false));
            out.textures_delta.clear();

            assert!(!app.sidebar_focused, "clicking inside filelist must clear sidebar_focused");
        }

        #[test]
        fn empty_loading_view_paints_loading_indicator_without_panicking() {
            let _com = Com::init();
            let (mut app, ctx) = make_app(0);
            app.active_mut().loading = true;
            let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 320.0));
            let mut out = ctx
                .run_ui(egui::RawInput { screen_rect: Some(rect), time: Some(0.5), ..Default::default() }, |ui| {
                    app.paint_content(ui, rect, false)
                });
            out.textures_delta.clear();
            // Painting both the top bar and centered spinner + text should produce non-trivial shapes.
            assert!(out.shapes.len() >= 3, "empty loading view must paint loading bar and centered indicator");
        }
    }
}
