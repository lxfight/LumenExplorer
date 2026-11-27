//! Fluent 2 categorized Settings Center modal (`Ctrl+,` or toolbar gear icon)
//! with bouncy ("Q弹") spring animations for modals, nav rail, switches, and segments.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::{Command, LumenApp};
use crate::core::settings::{SearchBackend, ThemeMode};
use crate::theme::{self, Theme};
use crate::ui::{icons, spring, widgets};

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum SettingsTab {
    #[default]
    General,
    Integration,
    Search,
    Shortcuts,
    Updates,
}

const NAV_TABS: [(SettingsTab, &str, &str); 5] = [
    (SettingsTab::General, icons::VIEW, "常规与外观"),
    (SettingsTab::Integration, icons::OPEN_EXTERNAL, "桌面协同"),
    (SettingsTab::Search, icons::SEARCH, "搜索与文件"),
    (SettingsTab::Shortcuts, icons::INFO, "快捷键"),
    (SettingsTab::Updates, icons::DOWNLOAD, "关于与更新"),
];

const PROBE_TTL: Duration = Duration::from_secs(5);
const SEGMENT_STACK_BREAKPOINT: f32 = 420.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
struct SystemProbes {
    installed: bool,
    has_context_menu: bool,
    is_auto_start: bool,
    everything_running: bool,
}

#[derive(Debug, Default)]
struct ProbeCache {
    snapshot: Option<SystemProbes>,
    updated_at: Option<Instant>,
    was_open: bool,
    last_tab: Option<SettingsTab>,
}

impl ProbeCache {
    fn on_closed(&mut self) {
        self.was_open = false;
        self.last_tab = None;
        self.snapshot = None;
        self.updated_at = None;
    }

    /// Tracks open edge and tab transitions; returns `was_already_open` before this frame.
    fn on_frame_open(&mut self, tab: SettingsTab) -> bool {
        let was_open = self.was_open;
        if !was_open {
            self.was_open = true;
            self.snapshot = None;
            self.updated_at = None;
        } else if self.last_tab != Some(tab)
            && matches!(tab, SettingsTab::Integration | SettingsTab::Search | SettingsTab::Updates)
        {
            self.snapshot = None;
            self.updated_at = None;
        }
        self.last_tab = Some(tab);
        was_open
    }

    fn invalidate(&mut self) {
        self.snapshot = None;
        self.updated_at = None;
    }

    fn get_or_refresh_with(&mut self, now: Instant, probe: impl FnOnce() -> SystemProbes) -> SystemProbes {
        if let (Some(snap), Some(ts)) = (self.snapshot, self.updated_at)
            && now.saturating_duration_since(ts) < PROBE_TTL
        {
            return snap;
        }
        let fresh = probe();
        self.snapshot = Some(fresh);
        self.updated_at = Some(now);
        fresh
    }
}

fn probe_cache() -> &'static Mutex<ProbeCache> {
    static CACHE: OnceLock<Mutex<ProbeCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(ProbeCache::default()))
}

fn run_system_probes() -> SystemProbes {
    SystemProbes {
        installed: crate::shell::integrate::is_installed(),
        has_context_menu: crate::shell::integrate::has_context_menu(),
        is_auto_start: crate::shell::integrate::is_auto_start(),
        everything_running: crate::shell::search::is_everything_running(),
    }
}

fn cached_system_probes() -> SystemProbes {
    let now = Instant::now();
    if let Ok(mut cache) = probe_cache().lock() {
        cache.get_or_refresh_with(now, run_system_probes)
    } else {
        run_system_probes()
    }
}

fn invalidate_system_probes() {
    if let Ok(mut cache) = probe_cache().lock() {
        cache.invalidate();
    }
}

#[derive(Clone, Default)]
struct SettingsFocusState {
    focus_ids: Vec<egui::Id>,
    keyboard_nav: bool,
}

fn focus_state_id() -> egui::Id {
    egui::Id::unique("settings-focus-state")
}

fn nav_tab_id(idx: usize) -> egui::Id {
    egui::Id::unique(("set-nav", idx))
}

fn close_btn_id() -> egui::Id {
    egui::Id::unique("settings-close")
}

#[derive(Clone, Copy, Debug)]
struct SettingsLayout {
    modal: Rect,
    rail_w: f32,
}

/// Computes responsive modal bounds and navigation rail width for any window size
/// (including the 820px minimum window width).
fn settings_layout(full: Rect) -> SettingsLayout {
    let target_w =
        if full.width() < 960.0 { (full.width() - 32.0).min(760.0) } else { (full.width() * 0.68).clamp(620.0, 780.0) };
    let modal_w = target_w.min((full.width() - 16.0).max(280.0));
    let target_h = if full.height() < 640.0 {
        (full.height() - 32.0).min(560.0)
    } else {
        (full.height() * 0.76).clamp(420.0, 580.0)
    };
    let modal_h = target_h.min((full.height() - 16.0).max(260.0));
    let rail_w = if modal_w < 680.0 { 136.0 } else { 156.0 };
    SettingsLayout { modal: Rect::from_center_size(full.center(), Vec2::new(modal_w, modal_h)), rail_w }
}

fn status_ok_color(t: &Theme) -> Color32 {
    if t.dark { Color32::from_rgb(0x6C, 0xCB, 0x5F) } else { Color32::from_rgb(0x10, 0x7C, 0x10) }
}

struct FocusTracker {
    ids: Vec<egui::Id>,
    show_ring: bool,
    activate_focused: bool,
    moved_by_kb: bool,
}

impl FocusTracker {
    fn register(&mut self, id: egui::Id) {
        self.ids.push(id);
    }

    fn is_activated(&self, ui: &egui::Ui, id: egui::Id) -> bool {
        self.activate_focused && ui.ctx().memory(|m| m.has_focus(id))
    }

    fn paint_ring_if_focused(&self, ui: &egui::Ui, rect: Rect, radius: u8, id: egui::Id, t: &Theme) {
        if self.show_ring && ui.ctx().memory(|m| m.has_focus(id)) {
            if self.moved_by_kb {
                ui.scroll_to_rect(rect.expand(6.0), None);
            }
            widgets::stroke_rect(ui.painter(), rect.expand(1.5), radius, Stroke::new(1.5, t.accent));
        }
    }
}

