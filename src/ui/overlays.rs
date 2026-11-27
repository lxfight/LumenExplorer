//! Floating overlays: menus, dialogs, address suggestions and toasts.

use egui::{Color32, CornerRadius, PointerButton, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::{Command, Dialog, LumenApp, now};
use crate::core::{SortKey, ViewMode};
use crate::theme::{self, Theme};
use crate::ui::{icons, widgets};

// ---- tab context menu ------------------------------------------------------

/// Right-click menu target. Transient chrome state, so it lives in egui memory
/// instead of the persisted tab model.
#[derive(Clone, Copy, Default)]
pub(crate) struct TabMenu {
    pub tab: usize,
    pub anchor: Pos2,
    armed: bool,
}

fn tab_menu_id() -> egui::Id {
    egui::Id::unique("tab-context-menu")
}

pub(crate) fn open_tab_menu(ctx: &egui::Context, tab: usize, anchor: Pos2) {
    ctx.data_mut(|data| data.insert_temp(tab_menu_id(), TabMenu { tab, anchor, armed: false }));
}

/// True while a custom popup that lives outside `menu_anchor` is open (the tab
/// context menu). Ordinary widget focus must NOT count: global shortcuts have to
/// keep working while a sidebar row or the search box is focused.
pub(crate) fn overlay_has_keyboard(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<TabMenu>(tab_menu_id()).is_some())
}

pub(crate) fn close_tab_menu(ctx: &egui::Context) {
    ctx.data_mut(|data| {
        data.remove_temp::<TabMenu>(tab_menu_id());
    });
}

// ---- "more" menu -----------------------------------------------------------

#[derive(Clone, Copy)]
enum MenuCheck {
    None,
    Sort(SortKey),
    Group(Option<SortKey>),
    View(ViewMode),
    Hidden,
    SystemFiles,
    DetailsPane,
    SplitView,
}

#[derive(Clone, Copy)]
enum MenuGate {
    Always,
    Selection,
    CanUndo,
    CanRedo,
}

#[derive(Clone, Copy)]
struct MenuItem {
    label: &'static str,
    shortcut: Option<&'static str>,
    command: Command,
    check: MenuCheck,
    gate: MenuGate,
}

#[derive(Clone, Copy)]
struct MenuSubmenu {
    id: &'static str,
    label: &'static str,
    items: &'static [MenuItem],
}

enum MenuEntry {
    Item(MenuItem),
    Submenu(MenuSubmenu),
    Separator,
}

const fn item(
    label: &'static str,
    shortcut: Option<&'static str>,
    command: Command,
    check: MenuCheck,
    gate: MenuGate,
) -> MenuEntry {
    MenuEntry::Item(MenuItem { label, shortcut, command, check, gate })
}

const fn sub_item(label: &'static str, command: Command, check: MenuCheck) -> MenuItem {
    MenuItem { label, shortcut: None, command, check, gate: MenuGate::Always }
}

const SEP: MenuEntry = MenuEntry::Separator;
const ALWAYS: MenuGate = MenuGate::Always;
const NEEDS_SELECTION: MenuGate = MenuGate::Selection;

const VIEW_SUBMENU: &[MenuItem] = &[
    sub_item("详细信息视图", Command::ViewDetails, MenuCheck::View(ViewMode::Details)),
    sub_item("列表视图", Command::ViewList, MenuCheck::View(ViewMode::List)),
    sub_item("平铺视图", Command::ViewTiles, MenuCheck::View(ViewMode::Tiles)),
    sub_item("大图标视图", Command::ViewIcons, MenuCheck::View(ViewMode::Icons)),
];

const SORT_SUBMENU: &[MenuItem] = &[
    sub_item("按名称排序", Command::SortBy(SortKey::Name), MenuCheck::Sort(SortKey::Name)),
    sub_item("按修改日期排序", Command::SortBy(SortKey::DateModified), MenuCheck::Sort(SortKey::DateModified)),
    sub_item("按类型排序", Command::SortBy(SortKey::Type), MenuCheck::Sort(SortKey::Type)),
    sub_item("按大小排序", Command::SortBy(SortKey::Size), MenuCheck::Sort(SortKey::Size)),
];

const GROUP_SUBMENU: &[MenuItem] = &[
    sub_item("不分组", Command::GroupBy(None), MenuCheck::Group(None)),
    sub_item("按名称分组", Command::GroupBy(Some(SortKey::Name)), MenuCheck::Group(Some(SortKey::Name))),
    sub_item(
        "按修改日期分组",
        Command::GroupBy(Some(SortKey::DateModified)),
        MenuCheck::Group(Some(SortKey::DateModified)),
    ),
    sub_item("按类型分组", Command::GroupBy(Some(SortKey::Type)), MenuCheck::Group(Some(SortKey::Type))),
    sub_item("按大小分组", Command::GroupBy(Some(SortKey::Size)), MenuCheck::Group(Some(SortKey::Size))),
];

/// Static menu definition: concise, grouped, and free of app-level system
/// toggles (which belong in Settings Center). View/Sort/Group fold into compact
/// submenus while high-frequency edit/preview actions live right at the top.
const MENU: &[MenuEntry] = &[
    item("撤销", Some("Ctrl+Z"), Command::Undo, MenuCheck::None, MenuGate::CanUndo),
    item("重做", Some("Ctrl+Y"), Command::Redo, MenuCheck::None, MenuGate::CanRedo),
    item("快速预览", Some("Space"), Command::ToggleQuickLook, MenuCheck::None, NEEDS_SELECTION),
    item("重命名 / 批量重命名", Some("F2"), Command::Rename, MenuCheck::None, NEEDS_SELECTION),
    SEP,
    MenuEntry::Submenu(MenuSubmenu { id: "view", label: "视图", items: VIEW_SUBMENU }),
    MenuEntry::Submenu(MenuSubmenu { id: "sort", label: "排序方式", items: SORT_SUBMENU }),
    MenuEntry::Submenu(MenuSubmenu { id: "group", label: "分组方式", items: GROUP_SUBMENU }),
    SEP,
    item("显示隐藏的项目", Some("Ctrl+H"), Command::ToggleHidden, MenuCheck::Hidden, ALWAYS),
    item("显示受保护的系统文件", None, Command::ToggleSystemFiles, MenuCheck::SystemFiles, ALWAYS),
    item("详细信息窗格", None, Command::ToggleDetails, MenuCheck::DetailsPane, ALWAYS),
    item("双栏分屏视图", Some("Ctrl+Shift+E"), Command::ToggleSplitView, MenuCheck::SplitView, ALWAYS),
    item("切换主题", Some("Ctrl+Shift+D"), Command::CycleTheme, MenuCheck::None, ALWAYS),
    SEP,
    item("新建文件夹", Some("Ctrl+Shift+N"), Command::NewFolder, MenuCheck::None, ALWAYS),
    item("新建文本文档", None, Command::NewFile, MenuCheck::None, ALWAYS),
    item("新建窗口", Some("Ctrl+N"), Command::NewWindow, MenuCheck::None, ALWAYS),
    item("吸收 Explorer 窗口", Some("Ctrl+Shift+O"), Command::ImportExplorerTabs, MenuCheck::None, ALWAYS),
    SEP,
    item("打开方式", None, Command::OpenWith, MenuCheck::None, NEEDS_SELECTION),
    item("打开文件所在位置", None, Command::ShowInFolder, MenuCheck::None, NEEDS_SELECTION),
    item("复制路径", Some("Ctrl+Shift+C"), Command::CopyPath, MenuCheck::None, ALWAYS),
    item("在终端中打开", None, Command::OpenTerminal, MenuCheck::None, ALWAYS),
    item("刷新", Some("F5"), Command::Refresh, MenuCheck::None, ALWAYS),
    SEP,
    item("设置…", Some("Ctrl+,"), Command::OpenSettings, MenuCheck::None, ALWAYS),
];

/// Width of the "more" popup; chrome anchors it against the button's right edge.
pub(crate) const MORE_MENU_W: f32 = 268.0;
const MORE_ROW_H: f32 = 28.0;
const MORE_SEP_H: f32 = 9.0;
const MORE_SUB_W: f32 = 188.0;

