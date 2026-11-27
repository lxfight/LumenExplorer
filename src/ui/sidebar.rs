//! Navigation pane: quick access, drives and system locations.

use std::path::{Path, PathBuf};

use egui::{CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};
use windows::Win32::UI::Shell::{FOLDERID_NetworkFolder, FOLDERID_Recent, FOLDERID_RecycleBinFolder};

use crate::app::{Command, LumenApp};
use crate::core::format_size;
use crate::shell::drives::Drive;
use crate::shell::location::{Location, user_profile};
use crate::theme::{self, Theme};
use crate::ui::{icons, widgets};

/// Segoe Fluent Icons E774 "Globe"; `icons` has no network glyph.
const GLOBE: &str = "\u{E774}";
/// Segoe Fluent Icons E74D "Delete" (a bin). The recycle bin is a virtual item
/// with no filesystem path, so the shell icon cache cannot serve it.
const BIN: &str = "\u{E74D}";
/// Segoe Fluent Icons E7F1 "Drive", shown until the shell icon is cached.
const DRIVE: &str = "\u{E7F1}";
/// Segoe Fluent Icons E753 "Cloud" for OneDrive.
const CLOUD: &str = "\u{E753}";
/// Segoe Fluent Icons E756 "CommandPrompt" for WSL Linux.
const LINUX: &str = "\u{E756}";

/// `::{GUID}` parsing names of the virtual folders. Display names are localized
/// by the shell, so they cannot identify a location.
const NETWORK_PARSING: &str = "::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}";

/// Detects user's local OneDrive folder(s) via standard Windows environment variables
/// once per process (zero per-frame disk/env overhead).
fn detect_onedrive() -> Option<&'static PathBuf> {
    static CACHE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            for var in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
                if let Some(val) = std::env::var_os(var) {
                    let p = PathBuf::from(val);
                    if p.is_dir() {
                        return Some(p);
                    }
                }
            }
            None
        })
        .as_ref()
}

fn recent_folder_path() -> Option<&'static PathBuf> {
    static CACHE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| crate::shell::location::known_path(&FOLDERID_Recent)).as_ref()
}

/// Checks locally in HKCU registry once per process whether any WSL distributions
/// are registered, avoiding any network/9P stall on machines without WSL.
fn has_wsl_distros() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        use windows::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryInfoKeyW,
        };
        use windows::core::w;
        let mut key = HKEY::default();
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                w!(r"Software\Microsoft\Windows\CurrentVersion\Lxss"),
                Some(0),
                KEY_READ,
                &mut key,
            )
        };
        if status.is_err() {
            return false;
        }
        let mut subkeys = 0u32;
        let ok = unsafe {
            RegQueryInfoKeyW(key, None, None, None, Some(&mut subkeys), None, None, None, None, None, None, None)
                .is_ok()
        };
        unsafe {
            let _ = RegCloseKey(key);
        }
        ok && subkeys > 0
    })
}

impl LumenApp {
    pub(crate) fn paint_sidebar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        self.sidebar_focused = false;
        let t = self.theme.clone();
        let profile = user_profile();
        let current = self.current_dir();
        // Virtual locations (Network, Recycle Bin) have no filesystem path;
        // their parsing name is the only stable identity to compare against.
        let virtual_name = current.is_none().then(|| self.active().location.parsing_name());
        // Enter belongs to the dialog or inline editor while it is open.
        let enter_ok = self.dialog.is_none() && self.rename.is_none();

        // Rows call back into &mut self (icons, drop zones), so move the lists
        // out for the frame instead of cloning them.
        let quick = std::mem::take(&mut self.sidebar.quick);
        let drives = std::mem::take(&mut self.sidebar.drives);
        let mut quick_open = self.sidebar.quick_open;
        let mut pc_open = self.sidebar.pc_open;
        let mut new_tab: Option<PathBuf> = None;
        let mut new_tab_virtual: Option<Location> = None;
        let mut unpin: Option<PathBuf> = None;
        let mut eject_target: Option<PathBuf> = None;
        let mut prop_target: Option<PathBuf> = None;
        let mut empty_bin = false;
        let drag_hover = crate::shell::dnd::drag_hover();
        let hover_target = drag_hover.as_ref().and_then(|h| h.target.as_deref());

