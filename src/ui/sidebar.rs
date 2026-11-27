//! Navigation pane: quick access, drives and system locations.

use std::path::{Path, PathBuf};

use egui::{CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};
use windows::Win32::UI::Shell::{
    FOLDERID_ComputerFolder, FOLDERID_NetworkFolder, FOLDERID_Recent, FOLDERID_RecycleBinFolder,
};

use crate::app::{Command, LumenApp};
use crate::core::format_size;
use crate::shell::drives::Drive;
use crate::shell::location::{Location, user_profile};
use crate::theme::{self, Theme};
use crate::ui::context_menu::{ContextMenuState, SidebarVirtualKind};
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

/// Clips a sidebar row or header drop target against the visible scroll viewport
/// so scrolled-off rows never leak hitboxes into the command bar, titlebar, or status bar.
pub(crate) fn clip_drop_rect(row_rect: Rect, viewport_clip: Rect) -> Option<Rect> {
    let clipped = row_rect.intersect(viewport_clip);
    (clipped.width() > 1.0 && clipped.height() > 1.0).then_some(clipped)
}

/// Sidebar drive capacity bar fill color: switches to `t.danger` above 90% full
/// (matching the `This PC` list view).
pub(crate) fn drive_bar_color(used_fraction: f32, t: &Theme) -> egui::Color32 {
    if used_fraction > 0.9 { t.danger } else { t.accent }
}

/// Tracks whether a sidebar interactive widget currently owns keyboard focus,
/// surrendering focus cleanly if the user clicked outside the sidebar or an overlay opened.
fn track_sidebar_focus(ui: &egui::Ui, resp: &egui::Response, clicked_outside: bool, overlay_open: bool) -> bool {
    if !resp.has_focus() {
        return false;
    }
    if clicked_outside || overlay_open {
        ui.ctx().memory_mut(|m| m.surrender_focus(resp.id));
        return false;
    }
    true
}

/// Advances the sidebar selection indicator Y spring in scroll-content-relative coordinates
/// so scrolling the navigation pane never causes the accent pill to lag or ghost behind the row.
fn animate_sidebar_sel_y(ctx: &egui::Context, target_top: f32, content_top: f32) -> (f32, f32) {
    let rel_y = target_top - content_top;
    let (sy_rel, vy) = crate::ui::spring::animate_vel(
        ctx,
        egui::Id::unique("sidebar-sel-y"),
        rel_y,
        rel_y,
        crate::ui::spring::MAGNETIC,
        0.15,
    );
    (content_top + sy_rel, vy)
}

impl LumenApp {
    fn open_sidebar_context(&mut self, state: ContextMenuState) {
        self.menu_anchor = None;
        self.crumb_menu = None;
        self.ctx_menu = Some(state);
    }