impl LumenApp {
    pub(crate) fn paint_settings_modal(&mut self, ui: &mut egui::Ui, full: Rect) {
        let ctx = ui.ctx().clone();
        let pop_id = egui::Id::unique("settings-modal-pop");
        if !self.settings_open {
            spring::clear(&ctx, pop_id);
            if let Ok(mut cache) = probe_cache().lock()
                && cache.was_open
            {
                cache.on_closed();
                let prev = ctx.data_mut(|d| d.remove_temp::<SettingsFocusState>(focus_state_id()));
                if let Some(prev) = prev
                    && let Some(focused) = ctx.memory(|m| m.focused())
                    && prev.focus_ids.contains(&focused)
                {
                    ctx.memory_mut(|m| m.surrender_focus(focused));
                }
            }
            return;
        }

        let was_open =
            if let Ok(mut cache) = probe_cache().lock() { cache.on_frame_open(self.settings_tab) } else { true };

        let mut prev_focus = ctx.data(|d| d.get_temp::<SettingsFocusState>(focus_state_id())).unwrap_or_default();
        if !was_open {
            // Opening edge: drop inline editors and pull focus cleanly inside the modal.
            self.address.editing = false;
            self.rename = None;
            self.search_focused = false;
            self.sidebar_focused = false;
            prev_focus.keyboard_nav = false;
            let active_idx = NAV_TABS.iter().position(|(tab, _, _)| *tab == self.settings_tab).unwrap_or(0);
            ctx.memory_mut(|m| m.request_focus(nav_tab_id(active_idx)));
        }

        let close_modal = |app: &mut Self, ctx: &egui::Context, prev: &SettingsFocusState| {
            app.settings_open = false;
            spring::clear(ctx, pop_id);
            if let Ok(mut cache) = probe_cache().lock() {
                cache.on_closed();
            }
            if let Some(focused) = ctx.memory(|m| m.focused())
                && prev.focus_ids.contains(&focused)
            {
                ctx.memory_mut(|m| m.surrender_focus(focused));
            }
            ctx.data_mut(|d| {
                d.remove_temp::<SettingsFocusState>(focus_state_id());
            });
        };

        // Handle modal close shortcuts only after the opening frame so Ctrl+, doesn't immediately close itself.
        if was_open {
            let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            let toggle_key = ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Comma));
            if esc || toggle_key {
                close_modal(self, &ctx, &prev_focus);
                return;
            }
        }

        let t = self.theme.clone();
        let painter = ui.painter().clone();

        let pop = spring::animate(&ctx, pop_id, 1.0, 0.0, spring::MODAL);
        let alpha = pop.clamp(0.0, 1.0);
        let scale = 0.94 + 0.06 * pop;

        let layout = settings_layout(full);
        let modal =
            spring::slide_y(spring::scale_rect_around(layout.modal, layout.modal.center(), scale), (1.0 - pop) * 10.0);

        // Backdrop scrim — strictly ignores clicks landing inside the modal card.
        painter.rect_filled(full, CornerRadius::ZERO, Color32::from_black_alpha((105.0 * alpha) as u8));
        let scrim = ui.interact(full, egui::Id::unique("settings-scrim"), Sense::click_and_drag());
        let clicked_outside =
            was_open && scrim.clicked() && ctx.pointer_interact_pos().is_none_or(|pos| !modal.contains(pos));
        if clicked_outside {
            close_modal(self, &ctx, &prev_focus);
            return;
        }

        // Modal card surface + click blocker so background clicks never leak to scrim.
        painter.rect_filled(modal.translate(Vec2::new(0.0, 6.0)).expand(3.0), CornerRadius::same(12), t.shadow);
        painter.rect_filled(modal, CornerRadius::same(10), t.popup);
        painter.rect_stroke(modal, CornerRadius::same(10), Stroke::new(1.0, t.border_strong), egui::StrokeKind::Inside);
        let _card_blocker = ui.interact(modal, egui::Id::unique("settings-card"), Sense::click_and_drag());

        // Keyboard focus trapping & modal navigation.
        if ctx.input(|i| i.pointer.any_pressed()) {
            prev_focus.keyboard_nav = false;
        }
        let active_idx = NAV_TABS.iter().position(|(tab, _, _)| *tab == self.settings_tab).unwrap_or(0);
        let mut current_focus = ctx.memory(|m| m.focused());
        if !prev_focus.focus_ids.is_empty() && current_focus.is_none_or(|id| !prev_focus.focus_ids.contains(&id)) {
            let fallback = nav_tab_id(active_idx);
            ctx.memory_mut(|m| m.request_focus(fallback));
            current_focus = Some(fallback);
        }

        let mut moved_by_kb = false;
        let shift_tab = ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab));
        let tab_fwd = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
        if (tab_fwd || shift_tab) && !prev_focus.focus_ids.is_empty() {
            prev_focus.keyboard_nav = true;
            moved_by_kb = true;
            let len = prev_focus.focus_ids.len();
            let pos = current_focus.and_then(|id| prev_focus.focus_ids.iter().position(|&x| x == id));
            let next_pos = match (pos, shift_tab) {
                (Some(p), false) => (p + 1) % len,
                (Some(p), true) => (p + len - 1) % len,
                (None, false) => 0,
                (None, true) => len - 1,
            };
            let next_id = prev_focus.focus_ids[next_pos];
            ctx.memory_mut(|m| m.request_focus(next_id));
            current_focus = Some(next_id);
        }

        let nav_focused_idx = current_focus.and_then(|id| (0..NAV_TABS.len()).find(|&idx| nav_tab_id(idx) == id));
        if nav_focused_idx.is_some() {
            let up = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
            let down = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
            if up || down {
                prev_focus.keyboard_nav = true;
                moved_by_kb = true;
                let len = NAV_TABS.len();
                let next_idx = if up { (active_idx + len - 1) % len } else { (active_idx + 1) % len };
                self.settings_tab = NAV_TABS[next_idx].0;
                let next_id = nav_tab_id(next_idx);
                ctx.memory_mut(|m| m.request_focus(next_id));
                current_focus = Some(next_id);
                if let Ok(mut cache) = probe_cache().lock() {
                    cache.on_frame_open(self.settings_tab);
                }
            }
        }

        let activate_focused = was_open
            && prev_focus.keyboard_nav
            && (ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
                || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space)));

        let lock_id = current_focus.unwrap_or_else(|| nav_tab_id(active_idx));
        ctx.memory_mut(|m| {
            m.set_focus_lock_filter(
                lock_id,
                egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
            );
        });

        let mut focus = FocusTracker {
            ids: Vec::with_capacity(24),
            show_ring: prev_focus.keyboard_nav,
            activate_focused,
            moved_by_kb,
        };

        // Header bar.
        let header = Rect::from_min_size(modal.min, Vec2::new(modal.width(), 48.0));
        widgets::icon_at(
            &painter,
            Pos2::new(header.left() + 24.0, header.center().y),
            icons::SETTINGS,
            16.0,
            t.accent_text,
        );
        widgets::text_left(
            ui,
            &painter,
            Rect::from_min_max(
                Pos2::new(header.left() + 42.0, header.top()),
                Pos2::new(header.right() - 52.0, header.bottom()),
            ),
            "Lumen 设置",
            theme::semibold(15.0),
            t.text,
            0.0,
        );
        let close_btn = Rect::from_center_size(Pos2::new(header.right() - 24.0, header.center().y), Vec2::splat(28.0));
        let close_id = close_btn_id();
        let close_resp = ui.interact(close_btn, close_id, Sense::click());
        let close_h = widgets::hover_t(&ctx, close_id, close_resp.hovered());
        if close_h > 0.0 {
            painter.rect_filled(close_btn, CornerRadius::same(5), widgets::fade(t.hover_strong, close_h));
        }
        widgets::icon_at(&painter, close_btn.center(), icons::CANCEL, 10.5, t.text_secondary);
        if close_resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let close_kb = focus.is_activated(ui, close_id);
        focus.paint_ring_if_focused(ui, close_btn, 6, close_id, &t);
        if close_resp.clicked() || close_kb {
            close_modal(self, &ctx, &prev_focus);
            return;
        }
        let _ = close_resp.on_hover_text("关闭设置 (Esc / Ctrl+,)");
        painter.line_segment(
            [Pos2::new(modal.left(), header.bottom()), Pos2::new(modal.right(), header.bottom())],
            Stroke::new(1.0, t.divider),
        );

        // Left navigation rail with elastic sliding active pill.
        let rail_w = layout.rail_w;
        let rail = Rect::from_min_max(
            Pos2::new(modal.left() + 10.0, header.bottom() + 12.0),
            Pos2::new(modal.left() + rail_w, modal.bottom() - 12.0),
        );
        let active_idx = NAV_TABS.iter().position(|(tab, _, _)| *tab == self.settings_tab).unwrap_or(0);
        let rail_bg_slot = painter.add(egui::Shape::Noop);
        let rail_bar_slot = painter.add(egui::Shape::Noop);
        let target_rail_y = rail.top() + active_idx as f32 * 36.0;
        let (ry, rvy) = spring::animate_vel(
            &ctx,
            egui::Id::unique("settings-rail-y"),
            target_rail_y,
            target_rail_y,
            spring::MAGNETIC,
            0.15,
        );
        let pill_rect = Rect::from_min_size(Pos2::new(rail.left(), ry), Vec2::new(rail.width() - 8.0, 32.0));
        let bar_h = 16.0 + (rvy.abs() * 0.01).min(9.0);
        painter.set(rail_bg_slot, egui::Shape::rect_filled(pill_rect, CornerRadius::same(5), t.selected));
        painter.set(
            rail_bar_slot,
            egui::Shape::rect_filled(
                Rect::from_center_size(Pos2::new(pill_rect.left() + 2.5, pill_rect.center().y), Vec2::new(3.0, bar_h)),
                CornerRadius::same(2),
                t.selected_bar,
            ),
        );

        for (idx, (tab, glyph, label)) in NAV_TABS.iter().enumerate() {
            let row = Rect::from_min_size(
                Pos2::new(rail.left(), rail.top() + idx as f32 * 36.0),
                Vec2::new(rail.width() - 8.0, 32.0),
            );
            let id = nav_tab_id(idx);
            focus.register(id);
            let resp = ui.interact(row, id, Sense::click());
            let active = self.settings_tab == *tab;
            let hover = widgets::hover_t(&ctx, id, resp.hovered());
            if !active && hover > 0.0 {
                painter.rect_filled(row, CornerRadius::same(5), widgets::fade(t.hover, hover));
            }
            widgets::icon_at(
                &painter,
                Pos2::new(row.left() + 18.0, row.center().y),
                glyph,
                14.0,
                if active { t.accent_text } else { t.text_secondary },
            );
            widgets::text_left(
                ui,
                &painter,
                Rect::from_min_max(Pos2::new(row.left() + 36.0, row.top()), Pos2::new(row.right() - 6.0, row.bottom())),
                label,
                theme::font(13.0),
                if active { t.text } else { t.text_secondary },
                0.0,
            );
            focus.paint_ring_if_focused(ui, row, 6, id, &t);
            if resp.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() || focus.is_activated(ui, id) {
                self.settings_tab = *tab;
                ctx.memory_mut(|m| m.request_focus(id));
                if let Ok(mut cache) = probe_cache().lock() {
                    cache.on_frame_open(self.settings_tab);
                }
            }
        }
        painter.line_segment(
            [
                Pos2::new(modal.left() + rail_w + 2.0, header.bottom()),
                Pos2::new(modal.left() + rail_w + 2.0, modal.bottom()),
            ],
            Stroke::new(1.0, t.divider),
        );

        // Right scrollable content pane.
        let content = Rect::from_min_max(
            Pos2::new(modal.left() + rail_w + 18.0, header.bottom() + 14.0),
            Pos2::new(modal.right() - 16.0, modal.bottom() - 14.0),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(content));
        child.set_clip_rect(content);
        let tab = self.settings_tab;
        egui::ScrollArea::vertical().id_salt(("settings-body", tab as u8)).auto_shrink([false, false]).show(
            &mut child,
            |ui| {
                let w = (content.width() - 12.0).max(160.0);
                match tab {
                    SettingsTab::General => self.paint_settings_general(ui, &t, w, &mut focus),
                    SettingsTab::Integration => self.paint_settings_integration(ui, &t, w, &mut focus),
                    SettingsTab::Search => self.paint_settings_search(ui, &t, w, &mut focus),
                    SettingsTab::Shortcuts => paint_settings_shortcuts(ui, &t, w),
                    SettingsTab::Updates => self.paint_settings_updates(ui, &t, w, &mut focus),
                }
            },
        );

        // Put the close button at the end of the modal Tab ring.
        focus.register(close_id);
        ctx.data_mut(|d| {
            d.insert_temp(focus_state_id(), SettingsFocusState { focus_ids: focus.ids, keyboard_nav: focus.show_ring });
        });
    }

    fn paint_settings_general(&mut self, ui: &mut egui::Ui, t: &Theme, w: f32, focus: &mut FocusTracker) {
        section_title(ui, t, "外观与动效");
        let modes = [(ThemeMode::System, "跟随系统"), (ThemeMode::Light, "浅色"), (ThemeMode::Dark, "深色")];
        if let Some(new_mode) = segmented_row(ui, t, w, "theme-seg", &modes, self.theme_mode, focus) {
            self.theme_mode = new_mode;
            let ctx = ui.ctx().clone();
            self.refresh_system_theme(&ctx);
        }
        ui.add_space(10.0);

        if toggle_card(
            ui,
            t,
            w,
            "set-spring",
            "分层物理动效",
            None,
            "按组件匹配专属曲线：开关果冻 Q弹、滑块磁吸、菜单丝绸展开、窗格与滚动零过冲平滑过渡",
            self.settings.spring_animations,
            focus,
        ) {
            self.settings.spring_animations = !self.settings.spring_animations;
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-mica",
            "Windows 11 Mica 透明背景",
            Some("重启生效"),
            "让桌面壁纸色调柔和地透出窗口标题栏与侧边栏（重启生效）",
            self.settings.mica_backdrop,
            focus,
        ) {
            self.command(Command::ToggleMica);
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-compact",
            "紧凑列表模式",
            None,
            "缩小详细信息视图行距（26px），同屏显示更多文件",
            self.settings.compact_mode,
            focus,
        ) {
            self.settings.compact_mode = !self.settings.compact_mode;
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-chk",
            "文件列表复选框",
            None,
            "悬停或选中行时在左侧显示复选框，无需按 Ctrl 即可勾选多选",
            self.settings.show_checkboxes,
            focus,
        ) {
            self.settings.show_checkboxes = !self.settings.show_checkboxes;
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-ext",
            "隐藏已知文件扩展名",
            None,
            "在列表中隐藏文件后缀名（F2 重命名时仍自动保留原扩展名）",
            self.settings.hide_extensions,
            focus,
        ) {
            self.settings.hide_extensions = !self.settings.hide_extensions;
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-bs",
            "Backspace 键执行“后退”",
            None,
            "开启后按 Backspace 返回上一访问历史；关闭时返回上一级父目录",
            self.settings.backspace_goes_back,
            focus,
        ) {
            self.settings.backspace_goes_back = !self.settings.backspace_goes_back;
        }
    }

    fn paint_settings_integration(&mut self, ui: &mut egui::Ui, t: &Theme, w: f32, focus: &mut FocusTracker) {
        section_title(ui, t, "桌面集成与协同（主程序零注册表写入，可选系统集成由安装程序管理）");

        let probes = cached_system_probes();
        let mode_line = if probes.installed {
            if probes.has_context_menu {
                "运行模式：安装版 · 右键「使用 Lumen 打开」已启用（不修改系统默认资源管理器）"
            } else {
                "运行模式：安装版 · 可重新运行安装程序按需勾选右键菜单 / PATH / 开机自启"
            }
        } else {
            "运行模式：开发 / 便携模式 · 推荐使用正式安装包体验右键菜单、PATH 命令与安全卸载"
        };
        let dot_color = if probes.installed { status_ok_color(t) } else { t.accent };
        if probe_status_strip(ui, t, w, "set-int-refresh", dot_color, mode_line, focus) {
            invalidate_system_probes();
        }
        ui.add_space(4.0);

        if action_card(
            ui,
            t,
            w,
            "set-import-exp",
            "一键吸收已打开的 Windows 资源管理器窗口",
            "通过只读 Shell COM 将其他软件弹出的系统资源管理器文件夹批量转为 Lumen 标签页（Ctrl+Shift+O）",
            "立即吸收",
            focus,
        ) {
            invalidate_system_probes();
            self.settings_open = false;
            self.command(Command::ImportExplorerTabs);
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-tray",
            "关闭窗口时常驻系统托盘",
            None,
            "隐藏窗口保持常驻以实现毫秒级呼出，升级或卸载时会自动安全退出（右键托盘可退出）",
            self.settings.tray_resident,
            focus,
        ) {
            invalidate_system_probes();
            self.command(Command::ToggleTrayResident);
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-wine",
            "全局呼出快捷键 (Ctrl+Alt+E)",
            None,
            "开启后将自动启用系统托盘常驻；使用标准 RegisterHotKey API（无键盘钩子），如需 Win+E 可在 PowerToys 映射",
            self.settings.intercept_win_e,
            focus,
        ) {
            invalidate_system_probes();
            self.command(Command::ToggleWinE);
        }

        let (auto_badge, auto_sub) = if probes.is_auto_start {
            ("已启用", "通过注册表 Run 键自启（安装程序托管，可在任务管理器「启动应用」中管理）")
        } else {
            ("未启用", "通过注册表 Run 键自启（主程序不写入注册表，可运行安装程序按需勾选）")
        };
        status_indicator_card(
            ui,
            t,
            w,
            "开机静默自启",
            auto_sub,
            auto_badge,
            if probes.is_auto_start { status_ok_color(t) } else { t.text_tertiary },
            probes.is_auto_start,
        );
    }

    fn paint_settings_search(&mut self, ui: &mut egui::Ui, t: &Theme, w: f32, focus: &mut FocusTracker) {
        section_title(ui, t, "搜索引擎后端");
        let probes = cached_system_probes();
        let status_line = if probes.everything_running {
            "Everything IPC 状态：已检测到运行中（毫秒级全局索引可用）"
        } else {
            "Everything IPC 状态：未检测到运行（自动回退至内置多线程并行搜索）"
        };
        let dot_color = if probes.everything_running { status_ok_color(t) } else { t.text_tertiary };
        if probe_status_strip(ui, t, w, "set-search-refresh", dot_color, status_line, focus) {
            invalidate_system_probes();
        }
        ui.add_space(4.0);

        let backends = [
            (SearchBackend::Auto, "自动（Everything 优先）"),
            (SearchBackend::WindowsSearch, "Windows 搜索索引"),
            (SearchBackend::BuiltIn, "仅内置并行"),
        ];
        if let Some(choice) = segmented_row(ui, t, w, "search-seg", &backends, self.settings.search_backend, focus) {
            invalidate_system_probes();
            self.settings.search_backend = choice;
        }
        ui.add_space(12.0);

        section_title(ui, t, "隐藏与系统文件");
        if toggle_card(
            ui,
            t,
            w,
            "set-hidden",
            "显示隐藏的文件和文件夹 (Ctrl+H)",
            None,
            "按 Windows 隐藏属性（FILE_ATTRIBUTE_HIDDEN）控制显隐，不再误藏 .git / .env",
            self.show_hidden,
            focus,
        ) {
            self.command(Command::ToggleHidden);
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-system",
            "显示受保护的操作系统文件",
            None,
            "显示同时具有隐藏和系统属性的文件（如 desktop.ini、pagefile.sys）",
            self.show_system,
            focus,
        ) {
            self.command(Command::ToggleSystemFiles);
        }
    }

    fn paint_settings_updates(&mut self, ui: &mut egui::Ui, t: &Theme, w: f32, focus: &mut FocusTracker) {
        use crate::core::format_size;
        use crate::shell::updater::{CURRENT_VERSION, UpdateState};

        let probes = cached_system_probes();

        section_title(ui, t, "关于 Lumen");
        let mode_label = if probes.installed { "Windows 安装版" } else { "开发 / 便携模式" };
        if action_card(
            ui,
            t,
            w,
            "set-about-releases",
            &format!("Lumen v{CURRENT_VERSION}（{mode_label}）"),
            "Windows 11 现代极速文件工作台 · 基于 Rust + 原生 Shell COM 构建",
            "打开发布页",
            focus,
        ) {
            self.command(Command::OpenReleasesPage);
        }

        if toggle_card(
            ui,
            t,
            w,
            "set-auto-update",
            "启动时自动检查更新",
            None,
            "后台静默查询 GitHub Releases（每天最多一次，离线不打扰，安装前自动校验 SHA-256）",
            self.settings.auto_check_updates,
            focus,
        ) {
            self.settings.auto_check_updates = !self.settings.auto_check_updates;
        }
        ui.add_space(6.0);

        section_title(ui, t, "版本更新");
        match self.update_state.clone() {
            UpdateState::Idle => {
                if action_card(
                    ui,
                    t,
                    w,
                    "set-upd-check-idle",
                    "检查新版本",
                    "从 GitHub Releases 检查最新安装包，自动支持系统代理",
                    "立即检查",
                    focus,
                ) {
                    self.command(Command::CheckUpdates);
                }
            }
            UpdateState::Checking { .. } => {
                status_banner(ui, t, w, t.accent, "正在连接 GitHub 检查新版本…", "使用系统 WinHTTP 代理配置");
            }
            UpdateState::UpToDate { .. } => {
                if action_card(
                    ui,
                    t,
                    w,
                    "set-upd-uptodate",
                    &format!("当前已是最新版本（v{CURRENT_VERSION}）"),
                    "暂未发现更新的正式安装包",
                    "重新检查",
                    focus,
                ) {
                    self.command(Command::CheckUpdates);
                }
            }
            UpdateState::Available(info) => {
                let size_str =
                    if info.asset_size > 0 { format!(" · {}", format_size(info.asset_size)) } else { String::new() };
                if action_card(
                    ui,
                    t,
                    w,
                    "set-upd-dl",
                    &format!("发现新版本 v{}{size_str}", info.version),
                    &format!("安装包：{}（将通过 SHA-256 完整性校验）", info.asset_name),
                    "下载更新",
                    focus,
                ) {
                    self.command(Command::DownloadUpdate);
                }
                let notes = info.notes.trim();
                if !notes.is_empty() {
                    paint_release_notes_box(ui, t, w, notes);
                }
                if action_card(
                    ui,
                    t,
                    w,
                    "set-upd-skip",
                    "暂不更新此版本",
                    &format!("跳过 v{} 的自动提醒，直到更高版本发布", info.version),
                    "跳过此版本",
                    focus,
                ) {
                    self.command(Command::SkipUpdateVersion);
                }
            }
            UpdateState::Downloading { info, downloaded, total } => {
                let pct = if total > 0 { ((downloaded as f64 / total as f64) * 100.0).clamp(0.0, 100.0) } else { 0.0 };
                let sub = if total > 0 {
                    format!("{} / {} ({pct:.0}%)", format_size(downloaded), format_size(total))
                } else {
                    format!("已下载 {}", format_size(downloaded))
                };
                progress_banner(ui, t, w, &format!("正在下载 Lumen v{}…", info.version), &sub, (pct / 100.0) as f32);
            }
            UpdateState::Verifying { info } => {
                progress_banner(
                    ui,
                    t,
                    w,
                    &format!("正在校验 Lumen v{} 安装包…", info.version),
                    "正在核对 SHA-256 完整性摘要",
                    1.0,
                );
            }
            UpdateState::ReadyToInstall { info, .. } => {
                let subtitle = if probes.installed {
                    "已通过 SHA-256 校验；点击后将静默升级并自动重新启动 Lumen"
                } else {
                    "已通过 SHA-256 校验；点击后将打开安装向导并退出当前实例"
                };
                if action_card(
                    ui,
                    t,
                    w,
                    "set-upd-install",
                    &format!("Lumen v{} 更新已就绪", info.version),
                    subtitle,
                    "立即安装",
                    focus,
                ) {
                    invalidate_system_probes();
                    self.command(Command::InstallUpdate);
                }
            }
            UpdateState::Error { message, .. } => {
                status_banner(ui, t, w, t.danger, "检查或下载更新失败", &message);
                if action_card(
                    ui,
                    t,
                    w,
                    "set-upd-retry",
                    "重新尝试检查更新",
                    "如果处于离线或受限网络环境，也可点击上方按钮前往 GitHub Releases 手动下载",
                    "重试检查",
                    focus,
                ) {
                    self.command(Command::CheckUpdates);
                }
            }
        }
    }
}