        // The floating scroll bar overlays the content, so keep the rows clear of it.
        let inner_width = rect.width() - 18.0;
        let area = egui::ScrollArea::vertical()
            .id_salt("sidebar")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(rect.min + Vec2::new(4.0, 6.0), Pos2::new(rect.right(), rect.bottom()))),
        );
        child.set_clip_rect(rect);
        area.show(&mut child, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let painter = ui.painter().clone();
            let sel_bg_slot = painter.add(egui::Shape::Noop);
            let sel_bar_slot = painter.add(egui::Shape::Noop);
            let mut selected_rect: Option<Rect> = None;

            // Home is the user profile folder, not a shell namespace item.
            let home = current.as_deref().is_some_and(|here| same_path(here, &profile));
            let row = nav_row(ui, &painter, &t, inner_width, "主页", None, icons::HOME, home, false);
            if home {
                selected_rect = Some(row.rect);
            }
            self.sidebar_focused |= row.has_focus();
            if activated(ui, &row, enter_ok) {
                self.navigate_to_path(&profile);
            }
            if row.middle_clicked() {
                new_tab = Some(profile.clone());
            }

            // Recent Files (Windows 11 Home / Recent items).
            if let Some(recent_path) = recent_folder_path() {
                let is_recent = current.as_deref().is_some_and(|here| same_path(here, recent_path));
                let row = nav_row(ui, &painter, &t, inner_width, "最近使用", None, icons::HISTORY, is_recent, false);
                if is_recent {
                    selected_rect = Some(row.rect);
                }
                self.sidebar_focused |= row.has_focus();
                if activated(ui, &row, enter_ok)
                    && let Ok(location) = Location::known(&FOLDERID_Recent)
                {
                    self.navigate_to(location);
                }
                if row.middle_clicked()
                    && let Ok(location) = Location::known(&FOLDERID_Recent)
                {
                    new_tab_virtual = Some(location);
                }
            }

            // OneDrive (auto-detected if installed on the user's PC).
            if let Some(onedrive) = detect_onedrive() {
                let is_od = current.as_deref().is_some_and(|here| same_path(here, onedrive));
                let icon = self.shell_icon(ui.ctx(), Some(onedrive), "", true, 16, false);
                let row = nav_row(ui, &painter, &t, inner_width, "OneDrive", icon.as_ref(), CLOUD, is_od, false);
                if is_od {
                    selected_rect = Some(row.rect);
                }
                if hover_target.is_some_and(|dst| same_path(dst, onedrive)) {
                    widgets::stroke_rect(&painter, row.rect, theme::RADIUS_ROW, Stroke::new(1.5, t.accent));
                }
                self.sidebar_focused |= row.has_focus();
                self.drop_zones.push((row.rect, onedrive.clone()));
                if activated(ui, &row, enter_ok) {
                    self.navigate_to_path(onedrive);
                }
                if row.middle_clicked() {
                    new_tab = Some(onedrive.clone());
                }
            }

            let header = section_row(ui, &painter, &t, inner_width, "快速访问", quick_open);
            self.pin_drop_rect = Some(header.rect);
            if drag_hover.as_ref().is_some_and(|h| header.rect.contains(h.position)) {
                widgets::stroke_rect(&painter, header.rect, theme::RADIUS_ROW, Stroke::new(1.5, t.accent));
            }
            if activated(ui, &header, enter_ok) {
                quick_open = !quick_open;
                self.sidebar.quick_open = quick_open;
            }
            if quick_open {
                for item in &quick {
                    let icon = self.shell_icon(ui.ctx(), Some(&item.path), "", true, 16, false);
                    let pinned = self
                        .settings
                        .sidebar_pinned
                        .iter()
                        .any(|pinned| item.path.to_string_lossy().eq_ignore_ascii_case(pinned));
                    let selected = current.as_deref().is_some_and(|here| same_path(here, &item.path));
                    let row =
                        nav_row(ui, &painter, &t, inner_width, &item.label, icon.as_ref(), item.icon, selected, pinned);
                    if selected {
                        selected_rect = Some(row.rect);
                    }
                    if hover_target.is_some_and(|dst| same_path(dst, &item.path)) {
                        widgets::stroke_rect(&painter, row.rect, theme::RADIUS_ROW, Stroke::new(1.5, t.accent));
                    }
                    self.sidebar_focused |= row.has_focus();
                    self.drop_zones.push((row.rect, item.path.clone()));
                    if activated(ui, &row, enter_ok) {
                        self.navigate_to_path(&item.path);
                    }
                    if row.middle_clicked() {
                        new_tab = Some(item.path.clone());
                    }
                    let _ = row.context_menu(|ui| {
                        if ui.button("在新标签中打开").clicked() {
                            new_tab = Some(item.path.clone());
                        }
                        if pinned && ui.button("从快速访问取消固定").clicked() {
                            unpin = Some(item.path.clone());
                        }
                    });
                }
            }

            ui.add_space(10.0);

            let header = section_row(ui, &painter, &t, inner_width, "此电脑", pc_open);
            if activated(ui, &header, enter_ok) {
                pc_open = !pc_open;
                self.sidebar.pc_open = pc_open;
            }
            if pc_open {
                for drive in &drives {
                    let root = Path::new(&drive.root);
                    let icon = self.shell_icon(ui.ctx(), Some(root), "", true, 16, false);
                    let selected = current.as_deref().is_some_and(|here| same_path(here, root));
                    let row = drive_row(ui, &painter, &t, inner_width, drive, icon.as_ref(), selected);
                    if selected {
                        selected_rect = Some(row.rect);
                    }
                    if hover_target.is_some_and(|dst| same_path(dst, root)) {
                        widgets::stroke_rect(&painter, row.rect, theme::RADIUS_ROW, Stroke::new(1.5, t.accent));
                    }
                    self.sidebar_focused |= row.has_focus();
                    let clicked = activated(ui, &row, enter_ok);
                    self.drop_zones.push((row.rect, drive.path()));
                    let row = if row.hovered() || row.has_focus() {
                        let hint = format!(
                            "{} · 可用 {} / 共 {}",
                            drive.filesystem,
                            format_size(drive.free),
                            format_size(drive.total)
                        );
                        row.on_hover_text(hint)
                    } else {
                        row
                    };
                    if clicked {
                        self.navigate_to_path(root);
                    }
                    if row.middle_clicked() {
                        new_tab = Some(drive.path());
                    }
                    let is_removable = drive.kind == crate::shell::drives::DriveKind::Removable;
                    let drive_path = drive.path();
                    let _ = row.context_menu(|ui| {
                        if ui.button("在新标签页中打开").clicked() {
                            new_tab = Some(drive_path.clone());
                            ui.close();
                        }
                        if is_removable && ui.button("安全弹出").clicked() {
                            eject_target = Some(drive_path.clone());
                            ui.close();
                        }
                        if ui.button("属性").clicked() {
                            prop_target = Some(drive_path.clone());
                            ui.close();
                        }
                    });
                }
            }

            ui.add_space(8.0);

            if has_wsl_distros() {
                let wsl_root = PathBuf::from(r"\\wsl.localhost");
                let is_wsl = current.as_deref().is_some_and(|here| same_path(here, &wsl_root));
                let row = nav_row(ui, &painter, &t, inner_width, "Linux (WSL)", None, LINUX, is_wsl, false);
                if is_wsl {
                    selected_rect = Some(row.rect);
                }
                self.sidebar_focused |= row.has_focus();
                if activated(ui, &row, enter_ok) {
                    self.navigate_to_path(&wsl_root);
                }
                if row.middle_clicked() {
                    new_tab = Some(wsl_root);
                }
            }

            let network = virtual_name.as_deref().is_some_and(|name| name.eq_ignore_ascii_case(NETWORK_PARSING));
            let row = nav_row(ui, &painter, &t, inner_width, "网络", None, GLOBE, network, false);
            if network {
                selected_rect = Some(row.rect);
            }
            self.sidebar_focused |= row.has_focus();
            if activated(ui, &row, enter_ok)
                && let Ok(location) = Location::known(&FOLDERID_NetworkFolder)
            {
                self.navigate_to(location);
            }
            if row.middle_clicked()
                && let Ok(location) = Location::known(&FOLDERID_NetworkFolder)
            {
                new_tab_virtual = Some(location);
            }

            let recycle = self.active().is_recycle_bin();
            let row = nav_row(ui, &painter, &t, inner_width, "回收站", None, BIN, recycle, false);
            if recycle {
                selected_rect = Some(row.rect);
            }
            self.sidebar_focused |= row.has_focus();
            if activated(ui, &row, enter_ok)
                && let Ok(location) = Location::known(&FOLDERID_RecycleBinFolder)
            {
                self.navigate_to(location);
            }
            if row.middle_clicked()
                && let Ok(location) = Location::known(&FOLDERID_RecycleBinFolder)
            {
                new_tab_virtual = Some(location);
            }
            let _ = row.context_menu(|ui| {
                if ui.button("在新标签页中打开").clicked()
                    && let Ok(location) = Location::known(&FOLDERID_RecycleBinFolder)
                {
                    new_tab_virtual = Some(location);
                    ui.close();
                }
                if ui.button("清空回收站").clicked() {
                    empty_bin = true;
                    ui.close();
                }
            });

            if let Some(target) = selected_rect {
                let (sy, vy) = crate::ui::spring::animate_vel(
                    ui.ctx(),
                    egui::Id::unique("sidebar-sel-y"),
                    target.top(),
                    target.top(),
                    crate::ui::spring::MAGNETIC,
                    0.15,
                );
                let sh = crate::ui::spring::animate(
                    ui.ctx(),
                    egui::Id::unique("sidebar-sel-h"),
                    target.height(),
                    target.height(),
                    crate::ui::spring::PANEL,
                );
                let bg_rect =
                    Rect::from_min_size(Pos2::new(target.left(), sy), Vec2::new(target.width(), sh.max(16.0)));
                let bar_stretch = (vy.abs() * 0.008).min(8.0);
                let bar_rect = Rect::from_center_size(
                    Pos2::new(bg_rect.left() + 3.5, bg_rect.center().y),
                    Vec2::new(3.0, 16.0 + bar_stretch),
                );
                painter.set(
                    sel_bg_slot,
                    egui::Shape::rect_filled(bg_rect, CornerRadius::same(theme::RADIUS_ROW), t.selected),
                );
                painter.set(sel_bar_slot, egui::Shape::rect_filled(bar_rect, CornerRadius::same(2), t.selected_bar));
            }

            ui.add_space(12.0);
        });

        self.sidebar.quick = quick;
        self.sidebar.drives = drives;

        if let Some(path) = new_tab {
            self.command(Command::NewTab);
            self.navigate_to_path(&path);
        }
        if let Some(loc) = new_tab_virtual {
            self.command(Command::NewTab);
            self.navigate_to(loc);
        }
        if let Some(path) = unpin {
            let key = path.to_string_lossy();
            self.settings.sidebar_pinned.retain(|pinned| !pinned.eq_ignore_ascii_case(&key));
            self.reload_sidebar();
        }
        if let Some(path) = eject_target {
            match crate::shell::ops::eject_drive(&path) {
                Ok(()) => {
                    self.toast("已发出安全弹出请求");
                    self.reload_sidebar();
                }
                Err(err) => self.toast_error(format!("{err:#}")),
            }
        }
        if let Some(path) = prop_target
            && let Err(err) = crate::shell::ops::properties(&[path])
        {
            self.toast_error(format!("{err:#}"));
        }
        if empty_bin {
            self.command(Command::EmptyRecycleBin);
        }
    }
}