    pub(crate) fn paint_sidebar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        self.sidebar_focused = false;
        let t = self.theme.clone();
        let profile = user_profile();
        let current = self.current_dir();
        let is_this_pc = self.active().is_this_pc();
        // Virtual locations (Network, Recycle Bin) have no filesystem path;
        // their parsing name is the only stable identity to compare against.
        let virtual_name = current.is_none().then(|| self.active().location.parsing_name());
        let overlay_open = self.has_blocking_overlay();
        // If the user presses the pointer outside the sidebar (e.g. clicking the file list),
        // release any lingering egui keyboard focus on sidebar rows immediately so file-list
        // keyboard shortcuts resume without requiring Esc.
        let clicked_outside =
            ui.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p)));
        // Enter belongs to the dialog, inline editor, or popup overlay while it is open.
        let enter_ok = self.dialog.is_none() && self.rename.is_none() && !overlay_open;
        let shift_held = ui.input(|i| i.modifiers.shift);

        // Rows call back into &mut self (icons, drop zones), so move the lists
        // out for the frame instead of cloning them.
        let quick = std::mem::take(&mut self.sidebar.quick);
        let drives = std::mem::take(&mut self.sidebar.drives);
        let mut quick_open = self.sidebar.quick_open;
        let mut pc_open = self.sidebar.pc_open;
        let mut new_tab: Option<PathBuf> = None;
        let mut new_tab_virtual: Option<Location> = None;
        let mut pending_ctx: Option<ContextMenuState> = None;
        let mut classic_ctx_path: Option<PathBuf> = None;
        let drag_hover = crate::shell::dnd::drag_hover();
        let hover_target = drag_hover.as_ref().and_then(|h| h.target.as_deref());

        // The floating scroll bar overlays the content, so keep the rows clear of it.
        let inner_width = rect.width() - 18.0;
        let area = egui::ScrollArea::vertical()
            .id_salt("sidebar")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded);
        let scroll_bounds = Rect::from_min_max(rect.min + Vec2::new(4.0, 6.0), Pos2::new(rect.right(), rect.bottom()));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(scroll_bounds));
        child.set_clip_rect(rect);
        area.show(&mut child, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let painter = ui.painter().clone();
            let viewport_clip = ui.clip_rect().intersect(rect);
            let content_top = ui.cursor().top();
            let sel_bg_slot = painter.add(egui::Shape::Noop);
            let sel_bar_slot = painter.add(egui::Shape::Noop);
            let mut selected_rect: Option<Rect> = None;
            let suppress_focus_ring = clicked_outside || overlay_open;

            // Home is the user profile folder, not a shell namespace item.
            let home = current.as_deref().is_some_and(|here| same_path(here, &profile));
            let row =
                nav_row(ui, &painter, &t, inner_width, "主页", None, icons::HOME, home, false, suppress_focus_ring);
            if home {
                selected_rect = Some(row.rect);
            }
            self.sidebar_focused |= track_sidebar_focus(ui, &row, clicked_outside, overlay_open);
            if activated(ui, &row, enter_ok) {
                self.navigate_to_path(&profile);
            }
            if row.middle_clicked() {
                new_tab = Some(profile.clone());
            }
            if row.secondary_clicked() {
                if shift_held {
                    classic_ctx_path = Some(profile.clone());
                } else {
                    let anchor = ui.ctx().pointer_interact_pos().unwrap_or(row.rect.center());
                    let pinned_idx = self
                        .settings
                        .sidebar_pinned
                        .iter()
                        .position(|p| profile.to_string_lossy().eq_ignore_ascii_case(p));
                    pending_ctx = Some(ContextMenuState::new_sidebar_quick(
                        anchor,
                        profile.clone(),
                        "主页".into(),
                        pinned_idx,
                        true,
                    ));
                }
            }

            // Recent Files (Windows 11 Home / Recent items).
            if let Some(recent_path) = recent_folder_path() {
                let is_recent = current.as_deref().is_some_and(|here| same_path(here, recent_path));
                let row = nav_row(
                    ui,
                    &painter,
                    &t,
                    inner_width,
                    "最近使用",
                    None,
                    icons::HISTORY,
                    is_recent,
                    false,
                    suppress_focus_ring,
                );
                if is_recent {
                    selected_rect = Some(row.rect);
                }
                self.sidebar_focused |= track_sidebar_focus(ui, &row, clicked_outside, overlay_open);
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
                if row.secondary_clicked() {
                    let anchor = ui.ctx().pointer_interact_pos().unwrap_or(row.rect.center());
                    pending_ctx = Some(ContextMenuState::new_sidebar_virtual(
                        anchor,
                        SidebarVirtualKind::Recent,
                        "最近使用".into(),
                    ));
                }
            }

            // OneDrive (auto-detected if installed on the user's PC).
            if let Some(onedrive) = detect_onedrive() {
                let is_od = current.as_deref().is_some_and(|here| same_path(here, onedrive));
                let icon = self.shell_icon(ui.ctx(), Some(onedrive), "", true, 16, false);
                let row = nav_row(
                    ui,
                    &painter,
                    &t,
                    inner_width,
                    "OneDrive",
                    icon.as_ref(),
                    CLOUD,
                    is_od,
                    false,
                    suppress_focus_ring,
                );
                if is_od {
                    selected_rect = Some(row.rect);
                }
                if let Some(clipped) = clip_drop_rect(row.rect, viewport_clip) {
                    if hover_target.is_some_and(|dst| same_path(dst, onedrive)) {
                        widgets::stroke_rect(&painter, row.rect, theme::RADIUS_ROW, Stroke::new(1.5, t.accent));
                    }
                    self.drop_zones.push((clipped, onedrive.clone()));
                }
                self.sidebar_focused |= track_sidebar_focus(ui, &row, clicked_outside, overlay_open);
                if activated(ui, &row, enter_ok) {
                    self.navigate_to_path(onedrive);
                }
                if row.middle_clicked() {
                    new_tab = Some(onedrive.clone());
                }
                if row.secondary_clicked() {
                    if shift_held {
                        classic_ctx_path = Some(onedrive.clone());
                    } else {
                        let anchor = ui.ctx().pointer_interact_pos().unwrap_or(row.rect.center());
                        let pinned_idx = self
                            .settings
                            .sidebar_pinned
                            .iter()
                            .position(|p| onedrive.to_string_lossy().eq_ignore_ascii_case(p));
                        pending_ctx = Some(ContextMenuState::new_sidebar_quick(
                            anchor,
                            onedrive.clone(),
                            "OneDrive".into(),
                            pinned_idx,
                            true,
                        ));
                    }
                }
            }

            let header = section_row(ui, &painter, &t, inner_width, "快速访问", quick_open, suppress_focus_ring);
            self.sidebar_focused |= track_sidebar_focus(ui, &header, clicked_outside, overlay_open);
            self.pin_drop_rect = clip_drop_rect(header.rect, viewport_clip);
            if self.pin_drop_rect.is_some_and(|r| drag_hover.as_ref().is_some_and(|h| r.contains(h.position))) {
                widgets::stroke_rect(&painter, header.rect, theme::RADIUS_ROW, Stroke::new(1.5, t.accent));
            }
            if activated(ui, &header, enter_ok) {
                quick_open = !quick_open;
                self.sidebar.quick_open = quick_open;
            }
            if quick_open {
                for item in &quick {
                    let icon = self.shell_icon(ui.ctx(), Some(&item.path), "", true, 16, false);
                    let pinned_idx = self
                        .settings
                        .sidebar_pinned
                        .iter()
                        .position(|pinned| item.path.to_string_lossy().eq_ignore_ascii_case(pinned));
                    let pinned = pinned_idx.is_some();
                    let selected = current.as_deref().is_some_and(|here| same_path(here, &item.path));
                    let row = nav_row(
                        ui,
                        &painter,
                        &t,
                        inner_width,
                        &item.label,
                        icon.as_ref(),
                        item.icon,
                        selected,
                        pinned,
                        suppress_focus_ring,
                    );
                    if selected {
                        selected_rect = Some(row.rect);
                    }
                    if let Some(clipped) = clip_drop_rect(row.rect, viewport_clip) {
                        if hover_target.is_some_and(|dst| same_path(dst, &item.path)) {
                            widgets::stroke_rect(&painter, row.rect, theme::RADIUS_ROW, Stroke::new(1.5, t.accent));
                        }
                        self.drop_zones.push((clipped, item.path.clone()));
                    }
                    self.sidebar_focused |= track_sidebar_focus(ui, &row, clicked_outside, overlay_open);
                    if activated(ui, &row, enter_ok) {
                        self.navigate_to_path(&item.path);
                    }
                    if row.middle_clicked() {
                        new_tab = Some(item.path.clone());
                    }
                    if row.secondary_clicked() {
                        if shift_held {
                            classic_ctx_path = Some(item.path.clone());
                        } else {
                            let anchor = ui.ctx().pointer_interact_pos().unwrap_or(row.rect.center());
                            pending_ctx = Some(ContextMenuState::new_sidebar_quick(
                                anchor,
                                item.path.clone(),
                                item.label.clone(),
                                pinned_idx,
                                false,
                            ));
                        }
                    }
                }
            }

            ui.add_space(10.0);

            let (pc_body, pc_chevron, pc_row_rect) =
                this_pc_section_row(ui, &painter, &t, inner_width, pc_open, is_this_pc, suppress_focus_ring);
            if is_this_pc {
                selected_rect = Some(pc_row_rect);
            }
            self.sidebar_focused |= track_sidebar_focus(ui, &pc_body, clicked_outside, overlay_open);
            self.sidebar_focused |= track_sidebar_focus(ui, &pc_chevron, clicked_outside, overlay_open);
            if activated(ui, &pc_body, enter_ok)
                && let Ok(location) = Location::known(&FOLDERID_ComputerFolder)
            {
                self.navigate_to(location);
            }
            if pc_body.middle_clicked()
                && let Ok(location) = Location::known(&FOLDERID_ComputerFolder)
            {
                new_tab_virtual = Some(location);
            }
            if pc_body.secondary_clicked() {
                let anchor = ui.ctx().pointer_interact_pos().unwrap_or(pc_row_rect.center());
                pending_ctx =
                    Some(ContextMenuState::new_sidebar_virtual(anchor, SidebarVirtualKind::ThisPc, "此电脑".into()));
            }
            if activated(ui, &pc_chevron, enter_ok) {
                pc_open = !pc_open;
                self.sidebar.pc_open = pc_open;
            }
            if pc_open {
                for drive in &drives {
                    let root = Path::new(&drive.root);
                    let icon = self.shell_icon(ui.ctx(), Some(root), "", true, 16, false);
                    let selected = current.as_deref().is_some_and(|here| same_path(here, root));
                    let row =
                        drive_row(ui, &painter, &t, inner_width, drive, icon.as_ref(), selected, suppress_focus_ring);
                    if selected {
                        selected_rect = Some(row.rect);
                    }
                    if let Some(clipped) = clip_drop_rect(row.rect, viewport_clip) {
                        if hover_target.is_some_and(|dst| same_path(dst, root)) {
                            widgets::stroke_rect(&painter, row.rect, theme::RADIUS_ROW, Stroke::new(1.5, t.accent));
                        }
                        self.drop_zones.push((clipped, drive.path()));
                    }
                    self.sidebar_focused |= track_sidebar_focus(ui, &row, clicked_outside, overlay_open);
                    let clicked = activated(ui, &row, enter_ok);
                    let secondary_clicked = row.secondary_clicked();
                    let middle_clicked = row.middle_clicked();
                    let row_rect = row.rect;
                    if row.hovered() || row.has_focus() {
                        let hint = format!(
                            "{} · 可用 {} / 共 {}",
                            drive.filesystem,
                            format_size(drive.free),
                            format_size(drive.total)
                        );
                        let _ = row.on_hover_text(hint);
                    }
                    if clicked {
                        self.navigate_to_path(root);
                    }
                    if middle_clicked {
                        new_tab = Some(drive.path());
                    }
                    if secondary_clicked {
                        let drive_path = drive.path();
                        if shift_held {
                            classic_ctx_path = Some(drive_path);
                        } else {
                            let is_removable = drive.kind == crate::shell::drives::DriveKind::Removable;
                            let anchor = ui.ctx().pointer_interact_pos().unwrap_or(row_rect.center());
                            pending_ctx = Some(ContextMenuState::new_sidebar_drive(
                                anchor,
                                drive_path,
                                drive.display_name(),
                                is_removable,
                            ));
                        }
                    }
                }
            }

            ui.add_space(8.0);

            if has_wsl_distros() {
                let wsl_root = PathBuf::from(r"\\wsl.localhost");
                let is_wsl = current.as_deref().is_some_and(|here| same_path(here, &wsl_root));
                let row = nav_row(
                    ui,
                    &painter,
                    &t,
                    inner_width,
                    "Linux (WSL)",
                    None,
                    LINUX,
                    is_wsl,
                    false,
                    suppress_focus_ring,
                );
                if is_wsl {
                    selected_rect = Some(row.rect);
                }
                self.sidebar_focused |= track_sidebar_focus(ui, &row, clicked_outside, overlay_open);
                if activated(ui, &row, enter_ok) {
                    self.navigate_to_path(&wsl_root);
                }
                if row.middle_clicked() {
                    new_tab = Some(wsl_root.clone());
                }
                if row.secondary_clicked() {
                    if shift_held {
                        classic_ctx_path = Some(wsl_root);
                    } else {
                        let anchor = ui.ctx().pointer_interact_pos().unwrap_or(row.rect.center());
                        let pinned_idx = self
                            .settings
                            .sidebar_pinned
                            .iter()
                            .position(|p| wsl_root.to_string_lossy().eq_ignore_ascii_case(p));
                        pending_ctx = Some(ContextMenuState::new_sidebar_quick(
                            anchor,
                            wsl_root,
                            "Linux (WSL)".into(),
                            pinned_idx,
                            true,
                        ));
                    }
                }
            }

            let network = virtual_name.as_deref().is_some_and(|name| name.eq_ignore_ascii_case(NETWORK_PARSING));
            let row = nav_row(ui, &painter, &t, inner_width, "网络", None, GLOBE, network, false, suppress_focus_ring);
            if network {
                selected_rect = Some(row.rect);
            }
            self.sidebar_focused |= track_sidebar_focus(ui, &row, clicked_outside, overlay_open);
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
            if row.secondary_clicked() {
                let anchor = ui.ctx().pointer_interact_pos().unwrap_or(row.rect.center());
                pending_ctx =
                    Some(ContextMenuState::new_sidebar_virtual(anchor, SidebarVirtualKind::Network, "网络".into()));
            }

            let recycle = self.active().is_recycle_bin();
            let row = nav_row(ui, &painter, &t, inner_width, "回收站", None, BIN, recycle, false, suppress_focus_ring);
            if recycle {
                selected_rect = Some(row.rect);
            }
            self.sidebar_focused |= track_sidebar_focus(ui, &row, clicked_outside, overlay_open);
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
            if row.secondary_clicked() {
                let anchor = ui.ctx().pointer_interact_pos().unwrap_or(row.rect.center());
                pending_ctx = Some(ContextMenuState::new_sidebar_virtual(
                    anchor,
                    SidebarVirtualKind::RecycleBin,
                    "回收站".into(),
                ));
            }

            if let Some(target) = selected_rect {
                let (sy, vy) = animate_sidebar_sel_y(ui.ctx(), target.top(), content_top);
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
            } else {
                crate::ui::spring::clear(ui.ctx(), egui::Id::unique("sidebar-sel-y"));
                crate::ui::spring::clear(ui.ctx(), egui::Id::unique("sidebar-sel-h"));
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
        if let Some(path) = classic_ctx_path {
            crate::shell::menu::show_classic_for_items(&[path], true);
        } else if let Some(ctx_state) = pending_ctx {
            self.open_sidebar_context(ctx_state);
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
    suppress_focus_ring: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 28.0), Sense::click());
    let focused = resp.has_focus() && !suppress_focus_ring;
    row_fill(ui, painter, t, rect, resp.id, resp.hovered(), selected, focused);
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
    suppress_focus_ring: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click());
    let focused = resp.has_focus() && !suppress_focus_ring;
    row_fill(ui, painter, t, rect.shrink2(Vec2::new(0.0, 2.0)), resp.id, resp.hovered(), false, focused);
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

