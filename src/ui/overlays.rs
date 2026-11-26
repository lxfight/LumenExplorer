//! Floating overlays: menus, dialogs, address suggestions and toasts.

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};

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
}

fn tab_menu_id() -> egui::Id {
    egui::Id::unique("tab-context-menu")
}

pub(crate) fn open_tab_menu(ctx: &egui::Context, tab: usize, anchor: Pos2) {
    ctx.data_mut(|data| data.insert_temp(tab_menu_id(), TabMenu { tab, anchor }));
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
    TrayResident,
    WinE,
    AutoStart,
    Mica,
    SplitView,
}

#[derive(Clone, Copy)]
enum MenuGate {
    Always,
    Selection,
}

#[derive(Clone, Copy)]
struct MenuItem {
    label: &'static str,
    shortcut: Option<&'static str>,
    command: Command,
    check: MenuCheck,
    gate: MenuGate,
}

enum MenuEntry {
    Item(MenuItem),
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

const SEP: MenuEntry = MenuEntry::Separator;
const ALWAYS: MenuGate = MenuGate::Always;
const NEEDS_SELECTION: MenuGate = MenuGate::Selection;

/// Static menu definition: no per-frame allocation, separators are explicit and
/// the check marks read the live view state below.
const MENU: &[MenuEntry] = &[
    item("按名称排序", None, Command::SortBy(SortKey::Name), MenuCheck::Sort(SortKey::Name), ALWAYS),
    item(
        "按修改日期排序",
        None,
        Command::SortBy(SortKey::DateModified),
        MenuCheck::Sort(SortKey::DateModified),
        ALWAYS,
    ),
    item("按类型排序", None, Command::SortBy(SortKey::Type), MenuCheck::Sort(SortKey::Type), ALWAYS),
    item("按大小排序", None, Command::SortBy(SortKey::Size), MenuCheck::Sort(SortKey::Size), ALWAYS),
    SEP,
    item("不分组", None, Command::GroupBy(None), MenuCheck::Group(None), ALWAYS),
    item("按名称分组", None, Command::GroupBy(Some(SortKey::Name)), MenuCheck::Group(Some(SortKey::Name)), ALWAYS),
    item(
        "按修改日期分组",
        None,
        Command::GroupBy(Some(SortKey::DateModified)),
        MenuCheck::Group(Some(SortKey::DateModified)),
        ALWAYS,
    ),
    item("按类型分组", None, Command::GroupBy(Some(SortKey::Type)), MenuCheck::Group(Some(SortKey::Type)), ALWAYS),
    item("按大小分组", None, Command::GroupBy(Some(SortKey::Size)), MenuCheck::Group(Some(SortKey::Size)), ALWAYS),
    SEP,
    item("详细信息视图", None, Command::ViewDetails, MenuCheck::View(ViewMode::Details), ALWAYS),
    item("列表视图", None, Command::ViewList, MenuCheck::View(ViewMode::List), ALWAYS),
    item("平铺视图", None, Command::ViewTiles, MenuCheck::View(ViewMode::Tiles), ALWAYS),
    item("大图标视图", None, Command::ViewIcons, MenuCheck::View(ViewMode::Icons), ALWAYS),
    SEP,
    item("显示隐藏的项目", Some("Ctrl+H"), Command::ToggleHidden, MenuCheck::Hidden, ALWAYS),
    item("显示受保护的系统文件", None, Command::ToggleSystemFiles, MenuCheck::SystemFiles, ALWAYS),
    item("双栏分屏视图", Some("Ctrl+Shift+E"), Command::ToggleSplitView, MenuCheck::SplitView, ALWAYS),
    item("切换主题", Some("Ctrl+Shift+D"), Command::CycleTheme, MenuCheck::None, ALWAYS),
    item("Mica 背景材质", None, Command::ToggleMica, MenuCheck::Mica, ALWAYS),
    SEP,
    item("设置中心…", Some("Ctrl+,"), Command::OpenSettings, MenuCheck::None, ALWAYS),
    item("吸收已打开的 Explorer 窗口", Some("Ctrl+Shift+O"), Command::ImportExplorerTabs, MenuCheck::None, ALWAYS),
    item("关闭时常驻托盘", None, Command::ToggleTrayResident, MenuCheck::TrayResident, ALWAYS),
    item("全局呼出快捷键 (Ctrl+Alt+E)", None, Command::ToggleWinE, MenuCheck::WinE, ALWAYS),
    item("打开 Windows 启动文件夹", None, Command::ToggleAutoStart, MenuCheck::AutoStart, ALWAYS),
    SEP,
    item("新建窗口", Some("Ctrl+N"), Command::NewWindow, MenuCheck::None, ALWAYS),
    item("新建文件夹", Some("Ctrl+Shift+N"), Command::NewFolder, MenuCheck::None, ALWAYS),
    item("新建文本文档", None, Command::NewFile, MenuCheck::None, ALWAYS),
    SEP,
    item("打开方式", None, Command::OpenWith, MenuCheck::None, NEEDS_SELECTION),
    item("打开文件所在位置", None, Command::ShowInFolder, MenuCheck::None, NEEDS_SELECTION),
    item("复制路径", Some("Ctrl+Shift+C"), Command::CopyPath, MenuCheck::None, ALWAYS),
    item("在终端中打开", None, Command::OpenTerminal, MenuCheck::None, ALWAYS),
    SEP,
    item("刷新", Some("F5"), Command::Refresh, MenuCheck::None, ALWAYS),
];

/// Width of the "more" popup; chrome anchors it against the button's right edge.
pub(crate) const MORE_MENU_W: f32 = 244.0;

fn menu_kbd_id() -> egui::Id {
    egui::Id::unique("more-menu-kbd")
}

fn menu_checked(app: &LumenApp, check: MenuCheck) -> bool {
    match check {
        MenuCheck::None => false,
        MenuCheck::Sort(key) => app.active().sort.key == key,
        MenuCheck::Group(key) => app.active().group_by == key,
        MenuCheck::View(view) => app.active().view == view,
        MenuCheck::Hidden => app.show_hidden,
        MenuCheck::SystemFiles => app.show_system,
        MenuCheck::TrayResident => app.settings.tray_resident,
        MenuCheck::WinE => app.settings.intercept_win_e,
        MenuCheck::AutoStart => crate::shell::integrate::is_auto_start(),
        MenuCheck::Mica => app.settings.mica_backdrop,
        MenuCheck::SplitView => app.secondary_tab.is_some(),
    }
}

fn menu_enabled(app: &LumenApp, gate: MenuGate) -> bool {
    match gate {
        MenuGate::Always => true,
        MenuGate::Selection => !app.active().selection.is_empty(),
    }
}

fn menu_selectable_count(app: &LumenApp) -> usize {
    MENU.iter().filter(|entry| matches!(entry, MenuEntry::Item(item) if menu_enabled(app, item.gate))).count()
}

impl LumenApp {
    pub(crate) fn paint_overlays(&mut self, ui: &mut egui::Ui, full: Rect) {
        self.paint_drag_badge(ui, full);
        self.paint_toasts(ui, full);
        self.paint_address_suggestions(ui, full);
        self.paint_crumb_dropdown(ui, full);
        self.paint_dialog(ui, full);
        self.paint_menu(ui, full);
        self.paint_tab_menu(ui, full);
        self.paint_context_menu(ui, full);
        self.paint_quick_look(ui, full);
        self.paint_settings_modal(ui, full);
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

        let blocker = ui.interact(Rect::EVERYTHING, egui::Id::unique("crumb-menu-blocker"), Sense::click());
        if blocker.clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            crate::ui::spring::clear(&ctx, pop_id);
            return;
        }