#[derive(Clone, Default)]
struct MoreMenuState {
    kbd: Option<usize>,
    open_sub: Option<&'static str>,
    sub_kbd: Option<usize>,
    sub_close_at: Option<f64>,
    last_sub_rect: Option<Rect>,
}

fn menu_state_id() -> egui::Id {
    egui::Id::unique("more-menu-state")
}

fn more_menu_natural_height() -> f32 {
    MENU.iter().map(|entry| if matches!(entry, MenuEntry::Separator) { MORE_SEP_H } else { MORE_ROW_H }).sum::<f32>()
        + 12.0
}

fn compute_more_menu_rect(anchor: Pos2, full: Rect, natural_h: f32) -> (Rect, bool) {
    let width = MORE_MENU_W;
    let max_h = (full.height() - 16.0).max(160.0);
    let scrollable = natural_h > max_h;
    let height = natural_h.min(max_h);
    let mut x = anchor.x;
    if x + width > full.right() - 8.0 {
        x = (full.right() - width - 8.0).max(full.left() + 8.0);
    }
    let mut y = anchor.y;
    if y + height > full.bottom() - 8.0 {
        let above = anchor.y - height - 40.0;
        y = if above >= full.top() + 8.0 { above } else { (full.bottom() - height - 8.0).max(full.top() + 8.0) };
    }
    (Rect::from_min_size(Pos2::new(x, y), Vec2::new(width, height)), scrollable)
}

/// Computes non-overlapping label and right-aligned shortcut rects for a `更多` row.
fn more_menu_row_rects(row: Rect, has_shortcut: bool, has_chevron: bool) -> (Rect, Option<Rect>) {
    let label_left = row.left() + 28.0;
    if has_shortcut {
        let label_right = (row.right() - 96.0).max(label_left);
        let shortcut_left = (row.right() - 90.0).max(label_right);
        let shortcut_right = (row.right() - 10.0).max(shortcut_left);
        (
            Rect::from_min_max(Pos2::new(label_left, row.top()), Pos2::new(label_right, row.bottom())),
            Some(Rect::from_min_max(Pos2::new(shortcut_left, row.top()), Pos2::new(shortcut_right, row.bottom()))),
        )
    } else {
        let right_pad = if has_chevron { 26.0 } else { 12.0 };
        let label_right = (row.right() - right_pad).max(label_left);
        (Rect::from_min_max(Pos2::new(label_left, row.top()), Pos2::new(label_right, row.bottom())), None)
    }
}

/// Computes non-overlapping name (~46%) and parent-path (~48%+) column rects for
/// an address suggestion row with an explicit gap between them.
fn address_suggestion_columns(row: Rect) -> (Rect, Rect) {
    let content_left = row.left() + 28.0;
    let content_right = (row.right() - 8.0).max(content_left);
    let col_gap = 10.0;
    let usable = (content_right - content_left - col_gap).max(0.0);
    let name_w = usable * 0.46;
    let name_right = content_left + name_w;
    let parent_left = (name_right + col_gap).min(content_right);
    (
        Rect::from_min_max(Pos2::new(content_left, row.top()), Pos2::new(name_right, row.bottom())),
        Rect::from_min_max(Pos2::new(parent_left, row.top()), Pos2::new(content_right, row.bottom())),
    )
}

fn menu_checked(app: &LumenApp, check: MenuCheck) -> bool {
    match check {
        MenuCheck::None => false,
        MenuCheck::Sort(key) => app.active().sort.key == key,
        MenuCheck::Group(key) => app.active().group_by == key,
        MenuCheck::View(view) => app.active().view == view,
        MenuCheck::Hidden => app.show_hidden,
        MenuCheck::SystemFiles => app.show_system,
        MenuCheck::DetailsPane => app.details_visible,
        MenuCheck::SplitView => app.secondary_tab.is_some(),
    }
}

/// App-level system settings relocated exclusively to Settings Center (UI-09).
const SETTINGS_ONLY_COMMANDS: [Command; 4] =
    [Command::ToggleTrayResident, Command::ToggleWinE, Command::ToggleAutoStart, Command::ToggleMica];

fn is_settings_only_command(cmd: Command) -> bool {
    SETTINGS_ONLY_COMMANDS.contains(&cmd)
}

fn menu_enabled(app: &LumenApp, item: &MenuItem) -> bool {
    if is_settings_only_command(item.command) {
        return false;
    }
    match item.gate {
        MenuGate::Always => true,
        MenuGate::Selection => !app.active().selection.is_empty(),
        MenuGate::CanUndo => app.can_undo(),
        MenuGate::CanRedo => app.can_redo(),
    }
}

fn menu_selectable_count(app: &LumenApp) -> usize {
    MENU.iter()
        .filter(|entry| match entry {
            MenuEntry::Item(item) => menu_enabled(app, item),
            MenuEntry::Submenu(_) => true,
            MenuEntry::Separator => false,
        })
        .count()
}

fn menu_entry_at_ordinal(app: &LumenApp, target: usize) -> Option<&'static MenuEntry> {
    let mut ordinal = 0usize;
    for entry in MENU {
        let enabled = match entry {
            MenuEntry::Item(item) => menu_enabled(app, item),
            MenuEntry::Submenu(_) => true,
            MenuEntry::Separator => false,
        };
        if !enabled {
            continue;
        }
        if ordinal == target {
            return Some(entry);
        }
        ordinal += 1;
    }
    None
}

fn trap_popup_focus(ctx: &egui::Context, blocker: &egui::Response) {
    if !blocker.has_focus() {
        blocker.request_focus();
    }
    ctx.memory_mut(|m| {
        m.move_focus(egui::FocusDirection::None);
        m.set_focus_lock_filter(
            blocker.id,
            egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
        );
    });
    ctx.input_mut(|i| {
        i.consume_key(egui::Modifiers::NONE, egui::Key::Tab);
        i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab);
    });
}

fn batch_rename_focus_ring() -> [egui::Id; 6] {
    [
        egui::Id::unique("batch-rename-find"),
        egui::Id::unique("batch-rename-replace"),
        egui::Id::unique("batch-rename-prefix"),
        egui::Id::unique("batch-rename-suffix"),
        dialog_cancel_id(),
        dialog_confirm_id(),
    ]
}

impl LumenApp {
    pub(crate) fn paint_overlays(&mut self, ui: &mut egui::Ui, full: Rect) {
        self.paint_drag_badge(ui, full);
        self.paint_address_suggestions(ui, full);
        self.paint_crumb_dropdown(ui, full);
        self.paint_dialog(ui, full);
        self.paint_menu(ui, full);
        self.paint_tab_menu(ui, full);
        self.paint_context_menu(ui, full);
        self.paint_quick_look(ui, full);
        self.paint_settings_modal(ui, full);
        // Toasts always paint last so modals, dialogs, and menus never occlude them.
        self.paint_toasts(ui, full);
    }

    fn paint_crumb_dropdown(&mut self, ui: &mut egui::Ui, full: Rect) {
        let ctx = ui.ctx().clone();
        let pop_id = egui::Id::unique("crumb-drop-pop");
        let Some(menu) = self.crumb_menu.take() else {
            crate::ui::spring::clear(&ctx, pop_id);
            return;
        };
        let t = self.theme.clone();
        let painter = ui.painter().clone();

        let row_h = 26.0;
        let visible = menu.items.len().clamp(1, 14);
        let width = 220.0;
        let height = (visible as f32 * row_h + 10.0).min((full.height() - 16.0).max(80.0));
        let mut x = menu.anchor.x;
        if x + width > full.right() - 8.0 {
            x = (full.right() - width - 8.0).max(full.left() + 8.0);
        }
        let mut y = menu.anchor.y;
        if y + height > full.bottom() - 8.0 {
            y = (full.bottom() - height - 8.0).max(full.top() + 8.0);
        }
        let reveal = crate::ui::spring::ease_out_reveal(&ctx, pop_id, 0.11);
        let rect = Rect::from_min_size(Pos2::new(x, y - (1.0 - reveal) * 5.0), Vec2::new(width, height));
        let clip_rect = Rect::from_min_size(rect.min, Vec2::new(width, (height * (0.80 + 0.20 * reveal)).max(20.0)));

        let blocker = ui.interact(Rect::EVERYTHING, egui::Id::unique("crumb-menu-blocker"), Sense::click());
        trap_popup_focus(&ctx, &blocker);
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            crate::ui::spring::clear(&ctx, pop_id);
            return;
        }