fn paint_release_notes_box(ui: &mut egui::Ui, t: &Theme, w: f32, notes: &str) {
    let text_w = (w - 36.0).max(80.0);
    let measured = widgets::galley_wrap(ui, notes, theme::font(11.5), t.text_secondary, text_w, usize::MAX);
    let box_h = (measured.size().y + 20.0).clamp(48.0, 168.0);
    let (box_rect, _) = ui.allocate_exact_size(Vec2::new(w, box_h), Sense::hover());
    ui.painter().rect_filled(box_rect, CornerRadius::same(6), t.surface_alt);
    ui.painter().rect_stroke(box_rect, CornerRadius::same(6), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);

    let inner = box_rect.shrink2(Vec2::new(12.0, 10.0));
    let mut notes_ui = ui.new_child(egui::UiBuilder::new().max_rect(inner));
    notes_ui.set_clip_rect(inner);
    egui::ScrollArea::vertical().id_salt("settings-release-notes").auto_shrink([false, false]).show(
        &mut notes_ui,
        |ui| {
            ui.set_width((inner.width() - 10.0).max(40.0));
            ui.add(egui::Label::new(egui::RichText::new(notes).font(theme::font(11.5)).color(t.text_secondary)).wrap());
        },
    );
    ui.add_space(6.0);
}