        let row_h = 26.0;
        let visible = menu.items.len().clamp(1, 14);
        let width = 220.0;
        let height = visible as f32 * row_h + 10.0;
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
            child.set_clip_rect(inner);
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

        if let Some(path) = picked {
            crate::ui::spring::clear(&ctx, pop_id);
            self.navigate_to_path(&path);
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
        painter.rect_filled(rect, CornerRadius::same(6), t.popup);
        painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
        // Swallow clicks on the panel padding; otherwise they fall through to the
        // file list under the dropdown.
        ui.interact(rect, egui::Id::unique("addr-suggest-bg"), Sense::click());

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
            widgets::icon_at(&painter, Pos2::new(row.left() + 14.0, row.center().y), icons::FOLDER, 13.0, t.accent);
            widgets::text_left(
                ui,
                &painter,
                Rect::from_min_max(
                    Pos2::new(row.left() + 28.0, row.top()),
                    Pos2::new(row.left() + row.width() * 0.55, row.bottom()),
                ),
                name,
                theme::font(12.5),
                t.text,
                0.0,
            );
            let parent_text = path.parent().map(|parent| parent.to_string_lossy().to_string()).unwrap_or_default();
            widgets::text_right(
                ui,
                &painter,
                Rect::from_min_max(
                    Pos2::new(row.left() + row.width() * 0.5, row.top()),
                    Pos2::new(row.right() - 8.0, row.bottom()),
                ),
                &parent_text,
                theme::font(11.0),
                t.text_tertiary,
            );
            if resp.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                clicked = Some(index);
            }
        }
        if let Some(index) = clicked {
            self.apply_suggestion(index);
        }
    }

    fn paint_menu(&mut self, ui: &mut egui::Ui, full: Rect) {
        let ctx = ui.ctx().clone();
        let pop_id = egui::Id::unique("more-menu-pop");
        let Some(anchor) = self.menu_anchor else {
            crate::ui::spring::clear(&ctx, pop_id);
            return;
        };
        let t = self.theme.clone();
        let painter = ui.painter().clone();

        // A full-window hit area closes the menu on outside clicks. It doubles as
        // the keyboard focus anchor: with focus on a rect that covers everything,
        // egui's spatial arrow-key focus movement cannot drift to the widgets
        // behind the menu, so list keys and Enter stay with the menu.
        let blocker = ui.interact(Rect::EVERYTHING, egui::Id::unique("menu-blocker"), Sense::click());
        if blocker.clicked_by(egui::PointerButton::Primary) {
            self.menu_anchor = None;
            crate::ui::spring::clear(&ctx, pop_id);
            ctx.data_mut(|data| {
                data.remove_temp::<Option<usize>>(menu_kbd_id());
            });
            return;
        }
        if !blocker.has_focus() {
            blocker.request_focus();
        }

        let row_h = 28.0;
        let width = MORE_MENU_W;
        let height =
            MENU.iter().map(|entry| if matches!(entry, MenuEntry::Separator) { 9.0 } else { row_h }).sum::<f32>()
                + 12.0;
        let mut pos = anchor;
        if pos.x + width > full.right() - 8.0 {
            pos.x = full.right() - width - 8.0;
        }
        if pos.y + height > full.bottom() - 8.0 {
            pos.y = (anchor.y - height - 40.0).max(full.top() + 8.0);
        }
        let reveal = crate::ui::spring::ease_out_reveal(&ctx, pop_id, 0.12);
        let menu = Rect::from_min_size(Pos2::new(pos.x, pos.y - (1.0 - reveal) * 5.0), Vec2::new(width, height));
        let clip_menu = Rect::from_min_size(menu.min, Vec2::new(width, (height * (0.82 + 0.18 * reveal)).max(24.0)));
        painter.rect_filled(clip_menu, CornerRadius::same(8), widgets::fade(t.popup, reveal));
        painter.rect_stroke(
            clip_menu,
            CornerRadius::same(8),
            Stroke::new(1.0, widgets::fade(t.border, reveal)),
            egui::StrokeKind::Inside,
        );
        let painter = painter.with_clip_rect(clip_menu);

        // Keyboard: Esc closes, Up/Down walk the enabled items, Enter runs the
        // highlighted one, mirroring the mouse path.
        let (escape, up, down, enter) = ctx.input(|input| {
            (
                input.key_pressed(egui::Key::Escape),
                input.key_pressed(egui::Key::ArrowUp),
                input.key_pressed(egui::Key::ArrowDown),
                input.key_pressed(egui::Key::Enter),
            )
        });
        if escape {
            self.menu_anchor = None;
            ctx.data_mut(|data| {
                data.remove_temp::<Option<usize>>(menu_kbd_id());
            });
            return;
        }
        let selectable = menu_selectable_count(self);
        let mut kbd = ctx.data_mut(|data| data.get_temp::<Option<usize>>(menu_kbd_id()).flatten());
        if let Some(index) = kbd {
            kbd = Some(index.min(selectable.saturating_sub(1)));
        }
        if selectable > 0 {
            if down {
                kbd = Some(match kbd {
                    Some(index) => (index + 1) % selectable,
                    None => 0,
                });
            } else if up {
                kbd = Some(match kbd {
                    Some(index) => (index + selectable - 1) % selectable,
                    None => selectable - 1,
                });
            }
        }

        let mut y = menu.top() + 6.0;
        let mut ordinal = 0usize;
        let mut chosen: Option<Command> = None;
        for entry in MENU {
            let MenuEntry::Item(item) = entry else {
                painter.line_segment(
                    [Pos2::new(menu.left() + 10.0, y + 4.0), Pos2::new(menu.right() - 10.0, y + 4.0)],
                    Stroke::new(1.0, t.divider),
                );
                y += 9.0;
                continue;
            };
            let enabled = menu_enabled(self, item.gate);
            let checked = menu_checked(self, item.check);
            let row = Rect::from_min_size(Pos2::new(menu.left() + 4.0, y), Vec2::new(width - 8.0, row_h));
            let id = egui::Id::unique(("menu-item", item.label));
            let resp = ui.interact(row, id, Sense::click());
            let hover = widgets::hover_t(&ctx, id, resp.hovered() && enabled);
            // Enter after hovering a row should activate that row, so mouse hover
            // seeds the keyboard selection while none exists yet.
            if enabled && kbd.is_none() && resp.hovered() {
                kbd = Some(ordinal);
            }
            let keyboard = enabled && kbd == Some(ordinal);
            if keyboard {
                painter.rect_filled(row, CornerRadius::same(4), t.selected);
            } else if hover > 0.0 {
                painter.rect_filled(row, CornerRadius::same(4), widgets::fade(t.hover_strong, hover));
            }
            if checked {
                let color = if enabled { t.accent } else { widgets::fade(t.accent, 0.45) };
                widgets::icon_at(&painter, Pos2::new(row.left() + 14.0, row.center().y), icons::CHECK, 10.0, color);
            }
            let text_color = if !enabled {
                t.text_disabled
            } else if checked {
                t.text
            } else {
                t.text_secondary
            };
            widgets::text_left(
                ui,
                &painter,
                Rect::from_min_max(
                    Pos2::new(row.left() + 28.0, row.top()),
                    Pos2::new(row.right() - 60.0, row.bottom()),
                ),
                item.label,
                theme::font(12.5),
                text_color,
                0.0,
            );
            if let Some(shortcut) = item.shortcut {
                widgets::text_right(
                    ui,
                    &painter,
                    Rect::from_min_max(
                        Pos2::new(row.right() - 90.0, row.top()),
                        Pos2::new(row.right() - 10.0, row.bottom()),
                    ),
                    shortcut,
                    theme::font(11.0),
                    if enabled { t.text_tertiary } else { t.text_disabled },
                );
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
            y += row_h;
        }
        if enter && let Some(kbd) = kbd {
            let mut index = 0usize;
            for entry in MENU {
                let MenuEntry::Item(item) = entry else { continue };
                if !menu_enabled(self, item.gate) {
                    continue;
                }
                if index == kbd {
                    chosen = Some(item.command);
                    break;
                }
                index += 1;
            }
        }
        ctx.data_mut(|data| {
            data.insert_temp(menu_kbd_id(), kbd);
        });
        if let Some(command) = chosen {
            self.menu_anchor = None;
            ctx.data_mut(|data| {
                data.remove_temp::<Option<usize>>(menu_kbd_id());
            });
            self.command(command);
        }
    }

    fn paint_tab_menu(&mut self, ui: &mut egui::Ui, full: Rect) {
        let Some(menu) = ui.ctx().data(|data| data.get_temp::<TabMenu>(tab_menu_id())) else { return };
        if menu.tab >= self.tabs.len() {
            close_tab_menu(ui.ctx());
            return;
        }
        let t = self.theme.clone();
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();

        // Same focus trick as the main menu: the blocker owns the keyboard while
        // the popup is open, so shortcuts do not act on the view behind it.
        let blocker = ui.interact(Rect::EVERYTHING, egui::Id::unique("tab-menu-blocker"), Sense::click());
        if !blocker.has_focus() {
            blocker.request_focus();
        }
        if blocker.clicked_by(egui::PointerButton::Primary) || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            close_tab_menu(&ctx);
            return;
        }

        let labels = ["关闭", "关闭其他", "复制标签路径", "在新标签页中打开"];
        let can_close_others = self.tabs.len() > 1;
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
        painter.rect_filled(rect, CornerRadius::same(8), t.popup);
        painter.rect_stroke(rect, CornerRadius::same(8), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);

        let mut chosen: Option<usize> = None;
        for (index, label) in labels.iter().enumerate() {
            let enabled = index != 1 || can_close_others;
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
        let Some(index) = chosen else { return };
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
            painter.rect_filled(rect, CornerRadius::same(6), widgets::fade(fill, alpha));
            if error {
                widgets::stroke_rect(&painter, rect, 6, Stroke::new(1.0, widgets::fade(t.danger, alpha)));
            }
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
        painter.rect_filled(card, CornerRadius::same(10), t.popup);
        painter.rect_stroke(card, CornerRadius::same(10), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);

        let left = card.left() + 24.0;
        let width = card.width() - 48.0;
        let mut y = card.top() + 22.0;

        // Tab must walk the two dialog buttons instead of leaking into the widgets
        // behind the modal shade, so the focus target is chosen explicitly.
        let mut request: Option<bool> = None;
        if ctx.input(|input| input.key_pressed(egui::Key::Tab)) {
            let cancel_focused = ctx.memory(|memory| memory.has_focus(dialog_cancel_id()));
            let confirm_focused = ctx.memory(|memory| memory.has_focus(dialog_confirm_id()));
            request = Some(cancel_focused && !confirm_focused);
        }

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
                let fields: [(&str, &mut String, bool); 4] = [
                    ("查找文本", find, true),
                    ("替换为", replace, false),
                    ("添加前缀", prefix, false),
                    ("添加后缀", suffix, false),
                ];
                for (hint, buf, wants_focus) in fields {
                    let field = Rect::from_min_size(Pos2::new(left, y), Vec2::new(width, 30.0));
                    painter.rect_filled(field, CornerRadius::same(5), t.surface_alt);
                    painter.rect_stroke(
                        field,
                        CornerRadius::same(5),
                        Stroke::new(1.0, t.border),
                        egui::StrokeKind::Inside,
                    );
                    let te = egui::TextEdit::singleline(buf)
                        .font(theme::font(12.5))
                        .hint_text(hint)
                        .frame(egui::Frame::NONE)
                        .desired_width(field.width() - 20.0);
                    let resp = ui.put(field.shrink2(Vec2::new(10.0, 5.0)), te);
                    if wants_focus && *focus {
                        resp.request_focus();
                        *focus = false;
                    }
                    if resp.has_focus() {
                        painter.rect_stroke(
                            field,
                            CornerRadius::same(5),
                            Stroke::new(1.0, t.accent),
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
                let clicked = dialog_buttons(ui, &painter, &t, card, card.bottom() - 52.0, "重命名", false, request);
                (clicked == 1, clicked == 2)
            }
        };

        if armed && shade.clicked() {
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
    if cancel_resp.has_focus() {
        widgets::stroke_rect(painter, cancel_rect.expand(1.5), 6, Stroke::new(1.5, t.accent));
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
    if confirm_resp.has_focus() {
        let ring = if danger { t.text } else { t.text_on_accent };
        widgets::stroke_rect(painter, confirm_rect.expand(1.5), 6, Stroke::new(1.5, ring));
    }
    if confirm_resp.clicked() {
        result = 1;
    }
    result
}

/// Destructive button fill; `Theme::danger` is the danger *text* token and is
/// too light in the dark theme to carry white label text.
const DANGER_FILL: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);
