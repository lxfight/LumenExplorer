//! Windows 11 Fluent 2 custom-drawn top-level context menu.
//!
//! Opens with zero latency (no COM initialization until the user explicitly
//! clicks "显示更多选项" or uses Shift+Right-Click), featuring:
//! - Top compact icon action bar (Cut / Copy / Rename / Copy Path / Delete or Paste / Refresh / Properties)
//! - Context-aware actions (Run as Admin, Compress/Extract, Create Shortcut, Recycle Bin Restore/Empty)
//! - Cascading "新建" submenu populated from live Windows `ShellNew` registry templates
//! - Cascading "查看" and "排序方式" submenus on folder background
//! - Lazy classic Win32 Shell menu fallback ("显示更多选项  Shift+F10")

use std::path::PathBuf;

use egui::{CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::{Command, LumenApp};
use crate::core::{SortKey, ViewMode};
use crate::shell::archive::is_archive_ext;
use crate::shell::shellnew::{self, NewTemplate};
use crate::theme::{self, Theme};
use crate::ui::{icons, widgets};

const MENU_W: f32 = 252.0;
const SUB_W: f32 = 224.0;
const ROW_H: f32 = 29.0;
const ACTION_BAR_H: f32 = 40.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextKind {
    Items,
    Background,
    RecycleBinItems,
    RecycleBinBackground,
}

#[derive(Clone, Copy, PartialEq, Eq)]
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
    pub all_pinned: bool,
    pub can_run_admin: bool,
    pub single_archive: Option<PathBuf>,
    pub in_search: bool,
    pub has_clipboard_files: bool,
    pub has_clipboard_image: bool,
    open_sub: Option<SubmenuKind>,
    sub_anchor_y: f32,
}

impl ContextMenuState {
    pub fn open(app: &LumenApp, anchor: Pos2) -> Self {
        let tab = app.active();
        let is_recycle = tab.location.path.is_none()
            && tab.location.parsing_name().eq_ignore_ascii_case("::{645FF040-5081-101B-9F08-00AA002F954E}");
        let paths = tab.selected_paths();
        let has_sel = !tab.selection.is_empty();
        let kind = if is_recycle {
            if has_sel { ContextKind::RecycleBinItems } else { ContextKind::RecycleBinBackground }
        } else if !paths.is_empty() {
            ContextKind::Items
        } else {
            ContextKind::Background
        };
        let all_folders = !paths.is_empty() && paths.iter().all(|p| p.is_dir());
        let all_pinned = !paths.is_empty()
            && paths.iter().all(|p| {
                let s = p.to_string_lossy();
                app.settings.sidebar_pinned.iter().any(|k| k.eq_ignore_ascii_case(&s))
            });
        let can_run_admin = paths.len() == 1
            && paths[0]
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "exe" | "msi" | "bat" | "cmd" | "ps1"));
        let single_archive = if paths.len() == 1
            && paths[0].is_file()
            && paths[0].extension().and_then(|e| e.to_str()).is_some_and(|e| is_archive_ext(&e.to_ascii_lowercase()))
        {
            Some(paths[0].clone())
        } else {
            None
        };
        // Kick off background ShellNew scan if not yet loaded.
        let _ = shellnew::templates();
        Self {
            anchor,
            kind,
            paths,
            all_folders,
            all_pinned,
            can_run_admin,
            single_archive,
            in_search: app.search_active(),
            has_clipboard_files: app.clipboard_files,
            has_clipboard_image: crate::shell::clipboard::has_image(),
            open_sub: None,
            sub_anchor_y: anchor.y,
        }
    }
}

enum CtxAction {
    Cmd(Command),
    Pin(Vec<PathBuf>),
    Unpin(Vec<PathBuf>),
    NewTemplate(NewTemplate),
    ClassicMenu,
}

enum CtxRow {
    Item { glyph: &'static str, label: String, shortcut: Option<&'static str>, action: CtxAction, enabled: bool },
    Submenu { glyph: &'static str, label: &'static str, kind: SubmenuKind },
    Sep,
}