        painter.rect_filled(
            clip_rect.translate(Vec2::new(0.0, 4.0)).expand(2.0),
            CornerRadius::same(8),
            widgets::fade(t.shadow, reveal),
        );
        painter.rect_filled(clip_rect, CornerRadius::same(7), widgets::fade(t.popup, reveal));
        painter.rect_stroke(
            clip_rect,
            CornerRadius::same(7),
            Stroke::new(1.0, widgets::fade(t.border, reveal)),
            egui::StrokeKind::Inside,
        );
        let _ = ui.interact(rect, egui::Id::unique("crumb-menu-surface"), Sense::click());

        let mut picked: Option<std::path::PathBuf> = None;
        if menu.items.is_empty() {
            widgets::text_left(ui, &painter, rect.shrink(8.0), "没有子文件夹", theme::font(12.0), t.text_tertiary, 4.0);
        } else {
            let inner = rect.shrink(4.0);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
            child.set_clip_rect(inner.intersect(clip_rect));
            egui::ScrollArea::vertical().id_salt("crumb-drop-scroll").auto_shrink([false, false]).show(
                &mut child,
                |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    let cp = ui.painter().clone();
                    for (idx, (label, path)) in menu.items.iter().enumerate() {
                        let (row, resp) = ui.allocate_exact_size(Vec2::new(inner.width() - 6.0, row_h), Sense::click());
                        let id = egui::Id::unique(("crumb-item", idx));
                        let hover = widgets::hover_t(&ctx, id, resp.hovered());
                        if hover > 0.0 {
                            cp.rect_filled(row, CornerRadius::same(4), widgets::fade(t.hover_strong, hover));
                        }
                        widgets::icon_at(
                            &cp,
                            Pos2::new(row.left() + 14.0, row.center().y),
                            icons::FOLDER,
                            12.5,
                            t.accent,
                        );
                        widgets::text_left(
                            ui,
                            &cp,
                            Rect::from_min_max(
                                Pos2::new(row.left() + 28.0, row.top()),
                                Pos2::new(row.right() - 6.0, row.bottom()),
                            ),
                            label,
                            theme::font(12.5),
                            t.text,
                            0.0,
                        );
                        if resp.hovered() {
                            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if resp.clicked() {
                            picked = Some(path.clone());
                        }
                    }
                },
            );
        }

        let outside_click = (blocker.clicked_by(PointerButton::Primary)
            || blocker.clicked_by(PointerButton::Secondary))
            && ctx.pointer_interact_pos().is_none_or(|pos| !rect.contains(pos));

