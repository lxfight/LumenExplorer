//! Windows 11 Fluent 2 custom-drawn top-level context menu.
//!
//! Opens with zero latency (no COM initialization until the user explicitly
//! clicks "显示更多选项" or uses Shift+Right-Click), featuring:
//! - Top compact icon action bar (Cut / Copy / Rename / Copy Path / Delete or Paste / Refresh / Properties)
//! - Context-aware actions (This PC drives, virtual shell items, Quick Look, Undo/Redo, Sidebar items,
//!   Run as Admin, Compress/Extract, Create Shortcut, Recycle Bin Restore/Empty)
//! - Unclipped cascading "新建", "查看", and "排序方式" submenus with diagonal safe-corridor & grace timer
//! - Full keyboard navigation (`ArrowUp`/`ArrowDown`/`ArrowLeft`/`ArrowRight`/`Enter`/`Space`/`Escape`)
//!   with focus trapping so `Tab` and arrows never leak to the background pane
//! - Lazy classic Win32 Shell menu fallback ("显示更多选项  Shift+F10")

use std::path::PathBuf;
use std::time::Duration;

use egui::{CornerRadius, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2};
use windows::Win32::UI::Shell::{
    FOLDERID_ComputerFolder, FOLDERID_NetworkFolder, FOLDERID_Recent, FOLDERID_RecycleBinFolder,
};

use crate::app::{Command, LumenApp, now};
use crate::core::{SortKey, ViewMode};
use crate::shell::archive::is_archive_ext;
use crate::shell::location::Location;
use crate::shell::shellnew::{self, NewTemplate};
use crate::theme::{self, Theme};
use crate::ui::{icons, widgets};

const MENU_W: f32 = 252.0;
const SUB_W: f32 = 224.0;
const ROW_H: f32 = 29.0;
const SEP_H: f32 = 9.0;
const ACTION_BAR_H: f32 = 40.0;
const SUBMENU_GRACE_SECS: f64 = 0.16;
const SUBMENU_CORRIDOR_MARGIN: f32 = 8.0;
const SUBMENU_APEX_BACKOFF: f32 = 4.0;

/// Segoe MDL2 / Fluent Icons E7A7 "Undo".
const ICON_UNDO: &str = "\u{E7A7}";
/// Segoe MDL2 / Fluent Icons E7A6 "Redo".
const ICON_REDO: &str = "\u{E7A6}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SidebarVirtualKind {
    ThisPc,
    Recent,
    Network,
    RecycleBin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ContextKind {
    New,
    Items,
    ThisPcItems,
    VirtualItem,
    Background,
    RecycleBinItems,
    RecycleBinBackground,
    SidebarQuick { path: PathBuf, label: String, pinned_idx: Option<usize>, allow_pin: bool },
    SidebarDrive { path: PathBuf, label: String, is_removable: bool },
    SidebarVirtual { kind: SidebarVirtualKind, label: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubmenuKind {
    New,
    View,
    Sort,
}

pub(crate) struct ContextMenuState {
    pub anchor: Pos2,
    pub kind: ContextKind,
    pub paths: Vec<PathBuf>,
    pub all_folders: bool,
    pub single_file: bool,
    pub all_pinned: bool,
    pub can_run_admin: bool,
    pub single_archive: Option<PathBuf>,
    pub in_search: bool,
    pub has_current_dir: bool,
    pub can_write_dir: bool,
    pub has_pidls: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub has_clipboard_files: bool,
    pub has_clipboard_image: bool,
    open_sub: Option<SubmenuKind>,
    sub_anchor_y: f32,
    last_sub_trigger_pos: Option<Pos2>,
    last_sub_rect: Option<Rect>,
    sub_leave_at: Option<f64>,
    focus_idx: Option<usize>,
    sub_focus_idx: Option<usize>,
    last_pointer_pos: Option<Pos2>,
}

/// Pure classifier mapping active tab & selection flags to a [`ContextKind`].
pub(crate) fn classify_context_kind(is_recycle: bool, is_this_pc: bool, has_sel: bool, has_paths: bool) -> ContextKind {
    if is_recycle {
        if has_sel { ContextKind::RecycleBinItems } else { ContextKind::RecycleBinBackground }
    } else if is_this_pc {
        if has_paths {
            ContextKind::ThisPcItems
        } else if has_sel {
            ContextKind::VirtualItem
        } else {
            ContextKind::Background
        }
    } else if has_paths {
        ContextKind::Items
    } else if has_sel {
        ContextKind::VirtualItem
    } else {
        ContextKind::Background
    }
}

impl ContextMenuState {
    pub fn open_new(app: &LumenApp, anchor: Pos2) -> Self {
        let mut state = Self::open(app, anchor);
        state.kind = ContextKind::New;
        state
    }

    fn base(anchor: Pos2, kind: ContextKind) -> Self {
        Self {
            anchor,
            kind,
            paths: Vec::new(),
            all_folders: false,
            single_file: false,
            all_pinned: false,
            can_run_admin: false,
            single_archive: None,
            in_search: false,
            has_current_dir: false,
            can_write_dir: false,
            has_pidls: false,
            can_undo: false,
            can_redo: false,
            has_clipboard_files: false,
            has_clipboard_image: false,
            open_sub: None,
            sub_anchor_y: anchor.y,
            last_sub_trigger_pos: None,
            last_sub_rect: None,
            sub_leave_at: None,
            focus_idx: None,
            sub_focus_idx: None,
            last_pointer_pos: None,
        }
    }

    pub fn open(app: &LumenApp, anchor: Pos2) -> Self {
        let tab = app.active();
        let is_recycle = tab.is_recycle_bin();
        let is_this_pc = tab.is_this_pc();
        let in_search = app.search_active();
        let has_current_dir = app.current_dir().is_some();
        let can_write_dir = has_current_dir && !in_search;
        let paths = tab.selected_paths();
        let has_pidls = !tab.selected_pidls().is_empty();
        let has_sel = !tab.selection.is_empty();
        let kind = classify_context_kind(is_recycle, is_this_pc, has_sel, !paths.is_empty());

        let selected_entries: Vec<&crate::core::Entry> =
            tab.selection.iter().filter_map(|&idx| tab.entries.get(idx)).collect();
        let all_folders = if !selected_entries.is_empty() {
            selected_entries.iter().all(|e| e.is_folder)
        } else {
            !paths.is_empty() && paths.iter().all(|p| p.is_dir())
        };
        let single_file =
            selected_entries.len() == 1 && !selected_entries[0].is_folder && selected_entries[0].path.is_some();
        let all_pinned = !paths.is_empty()
            && paths.iter().all(|p| {
                let s = p.to_string_lossy();
                app.settings.sidebar_pinned.iter().any(|k| k.eq_ignore_ascii_case(&s))
            });
        let can_run_admin = paths.len() == 1
            && !all_folders
            && paths[0]
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "exe" | "msi" | "bat" | "cmd" | "ps1"));
        let single_archive = if single_file
            && paths.len() == 1
            && paths[0].extension().and_then(|e| e.to_str()).is_some_and(|e| is_archive_ext(&e.to_ascii_lowercase()))
        {
            Some(paths[0].clone())
        } else {
            None
        };
        // Kick off background ShellNew scan if not yet loaded.
        if can_write_dir {
            let _ = shellnew::templates();
        }
        Self {
            anchor,
            kind,
            paths,
            all_folders,
            single_file,
            all_pinned,
            can_run_admin,
            single_archive,
            in_search,
            has_current_dir,
            can_write_dir,
            has_pidls,
            can_undo: app.can_undo(),
            can_redo: app.can_redo(),
            has_clipboard_files: app.clipboard_files,
            has_clipboard_image: can_write_dir && crate::shell::clipboard::has_image(),
            open_sub: None,
            sub_anchor_y: anchor.y,
            last_sub_trigger_pos: None,
            last_sub_rect: None,
            sub_leave_at: None,
            focus_idx: None,
            sub_focus_idx: None,
            last_pointer_pos: None,
        }
    }

    pub fn new_sidebar_quick(
        anchor: Pos2,
        path: PathBuf,
        label: String,
        pinned_idx: Option<usize>,
        allow_pin: bool,
    ) -> Self {
        let mut state =
            Self::base(anchor, ContextKind::SidebarQuick { path: path.clone(), label, pinned_idx, allow_pin });
        state.paths = vec![path];
        state.all_folders = true;
        state.all_pinned = pinned_idx.is_some();
        state
    }

    pub fn new_sidebar_drive(anchor: Pos2, path: PathBuf, label: String, is_removable: bool) -> Self {
        let mut state = Self::base(anchor, ContextKind::SidebarDrive { path: path.clone(), label, is_removable });
        state.paths = vec![path];
        state.all_folders = true;
        state
    }

    pub fn new_sidebar_virtual(anchor: Pos2, kind: SidebarVirtualKind, label: String) -> Self {
        Self::base(anchor, ContextKind::SidebarVirtual { kind, label })
    }

    fn close_submenu(&mut self) {
        self.open_sub = None;
        self.sub_focus_idx = None;
        self.last_sub_rect = None;
        self.last_sub_trigger_pos = None;
        self.sub_leave_at = None;
    }
}

#[derive(Clone, Debug)]
enum CtxAction {
    Cmd(Command),
    Pin(Vec<PathBuf>),
    Unpin(Vec<PathBuf>),
    NewTemplate(NewTemplate),
    ClassicMenu,
    VirtualOpen,
    VirtualProperties,
    NavigatePath(PathBuf),
    OpenPathNewTab(PathBuf),
    CopyPathText(PathBuf),
    OpenTerminalAt(PathBuf),
    PropertiesAt(Vec<PathBuf>),
    ClassicForPaths(Vec<PathBuf>),
    EjectDrive(PathBuf),
    NavigateSidebarVirtual(SidebarVirtualKind),
    OpenSidebarVirtualNewTab(SidebarVirtualKind),
}