impl LumenApp {
    pub(crate) fn paint_context_menu(&mut self, ui: &mut egui::Ui, full: Rect) {
        let ctx = ui.ctx().clone();
        let pop_id = egui::Id::unique("ctx-menu-pop");
        let Some(mut state) = self.ctx_menu.take() else {
            crate::ui::spring::clear(&ctx, pop_id);
            return;
        };
        let t = self.theme.clone();
        let painter = ui.painter().clone();

        let blocker = ui.interact(Rect::EVERYTHING, egui::Id::unique("ctx-menu-blocker"), Sense::click());
        if !blocker.has_focus() {
            blocker.request_focus();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            crate::ui::spring::clear(&ctx, pop_id);
            return;
        }
        if blocker.clicked_by(egui::PointerButton::Primary) || blocker.clicked_by(egui::PointerButton::Secondary) {
            crate::ui::spring::clear(&ctx, pop_id);
            return;
        }
        // Shift+F10 while menu is open jumps straight to classic Win32 Shell menu.
        if ctx.input(|i| i.modifiers.shift && i.key_pressed(egui::Key::F10)) {
            crate::ui::spring::clear(&ctx, pop_id);
            self.open_classic_context_menu(true);
            return;
        }

        let rows = build_rows(&state);
        let has_bar = matches!(state.kind, ContextKind::Items | ContextKind::Background);
        let rows_h: f32 = rows.iter().map(|r| if matches!(r, CtxRow::Sep) { 9.0 } else { ROW_H }).sum();
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
        // Soft drop shadow + Fluent surface.
        painter.rect_filled(
            clip_rect.translate(Vec2::new(0.0, 4.0)).expand(2.0),
            CornerRadius::same(10),
            widgets::fade(t.shadow, reveal),
        );
        painter.rect_filled(clip_rect, CornerRadius::same(8), widgets::fade(t.popup, reveal));
        painter.rect_stroke(
            clip_rect,
            CornerRadius::same(8),
            Stroke::new(1.0, widgets::fade(t.border, reveal)),
            egui::StrokeKind::Inside,
        );
        let painter = painter.with_clip_rect(clip_rect);
        let _ = ui.interact(rect, egui::Id::unique("ctx-menu-surface"), Sense::click());

        let mut chosen: Option<CtxAction> = None;
        let mut y = rect.top() + 6.0;

        // ---- Top Windows 11 compact icon action bar -------------------------
        if has_bar {
            let bar =
                Rect::from_min_size(Pos2::new(rect.left() + 6.0, y), Vec2::new(MENU_W - 12.0, ACTION_BAR_H - 4.0));
            let bar_items: Vec<(&str, &str, Command, bool)> = match state.kind {
                ContextKind::Items => vec![
                    (icons::CUT, "剪切 (Ctrl+X)", Command::Cut, true),
                    (icons::COPY, "复制 (Ctrl+C)", Command::Copy, true),
                    (icons::RENAME, "重命名 (F2)", Command::Rename, true),
                    (icons::LINK, "复制路径 (Ctrl+Shift+C)", Command::CopyPath, true),
                    (icons::DELETE, "删除 (Del)", Command::Delete, true),
                ],
                _ => vec![
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
            };
            let btn_w = bar.width() / bar_items.len() as f32;
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
        for (idx, row_item) in rows.into_iter().enumerate() {
            match row_item {
                CtxRow::Sep => {
                    painter.line_segment(
                        [Pos2::new(rect.left() + 10.0, y + 4.0), Pos2::new(rect.right() - 10.0, y + 4.0)],
                        Stroke::new(1.0, t.divider),
                    );
                    y += 9.0;
                }
                CtxRow::Submenu { glyph, label, kind } => {
                    let row = Rect::from_min_size(Pos2::new(rect.left() + 5.0, y), Vec2::new(MENU_W - 10.0, ROW_H));
                    let id = egui::Id::unique(("ctx-sub", idx));
                    let resp = ui.interact(row, id, Sense::click());
                    let is_open = state.open_sub == Some(kind);
                    let hover = widgets::hover_t(&ctx, id, resp.hovered() || is_open);
                    if hover > 0.0 {
                        painter.rect_filled(row, CornerRadius::same(5), widgets::fade(t.hover_strong, hover));
                    }
                    widgets::icon_at(
                        &painter,
                        Pos2::new(row.left() + 16.0, row.center().y),
                        glyph,
                        13.5,
                        t.text_secondary,
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
                    if resp.hovered() || resp.clicked() {
                        state.open_sub = Some(kind);
                        state.sub_anchor_y = row.top();
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    y += ROW_H;
                }
                CtxRow::Item { glyph, label, shortcut, action, enabled } => {
                    let row = Rect::from_min_size(Pos2::new(rect.left() + 5.0, y), Vec2::new(MENU_W - 10.0, ROW_H));
                    let id = egui::Id::unique(("ctx-row", idx));
                    let resp = ui.interact(row, id, Sense::click());
                    let hover = widgets::hover_t(&ctx, id, resp.hovered() && enabled);
                    if hover > 0.0 {
                        painter.rect_filled(row, CornerRadius::same(5), widgets::fade(t.hover_strong, hover));
                    }
                    let text_col = if enabled { t.text } else { t.text_disabled };
                    let icon_col = if enabled { t.text_secondary } else { t.text_disabled };
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
                        state.open_sub = None;
                        if enabled {
                            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                    }
                    if enabled && resp.clicked() {
                        chosen = Some(action);
                    }
                    y += ROW_H;
                }
            }
        }

        // ---- Cascading submenu ----------------------------------------------
        if let Some(sub) = state.open_sub
            && let Some(sub_action) = self.paint_context_submenu(ui, &painter, &t, rect, state.sub_anchor_y, full, sub)
        {
            chosen = Some(sub_action);
        }

        if let Some(action) = chosen {
            crate::ui::spring::clear(&ctx, pop_id);
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
        anchor_y: f32,
        full: Rect,
        kind: SubmenuKind,
    ) -> Option<CtxAction> {
        let ctx = ui.ctx().clone();
        let mut entries: Vec<(bool, &str, String, CtxAction)> = Vec::new();
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
                let cur = self.active().view;
                entries.push((
                    cur == ViewMode::Details,
                    icons::LIST_BULLETS,
                    "详细信息".into(),
                    CtxAction::Cmd(Command::ViewDetails),
                ));
                entries.push((cur == ViewMode::List, icons::LIST, "列表".into(), CtxAction::Cmd(Command::ViewList)));
                entries.push((cur == ViewMode::Tiles, icons::GRID, "平铺".into(), CtxAction::Cmd(Command::ViewTiles)));
                entries.push((
                    cur == ViewMode::Icons,
                    icons::VIEW,
                    "大图标".into(),
                    CtxAction::Cmd(Command::ViewIcons),
                ));
            }
            SubmenuKind::Sort => {
                let cur = self.active().sort.key;
                entries.push((
                    cur == SortKey::Name,
                    icons::SORT,
                    "名称".into(),
                    CtxAction::Cmd(Command::SortBy(SortKey::Name)),
                ));
                entries.push((
                    cur == SortKey::DateModified,
                    icons::HISTORY,
                    "修改日期".into(),
                    CtxAction::Cmd(Command::SortBy(SortKey::DateModified)),
                ));
                entries.push((
                    cur == SortKey::Type,
                    icons::FILE,
                    "类型".into(),
                    CtxAction::Cmd(Command::SortBy(SortKey::Type)),
                ));
                entries.push((
                    cur == SortKey::Size,
                    icons::INFO,
                    "大小".into(),
                    CtxAction::Cmd(Command::SortBy(SortKey::Size)),
                ));
            }
        }
        let h = entries.len() as f32 * ROW_H + 12.0;
        let x = if parent.right() + SUB_W + 4.0 <= full.right() {
            parent.right() + 2.0
        } else {
            (parent.left() - SUB_W - 2.0).max(full.left() + 8.0)
        };
        let y = anchor_y.min(full.bottom() - h - 8.0).max(full.top() + 8.0);
        let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(SUB_W, h));
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
            let hover = widgets::hover_t(&ctx, id, resp.hovered());
            if hover > 0.0 {
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
        }
    }
}

fn build_rows(state: &ContextMenuState) -> Vec<CtxRow> {
    let mut rows = Vec::new();
    match state.kind {
        ContextKind::Items => {
            rows.push(CtxRow::Item {
                glyph: icons::OPEN_EXTERNAL,
                label: "打开".into(),
                shortcut: Some("Enter"),
                action: CtxAction::Cmd(Command::Open),
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
        ContextKind::Background => {
            rows.push(CtxRow::Submenu { glyph: icons::VIEW, label: "查看", kind: SubmenuKind::View });
            rows.push(CtxRow::Submenu { glyph: icons::SORT, label: "排序方式", kind: SubmenuKind::Sort });
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
            rows.push(CtxRow::Sep);
            rows.push(CtxRow::Item {
                glyph: icons::MORE,
                label: "显示更多选项".into(),
                shortcut: Some("Shift+F10"),
                action: CtxAction::ClassicMenu,
                enabled: true,
            });
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
                label: "清空回收站".into(),
                shortcut: None,
                action: CtxAction::Cmd(Command::EmptyRecycleBin),
                enabled: true,
            });
            rows.push(CtxRow::Sep);
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
    }
    rows
}