fn paint_settings_shortcuts(ui: &mut egui::Ui, t: &Theme, w: f32) {
    const NAV_ROWS: [(&str, &str); 10] = [
        ("Alt + ← / → / ↑", "后退 / 前进 / 上一级目录"),
        ("Backspace", "返回上一级目录（可在常规设置中改为后退）"),
        ("F5", "刷新当前目录或重新执行搜索"),
        ("Enter", "打开选中文件或进入选中文件夹"),
        ("Space", "快速预览 (Quick Look) 选中项目"),
        ("Ctrl + L / Alt + D / F4", "聚焦地址栏（支持输入 cmd / pwsh / %TEMP%）"),
        ("Ctrl + F / Ctrl + E / F3", "聚焦搜索框（支持 ext: size: date: type:）"),
        ("Ctrl + Shift + E", "开关双栏分屏视图"),
        ("Ctrl + 滚轮", "缩放平铺 / 大图标视图尺寸"),
        ("Esc", "取消搜索 / 清除选中 / 关闭当前弹窗"),
    ];
    const TAB_ROWS: [(&str, &str); 9] = [
        ("Ctrl + T / Ctrl + W", "新建 / 关闭当前标签页"),
        ("Ctrl + Shift + T", "重新打开最近关闭的标签页"),
        ("Ctrl + Tab / Ctrl + Shift + Tab", "切换到下一个 / 上一个标签页"),
        ("Ctrl + 1..9", "快速切换到第 1..8 / 最后一个标签页"),
        ("Ctrl + N", "打开新的独立窗口"),
        ("Ctrl + Shift + O", "吸收当前已打开的 Windows 资源管理器窗口为标签页"),
        ("Ctrl + Shift + G", "跳转到剪贴板中复制的本地路径"),
        ("Ctrl + ,", "打开 / 关闭设置中心"),
        ("Ctrl + Alt + E", "全局呼出 Lumen（在桌面协同中开启）"),
    ];
    const EDIT_ROWS: [(&str, &str); 12] = [
        ("Ctrl + A / Ctrl + Shift + I", "全选 / 反选当前列表项目"),
        ("方向键 / Home / End / PgUp / PgDn", "列表导航（按住 Shift 连选，直接键入首字母跳转）"),
        ("Ctrl + C / Ctrl + X", "复制 / 剪切选中项目"),
        ("Ctrl + V", "粘贴文件，或将剪贴板截图保存为 PNG"),
        ("Ctrl + Shift + C", "复制选中项（或当前目录）的完整路径"),
        ("Ctrl + Shift + N", "新建文件夹"),
        ("F2", "重命名（多选时弹出批量重命名；编辑时 Tab 跳下一项）"),
        ("Delete / Shift + Delete", "移入回收站 / 永久删除选中项目"),
        ("Ctrl + Z / Ctrl + Y / Ctrl + Shift + Z", "撤销（含回收站还原）/ 重做"),
        ("Alt + Enter / Alt + I", "打开 Windows 原生属性弹窗"),
        ("Shift + 右键 / Shift + F10", "弹出完整 Windows 经典 Shell 菜单"),
        ("Ctrl + H / Ctrl + Shift + D", "切换隐藏文件 / 切换浅色深色主题"),
    ];

    paint_shortcut_group(ui, t, w, "导航与视图", &NAV_ROWS);
    ui.add_space(8.0);
    paint_shortcut_group(ui, t, w, "标签页与窗口", &TAB_ROWS);
    ui.add_space(8.0);
    paint_shortcut_group(ui, t, w, "文件编辑与选择", &EDIT_ROWS);
}