/// Split-interactive "此电脑" section header: clicking the label/body navigates directly
/// to `This PC`, while the dedicated right chevron toggles the drive list open/closed.
fn this_pc_section_row(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    t: &Theme,
    width: f32,
    open: bool,
    selected: bool,
    suppress_focus_ring: bool,
) -> (egui::Response, egui::Response, Rect) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::hover());
    let visual_rect = rect.shrink2(Vec2::new(0.0, 1.0));
    let chevron_w = 26.0;
    let body_rect =
        Rect::from_min_max(visual_rect.min, Pos2::new(visual_rect.right() - chevron_w, visual_rect.bottom()));
    let chevron_rect =
        Rect::from_min_max(Pos2::new(visual_rect.right() - chevron_w, visual_rect.top()), visual_rect.max);

    let body_resp = ui.interact(body_rect, egui::Id::unique("sidebar-this-pc-body"), Sense::click());
    let chevron_resp = ui.interact(chevron_rect, egui::Id::unique("sidebar-this-pc-chevron"), Sense::click());

    let body_focused = body_resp.has_focus() && !suppress_focus_ring;
    let chevron_focused = chevron_resp.has_focus() && !suppress_focus_ring;

    row_fill(ui, painter, t, visual_rect, body_resp.id, body_resp.hovered(), selected, body_focused);

    let chevron_pill = chevron_rect.shrink(3.0);
    let chev_hover = widgets::hover_t(ui.ctx(), chevron_resp.id, chevron_resp.hovered());
    if chev_hover > 0.0 {
        painter.rect_filled(
            chevron_pill,
            CornerRadius::same(theme::RADIUS_ROW),
            widgets::fade(t.hover_strong, chev_hover),
        );
    }
    if chevron_focused {
        widgets::stroke_rect(painter, chevron_pill, theme::RADIUS_ROW, Stroke::new(1.0, t.accent));
    }

    let label_col = if selected { t.text } else { t.text_tertiary };
    let g = widgets::galley(ui, "此电脑", theme::semibold(11.0), label_col, (body_rect.width() - 12.0).max(0.0));
    widgets::galley_at(
        painter,
        Pos2::new(visual_rect.left() + 8.0, visual_rect.center().y - g.size().y * 0.5),
        g,
        label_col,
    );
    widgets::icon_at(
        painter,
        chevron_rect.center(),
        if open { icons::CHEVRON_DOWN } else { icons::CHEVRON_RIGHT },
        8.0,
        if chevron_resp.hovered() || selected { t.text_secondary } else { t.text_tertiary },
    );
    hand_cursor(ui, &body_resp);
    hand_cursor(ui, &chevron_resp);
    let chevron_resp = if chevron_resp.hovered() {
        chevron_resp.on_hover_text(if open { "折叠此电脑" } else { "展开此电脑" })
    } else {
        chevron_resp
    };
    (body_resp, chevron_resp, visual_rect)
}