/// Row activation: pointer click, or Enter while the row owns keyboard focus.
fn activated(ui: &egui::Ui, resp: &egui::Response, enter_ok: bool) -> bool {
    resp.clicked() || (enter_ok && resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
}

fn trim_separator(s: &str) -> &str {
    s.trim_end_matches(['\\', '/'])
}

/// Windows paths compare ASCII-case-insensitively. Shell roots arrive with a
/// trailing separator (`C:\`) while a location path may not, so drop it.
fn same_path(a: &Path, b: &Path) -> bool {
    trim_separator(&a.to_string_lossy()).eq_ignore_ascii_case(trim_separator(&b.to_string_lossy()))
}

// Immediate-mode paint helper: each argument is distinct draw state.
#[allow(clippy::too_many_arguments)]
fn row_fill(
    ui: &egui::Ui,
    painter: &egui::Painter,
    t: &Theme,
    rect: Rect,
    id: egui::Id,
    hovered: bool,
    selected: bool,
    focused: bool,
) {
    let hover = widgets::hover_t(ui.ctx(), id, hovered);
    if !selected && hover > 0.0 {
        painter.rect_filled(rect, CornerRadius::same(theme::RADIUS_ROW), widgets::fade(t.hover, hover));
    }
    if focused {
        widgets::stroke_rect(painter, rect.shrink(0.5), theme::RADIUS_ROW, Stroke::new(1.0, t.accent));
    }
}

fn texture_at(painter: &egui::Painter, rect: Rect, texture: &egui::TextureHandle) {
    painter.image(texture.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), egui::Color32::WHITE);
}