        if let Some(path) = picked {
            crate::ui::spring::clear(&ctx, pop_id);
            let raw = path.to_string_lossy();
            if raw.eq_ignore_ascii_case(crate::core::tabs::THIS_PC_PARSING) {
                if let Ok(loc) =
                    crate::shell::location::Location::known(&windows::Win32::UI::Shell::FOLDERID_ComputerFolder)
                {
                    self.navigate_to(loc);
                }
            } else if raw.starts_with("::") {
                match crate::shell::location::Location::parse(&raw) {
                    Ok(loc) => self.navigate_to(loc),
                    Err(err) => self.toast_error(format!("{err:#}")),
                }
            } else {
                self.navigate_to_path(&path);
            }
        } else if outside_click {
            crate::ui::spring::clear(&ctx, pop_id);
        } else {
            self.crumb_menu = Some(menu);
        }
    }

    /// Shows a compact Fluent badge near the cursor while an OLE drag hovers over
    /// the window ("移动到 …" / "复制到 …" / "创建快捷方式到 …").
    fn paint_drag_badge(&self, ui: &egui::Ui, full: Rect) {
        let Some(hover) = crate::shell::dnd::drag_hover() else { return };
        let Some(action) = hover.action else { return };
        let verb = match action {
            crate::shell::dnd::DropAction::Move => "移动到",
            crate::shell::dnd::DropAction::Copy => "复制到",
            crate::shell::dnd::DropAction::Link => "创建快捷方式到",
        };
        let dest = hover
            .target
            .as_ref()
            .and_then(|p| {
                p.file_name().map(|n| n.to_string_lossy().to_string()).or_else(|| Some(p.to_string_lossy().to_string()))
            })
            .unwrap_or_else(|| self.active().title.clone());
        let label = format!("{verb} {dest}");
        let t = &self.theme;
        let painter = ui.painter();
        let g = widgets::galley(ui, &label, theme::semibold(11.5), t.text, 280.0);
        let size = Vec2::new(g.size().x + 20.0, 26.0);
        let x = (hover.position.x + 16.0).min(full.right() - size.x - 8.0).max(full.left() + 8.0);
        let y = (hover.position.y + 18.0).min(full.bottom() - size.y - 8.0).max(full.top() + 8.0);
        let rect = Rect::from_min_size(Pos2::new(x, y), size);
        painter.rect_filled(rect, CornerRadius::same(6), t.popup);
        painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
        widgets::galley_at(painter, Pos2::new(rect.left() + 10.0, rect.center().y - g.size().y * 0.5), g, t.text);
    }

    fn paint_address_suggestions(&mut self, ui: &mut egui::Ui, full: Rect) {
        if !self.address.editing || self.address.suggestions.is_empty() {
            return;
        }
        let Some(pill) = self.address.pill else { return };
        let t = self.theme.clone();
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let row_h = 26.0;
        let rows = self.address.suggestions.len().min(8);
        let max_height = (full.height() * 0.5).max(row_h * 3.0 + 8.0);
        let visible = (((max_height - 8.0) / row_h).floor() as usize).clamp(1, rows);
        let selected = self.address.suggestion_index.filter(|index| *index < rows);
        // The list is capped at eight rows, so a window offset keeps the keyboard
        // selection visible without any scroll state.
        let first = selected.map(|index| index.saturating_sub(visible - 1)).unwrap_or(0).min(rows - visible);
        let width = pill.width().max(300.0).min(full.width() * 0.6);
        let height = visible as f32 * row_h + 8.0;
        let mut x = pill.left();
        if x + width > full.right() - 8.0 {
            x = full.right() - width - 8.0;
        }
        let mut y = pill.bottom() + 4.0;
        if y + height > full.bottom() - 8.0 {
            y = (pill.top() - 4.0 - height).max(full.top() + 8.0);
        }
        let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(width, height));
        self.address_suggest_rect = Some(rect);
        painter.rect_filled(rect, CornerRadius::same(6), t.popup);
        painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
        // Swallow clicks on the panel padding; otherwise they fall through to the
        // file list under the dropdown.
        let bg_resp = ui.interact(rect, egui::Id::unique("addr-suggest-bg"), Sense::click());

        let mut clicked = None;
        for (offset, index) in (first..first + visible).enumerate() {
            let row = Rect::from_min_size(
                Pos2::new(rect.left() + 4.0, rect.top() + 4.0 + offset as f32 * row_h),
                Vec2::new(rect.width() - 8.0, row_h),
            );
            let id = egui::Id::unique(("addr-suggest", index));
            let resp = ui.interact(row, id, Sense::click());
            let hover = widgets::hover_t(&ctx, id, resp.hovered());
            if selected == Some(index) {
                painter.rect_filled(row, CornerRadius::same(4), t.selected);
            } else if hover > 0.0 {
                painter.rect_filled(row, CornerRadius::same(4), widgets::fade(t.hover, hover));
            }
            let (name, path) = &self.address.suggestions[index];
            let (name_rect, parent_rect) = address_suggestion_columns(row);
            widgets::icon_at(&painter, Pos2::new(row.left() + 14.0, row.center().y), icons::FOLDER, 13.0, t.accent);
            widgets::text_left(ui, &painter, name_rect, name, theme::font(12.5), t.text, 0.0);
            let parent_text = path.parent().map(|parent| parent.to_string_lossy().to_string()).unwrap_or_default();
            widgets::text_right(ui, &painter, parent_rect, &parent_text, theme::font(11.0), t.text_tertiary);
            if resp.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                clicked = Some(index);
            }
        }
        if let Some(index) = clicked {
            self.apply_suggestion(index);
        } else if bg_resp.clicked()
            || (!ctx.memory(|m| m.has_focus(egui::Id::unique("address-input"))) && !ctx.input(|i| i.pointer.any_down()))
        {
            // If the user pressed inside the suggestion popup and released on padding
            // or dragged out without picking a row, return focus to the address editor.
            self.address.focus = true;
        }
    }

    fn paint_menu(&mut self, ui: &mut egui::Ui, full: Rect) {
        let ctx = ui.ctx().clone();
        let pop_id = egui::Id::unique("more-menu-pop");
        let Some(anchor) = self.menu_anchor else {
            crate::ui::spring::clear(&ctx, pop_id);
            ctx.data_mut(|data| data.remove_temp::<MoreMenuState>(menu_state_id()));
            return;
        };
        let t = self.theme.clone();
        let base_painter = ui.painter().clone();

        let natural_h = more_menu_natural_height();
        let (base_menu, scrollable) = compute_more_menu_rect(anchor, full, natural_h);
        let reveal = crate::ui::spring::ease_out_reveal(&ctx, pop_id, 0.12);
        let menu =
            Rect::from_min_size(Pos2::new(base_menu.left(), base_menu.top() - (1.0 - reveal) * 5.0), base_menu.size());
        let clip_menu =
            Rect::from_min_size(menu.min, Vec2::new(menu.width(), (menu.height() * (0.82 + 0.18 * reveal)).max(24.0)));

        // Full-window hit area closes the menu on outside left/right clicks and
        // traps focus so Tab / arrow keys never drift to widgets behind the menu.
        let blocker = ui.interact(Rect::EVERYTHING, egui::Id::unique("menu-blocker"), Sense::click());
        trap_popup_focus(&ctx, &blocker);

        let (escape, up, down, left, right, enter) = ctx.input(|input| {
            (
                input.key_pressed(egui::Key::Escape),
                input.key_pressed(egui::Key::ArrowUp),
                input.key_pressed(egui::Key::ArrowDown),
                input.key_pressed(egui::Key::ArrowLeft),
                input.key_pressed(egui::Key::ArrowRight),
                input.key_pressed(egui::Key::Enter),
            )
        });
        if escape {
            self.menu_anchor = None;
            crate::ui::spring::clear(&ctx, pop_id);
            ctx.data_mut(|data| data.remove_temp::<MoreMenuState>(menu_state_id()));
            return;
        }

        let mut state = ctx.data_mut(|data| data.get_temp::<MoreMenuState>(menu_state_id()).unwrap_or_default());
        let selectable = menu_selectable_count(self);
        if let Some(index) = state.kbd {
            state.kbd = Some(index.min(selectable.saturating_sub(1)));
        }

        // Keyboard navigation routes to the open submenu when active, otherwise top-level list.
        let mut chosen: Option<Command> = None;
        let open_submenu_def = state.open_sub.and_then(|id| {
            MENU.iter().find_map(|e| match e {
                MenuEntry::Submenu(sub) if sub.id == id => Some(*sub),
                _ => None,
            })
        });
        if let Some(sub) = open_submenu_def {
            let sub_count = sub.items.len();
            if left {
                state.open_sub = None;
                state.sub_kbd = None;
                state.sub_close_at = None;
            } else if sub_count > 0 {
                if down {
                    state.sub_kbd = Some(match state.sub_kbd {
                        Some(index) => (index + 1) % sub_count,
                        None => 0,
                    });
                } else if up {
                    state.sub_kbd = Some(match state.sub_kbd {
                        Some(index) => (index + sub_count - 1) % sub_count,
                        None => sub_count - 1,
                    });
                } else if enter && let Some(idx) = state.sub_kbd.filter(|&i| i < sub_count) {
                    chosen = Some(sub.items[idx].command);
                }
            }
        } else if selectable > 0 {
            if down {
                state.kbd = Some(match state.kbd {
                    Some(index) => (index + 1) % selectable,
                    None => 0,
                });
            } else if up {
                state.kbd = Some(match state.kbd {
                    Some(index) => (index + selectable - 1) % selectable,
                    None => selectable - 1,
                });
            } else if (right || enter)
                && let Some(idx) = state.kbd
                && let Some(entry) = menu_entry_at_ordinal(self, idx)
            {
                match entry {
                    MenuEntry::Submenu(sub) => {
                        state.open_sub = Some(sub.id);
                        state.sub_kbd = Some(0);
                        state.sub_close_at = None;
                    }
                    MenuEntry::Item(item) if enter => {
                        chosen = Some(item.command);
                    }
                    _ => {}
                }
            }
        }

        base_painter.rect_filled(
            clip_menu.translate(Vec2::new(0.0, 4.0)).expand(2.0),
            CornerRadius::same(8),
            widgets::fade(t.shadow, reveal),
        );
        base_painter.rect_filled(clip_menu, CornerRadius::same(8), widgets::fade(t.popup, reveal));
        base_painter.rect_stroke(
            clip_menu,
            CornerRadius::same(8),
            Stroke::new(1.0, widgets::fade(t.border, reveal)),
            egui::StrokeKind::Inside,
        );
        let _ = ui.interact(menu, egui::Id::unique("more-menu-surface"), Sense::click());

        let inner = menu.shrink2(Vec2::new(4.0, 6.0));
        let row_w = (inner.width() - if scrollable { 8.0 } else { 0.0 }).max(120.0);
        let pointer_pos = ctx.pointer_hover_pos();
        let pointer_in_prev_sub = state.last_sub_rect.is_some_and(|sr| pointer_pos.is_some_and(|p| sr.contains(p)));
        let now_sec = ctx.input(|i| i.time);

        let mut hovered_sub: Option<&'static str> = None;
        let mut hovered_regular_row = false;
        let mut open_sub_anchor_y: Option<f32> = None;

        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        child.set_clip_rect(inner.intersect(clip_menu));
        egui::ScrollArea::vertical().id_salt("more-menu-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let painter = ui.painter().clone();
            let mut ordinal = 0usize;
            for entry in MENU {
                match entry {
                    MenuEntry::Separator => {
                        let (sep_rect, _) = ui.allocate_exact_size(Vec2::new(row_w, MORE_SEP_H), Sense::hover());
                        painter.line_segment(
                            [
                                Pos2::new(sep_rect.left() + 6.0, sep_rect.center().y),
                                Pos2::new(sep_rect.right() - 6.0, sep_rect.center().y),
                            ],
                            Stroke::new(1.0, t.divider),
                        );
                    }
                    MenuEntry::Submenu(sub) => {
                        let (row, resp) = ui.allocate_exact_size(Vec2::new(row_w, MORE_ROW_H), Sense::click());
                        let id = egui::Id::unique(("menu-sub", sub.id));
                        let is_open = state.open_sub == Some(sub.id);
                        let hover = widgets::hover_t(&ctx, id, resp.hovered() || is_open);
                        if state.kbd.is_none() && resp.hovered() {
                            state.kbd = Some(ordinal);
                        }
                        if state.open_sub == Some(sub.id) {
                            open_sub_anchor_y = Some(row.top());
                        }
                        let keyboard = state.kbd == Some(ordinal);
                        if keyboard || is_open {
                            painter.rect_filled(row, CornerRadius::same(4), t.selected);
                        } else if hover > 0.0 {
                            painter.rect_filled(row, CornerRadius::same(4), widgets::fade(t.hover_strong, hover));
                        }
                        let (label_rect, _) = more_menu_row_rects(row, false, true);
                        widgets::text_left(
                            ui,
                            &painter,
                            label_rect,
                            sub.label,
                            theme::font(12.5),
                            if is_open || keyboard { t.text } else { t.text_secondary },
                            0.0,
                        );
                        widgets::icon_at(
                            &painter,
                            Pos2::new(row.right() - 14.0, row.center().y),
                            icons::CHEVRON_RIGHT,
                            9.0,
                            t.text_tertiary,
                        );
                        if resp.hovered() {
                            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                            hovered_sub = Some(sub.id);
                            open_sub_anchor_y = Some(row.top());
                        }
                        if resp.clicked() {
                            state.open_sub = Some(sub.id);
                            state.sub_kbd = None;
                            state.sub_close_at = None;
                            open_sub_anchor_y = Some(row.top());
                        }
                        ordinal += 1;
                    }
                    MenuEntry::Item(item) => {
                        let enabled = menu_enabled(self, item);
                        let checked = menu_checked(self, item.check);
                        let (row, resp) = ui.allocate_exact_size(Vec2::new(row_w, MORE_ROW_H), Sense::click());
                        let id = egui::Id::unique(("menu-item", item.label));
                        let hover = widgets::hover_t(&ctx, id, resp.hovered() && enabled);
                        if enabled && state.kbd.is_none() && resp.hovered() {
                            state.kbd = Some(ordinal);
                        }
                        let keyboard = enabled && state.kbd == Some(ordinal) && state.open_sub.is_none();
                        if keyboard {
                            painter.rect_filled(row, CornerRadius::same(4), t.selected);
                        } else if hover > 0.0 {
                            painter.rect_filled(row, CornerRadius::same(4), widgets::fade(t.hover_strong, hover));
                        }
                        if checked {
                            let color = if enabled { t.accent } else { widgets::fade(t.accent, 0.45) };
                            widgets::icon_at(
                                &painter,
                                Pos2::new(row.left() + 14.0, row.center().y),
                                icons::CHECK,
                                10.0,
                                color,
                            );
                        }
                        let text_color = if !enabled {
                            t.text_disabled
                        } else if checked {
                            t.text
                        } else {
                            t.text_secondary
                        };
                        let (label_rect, shortcut_rect) = more_menu_row_rects(row, item.shortcut.is_some(), false);
                        widgets::text_left(ui, &painter, label_rect, item.label, theme::font(12.5), text_color, 0.0);
                        if let (Some(shortcut), Some(srect)) = (item.shortcut, shortcut_rect) {
                            widgets::text_right(
                                ui,
                                &painter,
                                srect,
                                shortcut,
                                theme::font(11.0),
                                if enabled { t.text_tertiary } else { t.text_disabled },
                            );
                        }
                        if resp.hovered() {
                            hovered_regular_row = true;
                        }
                        if enabled {
                            if resp.hovered() {
                                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if resp.clicked() {
                                chosen = Some(item.command);
                            }
                            ordinal += 1;
                        }
                    }
                }
            }
        });

        // Submenu hover switching with a brief grace window for diagonal pointer motion.
        if let Some(sub_id) = hovered_sub {
            if state.open_sub != Some(sub_id) {
                state.open_sub = Some(sub_id);
                state.sub_kbd = None;
            }
            state.sub_close_at = None;
        } else if pointer_in_prev_sub {
            state.sub_close_at = None;
        } else if hovered_regular_row && state.open_sub.is_some() {
            match state.sub_close_at {
                None => {
                    state.sub_close_at = Some(now_sec + 0.16);
                    ctx.request_repaint_after(std::time::Duration::from_millis(165));
                }
                Some(deadline) if now_sec >= deadline => {
                    state.open_sub = None;
                    state.sub_kbd = None;
                    state.sub_close_at = None;
                }
                Some(_) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(30));
                }
            }
        }

        // Paint active submenu using the UNCLIPPED `base_painter` so it is never cut off.
        let mut active_sub_rect: Option<Rect> = None;
        if let Some(sub_id) = state.open_sub
            && let Some(sub) = MENU.iter().find_map(|e| match e {
                MenuEntry::Submenu(s) if s.id == sub_id => Some(*s),
                _ => None,
            })
        {
            let sub_h = sub.items.len() as f32 * MORE_ROW_H + 12.0;
            let sub_x = if menu.right() + 2.0 + MORE_SUB_W <= full.right() - 8.0 {
                menu.right() + 2.0
            } else {
                (menu.left() - MORE_SUB_W - 2.0).max(full.left() + 8.0)
            };
            let anchor_y = open_sub_anchor_y.unwrap_or(menu.top() + 6.0);
            let max_sub_y = (full.bottom() - sub_h - 8.0).max(full.top() + 8.0);
            let sub_y = anchor_y.clamp(full.top() + 8.0, max_sub_y);
            let sub_rect = Rect::from_min_size(Pos2::new(sub_x, sub_y), Vec2::new(MORE_SUB_W, sub_h));
            active_sub_rect = Some(sub_rect);

            base_painter.rect_filled(
                sub_rect.translate(Vec2::new(0.0, 4.0)).expand(2.0),
                CornerRadius::same(8),
                t.shadow,
            );
            base_painter.rect_filled(sub_rect, CornerRadius::same(8), t.popup);
            base_painter.rect_stroke(
                sub_rect,
                CornerRadius::same(8),
                Stroke::new(1.0, t.border),
                egui::StrokeKind::Inside,
            );
            let _ = ui.interact(sub_rect, egui::Id::unique(("more-sub-surface", sub.id)), Sense::click());

            for (idx, item) in sub.items.iter().enumerate() {
                let checked = menu_checked(self, item.check);
                let row = Rect::from_min_size(
                    Pos2::new(sub_rect.left() + 4.0, sub_rect.top() + 6.0 + idx as f32 * MORE_ROW_H),
                    Vec2::new(MORE_SUB_W - 8.0, MORE_ROW_H),
                );
                let id = egui::Id::unique(("menu-sub-item", sub.id, item.label));
                let resp = ui.interact(row, id, Sense::click());
                let hover = widgets::hover_t(&ctx, id, resp.hovered());
                if resp.hovered() {
                    state.sub_kbd = Some(idx);
                    state.sub_close_at = None;
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let keyboard = state.sub_kbd == Some(idx);
                if keyboard {
                    base_painter.rect_filled(row, CornerRadius::same(4), t.selected);
                } else if hover > 0.0 {
                    base_painter.rect_filled(row, CornerRadius::same(4), widgets::fade(t.hover_strong, hover));
                }
                if checked {
                    widgets::icon_at(
                        &base_painter,
                        Pos2::new(row.left() + 14.0, row.center().y),
                        icons::CHECK,
                        10.0,
                        t.accent,
                    );
                }
                let (label_rect, _) = more_menu_row_rects(row, false, false);
                widgets::text_left(
                    ui,
                    &base_painter,
                    label_rect,
                    item.label,
                    theme::font(12.5),
                    if checked { t.text } else { t.text_secondary },
                    0.0,
                );
                if resp.clicked() {
                    chosen = Some(item.command);
                }
            }
        }
        state.last_sub_rect = active_sub_rect;

        let outside_click = (blocker.clicked_by(PointerButton::Primary)
            || blocker.clicked_by(PointerButton::Secondary))
            && ctx
                .pointer_interact_pos()
                .is_none_or(|pos| !menu.contains(pos) && active_sub_rect.is_none_or(|sr| !sr.contains(pos)));

        if let Some(command) = chosen {
            self.menu_anchor = None;
            crate::ui::spring::clear(&ctx, pop_id);
            ctx.data_mut(|data| data.remove_temp::<MoreMenuState>(menu_state_id()));
            self.command(command);
        } else if outside_click {
            self.menu_anchor = None;
            crate::ui::spring::clear(&ctx, pop_id);
            ctx.data_mut(|data| data.remove_temp::<MoreMenuState>(menu_state_id()));
        } else {
            ctx.data_mut(|data| data.insert_temp(menu_state_id(), state));
        }
    }

    fn paint_tab_menu(&mut self, ui: &mut egui::Ui, full: Rect) {
        let Some(mut menu) = ui.ctx().data(|data| data.get_temp::<TabMenu>(tab_menu_id())) else { return };
        if menu.tab >= self.tabs.len() {
            close_tab_menu(ui.ctx());
            return;
        }
        let t = self.theme.clone();
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();

        let labels = ["关闭", "关闭其他", "复制标签路径", "在新标签页中打开"];
        let can_close = self.tabs.len() > 1;
        let row_h = 28.0;
        let width = 204.0;
        let height = row_h * labels.len() as f32 + 8.0;
        let mut pos = menu.anchor;
        if pos.x + width > full.right() - 8.0 {
            pos.x = full.right() - width - 8.0;
        }
        if pos.y + height > full.bottom() - 8.0 {
            pos.y = (menu.anchor.y - height).max(full.top() + 8.0);
        }
        let rect = Rect::from_min_size(pos, Vec2::new(width, height));

        // Same focus trick as the main menu: the blocker owns the keyboard while
        // the popup is open, so shortcuts and Tab do not leak behind it.
        let blocker = ui.interact(Rect::EVERYTHING, egui::Id::unique("tab-menu-blocker"), Sense::click());
        trap_popup_focus(&ctx, &blocker);
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            close_tab_menu(&ctx);
            return;
        }

        painter.rect_filled(rect.translate(Vec2::new(0.0, 4.0)).expand(2.0), CornerRadius::same(8), t.shadow);
        painter.rect_filled(rect, CornerRadius::same(8), t.popup);
        painter.rect_stroke(rect, CornerRadius::same(8), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
        let _ = ui.interact(rect, egui::Id::unique("tab-menu-surface"), Sense::click());

        let mut chosen: Option<usize> = None;
        for (index, label) in labels.iter().enumerate() {
            // Both "关闭" and "关闭其他" are disabled when only a single tab exists.
            let enabled = match index {
                0 | 1 => can_close,
                _ => true,
            };
            let row = Rect::from_min_size(
                Pos2::new(rect.left() + 4.0, rect.top() + 4.0 + index as f32 * row_h),
                Vec2::new(width - 8.0, row_h),
            );
            let id = egui::Id::unique(("tab-menu", label));
            let resp = ui.interact(row, id, Sense::click());
            let hover = widgets::hover_t(&ctx, id, resp.hovered() && enabled);
            if hover > 0.0 {
                painter.rect_filled(row, CornerRadius::same(4), widgets::fade(t.hover_strong, hover));
            }
            widgets::text_left(
                ui,
                &painter,
                Rect::from_min_max(Pos2::new(row.left() + 12.0, row.top()), Pos2::new(row.right() - 8.0, row.bottom())),
                label,
                theme::font(12.5),
                if enabled { t.text_secondary } else { t.text_disabled },
                0.0,
            );
            if enabled {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    chosen = Some(index);
                }
            }
        }

        let armed = menu.armed;
        if !menu.armed {
            menu.armed = true;
            ctx.data_mut(|data| data.insert_temp(tab_menu_id(), menu));
        }

        let outside_click = armed
            && (blocker.clicked_by(PointerButton::Primary) || blocker.clicked_by(PointerButton::Secondary))
            && ctx.pointer_interact_pos().is_none_or(|p| !rect.contains(p));

        if let Some(index) = chosen {
            close_tab_menu(&ctx);
            match index {
                0 => self.close_tab(menu.tab),
                1 => self.close_other_tabs(menu.tab),
                2 => {
                    let path = self.tabs[menu.tab]
                        .location
                        .path
                        .clone()
                        .map(|path| path.to_string_lossy().to_string())
                        .unwrap_or_else(|| self.tabs[menu.tab].location.parsing_name());
                    match crate::shell::clipboard::set_text(&path) {
                        Ok(()) => self.toast("标签路径已复制"),
                        Err(err) => self.toast_error(format!("{err:#}")),
                    }
                }
                _ => {
                    self.activate_tab(menu.tab);
                    self.command(Command::NewTab);
                }
            }
        } else if outside_click {
            close_tab_menu(&ctx);
        }
    }

    /// Closes every tab except `keep`, tracking its shifting index.
    fn close_other_tabs(&mut self, keep: usize) {
        let mut keep = keep;
        while self.tabs.len() > 1 {
            let victim = if keep == 0 { 1 } else { 0 };
            if victim < keep {
                keep -= 1;
            }
            self.close_tab(victim);
        }
        self.activate_tab(keep);
    }

    fn paint_toasts(&mut self, ui: &mut egui::Ui, full: Rect) {
        let t = self.theme.clone();
        let painter = ui.painter().clone();
        let now = now();
        self.toasts.retain(|toast| now - toast.born < 4.2);
        let mut y = full.bottom() - theme::STATUS_H - 14.0;
        for toast in self.toasts.iter().rev().take(3) {
            let age = now - toast.born;
            let alpha = if age > 3.6 { ((4.2 - age) / 0.6) as f32 } else { 1.0 };
            let error = toast.error;
            // Dark toast surface in both themes: keeps light text readable and the
            // overlay clearly above the window content.
            let fill = if t.dark { t.popup } else { t.text };
            let text_color = if error {
                if t.dark { t.danger } else { Color32::from_rgb(0xFF, 0xB4, 0xBD) }
            } else {
                Color32::WHITE
            };
            let color = widgets::fade(text_color, alpha);
            let g = widgets::galley(ui, &toast.text, theme::font(12.5), color, 520.0);
            let w = g.size().x + 32.0;
            let pop = crate::ui::spring::animate(
                ui.ctx(),
                egui::Id::unique(("toast-pop", toast.born.to_bits())),
                1.0,
                0.0,
                crate::ui::spring::JELLY,
            );
            let exit_drop = if age > 3.6 { ((age - 3.6) / 0.6) as f32 * 10.0 } else { 0.0 };
            let base = Rect::from_min_size(Pos2::new(full.center().x - w * 0.5, y - 34.0), Vec2::new(w, 34.0));
            let rect = crate::ui::spring::slide_y(
                crate::ui::spring::scale_rect_around(base, base.center(), 0.92 + 0.08 * pop),
                (1.0 - pop) * 20.0 + exit_drop,
            );
            // Subtle drop shadow + 1px border keep toasts crisp even over dark modals/popups.
            painter.rect_filled(
                rect.translate(Vec2::new(0.0, 3.0)).expand(1.5),
                CornerRadius::same(7),
                widgets::fade(t.shadow, alpha * 0.85),
            );
            painter.rect_filled(rect, CornerRadius::same(6), widgets::fade(fill, alpha));
            let border = if error { t.danger } else { t.border };
            widgets::stroke_rect(&painter, rect, 6, Stroke::new(1.0, widgets::fade(border, alpha)));
            widgets::galley_at(
                &painter,
                Pos2::new(rect.center().x - g.size().x * 0.5, rect.center().y - g.size().y * 0.5),
                g,
                color,
            );
            y -= 42.0;
        }
    }

    fn paint_dialog(&mut self, ui: &mut egui::Ui, full: Rect) {
        let Some(mut dialog) = self.dialog.take() else {
            self.dialog_was_open = false;
            return;
        };
        // A dialog opened by this frame's click must not swallow that same click.
        let armed = self.dialog_was_open;
        self.dialog_was_open = true;
        if !armed {
            // The dialog owns the keyboard now; drop inline editors so a stale
            // editor does not keep consuming Enter/Esc under the modal shade.
            self.address.editing = false;
            self.rename = None;
        }
        let t = self.theme.clone();
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let shade = ui.interact(full, egui::Id::unique("dialog-shade"), Sense::click());
        painter.rect_filled(full, CornerRadius::ZERO, Color32::from_black_alpha(70));

        let card = Rect::from_center_size(full.center(), Vec2::new(460.0, 316.0));
        painter.rect_filled(card.translate(Vec2::new(0.0, 6.0)).expand(2.0), CornerRadius::same(11), t.shadow);
        painter.rect_filled(card, CornerRadius::same(10), t.popup);
        painter.rect_stroke(card, CornerRadius::same(10), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
        // Absorb pointer clicks on the dialog card (title, spacing, preview text)
        // so they never penetrate to `shade` and accidentally cancel the dialog.
        let card_catch = ui.interact(card, ui.scope_id().with("dialog-card-catch"), Sense::click());

        let left = card.left() + 24.0;
        let width = card.width() - 48.0;
        let mut y = card.top() + 22.0;

        // 6-stop focus ring for BatchRename: find -> replace -> prefix -> suffix -> 取消 -> 重命名.
        // Cancel egui's built-in focus_direction traversal (which fires even on the first
        // frame a widget gains focus before `had_focus_last_frame` becomes true) and
        // consume Tab/Shift+Tab explicitly so focus never escapes behind the modal.
        let ring = batch_rename_focus_ring();
        let current_focused = ctx.memory(|m| m.focused());
        ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        let current_idx = current_focused.and_then(|id| ring.iter().position(|&r| r == id));
        let tab_backward = ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab));
        let tab_forward = !tab_backward && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
        let target_focus: Option<egui::Id> = if tab_forward {
            let next = match current_idx {
                Some(i) => (i + 1) % ring.len(),
                None => 0,
            };
            Some(ring[next])
        } else if tab_backward {
            let prev = match current_idx {
                Some(i) => (i + ring.len() - 1) % ring.len(),
                None => ring.len() - 1,
            };
            Some(ring[prev])
        } else if current_focused.is_some_and(|id| !ring.contains(&id) && id != card_catch.id) {
            // Pull stray focus from background widgets back inside the dialog.
            Some(ring[0])
        } else {
            None
        };

        let button_focus_request: Option<bool> = match target_focus {
            Some(id) if id == dialog_cancel_id() => Some(false),
            Some(id) if id == dialog_confirm_id() => Some(true),
            _ => None,
        };

        let (confirm, mut cancel) = match &mut dialog {
            Dialog::BatchRename { find, replace, prefix, suffix, focus } => {
                let count = self.active().selection.len();
                widgets::text_left(
                    ui,
                    &painter,
                    Rect::from_min_size(Pos2::new(left, y), Vec2::new(width, 24.0)),
                    &format!("批量重命名 ({count} 个项目)"),
                    theme::semibold(16.0),
                    t.text,
                    0.0,
                );
                y += 34.0;
                let fields: [(egui::Id, &str, &mut String, bool); 4] = [
                    (ring[0], "查找文本", find, true),
                    (ring[1], "替换为", replace, false),
                    (ring[2], "添加前缀", prefix, false),
                    (ring[3], "添加后缀", suffix, false),
                ];
                for (field_id, hint, buf, wants_initial_focus) in fields {
                    let field = Rect::from_min_size(Pos2::new(left, y), Vec2::new(width, 30.0));
                    painter.rect_filled(field, CornerRadius::same(5), t.surface_alt);
                    painter.rect_stroke(
                        field,
                        CornerRadius::same(5),
                        Stroke::new(1.0, t.border),
                        egui::StrokeKind::Inside,
                    );
                    let te = egui::TextEdit::singleline(buf)
                        .id(field_id)
                        .font(theme::font(12.5))
                        .hint_text(hint)
                        .frame(egui::Frame::NONE)
                        .lock_focus(true)
                        .desired_width(field.width() - 20.0);
                    let resp = ui.put(field.shrink2(Vec2::new(10.0, 5.0)), te);
                    let take_focus = (wants_initial_focus && *focus) || target_focus == Some(field_id);
                    if take_focus {
                        resp.request_focus();
                        if wants_initial_focus {
                            *focus = false;
                        }
                    }
                    if resp.has_focus() || take_focus {
                        ctx.memory_mut(|m| {
                            m.set_focus_lock_filter(field_id, egui::EventFilter { tab: true, ..Default::default() });
                        });
                        painter.rect_stroke(
                            field,
                            CornerRadius::same(5),
                            Stroke::new(1.5, t.accent),
                            egui::StrokeKind::Inside,
                        );
                    }
                    y += 38.0;
                }
                // Live preview of the first selected entry.
                if let Some(first_idx) = self.active().selection.iter().next().copied()
                    && let Some(entry) = self.active().entries.get(first_idx)
                {
                    let preview =
                        crate::app::batch_rename_preview(&entry.name, entry.is_folder, find, replace, prefix, suffix);
                    widgets::text_left(
                        ui,
                        &painter,
                        Rect::from_min_size(Pos2::new(left, y), Vec2::new(width, 22.0)),
                        &format!("预览: {} → {}", entry.name, preview),
                        theme::font(11.5),
                        t.text_tertiary,
                        0.0,
                    );
                }
                let clicked =
                    dialog_buttons(ui, &painter, &t, card, card.bottom() - 52.0, "重命名", false, button_focus_request);
                (clicked == 1, clicked == 2)
            }
        };

        // Ensure Tab remains locked on whichever widget holds focus inside the dialog
        // (including `card_catch` if the user clicked on empty dialog chrome).
        if let Some(focused) = ctx.memory(|m| m.focused()) {
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(focused, egui::EventFilter { tab: true, ..Default::default() });
            });
        }

        let clicked_outside = shade.clicked() && ctx.pointer_interact_pos().is_none_or(|pos| !card.contains(pos));
        if armed && clicked_outside {
            cancel = true;
        }
        if confirm {
            self.dialog = Some(dialog);
            self.confirm_dialog();
        } else if cancel {
            self.dialog = None;
        } else {
            self.dialog = Some(dialog);
        }
    }
}