fn paint_shortcut_group(ui: &mut egui::Ui, t: &Theme, w: f32, title: &str, rows: &[(&str, &str)]) {
    section_title(ui, t, title);
    let stacked = w < 380.0;
    let key_col_w = (w * 0.36).clamp(138.0, 196.0);
    for (idx, (keys, desc)) in rows.iter().enumerate() {
        if stacked {
            let key_g = widgets::galley_wrap(ui, keys, theme::mono(11.0), t.accent_text, (w - 12.0).max(60.0), 2);
            let desc_g = widgets::galley_wrap(ui, desc, theme::font(11.5), t.text_secondary, (w - 12.0).max(60.0), 2);
            let row_h = (key_g.size().y + desc_g.size().y + 12.0).max(36.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(w, row_h), Sense::hover());
            if idx % 2 == 1 {
                ui.painter().rect_filled(rect, CornerRadius::same(4), widgets::fade(t.hover, 0.55));
            }
            widgets::galley_at(ui.painter(), Pos2::new(rect.left() + 6.0, rect.top() + 5.0), key_g, t.accent_text);
            widgets::galley_at(
                ui.painter(),
                Pos2::new(rect.left() + 6.0, rect.bottom() - desc_g.size().y - 5.0),
                desc_g,
                t.text_secondary,
            );
        } else {
            let key_w = (key_col_w - 8.0).max(80.0);
            let desc_w = (w - key_col_w - 10.0).max(80.0);
            let key_g = widgets::galley_wrap(ui, keys, theme::mono(11.0), t.accent_text, key_w, 2);
            let desc_g = widgets::galley_wrap(ui, desc, theme::font(12.0), t.text_secondary, desc_w, 2);
            let row_h = (key_g.size().y.max(desc_g.size().y) + 10.0).max(26.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(w, row_h), Sense::hover());
            if idx % 2 == 1 {
                ui.painter().rect_filled(rect, CornerRadius::same(4), widgets::fade(t.hover, 0.55));
            }
            let key_y = rect.center().y - key_g.size().y * 0.5;
            let desc_y = rect.center().y - desc_g.size().y * 0.5;
            widgets::galley_at(ui.painter(), Pos2::new(rect.left() + 6.0, key_y), key_g, t.accent_text);
            widgets::galley_at(
                ui.painter(),
                Pos2::new(rect.left() + key_col_w + 4.0, desc_y),
                desc_g,
                t.text_secondary,
            );
        }
    }
}