fn hand_cursor(ui: &egui::Ui, resp: &egui::Response) {
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

// Immediate-mode paint helper: each argument is distinct draw state.
#[allow(clippy::too_many_arguments)]
fn nav_row(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    t: &Theme,
    width: f32,
    label: &str,
    icon: Option<&egui::TextureHandle>,
    glyph: &str,
    selected: bool,
    pinned: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 28.0), Sense::click());
    row_fill(ui, painter, t, rect, resp.id, resp.hovered(), selected, resp.has_focus());
    let icon_rect = Rect::from_center_size(Pos2::new(rect.left() + 18.0, rect.center().y), Vec2::splat(16.0));
    match icon {
        Some(texture) => texture_at(painter, icon_rect, texture),
        None => widgets::icon_at(painter, icon_rect.center(), glyph, 15.0, t.text_secondary),
    }
    let label_right = if pinned { rect.right() - 24.0 } else { rect.right() - 6.0 };
    widgets::text_left(
        ui,
        painter,
        Rect::from_min_max(Pos2::new(rect.left() + 36.0, rect.top()), Pos2::new(label_right, rect.bottom())),
        label,
        theme::font(13.0),
        t.text,
        0.0,
    );
    if pinned {
        widgets::icon_at(painter, Pos2::new(rect.right() - 14.0, rect.center().y), icons::PIN, 10.0, t.text_tertiary);
    }
    hand_cursor(ui, &resp);
    resp
}