#[allow(clippy::too_many_arguments)]
fn drive_row(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    t: &Theme,
    width: f32,
    drive: &Drive,
    icon: Option<&egui::TextureHandle>,
    selected: bool,
    suppress_focus_ring: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 44.0), Sense::click());
    let focused = resp.has_focus() && !suppress_focus_ring;
    row_fill(ui, painter, t, rect, resp.id, resp.hovered(), selected, focused);
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
    painter.rect_filled(bar, CornerRadius::same(2), widgets::fade(t.border_strong, 0.7));
    let used = drive.used_fraction().clamp(0.0, 1.0);
    if used > 0.0 {
        let fill = Rect::from_min_size(bar.min, Vec2::new(bar.width() * used, bar.height()));
        painter.rect_filled(fill, CornerRadius::same(2), drive_bar_color(used, t));
    }
    hand_cursor(ui, &resp);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_drop_rect_excludes_scrolled_off_items_and_clips_partial_rows() {
        let viewport = Rect::from_min_max(Pos2::new(8.0, 92.0), Pos2::new(260.0, 500.0));

        // Scrolled completely above the sidebar viewport (into command bar / titlebar) -> excluded.
        let above = Rect::from_min_size(Pos2::new(12.0, 40.0), Vec2::new(230.0, 28.0));
        assert_eq!(clip_drop_rect(above, viewport), None);

        // Scrolled completely below the sidebar viewport (into status bar) -> excluded.
        let below = Rect::from_min_size(Pos2::new(12.0, 505.0), Vec2::new(230.0, 28.0));
        assert_eq!(clip_drop_rect(below, viewport), None);

        // Partially scrolled off top edge -> clipped strictly inside viewport.
        let partial = Rect::from_min_size(Pos2::new(12.0, 80.0), Vec2::new(230.0, 28.0));
        let clipped = clip_drop_rect(partial, viewport).expect("partially visible row must remain clipped");
        assert_eq!(clipped.top(), 92.0);
        assert_eq!(clipped.bottom(), 108.0);
        assert!(viewport.contains_rect(clipped));

        // Fully visible item -> preserved unchanged.
        let visible = Rect::from_min_size(Pos2::new(12.0, 150.0), Vec2::new(230.0, 28.0));
        assert_eq!(clip_drop_rect(visible, viewport), Some(visible));
    }

    #[test]
    fn drive_bar_uses_danger_above_ninety_percent() {
        let t = Theme::new(true, 0x00D4_7800);
        assert_eq!(drive_bar_color(0.50, &t), t.accent);
        assert_eq!(drive_bar_color(0.90, &t), t.accent);
        assert_eq!(drive_bar_color(0.91, &t), t.danger);
        assert_eq!(drive_bar_color(1.00, &t), t.danger);
    }

    #[test]
    fn sidebar_sel_y_spring_is_scroll_invariant_with_zero_lag() {
        let ctx = egui::Context::default();
        crate::ui::spring::begin_frame(true);
        let row_content_y = 140.0;

        // Frame 1: scroll offset = 0 -> content_top = 100.0, target_top = 240.0.
        let (sy1, vy1) = animate_sidebar_sel_y(&ctx, 100.0 + row_content_y, 100.0);
        assert!((sy1 - 240.0).abs() < 1e-3);
        assert_eq!(vy1, 0.0);

        // Frame 2: user scrolls down 65px -> content_top = 35.0, target_top = 175.0.
        // Because the spring runs in content-relative coordinates, it moves in exact lockstep!
        let (sy2, vy2) = animate_sidebar_sel_y(&ctx, 35.0 + row_content_y, 35.0);
        assert!((sy2 - 175.0).abs() < 1e-3, "expected exact scroll lockstep 175.0, got {sy2}");
        assert_eq!(vy2, 0.0, "scrolling must not inject spring velocity");
    }

    #[test]
    fn sidebar_focus_surrenders_when_clicking_outside_or_overlay_opens() {
        let ctx = egui::Context::default();
        let row_id = egui::Id::unique("test-sidebar-row");
        let mut kept_focus = false;
        let mut released_focus = true;
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw, |ui| {
            ui.ctx().memory_mut(|m| m.request_focus(row_id));
            let resp =
                ui.interact(Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(180.0, 28.0)), row_id, Sense::click());
            kept_focus = track_sidebar_focus(ui, &resp, false, false);
            released_focus = track_sidebar_focus(ui, &resp, true, false);
        });
        output.textures_delta.clear();
        assert!(kept_focus, "focused sidebar row should report sidebar_focused=true");
        assert!(!released_focus, "clicking outside sidebar must clear sidebar_focused and surrender egui focus");
        assert!(!ctx.memory(|m| m.has_focus(row_id)));
    }
}