fn section_title(ui: &mut egui::Ui, t: &Theme, title: &str) {
    let g = widgets::galley_wrap(ui, title, theme::semibold(12.5), t.text, ui.available_width().max(80.0), 2);
    let h = (g.size().y + 6.0).max(24.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
    widgets::galley_at(ui.painter(), Pos2::new(rect.left(), rect.center().y - g.size().y * 0.5), g, t.text);
    ui.add_space(4.0);
}

/// Compact wrapped system-probe status strip with a subtle manual re-probe trigger.
fn probe_status_strip(
    ui: &mut egui::Ui,
    t: &Theme,
    w: f32,
    refresh_salt: &str,
    dot: Color32,
    line: &str,
    focus: &mut FocusTracker,
) -> bool {
    let btn_size = 24.0;
    let text_w = (w - 20.0 - btn_size - 8.0).max(80.0);
    let g = widgets::galley_wrap(ui, line, theme::font(11.5), t.text_secondary, text_w, 2);
    let row_h = (g.size().y + 10.0).max(26.0);
    let (st_rect, _) = ui.allocate_exact_size(Vec2::new(w, row_h), Sense::hover());
    ui.painter().circle_filled(Pos2::new(st_rect.left() + 8.0, st_rect.center().y), 4.0, dot);
    widgets::galley_at(
        ui.painter(),
        Pos2::new(st_rect.left() + 18.0, st_rect.center().y - g.size().y * 0.5),
        g,
        t.text_secondary,
    );

    let refresh_rect =
        Rect::from_center_size(Pos2::new(st_rect.right() - btn_size * 0.5, st_rect.center().y), Vec2::splat(btn_size));
    let id = egui::Id::unique(refresh_salt);
    focus.register(id);
    let resp = ui.interact(refresh_rect, id, Sense::click());
    let hover = widgets::hover_t(ui.ctx(), id, resp.hovered());
    if hover > 0.0 {
        ui.painter().rect_filled(refresh_rect, CornerRadius::same(4), widgets::fade(t.hover_strong, hover));
    }
    widgets::icon_at(ui.painter(), refresh_rect.center(), icons::REFRESH, 11.5, t.text_secondary);
    focus.paint_ring_if_focused(ui, refresh_rect, 5, id, t);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let clicked = resp.clicked() || focus.is_activated(ui, id);
    let _ = resp.on_hover_text("重新检测系统状态");
    clicked
}

fn status_banner(ui: &mut egui::Ui, t: &Theme, w: f32, dot: Color32, title: &str, subtitle: &str) {
    let text_w = (w - 40.0).max(80.0);
    let sub_g = widgets::galley_wrap(ui, subtitle, theme::font(11.0), t.text_tertiary, text_w, 3);
    let card_h = (10.0 + 20.0 + 3.0 + sub_g.size().y + 10.0).max(56.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, card_h), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(6), t.surface_alt);
    ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    ui.painter().circle_filled(Pos2::new(rect.left() + 16.0, rect.top() + 20.0), 4.0, dot);
    widgets::text_left(
        ui,
        ui.painter(),
        Rect::from_min_max(
            Pos2::new(rect.left() + 28.0, rect.top() + 9.0),
            Pos2::new(rect.right() - 12.0, rect.top() + 29.0),
        ),
        title,
        theme::semibold(12.5),
        t.text,
        0.0,
    );
    widgets::galley_at(ui.painter(), Pos2::new(rect.left() + 28.0, rect.top() + 32.0), sub_g, t.text_tertiary);
    ui.add_space(6.0);
}

#[allow(clippy::too_many_arguments)]
fn status_indicator_card(
    ui: &mut egui::Ui,
    t: &Theme,
    w: f32,
    title: &str,
    subtitle: &str,
    badge_label: &str,
    dot_color: Color32,
    active: bool,
) {
    let badge_g = widgets::galley(ui, badge_label, theme::semibold(11.0), t.text, 96.0);
    let badge_w = (badge_g.size().x + 26.0).clamp(68.0, 108.0);
    let text_w = (w - badge_w - 32.0).max(80.0);
    let sub_g = widgets::galley_wrap(ui, subtitle, theme::font(11.0), t.text_tertiary, text_w, 2);
    let card_h = (10.0 + 20.0 + 3.0 + sub_g.size().y + 10.0).max(56.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, card_h), Sense::hover());

    ui.painter().rect_filled(rect, CornerRadius::same(6), t.surface_alt);
    ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);

    widgets::text_left(
        ui,
        ui.painter(),
        Rect::from_min_max(
            Pos2::new(rect.left() + 12.0, rect.top() + 9.0),
            Pos2::new(rect.right() - badge_w - 20.0, rect.top() + 29.0),
        ),
        title,
        theme::semibold(12.5),
        t.text,
        0.0,
    );
    widgets::galley_at(ui.painter(), Pos2::new(rect.left() + 12.0, rect.top() + 32.0), sub_g, t.text_tertiary);

    let badge_rect = Rect::from_center_size(
        Pos2::new(rect.right() - badge_w * 0.5 - 12.0, rect.center().y),
        Vec2::new(badge_w, 24.0),
    );
    let badge_bg = if active { t.accent_soft } else { t.surface_raised };
    let badge_fg = if active { t.accent_text } else { t.text_secondary };
    ui.painter().rect_filled(badge_rect, CornerRadius::same(12), badge_bg);
    ui.painter().rect_stroke(badge_rect, CornerRadius::same(12), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    ui.painter().circle_filled(Pos2::new(badge_rect.left() + 10.0, badge_rect.center().y), 3.5, dot_color);
    let fg_g = widgets::galley(ui, badge_label, theme::semibold(11.0), badge_fg, badge_w - 20.0);
    widgets::galley_at(
        ui.painter(),
        Pos2::new(badge_rect.left() + 18.0, badge_rect.center().y - fg_g.size().y * 0.5),
        fg_g,
        badge_fg,
    );
    ui.add_space(6.0);
}

fn progress_banner(ui: &mut egui::Ui, t: &Theme, w: f32, title: &str, subtitle: &str, frac: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 66.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(6), t.surface_alt);
    ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    widgets::text_left(
        ui,
        ui.painter(),
        Rect::from_min_max(
            Pos2::new(rect.left() + 12.0, rect.top() + 8.0),
            Pos2::new(rect.right() - 12.0, rect.top() + 26.0),
        ),
        title,
        theme::semibold(12.5),
        t.text,
        0.0,
    );
    widgets::text_left(
        ui,
        ui.painter(),
        Rect::from_min_max(
            Pos2::new(rect.left() + 12.0, rect.top() + 26.0),
            Pos2::new(rect.right() - 12.0, rect.top() + 44.0),
        ),
        subtitle,
        theme::font(11.0),
        t.text_tertiary,
        0.0,
    );
    let track = Rect::from_min_size(Pos2::new(rect.left() + 12.0, rect.bottom() - 14.0), Vec2::new(w - 24.0, 5.0));
    ui.painter().rect_filled(track, CornerRadius::same(3), t.surface_raised);
    let fill_w = (track.width() * frac.clamp(0.0, 1.0)).max(8.0).min(track.width());
    ui.painter().rect_filled(
        Rect::from_min_size(track.min, Vec2::new(fill_w, track.height())),
        CornerRadius::same(3),
        t.accent,
    );
    ui.add_space(6.0);
}

fn should_stack_segments(w: f32, option_count: usize, max_label_w: f32) -> bool {
    if option_count <= 1 {
        return false;
    }
    if w < SEGMENT_STACK_BREAKPOINT {
        return true;
    }
    let cell_w = (w - 6.0) / option_count as f32;
    max_label_w + 14.0 > cell_w
}