/// True while the dialog's cancel button owns the keyboard, so Enter cancels.
pub(crate) fn dialog_cancel_focused(ctx: &egui::Context) -> bool {
    ctx.memory(|memory| memory.has_focus(dialog_cancel_id()))
}

fn dialog_cancel_id() -> egui::Id {
    egui::Id::unique("dialog-cancel")
}

fn dialog_confirm_id() -> egui::Id {
    egui::Id::unique("dialog-confirm")
}

/// Draws the Cancel/Confirm pair and returns 1 for confirm, 2 for cancel.
/// `request` moves keyboard focus onto that button (true = confirm).
// Immediate-mode paint helper: each argument is distinct draw state.
#[allow(clippy::too_many_arguments)]
fn dialog_buttons(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    t: &Theme,
    card: Rect,
    y: f32,
    confirm_label: &str,
    danger: bool,
    request: Option<bool>,
) -> u8 {
    let ctx = ui.ctx().clone();
    let mut result = 0;
    let confirm_w = 92.0;
    let cancel_w = 76.0;
    let right = card.right() - 24.0;
    let confirm_rect = Rect::from_min_size(Pos2::new(right - confirm_w, y), Vec2::new(confirm_w, 32.0));
    let cancel_rect =
        Rect::from_min_size(Pos2::new(confirm_rect.left() - 10.0 - cancel_w, y), Vec2::new(cancel_w, 32.0));

    let cancel_id = dialog_cancel_id();
    let cancel_resp = ui.interact(cancel_rect, cancel_id, Sense::click());
    if request == Some(false) {
        cancel_resp.request_focus();
    }
    let cancel_hover = widgets::hover_t(&ctx, cancel_id, cancel_resp.hovered());
    painter.rect_filled(cancel_rect, CornerRadius::same(5), if cancel_hover > 0.0 { t.hover_strong } else { t.hover });
    painter.rect_stroke(cancel_rect, CornerRadius::same(5), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let g = widgets::galley(ui, "取消", theme::font(12.5), t.text, cancel_rect.width());
    widgets::galley_at(
        painter,
        Pos2::new(cancel_rect.center().x - g.size().x * 0.5, cancel_rect.center().y - g.size().y * 0.5),
        g,
        t.text,
    );
    if cancel_resp.has_focus() || request == Some(false) {
        ctx.memory_mut(|m| {
            m.set_focus_lock_filter(cancel_id, egui::EventFilter { tab: true, ..Default::default() });
        });
        widgets::stroke_rect(painter, cancel_rect.expand(1.5), 6, Stroke::new(1.5, t.accent));
    }
    if cancel_resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if cancel_resp.clicked() {
        result = 2;
    }

    let confirm_id = dialog_confirm_id();
    let confirm_resp = ui.interact(confirm_rect, confirm_id, Sense::click());
    if request == Some(true) {
        confirm_resp.request_focus();
    }
    let confirm_hover = widgets::hover_t(&ctx, confirm_id, confirm_resp.hovered());
    let base = if danger { DANGER_FILL } else { t.accent };
    let hover_fill = if danger { base.gamma_multiply(1.08) } else { t.accent_strong };
    painter.rect_filled(confirm_rect, CornerRadius::same(5), if confirm_hover > 0.0 { hover_fill } else { base });
    let text_color = if danger { Color32::WHITE } else { t.text_on_accent };
    let g = widgets::galley(ui, confirm_label, theme::font(12.5), text_color, confirm_rect.width());
    widgets::galley_at(
        painter,
        Pos2::new(confirm_rect.center().x - g.size().x * 0.5, confirm_rect.center().y - g.size().y * 0.5),
        g,
        text_color,
    );
    if confirm_resp.has_focus() || request == Some(true) {
        ctx.memory_mut(|m| {
            m.set_focus_lock_filter(confirm_id, egui::EventFilter { tab: true, ..Default::default() });
        });
        widgets::stroke_rect(painter, confirm_rect.expand(1.5), 6, Stroke::new(1.5, t.text));
    }
    if confirm_resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if confirm_resp.clicked() {
        result = 1;
    }
    result
}

/// Destructive button fill; `Theme::danger` is the danger *text* token and is
/// too light in the dark theme to carry white label text.
const DANGER_FILL: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);