fn section_row(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    t: &Theme,
    width: f32,
    label: &str,
    open: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click());
    row_fill(ui, painter, t, rect.shrink2(Vec2::new(0.0, 2.0)), resp.id, resp.hovered(), false, resp.has_focus());
    let g = widgets::galley(ui, label, theme::semibold(11.0), t.text_tertiary, width - 30.0);
    widgets::galley_at(painter, Pos2::new(rect.left() + 8.0, rect.center().y - g.size().y * 0.5), g, t.text_tertiary);
    widgets::icon_at(
        painter,
        Pos2::new(rect.right() - 14.0, rect.center().y),
        if open { icons::CHEVRON_DOWN } else { icons::CHEVRON_RIGHT },
        8.0,
        t.text_tertiary,
    );
    hand_cursor(ui, &resp);
    resp
}

fn drive_row(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    t: &Theme,
    width: f32,
    drive: &Drive,
    icon: Option<&egui::TextureHandle>,
    selected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 44.0), Sense::click());
    row_fill(ui, painter, t, rect, resp.id, resp.hovered(), selected, resp.has_focus());
    let icon_rect = Rect::from_center_size(Pos2::new(rect.left() + 18.0, rect.top() + 15.0), Vec2::splat(16.0));
    match icon {
        Some(texture) => texture_at(painter, icon_rect, texture),
        None => widgets::icon_at(painter, icon_rect.center(), DRIVE, 15.0, t.text_secondary),
    }
    let name = drive.display_name();
    widgets::text_left(
        ui,
        painter,
        Rect::from_min_max(
            Pos2::new(rect.left() + 36.0, rect.top() + 2.0),
            Pos2::new(rect.right() - 6.0, rect.top() + 26.0),
        ),
        &name,
        theme::font(13.0),
        t.text,
        0.0,
    );

    // Capacity: usage bar on the left, "可用 / 共" on the right. The text is
    // measured first so neither side can overlap the other at any nav width.
    let free_text = format!("可用 {} / 共 {}", format_size(drive.free), format_size(drive.total));
    let line_left = rect.left() + 36.0;
    let line_right = rect.right() - 10.0;
    let cap =
        widgets::galley(ui, &free_text, theme::font(10.5), t.text_tertiary, (line_right - line_left - 28.0).max(0.0));
    let cap_w = cap.size().x;
    let cy = rect.top() + 32.0;
    widgets::galley_at(painter, Pos2::new(line_right - cap_w, cy - cap.size().y * 0.5), cap, t.text_tertiary);
    let bar_w = (line_right - cap_w - 8.0 - line_left).max(20.0);
    let bar = Rect::from_center_size(Pos2::new(line_left + bar_w * 0.5, cy), Vec2::new(bar_w, 3.0));
    painter.rect_filled(bar, CornerRadius::same(2), widgets::fade(t.border_strong, 0.5));
    let used = drive.used_fraction().clamp(0.0, 1.0);
    if used > 0.0 {
        let fill = Rect::from_min_size(bar.min, Vec2::new(bar.width() * used, bar.height()));
        painter.rect_filled(fill, CornerRadius::same(2), t.accent);
    }
    hand_cursor(ui, &resp);
    resp
}