fn segmented_row<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    t: &Theme,
    w: f32,
    salt: &str,
    options: &[(T, &str)],
    current: T,
    focus: &mut FocusTracker,
) -> Option<T> {
    let max_label_w =
        options.iter().map(|(_, label)| widgets::ellipsis_size(ui, label, theme::font(12.0)).x).fold(0.0f32, f32::max);
    let stacked = should_stack_segments(w, options.len(), max_label_w);
    let active_idx = options.iter().position(|(v, _)| *v == current).unwrap_or(0);
    let mut picked = None;

    if stacked {
        let row_h = 30.0;
        let total_h = options.len() as f32 * row_h + 6.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, total_h), Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(6), t.surface_alt);
        ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);

        let pill_slot = ui.painter().add(egui::Shape::Noop);
        let target_y = rect.top() + 3.0 + active_idx as f32 * row_h;
        let (py, pv) = spring::animate_vel(
            ui.ctx(),
            egui::Id::unique((salt, "pill-y")),
            target_y,
            target_y,
            spring::MAGNETIC,
            0.15,
        );
        let stretch = (pv.abs() * 0.008).min(8.0);
        let pill = Rect::from_min_size(
            Pos2::new(rect.left() + 3.0, (py - stretch * 0.5).clamp(rect.top() + 2.0, rect.bottom() - row_h - 2.0)),
            Vec2::new((rect.width() - 6.0).max(0.0), (row_h + stretch).min(rect.height() - 4.0)),
        );
        ui.painter().set(pill_slot, egui::Shape::rect_filled(pill, CornerRadius::same(4), t.accent));

        for (i, (val, label)) in options.iter().enumerate() {
            let cell = Rect::from_min_size(
                Pos2::new(rect.left() + 3.0, rect.top() + 3.0 + i as f32 * row_h),
                Vec2::new((rect.width() - 6.0).max(0.0), row_h),
            );
            let id = egui::Id::unique((salt, i));
            focus.register(id);
            let resp = ui.interact(cell, id, Sense::click());
            let active = *val == current;
            let hover = widgets::hover_t(ui.ctx(), id, resp.hovered());
            if !active && hover > 0.0 {
                ui.painter().rect_filled(cell, CornerRadius::same(4), widgets::fade(t.hover_strong, hover));
            }
            let color = if active { t.text_on_accent } else { t.text };
            let g = widgets::galley(ui, label, theme::font(12.0), color, (cell.width() - 16.0).max(0.0));
            widgets::galley_at(
                ui.painter(),
                Pos2::new(cell.left() + 10.0, cell.center().y - g.size().y * 0.5),
                g,
                color,
            );
            focus.paint_ring_if_focused(ui, cell, 5, id, t);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if (resp.clicked() || focus.is_activated(ui, id)) && !active {
                picked = Some(*val);
            }
        }
        return picked;
    }

    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 34.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(6), t.surface_alt);
    ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let cell_w = (rect.width() - 6.0) / options.len().max(1) as f32;

    // Elastic Q弹 sliding accent pill behind labels.
    let pill_slot = ui.painter().add(egui::Shape::Noop);
    let target_x = rect.left() + 3.0 + active_idx as f32 * cell_w;
    let (px, pv) =
        spring::animate_vel(ui.ctx(), egui::Id::unique((salt, "pill-x")), target_x, target_x, spring::MAGNETIC, 0.15);
    let stretch = (pv.abs() * 0.008).min(14.0);
    let pill = Rect::from_min_size(
        Pos2::new((px - stretch * 0.5).clamp(rect.left() + 2.0, rect.right() - cell_w - 2.0), rect.top() + 3.0),
        Vec2::new((cell_w + stretch).min(rect.width() - 4.0), rect.height() - 6.0),
    );
    ui.painter().set(pill_slot, egui::Shape::rect_filled(pill, CornerRadius::same(4), t.accent));

    for (i, (val, label)) in options.iter().enumerate() {
        let cell = Rect::from_min_size(
            Pos2::new(rect.left() + 3.0 + i as f32 * cell_w, rect.top() + 3.0),
            Vec2::new(cell_w, rect.height() - 6.0),
        );
        let id = egui::Id::unique((salt, i));
        focus.register(id);
        let resp = ui.interact(cell, id, Sense::click());
        let active = *val == current;
        let hover = widgets::hover_t(ui.ctx(), id, resp.hovered());
        if !active && hover > 0.0 {
            ui.painter().rect_filled(cell, CornerRadius::same(4), widgets::fade(t.hover_strong, hover));
        }
        let color = if active { t.text_on_accent } else { t.text };
        let g = widgets::galley(ui, label, theme::font(12.0), color, (cell.width() - 8.0).max(0.0));
        widgets::galley_at(ui.painter(), cell.center() - g.size() * 0.5, g, color);
        focus.paint_ring_if_focused(ui, cell, 5, id, t);
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if (resp.clicked() || focus.is_activated(ui, id)) && !active {
            picked = Some(*val);
        }
    }
    picked
}

#[allow(clippy::too_many_arguments)]
fn action_card(
    ui: &mut egui::Ui,
    t: &Theme,
    w: f32,
    salt: &str,
    title: &str,
    subtitle: &str,
    button_label: &str,
    focus: &mut FocusTracker,
) -> bool {
    let label_size = widgets::ellipsis_size(ui, button_label, theme::semibold(11.5));
    let btn_w = (label_size.x + 24.0).clamp(88.0, 112.0);
    let text_w = (w - btn_w - 32.0).max(80.0);
    let sub_g = widgets::galley_wrap(ui, subtitle, theme::font(11.0), t.text_tertiary, text_w, 2);
    let card_h = (10.0 + 20.0 + 3.0 + sub_g.size().y + 10.0).max(60.0);

    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, card_h), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(6), t.surface_alt);
    ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);

    let text_right = rect.right() - btn_w - 20.0;
    widgets::text_left(
        ui,
        ui.painter(),
        Rect::from_min_max(Pos2::new(rect.left() + 12.0, rect.top() + 9.0), Pos2::new(text_right, rect.top() + 29.0)),
        title,
        theme::semibold(12.5),
        t.text,
        0.0,
    );
    widgets::galley_at(ui.painter(), Pos2::new(rect.left() + 12.0, rect.top() + 32.0), sub_g, t.text_tertiary);

    // Button-only click hit target so reading descriptions never triggers accidental actions.
    let btn =
        Rect::from_center_size(Pos2::new(rect.right() - btn_w * 0.5 - 12.0, rect.center().y), Vec2::new(btn_w, 28.0));
    let id = egui::Id::unique(salt);
    focus.register(id);
    let btn_resp = ui.interact(btn, id, Sense::click());
    let btn_hover = widgets::hover_t(ui.ctx(), id, btn_resp.hovered());
    let press_scale = spring::animate(
        ui.ctx(),
        id.with("press"),
        if btn_resp.is_pointer_button_down_on() { 0.96 } else { 1.0 },
        1.0,
        spring::MAGNETIC,
    );
    let draw_btn = spring::scale_rect_around(btn, btn.center(), press_scale);
    ui.painter().rect_filled(draw_btn, CornerRadius::same(5), t.accent);
    if btn_hover > 0.0 {
        ui.painter().rect_stroke(
            draw_btn,
            CornerRadius::same(5),
            Stroke::new(1.2, widgets::fade(t.text_on_accent, 0.35 * btn_hover)),
            egui::StrokeKind::Inside,
        );
    }
    let bg = widgets::galley(ui, button_label, theme::semibold(11.5), t.text_on_accent, (btn_w - 10.0).max(0.0));
    widgets::galley_at(ui.painter(), draw_btn.center() - bg.size() * 0.5, bg, t.text_on_accent);
    focus.paint_ring_if_focused(ui, draw_btn, 6, id, t);

    if btn_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.add_space(6.0);
    btn_resp.clicked() || focus.is_activated(ui, id)
}