#[cfg(test)]
mod tests {
    use super::*;

    /// UI-10: address suggestions name and parent-path columns must never overlap
    /// and must maintain an explicit horizontal gap between them.
    #[test]
    fn address_suggestions_columns_have_positive_gap_and_never_overlap() {
        for width in [300.0f32, 420.0, 640.0] {
            let row = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(width, 26.0));
            let (name_rect, parent_rect) = address_suggestion_columns(row);
            assert!(
                name_rect.right() < parent_rect.left(),
                "name ({name_rect:?}) overlapped parent ({parent_rect:?}) at width {width}"
            );
            assert!(
                parent_rect.left() - name_rect.right() >= 8.0,
                "gap too small between name and parent at width {width}: {}",
                parent_rect.left() - name_rect.right()
            );
            assert!(name_rect.width() > 0.0 && parent_rect.width() > 0.0);
            assert!(parent_rect.right() <= row.right());
        }
    }

    /// BUG-16 & UI-09: `更多` menu height must stay bounded to `full.height() - 16.0`
    /// (scrolling when needed), and label vs shortcut columns must never overlap.
    #[test]
    fn more_menu_bounds_height_and_separates_label_from_shortcut() {
        let natural = more_menu_natural_height();
        assert!(natural < 750.0, "slimmed more menu should be far shorter than legacy 943px, got {natural}");

        for win_h in [520.0f32, 600.0, 900.0] {
            let full = Rect::from_min_size(Pos2::ZERO, Vec2::new(820.0, win_h));
            let anchor = Pos2::new(full.right() - MORE_MENU_W - 10.0, 92.0);
            let (rect, scrollable) = compute_more_menu_rect(anchor, full, natural);
            let cap = (full.height() - 16.0).max(160.0);
            assert!(
                rect.height() <= cap + 0.01,
                "more menu height {} exceeded cap {} at window height {win_h}",
                rect.height(),
                cap
            );
            assert!(rect.top() >= full.top() + 7.99);
            assert!(rect.bottom() <= full.bottom() - 7.99);
            assert_eq!(scrollable, natural > cap);
        }

        // Verify label and right-aligned shortcut have positive clearance.
        let row = Rect::from_min_size(Pos2::new(4.0, 6.0), Vec2::new(MORE_MENU_W - 16.0, MORE_ROW_H));
        let (label_rect, shortcut_rect) = more_menu_row_rects(row, true, false);
        let shortcut_rect = shortcut_rect.expect("shortcut rect");
        assert!(
            label_rect.right() < shortcut_rect.left(),
            "label ({label_rect:?}) overlapped shortcut ({shortcut_rect:?})"
        );
        assert!(
            (label_rect.right() - (row.right() - 96.0)).abs() < 0.01,
            "label right edge must align with row.right() - 96.0"
        );
        // Ensure no system-only settings toggle leaked back into `MENU`.
        for entry in MENU {
            if let MenuEntry::Item(item) = entry {
                assert!(
                    !is_settings_only_command(item.command),
                    "settings-only command {:?} must stay out of more menu",
                    item.command
                );
            }
        }
    }

    /// BUG-15: clicking inside the BatchRename card (e.g. title or spacing) must
    /// NOT penetrate to the backdrop shade and cancel the dialog, whereas clicking
    /// outside the card on the shade DOES cancel it.
    #[test]
    fn batch_rename_card_clicks_do_not_cancel_while_backdrop_click_cancels() {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }

        let ctx = egui::Context::default();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let settings =
            crate::core::settings::Settings { tabs: vec![dir.to_string_lossy().to_string()], ..Default::default() };
        let mut app = LumenApp::with_context(&ctx, settings);
        app.dialog = Some(Dialog::BatchRename {
            find: String::new(),
            replace: String::new(),
            prefix: String::new(),
            suffix: String::new(),
            focus: true,
        });

        let full = Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0));
        let card = Rect::from_center_size(full.center(), Vec2::new(460.0, 316.0));
        let run_frame = |app: &mut LumenApp, raw: egui::RawInput| {
            let mut out = ctx.run_ui(raw, |ui| {
                app.paint_dialog(ui, full);
            });
            out.textures_delta.clear();
        };

        // Frame 1: arm the dialog.
        run_frame(&mut app, egui::RawInput { screen_rect: Some(full), ..Default::default() });
        assert!(app.dialog.is_some());

        // Click on the dialog title bar area inside `card` (not on any input or button).
        let title_pos = Pos2::new(card.center().x, card.top() + 18.0);
        for pressed in [true, false] {
            let mut raw = egui::RawInput { screen_rect: Some(full), ..Default::default() };
            raw.events.push(egui::Event::PointerMoved(title_pos));
            raw.events.push(egui::Event::PointerButton {
                pos: title_pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            run_frame(&mut app, raw);
        }
        assert!(app.dialog.is_some(), "clicking inside BatchRename card must NOT cancel dialog");

        // Click on the dark backdrop shade outside `card`.
        let shade_pos = Pos2::new(20.0, 20.0);
        for pressed in [true, false] {
            let mut raw = egui::RawInput { screen_rect: Some(full), ..Default::default() };
            raw.events.push(egui::Event::PointerMoved(shade_pos));
            raw.events.push(egui::Event::PointerButton {
                pos: shade_pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            run_frame(&mut app, raw);
        }
        assert!(app.dialog.is_none(), "clicking outside card on backdrop shade should cancel dialog");

        unsafe { CoUninitialize() };
    }

    /// BUG-30: BatchRename Tab and Shift+Tab must walk the full 6-stop focus ring
    /// (`find` -> `replace` -> `prefix` -> `suffix` -> `取消` -> `重命名` -> `find`)
    /// without ever leaking focus outside the dialog.
    #[test]
    fn batch_rename_tab_walks_full_six_stop_ring() {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }

        let ctx = egui::Context::default();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let settings =
            crate::core::settings::Settings { tabs: vec![dir.to_string_lossy().to_string()], ..Default::default() };
        let mut app = LumenApp::with_context(&ctx, settings);
        app.dialog = Some(Dialog::BatchRename {
            find: String::new(),
            replace: String::new(),
            prefix: String::new(),
            suffix: String::new(),
            focus: true,
        });

        let full = Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0));
        let run_frame = |app: &mut LumenApp, raw: egui::RawInput| {
            let mut out = ctx.run_ui(raw, |ui| {
                app.paint_dialog(ui, full);
            });
            out.textures_delta.clear();
        };

        // Initial frame focuses `find` (ring[0]).
        run_frame(&mut app, egui::RawInput { screen_rect: Some(full), ..Default::default() });
        let ring = batch_rename_focus_ring();
        assert_eq!(ctx.memory(|m| m.focused()), Some(ring[0]), "initial focus must land on find field");

        // Pressing Tab 6 times cycles through all 6 stops and wraps back to `find`.
        for expected_idx in [1usize, 2, 3, 4, 5, 0] {
            let mut raw = egui::RawInput { screen_rect: Some(full), ..Default::default() };
            raw.events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
            raw.events.push(egui::Event::Key {
                key: egui::Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            run_frame(&mut app, raw);
            assert_eq!(
                ctx.memory(|m| m.focused()),
                Some(ring[expected_idx]),
                "Tab should advance focus to stop {expected_idx}"
            );
        }

        // Pressing Shift+Tab reverses around the ring back to `重命名` then `取消`.
        for expected_idx in [5usize, 4, 3] {
            let mut raw = egui::RawInput { screen_rect: Some(full), ..Default::default() };
            raw.events.push(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
            raw.events.push(egui::Event::Key {
                key: egui::Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::SHIFT,
            });
            run_frame(&mut app, raw);
            assert_eq!(
                ctx.memory(|m| m.focused()),
                Some(ring[expected_idx]),
                "Shift+Tab should reverse focus to stop {expected_idx}"
            );
        }

        unsafe { CoUninitialize() };
    }
}