enum CtxRow {
    Item { glyph: &'static str, label: String, shortcut: Option<&'static str>, action: CtxAction, enabled: bool },
    Submenu { glyph: &'static str, label: &'static str, kind: SubmenuKind },
    Sep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuKeyAction {
    None,
    CloseAll,
    ActivateMain(usize),
    ActivateSub { kind: SubmenuKind, idx: usize },
}

fn is_row_focusable(row: &CtxRow) -> bool {
    matches!(row, CtxRow::Item { enabled: true, .. } | CtxRow::Submenu { .. })
}

fn step_focus_index(rows: &[CtxRow], current: Option<usize>, delta: isize) -> Option<usize> {
    let focusable: Vec<usize> =
        rows.iter().enumerate().filter_map(|(i, row)| is_row_focusable(row).then_some(i)).collect();
    if focusable.is_empty() {
        return None;
    }
    if let Some(cur) = current
        && let Some(pos) = focusable.iter().position(|&idx| idx == cur)
    {
        let next = (pos as isize + delta).rem_euclid(focusable.len() as isize) as usize;
        return Some(focusable[next]);
    }
    if delta < 0 { focusable.last().copied() } else { focusable.first().copied() }
}

fn step_sub_focus_index(len: usize, current: Option<usize>, delta: isize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    if let Some(cur) = current
        && cur < len
    {
        return Some((cur as isize + delta).rem_euclid(len as isize) as usize);
    }
    Some(if delta < 0 { len - 1 } else { 0 })
}

#[allow(clippy::too_many_arguments)]
fn apply_keyboard_navigation<F, G>(
    state: &mut ContextMenuState,
    rows: &[CtxRow],
    sub_len_fn: F,
    row_top_fn: G,
    escape: bool,
    arrow_up: bool,
    arrow_down: bool,
    arrow_right: bool,
    arrow_left: bool,
    activate: bool,
) -> MenuKeyAction
where
    F: Fn(SubmenuKind) -> usize,
    G: Fn(usize) -> f32,
{
    if escape {
        if let Some(open_sub) = state.open_sub {
            state.close_submenu();
            if let Some(parent_idx) =
                rows.iter().position(|r| matches!(r, CtxRow::Submenu { kind, .. } if *kind == open_sub))
            {
                state.focus_idx = Some(parent_idx);
            }
            return MenuKeyAction::None;
        }
        return MenuKeyAction::CloseAll;
    }
    if arrow_left && let Some(open_sub) = state.open_sub {
        state.close_submenu();
        if let Some(parent_idx) =
            rows.iter().position(|r| matches!(r, CtxRow::Submenu { kind, .. } if *kind == open_sub))
        {
            state.focus_idx = Some(parent_idx);
        }
        return MenuKeyAction::None;
    }
    if arrow_right
        && state.open_sub.is_none()
        && let Some(idx) = state.focus_idx
        && let Some(CtxRow::Submenu { kind, .. }) = rows.get(idx)
    {
        let sub_len = sub_len_fn(*kind);
        state.open_sub = Some(*kind);
        state.sub_anchor_y = row_top_fn(idx);
        state.sub_focus_idx = (sub_len > 0).then_some(0);
        state.last_sub_trigger_pos = None;
        state.sub_leave_at = None;
        return MenuKeyAction::None;
    }
    if arrow_down || arrow_up {
        let delta = if arrow_down { 1 } else { -1 };
        if let Some(sub) = state.open_sub {
            state.sub_focus_idx = step_sub_focus_index(sub_len_fn(sub), state.sub_focus_idx, delta);
        } else {
            state.focus_idx = step_focus_index(rows, state.focus_idx, delta);
        }
        return MenuKeyAction::None;
    }
    if activate {
        if let Some(sub) = state.open_sub {
            if let Some(sub_idx) = state.sub_focus_idx
                && sub_idx < sub_len_fn(sub)
            {
                return MenuKeyAction::ActivateSub { kind: sub, idx: sub_idx };
            }
            return MenuKeyAction::None;
        }
        if let Some(idx) = state.focus_idx {
            match rows.get(idx) {
                Some(CtxRow::Item { enabled: true, .. }) => return MenuKeyAction::ActivateMain(idx),
                Some(CtxRow::Submenu { kind, .. }) => {
                    let sub_len = sub_len_fn(*kind);
                    state.open_sub = Some(*kind);
                    state.sub_anchor_y = row_top_fn(idx);
                    state.sub_focus_idx = (sub_len > 0).then_some(0);
                    state.last_sub_trigger_pos = None;
                    state.sub_leave_at = None;
                }
                _ => {}
            }
        }
    }
    MenuKeyAction::None
}

fn cross_2d(o: Pos2, a: Pos2, b: Pos2) -> f32 {
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
}

/// Inclusive 2D point-in-triangle test independent of vertex winding.
pub(crate) fn point_in_triangle(p: Pos2, a: Pos2, b: Pos2, c: Pos2) -> bool {
    let d1 = cross_2d(p, a, b);
    let d2 = cross_2d(p, b, c);
    let d3 = cross_2d(p, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

/// True when `pointer` lies inside `sub_rect` (including the narrow parent/submenu bridge)
/// or inside the diagonal safe-corridor triangle leading from `trigger` toward `sub_rect`.
pub(crate) fn in_submenu_safe_corridor(pointer: Pos2, trigger: Option<Pos2>, sub_rect: Rect) -> bool {
    if sub_rect.expand2(Vec2::new(4.0, 2.0)).contains(pointer) {
        return true;
    }
    let Some(t) = trigger else { return false };
    let (apex, near_top, near_bottom) = if sub_rect.left() >= t.x {
        (
            Pos2::new(t.x - SUBMENU_APEX_BACKOFF, t.y),
            Pos2::new(sub_rect.left() + 1.0, sub_rect.top() - SUBMENU_CORRIDOR_MARGIN),
            Pos2::new(sub_rect.left() + 1.0, sub_rect.bottom() + SUBMENU_CORRIDOR_MARGIN),
        )
    } else if sub_rect.right() <= t.x {
        (
            Pos2::new(t.x + SUBMENU_APEX_BACKOFF, t.y),
            Pos2::new(sub_rect.right() - 1.0, sub_rect.top() - SUBMENU_CORRIDOR_MARGIN),
            Pos2::new(sub_rect.right() - 1.0, sub_rect.bottom() + SUBMENU_CORRIDOR_MARGIN),
        )
    } else {
        return false;
    };
    point_in_triangle(pointer, apex, near_top, near_bottom)
}

/// True while the open submenu should remain open despite the pointer grazing an adjacent row.
pub(crate) fn should_keep_submenu_open(
    pointer: Option<Pos2>,
    trigger: Option<Pos2>,
    sub_rect: Option<Rect>,
    leave_at: Option<f64>,
    now_secs: f64,
) -> bool {
    if let (Some(p), Some(rect)) = (pointer, sub_rect)
        && in_submenu_safe_corridor(p, trigger, rect)
    {
        return true;
    }
    leave_at.is_some_and(|t| now_secs - t < SUBMENU_GRACE_SECS)
}

fn submenu_rect(parent: Rect, anchor_y: f32, full: Rect, entry_count: usize) -> Rect {
    let h = entry_count as f32 * ROW_H + 12.0;
    let x = if parent.right() + SUB_W + 4.0 <= full.right() {
        parent.right() + 2.0
    } else {
        (parent.left() - SUB_W - 2.0).max(full.left() + 8.0)
    };
    let y = anchor_y.min(full.bottom() - h - 8.0).max(full.top() + 8.0);
    Rect::from_min_size(Pos2::new(x, y), Vec2::new(SUB_W, h))
}

fn has_top_action_bar(state: &ContextMenuState) -> bool {
    matches!(state.kind, ContextKind::Items) || (matches!(state.kind, ContextKind::Background) && state.has_current_dir)
}

fn top_action_bar_items(state: &ContextMenuState) -> Vec<(&'static str, &'static str, Command, bool)> {
    match state.kind {
        ContextKind::Items => vec![
            (icons::CUT, "剪切 (Ctrl+X)", Command::Cut, true),
            (icons::COPY, "复制 (Ctrl+C)", Command::Copy, true),
            (icons::RENAME, "重命名 (F2)", Command::Rename, true),
            (icons::LINK, "复制路径 (Ctrl+Shift+C)", Command::CopyPath, true),
            (icons::DELETE, "删除 (Del)", Command::Delete, true),
        ],
        ContextKind::Background if state.can_write_dir => vec![
            (
                icons::PASTE,
                if state.has_clipboard_files {
                    "粘贴 (Ctrl+V)"
                } else if state.has_clipboard_image {
                    "粘贴图片 (Ctrl+V)"
                } else {
                    "粘贴 (Ctrl+V)"
                },
                Command::Paste,
                state.has_clipboard_files || state.has_clipboard_image,
            ),
            (icons::REFRESH, "刷新 (F5)", Command::Refresh, true),
            (icons::LINK, "复制当前路径", Command::CopyPath, true),
            (icons::INFO, "属性 (Alt+Enter)", Command::Properties, true),
        ],
        ContextKind::Background if state.has_current_dir => vec![
            (icons::REFRESH, "刷新 (F5)", Command::Refresh, true),
            (icons::LINK, "复制当前路径", Command::CopyPath, true),
            (icons::INFO, "属性 (Alt+Enter)", Command::Properties, true),
        ],
        _ => Vec::new(),
    }
}

fn submenu_entries(
    view: ViewMode,
    sort_key: SortKey,
    kind: SubmenuKind,
) -> Vec<(bool, &'static str, String, CtxAction)> {
    let mut entries: Vec<(bool, &'static str, String, CtxAction)> = Vec::new();
    match kind {
        SubmenuKind::New => {
            entries.push((false, icons::FOLDER_NEW, "文件夹".into(), CtxAction::Cmd(Command::NewFolder)));
            entries.push((false, icons::FILE, "文本文档 (.txt)".into(), CtxAction::Cmd(Command::NewFile)));
            for tpl in shellnew::templates() {
                let label = format!("{} (.{})", tpl.label, tpl.ext);
                entries.push((false, icons::FILE, label, CtxAction::NewTemplate(tpl)));
            }
        }
        SubmenuKind::View => {
            entries.push((
                view == ViewMode::Details,
                icons::LIST_BULLETS,
                "详细信息".into(),
                CtxAction::Cmd(Command::ViewDetails),
            ));
            entries.push((view == ViewMode::List, icons::LIST, "列表".into(), CtxAction::Cmd(Command::ViewList)));
            entries.push((view == ViewMode::Tiles, icons::GRID, "平铺".into(), CtxAction::Cmd(Command::ViewTiles)));
            entries.push((view == ViewMode::Icons, icons::VIEW, "大图标".into(), CtxAction::Cmd(Command::ViewIcons)));
        }
        SubmenuKind::Sort => {
            entries.push((
                sort_key == SortKey::Name,
                icons::SORT,
                "名称".into(),
                CtxAction::Cmd(Command::SortBy(SortKey::Name)),
            ));
            entries.push((
                sort_key == SortKey::DateModified,
                icons::HISTORY,
                "修改日期".into(),
                CtxAction::Cmd(Command::SortBy(SortKey::DateModified)),
            ));
            entries.push((
                sort_key == SortKey::Type,
                icons::FILE,
                "类型".into(),
                CtxAction::Cmd(Command::SortBy(SortKey::Type)),
            ));
            entries.push((
                sort_key == SortKey::Size,
                icons::INFO,
                "大小".into(),
                CtxAction::Cmd(Command::SortBy(SortKey::Size)),
            ));
        }
    }
    entries
}

fn sidebar_virtual_location(kind: SidebarVirtualKind) -> anyhow::Result<Location> {
    match kind {
        SidebarVirtualKind::ThisPc => Location::known(&FOLDERID_ComputerFolder),
        SidebarVirtualKind::Recent => Location::known(&FOLDERID_Recent),
        SidebarVirtualKind::Network => Location::known(&FOLDERID_NetworkFolder),
        SidebarVirtualKind::RecycleBin => Location::known(&FOLDERID_RecycleBinFolder),
    }
}

fn close_menu_focus(ctx: &egui::Context, pop_id: egui::Id, blocker_id: egui::Id) {
    crate::ui::spring::clear(ctx, pop_id);
    ctx.memory_mut(|m| {
        if m.has_focus(blocker_id) {
            m.surrender_focus(blocker_id);
        }
    });
}

fn update_pointer_moved(last_pos: &mut Option<Pos2>, current: Option<Pos2>) -> bool {
    let Some(cur) = current else { return false };
    match *last_pos {
        None => {
            *last_pos = Some(cur);
            false
        }
        Some(prev) if cur.distance_sq(prev) > 1.0 => {
            *last_pos = Some(cur);
            true
        }
        Some(_) => false,
    }
}

impl LumenApp {
    pub(crate) fn paint_context_menu(&mut self, ui: &mut egui::Ui, full: Rect) {
        let ctx = ui.ctx().clone();
        let pop_id = egui::Id::unique("ctx-menu-pop");
        let blocker_id = egui::Id::unique("ctx-menu-blocker");
        let Some(mut state) = self.ctx_menu.take() else {
            crate::ui::spring::clear(&ctx, pop_id);
            return;
        };
        let t = self.theme.clone();
        // Capture unclipped painter BEFORE narrowing to the 252px parent menu so
        // cascading submenus ("新建", "查看", "排序方式") paint 100% unclipped!
        let base_painter = ui.painter().with_clip_rect(full);

        let blocker = ui.interact(Rect::EVERYTHING, blocker_id, Sense::click());
        const FOCUS_LOCK: egui::EventFilter =
            egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };
        ctx.memory_mut(|m| {
            if !m.has_focus(blocker_id) {
                m.request_focus(blocker_id);
            }
            m.move_focus(egui::FocusDirection::None);
            m.set_focus_lock_filter(blocker_id, FOCUS_LOCK);
        });

        if blocker.clicked_by(egui::PointerButton::Primary) || blocker.clicked_by(egui::PointerButton::Secondary) {
            close_menu_focus(&ctx, pop_id, blocker_id);
            return;
        }
        // Shift+F10 while menu is open jumps straight to classic Win32 Shell menu.
        if ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::F10)) {
            close_menu_focus(&ctx, pop_id, blocker_id);
            match &state.kind {
                ContextKind::SidebarQuick { path, .. } | ContextKind::SidebarDrive { path, .. } => {
                    crate::shell::menu::show_classic_for_items(std::slice::from_ref(path), true);
                }
                ContextKind::SidebarVirtual { .. } => {}
                _ => self.open_classic_context_menu(true),
            }
            return;
        }

        let rows = build_rows(&state);
        let has_bar = has_top_action_bar(&state);
        let rows_h: f32 = rows.iter().map(|r| if matches!(r, CtxRow::Sep) { SEP_H } else { ROW_H }).sum();
        let total_h = (if has_bar { ACTION_BAR_H + 5.0 } else { 0.0 }) + rows_h + 12.0;

        let mut pos = state.anchor;
        if pos.x + MENU_W > full.right() - 8.0 {
            pos.x = (full.right() - MENU_W - 8.0).max(full.left() + 8.0);
        }
        if pos.y + total_h > full.bottom() - 8.0 {
            pos.y = (full.bottom() - total_h - 8.0).max(full.top() + 8.0);
        }
        // WinUI 3 Fluent clip-unfold reveal: crisp cubic ease-out, zero wobble.
        let reveal = crate::ui::spring::ease_out_reveal(&ctx, pop_id, 0.12);
        let vis_h = (total_h * (0.82 + 0.18 * reveal)).max(24.0);
        let rect = Rect::from_min_size(Pos2::new(pos.x, pos.y - (1.0 - reveal) * 5.0), Vec2::new(MENU_W, total_h));
        let clip_rect = Rect::from_min_size(rect.min, Vec2::new(MENU_W, vis_h));

        // Precompute each row's Y top so keyboard ArrowRight knows the exact anchor immediately.
        let rows_start_y = rect.top() + 6.0 + if has_bar { ACTION_BAR_H + 5.0 } else { 0.0 };
        let mut row_tops: Vec<f32> = Vec::with_capacity(rows.len());
        let mut cursor_y = rows_start_y;
        for row in &rows {
            row_tops.push(cursor_y);
            cursor_y += if matches!(row, CtxRow::Sep) { SEP_H } else { ROW_H };
        }

        // Consume keyboard navigation keys and trap Tab while menu is open.
        let (escape, arrow_up, arrow_down, arrow_right, arrow_left, activate) = ctx.input_mut(|i| {
            let _ = i.consume_key(Modifiers::NONE, Key::Tab);
            let _ = i.consume_key(Modifiers::SHIFT, Key::Tab);
            let esc = i.consume_key(Modifiers::NONE, Key::Escape);
            let up = i.consume_key(Modifiers::NONE, Key::ArrowUp);
            let down = i.consume_key(Modifiers::NONE, Key::ArrowDown);
            let right = i.consume_key(Modifiers::NONE, Key::ArrowRight);
            let left = i.consume_key(Modifiers::NONE, Key::ArrowLeft);
            let act = i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Space);
            (esc, up, down, right, left, act)
        });

        let active_view = self.active().view;
        let active_sort = self.active().sort.key;
        let kbd_action = apply_keyboard_navigation(
            &mut state,
            &rows,
            |sub| submenu_entries(active_view, active_sort, sub).len(),
            |idx| row_tops.get(idx).copied().unwrap_or(rows_start_y),
            escape,
            arrow_up,
            arrow_down,
            arrow_right,
            arrow_left,
            activate,
        );

        let mut chosen: Option<CtxAction> = None;
        match kbd_action {
            MenuKeyAction::CloseAll => {
                close_menu_focus(&ctx, pop_id, blocker_id);
                return;
            }
            MenuKeyAction::ActivateMain(idx) => {
                if let Some(CtxRow::Item { action, enabled: true, .. }) = rows.get(idx) {
                    chosen = Some(action.clone());
                }
            }
            MenuKeyAction::ActivateSub { kind, idx } => {
                let entries = submenu_entries(active_view, active_sort, kind);
                if let Some((_, _, _, action)) = entries.into_iter().nth(idx) {
                    chosen = Some(action);
                }
            }
            MenuKeyAction::None => {}
        }

        // Track genuine pointer movement so a stationary cursor never fights keyboard arrows,
        // while still detecting slow high-refresh-rate movement by only advancing the anchor on move.
        let pointer_pos = ctx.pointer_hover_pos();
        let pointer_moved = update_pointer_moved(&mut state.last_pointer_pos, pointer_pos);

        let now_secs = now();
        // Keep last_sub_rect up-to-date before main-row hover checks.
        if let Some(sub) = state.open_sub {
            let entry_count = submenu_entries(active_view, active_sort, sub).len();
            state.last_sub_rect = Some(submenu_rect(rect, state.sub_anchor_y, full, entry_count));
        }
        let pointer_in_sub =
            pointer_pos.zip(state.last_sub_rect).is_some_and(|(p, r)| r.expand2(Vec2::new(4.0, 2.0)).contains(p));
        if pointer_in_sub && state.open_sub.is_some() {
            state.sub_leave_at = None;
            state.last_sub_trigger_pos = None;
        }

        // Soft drop shadow + Fluent surface.
        base_painter.rect_filled(
            clip_rect.translate(Vec2::new(0.0, 4.0)).expand(2.0),
            CornerRadius::same(10),
            widgets::fade(t.shadow, reveal),
        );
        base_painter.rect_filled(clip_rect, CornerRadius::same(8), widgets::fade(t.popup, reveal));
        base_painter.rect_stroke(
            clip_rect,
            CornerRadius::same(8),
            Stroke::new(1.0, widgets::fade(t.border, reveal)),
            egui::StrokeKind::Inside,
        );
        let painter = base_painter.with_clip_rect(clip_rect);
        let _ = ui.interact(rect, egui::Id::unique("ctx-menu-surface"), Sense::click());

        let mut y = rect.top() + 6.0;

        // ---- Top Windows 11 compact icon action bar -------------------------
        if has_bar {
            let bar =
                Rect::from_min_size(Pos2::new(rect.left() + 6.0, y), Vec2::new(MENU_W - 12.0, ACTION_BAR_H - 4.0));
            let bar_items = top_action_bar_items(&state);
            let btn_w = bar.width() / bar_items.len().max(1) as f32;
            for (i, (glyph, tip, cmd, enabled)) in bar_items.into_iter().enumerate() {
                let cell = Rect::from_min_size(
                    Pos2::new(bar.left() + i as f32 * btn_w, bar.top()),
                    Vec2::new(btn_w - 2.0, bar.height()),
                );
                let id = egui::Id::unique(("ctx-bar", i));
                let resp = ui.interact(cell, id, Sense::click());
                let hover = widgets::hover_t(&ctx, id, resp.hovered() && enabled);
                if hover > 0.0 {
                    painter.rect_filled(cell, CornerRadius::same(5), widgets::fade(t.hover_strong, hover));
                }
                let color = if !enabled {
                    t.text_disabled
                } else if cmd == Command::Delete {
                    t.danger
                } else {
                    t.text
                };
                widgets::icon_at(&painter, cell.center(), glyph, 15.0, color);
                if enabled && resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.hovered() {
                    let _ = resp.clone().on_hover_text(tip);
                }
                if enabled && resp.clicked() {
                    chosen = Some(CtxAction::Cmd(cmd));
                }
            }
            y += ACTION_BAR_H;
            painter.line_segment(
                [Pos2::new(rect.left() + 10.0, y), Pos2::new(rect.right() - 10.0, y)],
                Stroke::new(1.0, t.divider),
            );
            y += 5.0;
        }

        // ---- Main rows ------------------------------------------------------
        let mut hovered_other_sub: Option<(SubmenuKind, f32, usize)> = None;
        let mut hovered_active_trigger = false;
        let mut hovered_plain_item = false;

        for (idx, row_item) in rows.into_iter().enumerate() {
            match row_item {
                CtxRow::Sep => {
                    painter.line_segment(
                        [Pos2::new(rect.left() + 10.0, y + 4.0), Pos2::new(rect.right() - 10.0, y + 4.0)],
                        Stroke::new(1.0, t.divider),
                    );
                    y += SEP_H;
                }
                CtxRow::Submenu { glyph, label, kind } => {
                    let row = Rect::from_min_size(Pos2::new(rect.left() + 5.0, y), Vec2::new(MENU_W - 10.0, ROW_H));
                    let id = egui::Id::unique(("ctx-sub", idx));
                    let resp = ui.interact(row, id, Sense::click());
                    let is_open = state.open_sub == Some(kind);
                    let is_focused = state.focus_idx == Some(idx);
                    let hover = widgets::hover_t(&ctx, id, resp.hovered() || is_open);
                    if is_focused || is_open {
                        painter.rect_filled(row, CornerRadius::same(5), t.selected);
                        if is_focused && state.open_sub.is_none() {
                            widgets::stroke_rect(
                                &painter,
                                row.shrink(0.5),
                                5,
                                Stroke::new(1.0, widgets::fade(t.accent, 0.75)),
                            );
                        }
                    } else if hover > 0.0 {
                        painter.rect_filled(row, CornerRadius::same(5), widgets::fade(t.hover_strong, hover));
                    }
                    widgets::icon_at(
                        &painter,
                        Pos2::new(row.left() + 16.0, row.center().y),
                        glyph,
                        13.5,
                        if is_open || is_focused { t.accent } else { t.text_secondary },
                    );
                    widgets::text_left(
                        ui,
                        &painter,
                        Rect::from_min_max(
                            Pos2::new(row.left() + 34.0, row.top()),
                            Pos2::new(row.right() - 24.0, row.bottom()),
                        ),
                        label,
                        theme::font(12.5),
                        t.text,
                        0.0,
                    );
                    widgets::icon_at(
                        &painter,
                        Pos2::new(row.right() - 12.0, row.center().y),
                        icons::CHEVRON_RIGHT,
                        8.5,
                        t.text_tertiary,
                    );
                    if resp.hovered() {
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                        if is_open {
                            hovered_active_trigger = true;
                            if pointer_moved {
                                state.last_sub_trigger_pos = pointer_pos;
                                state.sub_leave_at = None;
                                state.focus_idx = Some(idx);
                            }
                        } else {
                            hovered_other_sub = Some((kind, row.top(), idx));
                        }
                    }
                    if resp.clicked() {
                        state.open_sub = Some(kind);
                        state.sub_anchor_y = row.top();
                        state.focus_idx = Some(idx);
                        state.sub_focus_idx = None;
                        state.last_sub_trigger_pos = pointer_pos;
                        state.sub_leave_at = None;
                        let entry_count = submenu_entries(active_view, active_sort, kind).len();
                        state.last_sub_rect = Some(submenu_rect(rect, row.top(), full, entry_count));
                    }
                    y += ROW_H;
                }
                CtxRow::Item { glyph, label, shortcut, action, enabled } => {
                    let row = Rect::from_min_size(Pos2::new(rect.left() + 5.0, y), Vec2::new(MENU_W - 10.0, ROW_H));
                    let id = egui::Id::unique(("ctx-row", idx));
                    let resp = ui.interact(row, id, Sense::click());
                    let is_focused = enabled && state.open_sub.is_none() && state.focus_idx == Some(idx);
                    let in_corridor = state.open_sub.is_some()
                        && should_keep_submenu_open(
                            pointer_pos,
                            state.last_sub_trigger_pos,
                            state.last_sub_rect,
                            state.sub_leave_at,
                            now_secs,
                        );
                    let hover = widgets::hover_t(&ctx, id, resp.hovered() && enabled && !in_corridor);
                    if is_focused {
                        painter.rect_filled(row, CornerRadius::same(5), t.selected);
                        widgets::stroke_rect(
                            &painter,
                            row.shrink(0.5),
                            5,
                            Stroke::new(1.0, widgets::fade(t.accent, 0.75)),
                        );
                    } else if hover > 0.0 {
                        painter.rect_filled(row, CornerRadius::same(5), widgets::fade(t.hover_strong, hover));
                    }
                    let is_danger = matches!(action, CtxAction::Cmd(Command::PermanentDelete));
                    let text_col = if !enabled {
                        t.text_disabled
                    } else if is_danger {
                        t.danger
                    } else {
                        t.text
                    };
                    let icon_col = if !enabled {
                        t.text_disabled
                    } else if is_danger {
                        t.danger
                    } else {
                        t.text_secondary
                    };
                    widgets::icon_at(&painter, Pos2::new(row.left() + 16.0, row.center().y), glyph, 13.5, icon_col);
                    let right_pad = if shortcut.is_some() { 76.0 } else { 10.0 };
                    widgets::text_left(
                        ui,
                        &painter,
                        Rect::from_min_max(
                            Pos2::new(row.left() + 34.0, row.top()),
                            Pos2::new(row.right() - right_pad, row.bottom()),
                        ),
                        &label,
                        theme::font(12.5),
                        text_col,
                        0.0,
                    );
                    if let Some(sc) = shortcut {
                        widgets::text_right(
                            ui,
                            &painter,
                            Rect::from_min_max(
                                Pos2::new(row.right() - 80.0, row.top()),
                                Pos2::new(row.right() - 10.0, row.bottom()),
                            ),
                            sc,
                            theme::font(11.0),
                            if enabled { t.text_tertiary } else { t.text_disabled },
                        );
                    }
                    if resp.hovered() {
                        hovered_plain_item = true;
                        if enabled {
                            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                            if pointer_moved && !in_corridor && state.open_sub.is_none() {
                                state.focus_idx = Some(idx);
                            }
                        }
                    }
                    if enabled && resp.clicked() {
                        chosen = Some(action);
                    }
                    y += ROW_H;
                }
            }
        }

        // ---- Submenu hover transitions with diagonal safe-corridor + grace timer ----
        if let Some((other_kind, other_top, other_idx)) = hovered_other_sub {
            if state.open_sub.is_none() {
                if pointer_moved {
                    state.open_sub = Some(other_kind);
                    state.sub_anchor_y = other_top;
                    state.focus_idx = Some(other_idx);
                    state.sub_focus_idx = None;
                    state.last_sub_trigger_pos = pointer_pos;
                    state.sub_leave_at = None;
                    let entry_count = submenu_entries(active_view, active_sort, other_kind).len();
                    state.last_sub_rect = Some(submenu_rect(rect, other_top, full, entry_count));
                }
            } else if pointer_moved || state.sub_leave_at.is_some() {
                if state.sub_leave_at.is_none() {
                    state.sub_leave_at = Some(now_secs);
                }
                if should_keep_submenu_open(
                    pointer_pos,
                    state.last_sub_trigger_pos,
                    state.last_sub_rect,
                    state.sub_leave_at,
                    now_secs,
                ) {
                    if let Some(leave_at) = state.sub_leave_at {
                        let rem = SUBMENU_GRACE_SECS - (now_secs - leave_at);
                        if rem > 0.0 {
                            ctx.request_repaint_after(Duration::from_secs_f64(rem.max(0.01)));
                        }
                    }
                } else {
                    state.open_sub = Some(other_kind);
                    state.sub_anchor_y = other_top;
                    state.focus_idx = Some(other_idx);
                    state.sub_focus_idx = None;
                    state.last_sub_trigger_pos = pointer_pos;
                    state.sub_leave_at = None;
                    let entry_count = submenu_entries(active_view, active_sort, other_kind).len();
                    state.last_sub_rect = Some(submenu_rect(rect, other_top, full, entry_count));
                }
            }
        } else if state.open_sub.is_some() && !hovered_active_trigger && !pointer_in_sub {
            if hovered_plain_item && (pointer_moved || state.sub_leave_at.is_some()) {
                if state.sub_leave_at.is_none() {
                    state.sub_leave_at = Some(now_secs);
                }
                if should_keep_submenu_open(
                    pointer_pos,
                    state.last_sub_trigger_pos,
                    state.last_sub_rect,
                    state.sub_leave_at,
                    now_secs,
                ) {
                    if let Some(leave_at) = state.sub_leave_at {
                        let rem = SUBMENU_GRACE_SECS - (now_secs - leave_at);
                        if rem > 0.0 {
                            ctx.request_repaint_after(Duration::from_secs_f64(rem.max(0.01)));
                        }
                    }
                } else {
                    state.close_submenu();
                }
            } else if let Some(leave_at) = state.sub_leave_at {
                if should_keep_submenu_open(
                    pointer_pos,
                    state.last_sub_trigger_pos,
                    state.last_sub_rect,
                    Some(leave_at),
                    now_secs,
                ) {
                    let rem = SUBMENU_GRACE_SECS - (now_secs - leave_at);
                    if rem > 0.0 {
                        ctx.request_repaint_after(Duration::from_secs_f64(rem.max(0.01)));
                    }
                } else {
                    state.close_submenu();
                }
            }
        }

        // ---- Unclipped cascading submenu ------------------------------------
        if chosen.is_none()
            && let Some(sub) = state.open_sub
            && let Some(sub_action) =
                self.paint_context_submenu(ui, &base_painter, &t, rect, full, sub, pointer_moved, &mut state)
        {
            chosen = Some(sub_action);
        }

        if let Some(action) = chosen {
            close_menu_focus(&ctx, pop_id, blocker_id);
            self.dispatch_context_action(action);
        } else {
            self.ctx_menu = Some(state);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_context_submenu(
        &self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        t: &Theme,
        parent: Rect,
        full: Rect,
        kind: SubmenuKind,
        pointer_moved: bool,
        state: &mut ContextMenuState,
    ) -> Option<CtxAction> {
        let ctx = ui.ctx().clone();
        let entries = submenu_entries(self.active().view, self.active().sort.key, kind);
        let rect = submenu_rect(parent, state.sub_anchor_y, full, entries.len());
        state.last_sub_rect = Some(rect);
        painter.rect_filled(rect.translate(Vec2::new(0.0, 4.0)).expand(2.0), CornerRadius::same(10), t.shadow);
        painter.rect_filled(rect, CornerRadius::same(8), t.popup);
        painter.rect_stroke(rect, CornerRadius::same(8), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
        let _ = ui.interact(rect, egui::Id::unique("ctx-sub-bg"), Sense::click());

        let mut chosen = None;
        for (i, (checked, glyph, label, action)) in entries.into_iter().enumerate() {
            let row = Rect::from_min_size(
                Pos2::new(rect.left() + 5.0, rect.top() + 6.0 + i as f32 * ROW_H),
                Vec2::new(SUB_W - 10.0, ROW_H),
            );
            let id = egui::Id::unique(("ctx-sub-item", i));
            let resp = ui.interact(row, id, Sense::click());
            if pointer_moved && resp.hovered() {
                state.sub_focus_idx = Some(i);
            }
            let is_focused = state.sub_focus_idx == Some(i);
            let hover = widgets::hover_t(&ctx, id, resp.hovered());
            if is_focused {
                painter.rect_filled(row, CornerRadius::same(5), t.selected);
                widgets::stroke_rect(painter, row.shrink(0.5), 5, Stroke::new(1.0, widgets::fade(t.accent, 0.75)));
            } else if hover > 0.0 {
                painter.rect_filled(row, CornerRadius::same(5), widgets::fade(t.hover_strong, hover));
            }
            let icon_glyph = if checked { icons::CHECK } else { glyph };
            let icon_col = if checked { t.accent } else { t.text_secondary };
            widgets::icon_at(painter, Pos2::new(row.left() + 16.0, row.center().y), icon_glyph, 12.5, icon_col);
            widgets::text_left(
                ui,
                painter,
                Rect::from_min_max(Pos2::new(row.left() + 34.0, row.top()), Pos2::new(row.right() - 8.0, row.bottom())),
                &label,
                theme::font(12.5),
                t.text,
                0.0,
            );
            if resp.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                chosen = Some(action);
            }
        }
        chosen
    }

    fn dispatch_context_action(&mut self, action: CtxAction) {
        match action {
            CtxAction::Cmd(cmd) => self.command(cmd),
            CtxAction::Pin(paths) => {
                for path in paths {
                    let key = path.to_string_lossy().to_string();
                    if !self.settings.sidebar_pinned.iter().any(|k| k.eq_ignore_ascii_case(&key)) {
                        self.settings.sidebar_pinned.push(key);
                    }
                }
                self.reload_sidebar();
            }
            CtxAction::Unpin(paths) => {
                for path in paths {
                    let key = path.to_string_lossy();
                    self.settings.sidebar_pinned.retain(|k| !k.eq_ignore_ascii_case(&key));
                }
                self.reload_sidebar();
            }
            CtxAction::NewTemplate(tpl) => {
                if let Some(dir) = self.current_dir() {
                    match shellnew::create_from_template(&dir, &tpl) {
                        Ok((created, name)) => {
                            self.push_undo(crate::app::UndoEntry::Created { paths: vec![created] });
                            self.active_mut().pending_select = Some((name, true));
                            self.reload_active();
                        }
                        Err(err) => self.toast_error(format!("{err:#}")),
                    }
                }
            }
            CtxAction::ClassicMenu => self.open_classic_context_menu(false),
            CtxAction::VirtualOpen => {
                if let Some(loc) = self.active().open_target() {
                    self.navigate_to(loc);
                } else {
                    let pidls = self.active().selected_pidls();
                    if !pidls.is_empty()
                        && let Err(err) = crate::shell::ops::invoke_pidl_verb(&pidls, "open")
                    {
                        self.toast_error(format!("{err:#}"));
                    }
                }
            }
            CtxAction::VirtualProperties => {
                let pidls = self.active().selected_pidls();
                if !pidls.is_empty()
                    && let Err(err) = crate::shell::ops::invoke_pidl_verb(&pidls, "properties")
                {
                    self.toast_error(format!("{err:#}"));
                }
            }
            CtxAction::NavigatePath(path) => self.navigate_to_path(&path),
            CtxAction::OpenPathNewTab(path) => {
                self.command(Command::NewTab);
                self.navigate_to_path(&path);
            }
            CtxAction::CopyPathText(path) => match crate::shell::clipboard::set_text(&path.to_string_lossy()) {
                Ok(()) => self.toast("路径已复制"),
                Err(err) => self.toast_error(format!("{err:#}")),
            },
            CtxAction::OpenTerminalAt(path) => {
                if let Err(err) = crate::shell::ops::open_terminal(&path) {
                    self.toast_error(format!("{err:#}"));
                }
            }
            CtxAction::PropertiesAt(paths) => {
                if let Err(err) = crate::shell::ops::properties(&paths) {
                    self.toast_error(format!("{err:#}"));
                }
            }
            CtxAction::ClassicForPaths(paths) => {
                crate::shell::menu::show_classic_for_items(&paths, false);
            }
            CtxAction::EjectDrive(path) => match crate::shell::ops::eject_drive(&path) {
                Ok(()) => {
                    self.toast("已发出安全弹出请求");
                    self.reload_sidebar();
                }
                Err(err) => self.toast_error(format!("{err:#}")),
            },
            CtxAction::NavigateSidebarVirtual(kind) => match sidebar_virtual_location(kind) {
                Ok(loc) => self.navigate_to(loc),
                Err(err) => self.toast_error(format!("{err:#}")),
            },
            CtxAction::OpenSidebarVirtualNewTab(kind) => match sidebar_virtual_location(kind) {
                Ok(loc) => {
                    self.command(Command::NewTab);
                    self.navigate_to(loc);
                }
                Err(err) => self.toast_error(format!("{err:#}")),
            },
        }
    }
}

fn new_entries() -> Vec<(bool, &'static str, String, CtxAction)> {
    let mut entries = vec![
        (false, icons::FOLDER_NEW, "文件夹".into(), CtxAction::Cmd(Command::NewFolder)),
        (false, icons::FILE, "文本文档 (.txt)".into(), CtxAction::Cmd(Command::NewFile)),
    ];
    for tpl in shellnew::templates() {
        entries.push((false, icons::FILE, format!("{} (.{})", tpl.label, tpl.ext), CtxAction::NewTemplate(tpl)));
    }
    entries
}

fn build_rows(state: &ContextMenuState) -> Vec<CtxRow> {
    let mut rows = Vec::new();
    match &state.kind {
        ContextKind::New => {
            rows.extend(new_entries().into_iter().map(|(_, glyph, label, action)| CtxRow::Item {
                glyph,
                label,
                shortcut: None,
                action,
                enabled: true,
            }));
        }
        ContextKind::Items => {
            rows.push(CtxRow::Item {
                glyph: icons::OPEN_EXTERNAL,
                label: "打开".into(),
                shortcut: Some("Enter"),
                action: CtxAction::Cmd(Command::Open),
                enabled: true,
            });
            if state.single_file {
                rows.push(CtxRow::Item {
                    glyph: icons::VIEW,
                    label: "快速预览".into(),
                    shortcut: Some("Space"),
                    action: CtxAction::Cmd(Command::ToggleQuickLook),
                    enabled: true,
                });
            }
            if state.all_folders {
                rows.push(CtxRow::Item {
                    glyph: icons::ADD,
                    label: "在新标签页中打开".into(),
                    shortcut: None,
                    action: CtxAction::Cmd(Command::OpenNewTab),
                    enabled: true,
                });
            } else {
                rows.push(CtxRow::Item {
                    glyph: icons::OPEN_EXTERNAL,
                    label: "打开方式…".into(),
                    shortcut: None,
                    action: CtxAction::Cmd(Command::OpenWith),
                    enabled: true,
                });
            }
            if state.can_run_admin {
                rows.push(CtxRow::Item {
                    glyph: icons::SETTINGS,
                    label: "以管理员身份运行".into(),
                    shortcut: None,
                    action: CtxAction::Cmd(Command::RunAsAdmin),
                    enabled: true,
                });
            }
            if state.in_search {
                rows.push(CtxRow::Item {
                    glyph: icons::FOLDER,
                    label: "打开文件所在位置".into(),
                    shortcut: None,
                    action: CtxAction::Cmd(Command::ShowInFolder),
                    enabled: true,
                });
            }
            rows.push(CtxRow::Sep);
            if state.all_folders {
                if state.all_pinned {
                    rows.push(CtxRow::Item {
                        glyph: icons::PIN,
                        label: "从快速访问取消固定".into(),
                        shortcut: None,
                        action: CtxAction::Unpin(state.paths.clone()),
                        enabled: true,
                    });
                } else {
                    rows.push(CtxRow::Item {
                        glyph: icons::PIN,
                        label: "固定到快速访问".into(),
                        shortcut: None,
                        action: CtxAction::Pin(state.paths.clone()),
                        enabled: true,
                    });
                }
                rows.push(CtxRow::Item {
                    glyph: icons::EDIT,
                    label: "在终端中打开".into(),
                    shortcut: None,
                    action: CtxAction::Cmd(Command::OpenTerminal),
                    enabled: true,
                });
            }
            rows.push(CtxRow::Item {
                glyph: icons::SAVE,
                label: "压缩为 ZIP 文件".into(),
                shortcut: None,
                action: CtxAction::Cmd(Command::CompressZip),
                enabled: !state.in_search,
            });
            if let Some(archive) = &state.single_archive {
                let stem =
                    archive.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "文件夹".into());
                rows.push(CtxRow::Item {
                    glyph: icons::DOWNLOAD,
                    label: format!("解压到 “{stem}\\”"),
                    shortcut: None,
                    action: CtxAction::Cmd(Command::ExtractSubfolder),
                    enabled: true,
                });
                rows.push(CtxRow::Item {
                    glyph: icons::DOWNLOAD,
                    label: "解压到当前文件夹".into(),
                    shortcut: None,
                    action: CtxAction::Cmd(Command::ExtractHere),
                    enabled: true,
                });
            }
            rows.push(CtxRow::Item {
                glyph: icons::LINK,
                label: "创建快捷方式".into(),
                shortcut: None,
                action: CtxAction::Cmd(Command::CreateShortcut),
                enabled: !state.in_search,
            });
            rows.push(CtxRow::Sep);
            rows.push(CtxRow::Item {
                glyph: icons::INFO,
                label: "属性".into(),
                shortcut: Some("Alt+Enter"),
                action: CtxAction::Cmd(Command::Properties),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::MORE,
                label: "显示更多选项".into(),
                shortcut: Some("Shift+F10"),
                action: CtxAction::ClassicMenu,
                enabled: true,
            });
        }
        ContextKind::ThisPcItems => {
            rows.push(CtxRow::Item {
                glyph: icons::OPEN_EXTERNAL,
                label: "打开".into(),
                shortcut: Some("Enter"),
                action: CtxAction::Cmd(Command::Open),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::ADD,
                label: "在新标签页中打开".into(),
                shortcut: None,
                action: CtxAction::Cmd(Command::OpenNewTab),
                enabled: true,
            });
            rows.push(CtxRow::Sep);
            rows.push(CtxRow::Item {
                glyph: icons::LINK,
                label: "复制路径".into(),
                shortcut: Some("Ctrl+Shift+C"),
                action: CtxAction::Cmd(Command::CopyPath),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::EDIT,
                label: "在终端中打开".into(),
                shortcut: None,
                action: CtxAction::Cmd(Command::OpenTerminal),
                enabled: true,
            });
            rows.push(CtxRow::Sep);
            rows.push(CtxRow::Item {
                glyph: icons::INFO,
                label: "属性".into(),
                shortcut: Some("Alt+Enter"),
                action: CtxAction::Cmd(Command::Properties),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::MORE,
                label: "显示更多选项".into(),
                shortcut: Some("Shift+F10"),
                action: CtxAction::ClassicMenu,
                enabled: true,
            });
        }
        ContextKind::VirtualItem => {
            rows.push(CtxRow::Item {
                glyph: icons::OPEN_EXTERNAL,
                label: "打开".into(),
                shortcut: Some("Enter"),
                action: CtxAction::VirtualOpen,
                enabled: true,
            });
            if state.all_folders {
                rows.push(CtxRow::Item {
                    glyph: icons::ADD,
                    label: "在新标签页中打开".into(),
                    shortcut: None,
                    action: CtxAction::Cmd(Command::OpenNewTab),
                    enabled: true,
                });
            }
            rows.push(CtxRow::Sep);
            rows.push(CtxRow::Item {
                glyph: icons::INFO,
                label: "属性".into(),
                shortcut: Some("Alt+Enter"),
                action: CtxAction::VirtualProperties,
                enabled: state.has_pidls,
            });
            rows.push(CtxRow::Item {
                glyph: icons::MORE,
                label: "显示更多选项".into(),
                shortcut: Some("Shift+F10"),
                action: CtxAction::ClassicMenu,
                enabled: state.has_pidls,
            });
        }
        ContextKind::Background => {
            rows.push(CtxRow::Submenu { glyph: icons::VIEW, label: "查看", kind: SubmenuKind::View });
            rows.push(CtxRow::Submenu { glyph: icons::SORT, label: "排序方式", kind: SubmenuKind::Sort });
            if state.can_write_dir {
                rows.push(CtxRow::Sep);
                rows.push(CtxRow::Submenu { glyph: icons::ADD, label: "新建", kind: SubmenuKind::New });
                if state.has_clipboard_image {
                    rows.push(CtxRow::Item {
                        glyph: icons::PASTE,
                        label: "粘贴剪贴板图片为 PNG".into(),
                        shortcut: None,
                        action: CtxAction::Cmd(Command::PasteImage),
                        enabled: true,
                    });
                }
                rows.push(CtxRow::Item {
                    glyph: icons::EDIT,
                    label: "在终端中打开".into(),
                    shortcut: None,
                    action: CtxAction::Cmd(Command::OpenTerminal),
                    enabled: true,
                });
            } else if !state.has_current_dir {
                rows.push(CtxRow::Sep);
                rows.push(CtxRow::Item {
                    glyph: icons::REFRESH,
                    label: "刷新".into(),
                    shortcut: Some("F5"),
                    action: CtxAction::Cmd(Command::Refresh),
                    enabled: true,
                });
            }
            if state.can_undo || state.can_redo {
                rows.push(CtxRow::Sep);
                if state.can_undo {
                    rows.push(CtxRow::Item {
                        glyph: ICON_UNDO,
                        label: "撤销".into(),
                        shortcut: Some("Ctrl+Z"),
                        action: CtxAction::Cmd(Command::Undo),
                        enabled: true,
                    });
                }
                if state.can_redo {
                    rows.push(CtxRow::Item {
                        glyph: ICON_REDO,
                        label: "重做".into(),
                        shortcut: Some("Ctrl+Y"),
                        action: CtxAction::Cmd(Command::Redo),
                        enabled: true,
                    });
                }
            }
            if state.has_current_dir {
                rows.push(CtxRow::Sep);
                rows.push(CtxRow::Item {
                    glyph: icons::MORE,
                    label: "显示更多选项".into(),
                    shortcut: Some("Shift+F10"),
                    action: CtxAction::ClassicMenu,
                    enabled: true,
                });
            }
        }
        ContextKind::RecycleBinItems => {
            rows.push(CtxRow::Item {
                glyph: icons::HISTORY,
                label: "还原所选项目".into(),
                shortcut: None,
                action: CtxAction::Cmd(Command::RestoreSelected),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::DELETE,
                label: "永久删除".into(),
                shortcut: Some("Del"),
                action: CtxAction::Cmd(Command::PermanentDelete),
                enabled: true,
            });
            rows.push(CtxRow::Sep);
            rows.push(CtxRow::Item {
                glyph: icons::DELETE,
                label: "清空回收站".into(),
                shortcut: None,
                action: CtxAction::Cmd(Command::EmptyRecycleBin),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::INFO,
                label: "属性".into(),
                shortcut: Some("Alt+Enter"),
                action: CtxAction::Cmd(Command::Properties),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::MORE,
                label: "显示更多选项".into(),
                shortcut: Some("Shift+F10"),
                action: CtxAction::ClassicMenu,
                enabled: true,
            });
        }
        ContextKind::RecycleBinBackground => {
            rows.push(CtxRow::Item {
                glyph: icons::DELETE,
                label: "清空回收站".into(),
                shortcut: None,
                action: CtxAction::Cmd(Command::EmptyRecycleBin),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::REFRESH,
                label: "刷新".into(),
                shortcut: Some("F5"),
                action: CtxAction::Cmd(Command::Refresh),
                enabled: true,
            });
        }
        ContextKind::SidebarQuick { path, pinned_idx, allow_pin, .. } => {
            rows.push(CtxRow::Item {
                glyph: icons::OPEN_EXTERNAL,
                label: "打开".into(),
                shortcut: None,
                action: CtxAction::NavigatePath(path.clone()),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::ADD,
                label: "在新标签页中打开".into(),
                shortcut: None,
                action: CtxAction::OpenPathNewTab(path.clone()),
                enabled: true,
            });
            rows.push(CtxRow::Sep);
            if pinned_idx.is_some() {
                rows.push(CtxRow::Item {
                    glyph: icons::PIN,
                    label: "从快速访问取消固定".into(),
                    shortcut: None,
                    action: CtxAction::Unpin(vec![path.clone()]),
                    enabled: true,
                });
            } else if *allow_pin {
                rows.push(CtxRow::Item {
                    glyph: icons::PIN,
                    label: "固定到快速访问".into(),
                    shortcut: None,
                    action: CtxAction::Pin(vec![path.clone()]),
                    enabled: true,
                });
            }
            rows.push(CtxRow::Item {
                glyph: icons::LINK,
                label: "复制路径".into(),
                shortcut: None,
                action: CtxAction::CopyPathText(path.clone()),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::EDIT,
                label: "在终端中打开".into(),
                shortcut: None,
                action: CtxAction::OpenTerminalAt(path.clone()),
                enabled: true,
            });
            rows.push(CtxRow::Sep);
            rows.push(CtxRow::Item {
                glyph: icons::INFO,
                label: "属性".into(),
                shortcut: None,
                action: CtxAction::PropertiesAt(vec![path.clone()]),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::MORE,
                label: "显示更多选项".into(),
                shortcut: Some("Shift+F10"),
                action: CtxAction::ClassicForPaths(vec![path.clone()]),
                enabled: true,
            });
        }
        ContextKind::SidebarDrive { path, is_removable, .. } => {
            rows.push(CtxRow::Item {
                glyph: icons::OPEN_EXTERNAL,
                label: "打开".into(),
                shortcut: None,
                action: CtxAction::NavigatePath(path.clone()),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::ADD,
                label: "在新标签页中打开".into(),
                shortcut: None,
                action: CtxAction::OpenPathNewTab(path.clone()),
                enabled: true,
            });
            rows.push(CtxRow::Sep);
            if *is_removable {
                rows.push(CtxRow::Item {
                    glyph: icons::ARROW_UP,
                    label: "安全弹出".into(),
                    shortcut: None,
                    action: CtxAction::EjectDrive(path.clone()),
                    enabled: true,
                });
            }
            rows.push(CtxRow::Item {
                glyph: icons::LINK,
                label: "复制路径".into(),
                shortcut: None,
                action: CtxAction::CopyPathText(path.clone()),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::EDIT,
                label: "在终端中打开".into(),
                shortcut: None,
                action: CtxAction::OpenTerminalAt(path.clone()),
                enabled: true,
            });
            rows.push(CtxRow::Sep);
            rows.push(CtxRow::Item {
                glyph: icons::INFO,
                label: "属性".into(),
                shortcut: None,
                action: CtxAction::PropertiesAt(vec![path.clone()]),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::MORE,
                label: "显示更多选项".into(),
                shortcut: Some("Shift+F10"),
                action: CtxAction::ClassicForPaths(vec![path.clone()]),
                enabled: true,
            });
        }
        ContextKind::SidebarVirtual { kind, .. } => {
            rows.push(CtxRow::Item {
                glyph: icons::OPEN_EXTERNAL,
                label: "打开".into(),
                shortcut: None,
                action: CtxAction::NavigateSidebarVirtual(*kind),
                enabled: true,
            });
            rows.push(CtxRow::Item {
                glyph: icons::ADD,
                label: "在新标签页中打开".into(),
                shortcut: None,
                action: CtxAction::OpenSidebarVirtualNewTab(*kind),
                enabled: true,
            });
            if *kind == SidebarVirtualKind::RecycleBin {
                rows.push(CtxRow::Sep);
                rows.push(CtxRow::Item {
                    glyph: icons::DELETE,
                    label: "清空回收站".into(),
                    shortcut: None,
                    action: CtxAction::Cmd(Command::EmptyRecycleBin),
                    enabled: true,
                });
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::settings::Settings;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

    fn paint(app: &mut LumenApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) -> egui::FullOutput {
        let full = Rect::from_min_size(Pos2::ZERO, Vec2::new(820.0, 600.0));
        let input = egui::RawInput { screen_rect: Some(full), time: Some(time), events, ..Default::default() };
        let mut output = ctx.run_pass(input, |ctx| {
            let mut ui =
                egui::Ui::new(ctx.clone(), egui::Id::unique("submenu-test"), egui::UiBuilder::new().max_rect(full));
            app.paint_context_menu(&mut ui, full);
        });
        output.textures_delta.clear();
        output
    }

    #[test]
    fn cascading_menus_are_visible_on_both_sides_and_new_document_is_clickable() {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok().unwrap();
        let ctx = egui::Context::default();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("tmp")
            .join(format!("submenu-{stamp}"));
        std::fs::create_dir_all(&root).unwrap();
        let settings = Settings { tabs: vec![root.to_string_lossy().into_owned()], ..Default::default() };
        let mut app = LumenApp::with_context(&ctx, settings);
        for x in [60.0, 600.0] {
            for kind in [SubmenuKind::New, SubmenuKind::View, SubmenuKind::Sort] {
                let mut state = ContextMenuState::open(&app, Pos2::new(x, 80.0));
                state.open_sub = Some(kind);
                state.sub_anchor_y = 180.0;
                app.ctx_menu = Some(state);
                paint(&mut app, &ctx, 1.0, Vec::new());
                let output = paint(&mut app, &ctx, 2.0, Vec::new());
                let surface = output
                    .shapes
                    .iter()
                    .find_map(|clipped| {
                        if let egui::Shape::Rect(shape) = &clipped.shape
                            && (shape.rect.width() - SUB_W).abs() < 0.01
                            && shape.fill == app.theme.popup
                        {
                            Some((clipped.clip_rect, shape.rect))
                        } else {
                            None
                        }
                    })
                    .expect("submenu surface must be painted");
                assert!(surface.0.contains_rect(surface.1), "submenu must not inherit the parent menu's clip");
            }
        }

        let mut state = ContextMenuState::open(&app, Pos2::new(60.0, 80.0));
        state.open_sub = Some(SubmenuKind::New);
        state.sub_anchor_y = 8.0;
        app.ctx_menu = Some(state);
        paint(&mut app, &ctx, 2.5, Vec::new());
        let point = Pos2::new(60.0 + MENU_W + 40.0, 8.0 + 6.0 + ROW_H * 1.5);
        paint(&mut app, &ctx, 3.0, vec![egui::Event::PointerMoved(point)]);
        paint(
            &mut app,
            &ctx,
            3.01,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        paint(
            &mut app,
            &ctx,
            3.02,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(root.join("新建文本文档.txt").is_file(), "clicking the submenu must create the selected file type");
        assert!(app.ctx_menu.is_none());
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
        unsafe { CoUninitialize() };
    }

    fn row_labels(rows: &[CtxRow]) -> Vec<String> {
        rows.iter()
            .filter_map(|r| match r {
                CtxRow::Item { label, .. } => Some(label.clone()),
                CtxRow::Submenu { label, .. } => Some((*label).to_string()),
                CtxRow::Sep => None,
            })
            .collect()
    }

    #[test]
    fn diagonal_safe_corridor_holds_submenu_open_while_moving_diagonally() {
        let trigger = Pos2::new(140.0, 110.0);
        let sub_rect = Rect::from_min_size(Pos2::new(260.0, 100.0), Vec2::new(224.0, 140.0));

        // Moving diagonally down-right across an adjacent row toward the submenu bottom half.
        let diag_mid = Pos2::new(200.0, 165.0);
        assert!(in_submenu_safe_corridor(diag_mid, Some(trigger), sub_rect));
        assert!(should_keep_submenu_open(Some(diag_mid), Some(trigger), Some(sub_rect), Some(10.0), 10.5));

        // Pointer inside submenu rect stays open even without trigger pos.
        let inside_sub = Pos2::new(300.0, 150.0);
        assert!(in_submenu_safe_corridor(inside_sub, None, sub_rect));

        // Moving straight down (far outside the triangle) closes once the 0.16s grace expires.
        let straight_down = Pos2::new(140.0, 220.0);
        assert!(!in_submenu_safe_corridor(straight_down, Some(trigger), sub_rect));
        // Within 160ms grace -> still held open.
        assert!(should_keep_submenu_open(Some(straight_down), Some(trigger), Some(sub_rect), Some(10.0), 10.08));
        // After 160ms grace -> closes.
        assert!(!should_keep_submenu_open(Some(straight_down), Some(trigger), Some(sub_rect), Some(10.0), 10.20));

        // Left-opening submenu (near right edge of viewport) works symmetrically.
        let trigger_left = Pos2::new(700.0, 110.0);
        let sub_left = Rect::from_min_size(Pos2::new(400.0, 100.0), Vec2::new(224.0, 140.0));
        let diag_left = Pos2::new(650.0, 150.0);
        assert!(in_submenu_safe_corridor(diag_left, Some(trigger_left), sub_left));
        assert!(!in_submenu_safe_corridor(Pos2::new(710.0, 220.0), Some(trigger_left), sub_left));
    }

    #[test]
    fn keyboard_navigation_skips_separators_opens_and_closes_submenus_and_activates() {
        let mut state = ContextMenuState::base(Pos2::new(50.0, 50.0), ContextKind::Background);
        state.has_current_dir = true;
        state.can_write_dir = true;
        let rows = build_rows(&state);
        // Expected order for writable Background:
        // 0: Submenu(View), 1: Submenu(Sort), 2: Sep, 3: Submenu(New), 4: Item(OpenTerminal), 5: Sep, 6: Item(ClassicMenu)
        assert!(matches!(rows[0], CtxRow::Submenu { kind: SubmenuKind::View, .. }));
        assert!(matches!(rows[1], CtxRow::Submenu { kind: SubmenuKind::Sort, .. }));
        assert!(matches!(rows[2], CtxRow::Sep));
        assert!(matches!(rows[3], CtxRow::Submenu { kind: SubmenuKind::New, .. }));

        let sub_len = |k| match k {
            SubmenuKind::View | SubmenuKind::Sort => 4,
            SubmenuKind::New => 2,
        };
        let row_top = |idx: usize| 100.0 + idx as f32 * ROW_H;

        // First ArrowDown focuses index 0 (View).
        assert_eq!(
            apply_keyboard_navigation(&mut state, &rows, sub_len, row_top, false, false, true, false, false, false),
            MenuKeyAction::None
        );
        assert_eq!(state.focus_idx, Some(0));

        // ArrowDown -> 1 (Sort), then ArrowDown skips Sep (2) -> 3 (New).
        apply_keyboard_navigation(&mut state, &rows, sub_len, row_top, false, false, true, false, false, false);
        assert_eq!(state.focus_idx, Some(1));
        apply_keyboard_navigation(&mut state, &rows, sub_len, row_top, false, false, true, false, false, false);
        assert_eq!(state.focus_idx, Some(3));

        // ArrowUp skips Sep (2) back to 1 (Sort).
        apply_keyboard_navigation(&mut state, &rows, sub_len, row_top, false, true, false, false, false, false);
        assert_eq!(state.focus_idx, Some(1));

        // ArrowRight opens Sort submenu and focuses its first entry (0).
        apply_keyboard_navigation(&mut state, &rows, sub_len, row_top, false, false, false, true, false, false);
        assert_eq!(state.open_sub, Some(SubmenuKind::Sort));
        assert_eq!(state.sub_focus_idx, Some(0));

        // ArrowDown moves inside submenu to index 1; Enter activates submenu item 1.
        apply_keyboard_navigation(&mut state, &rows, sub_len, row_top, false, false, true, false, false, false);
        assert_eq!(state.sub_focus_idx, Some(1));
        assert_eq!(
            apply_keyboard_navigation(&mut state, &rows, sub_len, row_top, false, false, false, false, false, true),
            MenuKeyAction::ActivateSub { kind: SubmenuKind::Sort, idx: 1 }
        );

        // ArrowLeft closes submenu and keeps main menu open on parent trigger row (1).
        assert_eq!(
            apply_keyboard_navigation(&mut state, &rows, sub_len, row_top, false, false, false, false, true, false),
            MenuKeyAction::None
        );
        assert_eq!(state.open_sub, None);
        assert_eq!(state.sub_focus_idx, None);
        assert_eq!(state.focus_idx, Some(1));

        // Escape on open submenu closes only submenu; second Escape closes entire menu.
        apply_keyboard_navigation(&mut state, &rows, sub_len, row_top, false, false, false, true, false, false);
        assert_eq!(state.open_sub, Some(SubmenuKind::Sort));
        assert_eq!(
            apply_keyboard_navigation(&mut state, &rows, sub_len, row_top, true, false, false, false, false, false),
            MenuKeyAction::None
        );
        assert_eq!(state.open_sub, None);
        assert_eq!(state.focus_idx, Some(1));
        assert_eq!(
            apply_keyboard_navigation(&mut state, &rows, sub_len, row_top, true, false, false, false, false, false),
            MenuKeyAction::CloseAll
        );

        // ArrowUp from 0 wraps to last enabled row, and Enter activates it.
        state.focus_idx = Some(0);
        apply_keyboard_navigation(&mut state, &rows, sub_len, row_top, false, true, false, false, false, false);
        let last_idx = rows.len() - 1;
        assert_eq!(state.focus_idx, Some(last_idx));
        assert_eq!(
            apply_keyboard_navigation(&mut state, &rows, sub_len, row_top, false, false, false, false, false, true),
            MenuKeyAction::ActivateMain(last_idx)
        );
    }

    #[test]
    fn this_pc_virtual_and_non_writable_context_classification_and_rows() {
        // Classification checks (BUG-41).
        assert_eq!(classify_context_kind(false, true, true, true), ContextKind::ThisPcItems);
        assert_eq!(classify_context_kind(false, true, true, false), ContextKind::VirtualItem);
        assert_eq!(classify_context_kind(false, false, true, false), ContextKind::VirtualItem);
        assert_eq!(classify_context_kind(false, false, false, false), ContextKind::Background);
        assert_eq!(classify_context_kind(true, false, true, false), ContextKind::RecycleBinItems);
        assert_eq!(classify_context_kind(true, false, false, false), ContextKind::RecycleBinBackground);

        // This PC drive items must have zero destructive/creation actions and no destructive top bar.
        let mut this_pc_drive = ContextMenuState::base(Pos2::ZERO, ContextKind::ThisPcItems);
        this_pc_drive.paths = vec![PathBuf::from(r"C:\")];
        assert!(!has_top_action_bar(&this_pc_drive));
        let labels = row_labels(&build_rows(&this_pc_drive));
        for forbidden in ["剪切", "重命名", "删除", "永久删除", "压缩为 ZIP 文件", "新建", "创建快捷方式"]
        {
            assert!(!labels.iter().any(|l| l == forbidden), "This PC menu must not expose {forbidden:?}: {labels:?}");
        }
        for expected in ["打开", "在新标签页中打开", "复制路径", "在终端中打开", "属性", "显示更多选项"]
        {
            assert!(labels.iter().any(|l| l == expected), "This PC menu missing {expected:?}: {labels:?}");
        }

        // Virtual items (e.g. Network node with path == None) do NOT fall back to Background.
        let mut virt = ContextMenuState::base(Pos2::ZERO, ContextKind::VirtualItem);
        virt.has_pidls = true;
        let virt_labels = row_labels(&build_rows(&virt));
        assert!(virt_labels.iter().any(|l| l == "打开"));
        assert!(virt_labels.iter().any(|l| l == "属性"));
        assert!(virt_labels.iter().any(|l| l == "显示更多选项"));
        assert!(!virt_labels.iter().any(|l| l == "新建"));

        // This PC / Network background hides New, Open in Terminal, and Paste.
        let this_pc_bg = ContextMenuState::base(Pos2::ZERO, ContextKind::Background);
        assert!(!has_top_action_bar(&this_pc_bg));
        let bg_labels = row_labels(&build_rows(&this_pc_bg));
        assert!(!bg_labels.iter().any(|l| l == "新建" || l == "在终端中打开"));

        // Search background has current_dir = true, in_search = true, can_write_dir = false:
        // hides New, Open in Terminal, and Paste from action bar.
        let mut search_bg = ContextMenuState::base(Pos2::ZERO, ContextKind::Background);
        search_bg.has_current_dir = true;
        search_bg.in_search = true;
        search_bg.can_write_dir = false;
        search_bg.can_undo = true;
        search_bg.can_redo = true;
        let bar_cmds: Vec<Command> = top_action_bar_items(&search_bg).into_iter().map(|(_, _, c, _)| c).collect();
        assert!(!bar_cmds.contains(&Command::Paste));
        let search_labels = row_labels(&build_rows(&search_bg));
        assert!(!search_labels.iter().any(|l| l == "新建" || l == "在终端中打开"));
        assert!(search_labels.iter().any(|l| l == "撤销"));
        assert!(search_labels.iter().any(|l| l == "重做"));

        // Single filesystem file item includes Quick Look ("快速预览").
        let mut single_file = ContextMenuState::base(Pos2::ZERO, ContextKind::Items);
        single_file.paths = vec![PathBuf::from(r"C:\notes.txt")];
        single_file.single_file = true;
        let file_labels = row_labels(&build_rows(&single_file));
        assert!(file_labels.iter().any(|l| l == "快速预览"));

        // Sidebar menus use consistent wording "在新标签页中打开".
        let sb_quick =
            ContextMenuState::new_sidebar_quick(Pos2::ZERO, PathBuf::from(r"C:\Work"), "Work".into(), Some(0), false);
        let sb_labels = row_labels(&build_rows(&sb_quick));
        assert!(sb_labels.iter().any(|l| l == "在新标签页中打开"));
        assert!(sb_labels.iter().any(|l| l == "从快速访问取消固定"));
        assert!(!sb_labels.iter().any(|l| l == "在新标签中打开"));

        let sb_drive = ContextMenuState::new_sidebar_drive(Pos2::ZERO, PathBuf::from(r"E:\"), "USB (E:)".into(), true);
        let drive_labels = row_labels(&build_rows(&sb_drive));
        assert!(drive_labels.iter().any(|l| l == "在新标签页中打开"));
        assert!(drive_labels.iter().any(|l| l == "安全弹出"));
        assert!(drive_labels.iter().any(|l| l == "在终端中打开"));
    }

    #[test]
    fn pointer_movement_ignores_stationary_cursor_and_detects_cumulative_micro_deltas() {
        let mut last = None;
        assert!(!update_pointer_moved(&mut last, Some(Pos2::new(100.0, 100.0))));
        // Stationary / sub-pixel jitter does not hijack keyboard focus.
        assert!(!update_pointer_moved(&mut last, Some(Pos2::new(100.4, 100.0))));
        assert!(!update_pointer_moved(&mut last, Some(Pos2::new(100.8, 100.0))));
        // Cumulative movement beyond 1px from anchor registers as genuine motion.
        assert!(update_pointer_moved(&mut last, Some(Pos2::new(101.2, 100.0))));
        assert_eq!(last, Some(Pos2::new(101.2, 100.0)));
    }
}