#[allow(clippy::too_many_arguments)]
fn toggle_card(
    ui: &mut egui::Ui,
    t: &Theme,
    w: f32,
    salt: &str,
    title: &str,
    badge: Option<&str>,
    subtitle: &str,
    checked: bool,
    focus: &mut FocusTracker,
) -> bool {
    let text_w = (w - 74.0).max(80.0);
    let sub_g = widgets::galley_wrap(ui, subtitle, theme::font(11.0), t.text_tertiary, text_w, 2);
    let card_h = (10.0 + 20.0 + 3.0 + sub_g.size().y + 10.0).max(56.0);

    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, card_h), Sense::hover());
    let id = egui::Id::unique(salt);
    focus.register(id);
    let resp = ui.interact(rect, id, Sense::click());
    let hover = widgets::hover_t(ui.ctx(), id, resp.hovered());
    let press_scale = spring::animate(
        ui.ctx(),
        id.with("press"),
        if resp.is_pointer_button_down_on() { 0.99 } else { 1.0 },
        1.0,
        spring::MAGNETIC,
    );
    let draw = spring::scale_rect_around(rect, rect.center(), press_scale);
    let fill = if hover > 0.0 { t.hover } else { t.surface_alt };
    ui.painter().rect_filled(draw, CornerRadius::same(6), fill);
    ui.painter().rect_stroke(draw, CornerRadius::same(6), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);

    let text_right = draw.right() - 62.0;
    let title_top = draw.top() + 9.0;
    let title_h = 20.0;
    if let Some(badge_text) = badge {
        let badge_g = widgets::galley(ui, badge_text, theme::semibold(10.0), t.accent_text, 88.0);
        let badge_w = badge_g.size().x + 12.0;
        let max_title_w = (text_right - (draw.left() + 12.0) - badge_w - 8.0).max(40.0);
        let title_g = widgets::galley(ui, title, theme::semibold(12.5), t.text, max_title_w);
        let title_w = title_g.size().x;
        let title_cy = title_top + title_h * 0.5;
        widgets::galley_at(
            ui.painter(),
            Pos2::new(draw.left() + 12.0, title_cy - title_g.size().y * 0.5),
            title_g,
            t.text,
        );
        let badge_rect = Rect::from_min_size(
            Pos2::new((draw.left() + 12.0 + title_w + 8.0).min(text_right - badge_w), title_cy - 8.5),
            Vec2::new(badge_w, 17.0),
        );
        ui.painter().rect_filled(badge_rect, CornerRadius::same(4), t.accent_soft);
        widgets::galley_at(ui.painter(), badge_rect.center() - badge_g.size() * 0.5, badge_g, t.accent_text);
    } else {
        widgets::text_left(
            ui,
            ui.painter(),
            Rect::from_min_max(Pos2::new(draw.left() + 12.0, title_top), Pos2::new(text_right, title_top + title_h)),
            title,
            theme::semibold(12.5),
            t.text,
            0.0,
        );
    }
    widgets::galley_at(ui.painter(), Pos2::new(draw.left() + 12.0, title_top + title_h + 3.0), sub_g, t.text_tertiary);

    // Fluent 2 Q弹 pill switch with jelly stretch while sliding.
    let target = if checked { 1.0 } else { 0.0 };
    let (s, vel) = spring::animate_vel(ui.ctx(), id.with("knob"), target, target, spring::JELLY, 0.002);
    let pill = Rect::from_center_size(Pos2::new(draw.right() - 32.0, draw.center().y), Vec2::new(40.0, 20.0));
    let track_bg = spring::mix_color(t.surface, t.accent, s);
    ui.painter().rect_filled(pill, CornerRadius::same(10), track_bg);
    if s < 0.95 {
        ui.painter().rect_stroke(
            pill,
            CornerRadius::same(10),
            Stroke::new(1.2, widgets::fade(t.border_strong, (1.0 - s).clamp(0.0, 1.0))),
            egui::StrokeKind::Inside,
        );
    }
    let stretch = (vel.abs() * 0.45).min(6.5);
    let knob_w = 12.0 + stretch;
    let knob_h = (12.0 - stretch * 0.2).max(10.0);
    let travel = pill.width() - 8.0 - knob_w;
    let knob_cx = (pill.left() + 4.0 + knob_w * 0.5 + travel * s)
        .clamp(pill.left() + 4.0 + knob_w * 0.5, pill.right() - 4.0 - knob_w * 0.5);
    let knob_rect = Rect::from_center_size(Pos2::new(knob_cx, pill.center().y), Vec2::new(knob_w, knob_h));
    let knob_col = spring::mix_color(t.text_secondary, t.text_on_accent, s);
    ui.painter().rect_filled(knob_rect, CornerRadius::same(6), knob_col);

    focus.paint_ring_if_focused(ui, draw, 7, id, t);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.add_space(6.0);
    resp.clicked() || focus.is_activated(ui, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_cache_hits_within_ttl_and_refreshes_on_transitions_and_invalidation() {
        let mut cache = ProbeCache::default();
        let t0 = Instant::now();
        let mut calls = 0usize;
        let sample = |n: usize| SystemProbes {
            installed: n % 2 == 1,
            has_context_menu: true,
            is_auto_start: n > 1,
            everything_running: true,
        };

        assert!(!cache.on_frame_open(SettingsTab::Integration));
        let first = cache.get_or_refresh_with(t0, || {
            calls += 1;
            sample(calls)
        });
        assert_eq!(calls, 1);
        assert_eq!(first, sample(1));

        // Subsequent frames within 5s TTL reuse cached snapshot without re-probing.
        assert!(cache.on_frame_open(SettingsTab::Integration));
        let second = cache.get_or_refresh_with(t0 + Duration::from_secs(2), || {
            calls += 1;
            sample(calls)
        });
        assert_eq!(calls, 1);
        assert_eq!(second, first);

        // Switching to Search tab invalidates cache immediately.
        assert!(cache.on_frame_open(SettingsTab::Search));
        let after_tab = cache.get_or_refresh_with(t0 + Duration::from_secs(2), || {
            calls += 1;
            sample(calls)
        });
        assert_eq!(calls, 2);
        assert_eq!(after_tab, sample(2));

        // Explicit invalidation (e.g. button click) forces re-probe on next read.
        cache.invalidate();
        let after_inv = cache.get_or_refresh_with(t0 + Duration::from_secs(3), || {
            calls += 1;
            sample(calls)
        });
        assert_eq!(calls, 3);
        assert_eq!(after_inv, sample(3));

        // TTL expiration (> 5s) refreshes automatically.
        let after_ttl = cache.get_or_refresh_with(t0 + Duration::from_secs(9), || {
            calls += 1;
            sample(calls)
        });
        assert_eq!(calls, 4);
        assert_eq!(after_ttl, sample(4));

        // Closing and reopening clears cached snapshot so next open probes fresh state.
        cache.on_closed();
        assert!(!cache.on_frame_open(SettingsTab::Search));
        let after_reopen = cache.get_or_refresh_with(t0 + Duration::from_secs(9), || {
            calls += 1;
            sample(calls)
        });
        assert_eq!(calls, 5);
        assert_eq!(after_reopen, sample(5));
    }

    #[test]
    fn settings_layout_adapts_cleanly_at_narrow_820px_and_wide_windows() {
        let narrow = settings_layout(Rect::from_min_size(Pos2::ZERO, Vec2::new(820.0, 520.0)));
        assert!(narrow.modal.width() >= 740.0 && narrow.modal.width() <= 788.0);
        assert!(narrow.modal.height() <= 504.0);
        assert!(narrow.modal.width() - narrow.rail_w >= 580.0);

        let very_narrow = settings_layout(Rect::from_min_size(Pos2::ZERO, Vec2::new(620.0, 480.0)));
        assert_eq!(very_narrow.rail_w, 136.0);
        assert!(very_narrow.modal.width() <= 604.0);

        let wide = settings_layout(Rect::from_min_size(Pos2::ZERO, Vec2::new(1440.0, 900.0)));
        assert_eq!(wide.rail_w, 156.0);
        assert!(wide.modal.width() >= 740.0 && wide.modal.width() <= 780.0);
    }

    #[test]
    fn segmented_row_stacks_below_420px_or_when_labels_overflow() {
        assert!(should_stack_segments(410.0, 3, 90.0));
        assert!(!should_stack_segments(460.0, 3, 120.0));
        assert!(should_stack_segments(460.0, 3, 155.0));
        assert!(!should_stack_segments(300.0, 1, 200.0));
    }

    #[test]
    fn release_notes_galley_wrap_preserves_multiline_notes() {
        let ctx = egui::Context::default();
        let notes = "- Fixed crash on narrow split view\n- Added multi-line release notes\n- Improved Mica contrast\n- Fixed search date overflow";
        let mut rows = 0usize;
        let mut output = ctx.run_pass(egui::RawInput::default(), |ctx| {
            let g = widgets::layout_wrap(ctx, notes, theme::font(11.5), Color32::WHITE, 400.0, usize::MAX);
            rows = g.rows.len();
        });
        output.textures_delta.clear();
        assert!(rows >= 4, "expected at least 4 rendered rows for 4-line release notes, got {rows}");
    }
}
