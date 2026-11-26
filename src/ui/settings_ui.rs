//! Fluent 2 categorized Settings Center modal (`Ctrl+,` or toolbar gear icon)
//! with bouncy ("Q弹") spring animations for modals, nav rail, switches, and segments.

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::{Command, LumenApp};
use crate::core::settings::{SearchBackend, ThemeMode};
use crate::theme::{self, Theme};
use crate::ui::{icons, spring, widgets};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SettingsTab {
    #[default]
    General,
    Integration,
    Search,
    Shortcuts,
}

const NAV_TABS: [(SettingsTab, &str, &str); 4] = [
    (SettingsTab::General, icons::VIEW, "常规与外观"),
    (SettingsTab::Integration, icons::OPEN_EXTERNAL, "桌面协同"),
    (SettingsTab::Search, icons::SEARCH, "搜索与文件"),
    (SettingsTab::Shortcuts, icons::INFO, "快捷键"),
];

impl LumenApp {
    pub(crate) fn paint_settings_modal(&mut self, ui: &mut egui::Ui, full: Rect) {
        let ctx = ui.ctx().clone();
        let pop_id = egui::Id::unique("settings-modal-pop");
        if !self.settings_open {
            spring::clear(&ctx, pop_id);
            return;
        }
        let t = self.theme.clone();
        let painter = ui.painter().clone();

        let pop = spring::animate(&ctx, pop_id, 1.0, 0.0, spring::MODAL);
        let alpha = pop.clamp(0.0, 1.0);
        let scale = 0.94 + 0.06 * pop;

        painter.rect_filled(full, CornerRadius::ZERO, Color32::from_black_alpha((105.0 * alpha) as u8));
        let scrim = ui.interact(full, egui::Id::unique("settings-scrim"), Sense::click());
        if scrim.clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.settings_open = false;
            spring::clear(&ctx, pop_id);
            return;
        }

        let modal_w = (full.width() * 0.68).clamp(540.0, 740.0);
        let modal_h = (full.height() * 0.74).clamp(400.0, 560.0);
        let base_modal = Rect::from_center_size(full.center(), Vec2::new(modal_w, modal_h));
        let modal =
            spring::slide_y(spring::scale_rect_around(base_modal, base_modal.center(), scale), (1.0 - pop) * 10.0);
        painter.rect_filled(modal.translate(Vec2::new(0.0, 6.0)).expand(3.0), CornerRadius::same(12), t.shadow);
        painter.rect_filled(modal, CornerRadius::same(10), t.popup);
        painter.rect_stroke(modal, CornerRadius::same(10), Stroke::new(1.0, t.border_strong), egui::StrokeKind::Inside);
        let _ = ui.interact(modal, egui::Id::unique("settings-card"), Sense::click());

        // Header bar.
        let header = Rect::from_min_size(modal.min, Vec2::new(modal.width(), 48.0));
        widgets::icon_at(&painter, Pos2::new(header.left() + 24.0, header.center().y), icons::SETTINGS, 16.0, t.accent);
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
        let close_id = egui::Id::unique("settings-close");
        let close_resp = ui.interact(close_btn, close_id, Sense::click());
        let close_h = widgets::hover_t(&ctx, close_id, close_resp.hovered());
        if close_h > 0.0 {
            painter.rect_filled(close_btn, CornerRadius::same(5), widgets::fade(t.hover_strong, close_h));
        }
        widgets::icon_at(&painter, close_btn.center(), icons::CANCEL, 10.5, t.text_secondary);
        if close_resp.clicked() {
            self.settings_open = false;
            spring::clear(&ctx, pop_id);
            return;
        }
        painter.line_segment(
            [Pos2::new(modal.left(), header.bottom()), Pos2::new(modal.right(), header.bottom())],
            Stroke::new(1.0, t.divider),
        );

        // Left navigation rail with elastic sliding active pill.
        let rail_w = 160.0;
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
            let id = egui::Id::unique(("set-nav", idx));
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
                if active { t.accent } else { t.text_secondary },
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
            if resp.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                self.settings_tab = *tab;
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
                let w = content.width() - 12.0;
                match tab {
                    SettingsTab::General => self.paint_settings_general(ui, &t, w),
                    SettingsTab::Integration => self.paint_settings_integration(ui, &t, w),
                    SettingsTab::Search => self.paint_settings_search(ui, &t, w),
                    SettingsTab::Shortcuts => paint_settings_shortcuts(ui, &t, w),
                }
            },
        );
    }

    fn paint_settings_general(&mut self, ui: &mut egui::Ui, t: &Theme, w: f32) {
        section_title(ui, t, "外观与动效");
        let modes = [(ThemeMode::System, "跟随系统"), (ThemeMode::Light, "浅色"), (ThemeMode::Dark, "深色")];
        if let Some(new_mode) = segmented_row(ui, t, w, "theme-seg", &modes, self.theme_mode) {
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
            "按组件匹配专属曲线：开关果冻 Q弹、滑块磁吸、菜单丝绸展开、窗格与滚动零过冲平滑过渡",
            self.settings.spring_animations,
        ) {
            self.settings.spring_animations = !self.settings.spring_animations;
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-mica",
            "Windows 11 Mica 透明背景",
            "让桌面壁纸色调柔和地透出窗口标题栏与侧边栏",
            self.settings.mica_backdrop,
        ) {
            self.command(Command::ToggleMica);
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-compact",
            "紧凑列表模式",
            "缩小详细信息视图行距（26px），同屏显示更多文件",
            self.settings.compact_mode,
        ) {
            self.settings.compact_mode = !self.settings.compact_mode;
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-chk",
            "文件列表复选框",
            "悬停或选中行时在左侧显示复选框，无需按 Ctrl 即可勾选多选",
            self.settings.show_checkboxes,
        ) {
            self.settings.show_checkboxes = !self.settings.show_checkboxes;
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-ext",
            "隐藏已知文件扩展名",
            "在列表中隐藏文件后缀名（F2 重命名时仍自动保留原扩展名）",
            self.settings.hide_extensions,
        ) {
            self.settings.hide_extensions = !self.settings.hide_extensions;
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-bs",
            "Backspace 键执行“后退”",
            "开启后按 Backspace 返回上一访问历史；关闭时返回上一级父目录",
            self.settings.backspace_goes_back,
        ) {
            self.settings.backspace_goes_back = !self.settings.backspace_goes_back;
        }
    }

    fn paint_settings_integration(&mut self, ui: &mut egui::Ui, t: &Theme, w: f32) {
        section_title(ui, t, "零注册表 · 安全共存与高效协同（不触发杀软误报）");
        if action_card(
            ui,
            t,
            w,
            "set-import-exp",
            "一键吸收已打开的 Windows 资源管理器窗口",
            "通过只读 Shell COM 将其他软件弹出的系统资源管理器文件夹批量转为 Lumen 标签页（Ctrl+Shift+O）",
            "立即吸收",
        ) {
            self.settings_open = false;
            self.command(Command::ImportExplorerTabs);
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-tray",
            "关闭窗口时常驻系统托盘",
            "隐藏窗口保持常驻以实现毫秒级呼出，配合固定任务栏日常使用更顺手（右键托盘可退出）",
            self.settings.tray_resident,
        ) {
            self.command(Command::ToggleTrayResident);
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-wine",
            "全局呼出快捷键 (Ctrl+Alt+E)",
            "使用系统标准 RegisterHotKey API（无键盘钩子）；如需 Win+E 呼出可在微软 PowerToys 中映射",
            self.settings.intercept_win_e,
        ) {
            self.command(Command::ToggleWinE);
        }
        let is_auto = crate::shell::integrate::is_auto_start();
        if action_card(
            ui,
            t,
            w,
            "set-autostart",
            if is_auto {
                "开机自启：已检测到启动文件夹快捷方式"
            } else {
                "打开 Windows 启动文件夹 (shell:startup)"
            },
            "将 Lumen 快捷方式（可附 --background 参数）手动拖入启动文件夹即可，程序本身不写入注册表",
            "打开目录",
        ) {
            self.command(Command::ToggleAutoStart);
        }
    }

    fn paint_settings_search(&mut self, ui: &mut egui::Ui, t: &Theme, w: f32) {
        section_title(ui, t, "搜索引擎后端");
        let ev_running = crate::shell::search::is_everything_running();
        let status_line = if ev_running {
            "Everything IPC 状态：已检测到运行中（毫秒级全局索引可用）"
        } else {
            "Everything IPC 状态：未检测到运行（自动回退至内置多线程并行搜索）"
        };
        let (st_rect, _) = ui.allocate_exact_size(Vec2::new(w, 26.0), Sense::hover());
        let dot_color = if ev_running { Color32::from_rgb(0x10, 0x7C, 0x10) } else { t.text_tertiary };
        ui.painter().circle_filled(Pos2::new(st_rect.left() + 8.0, st_rect.center().y), 4.0, dot_color);
        widgets::text_left(
            ui,
            ui.painter(),
            Rect::from_min_max(Pos2::new(st_rect.left() + 18.0, st_rect.top()), st_rect.max),
            status_line,
            theme::font(11.5),
            t.text_secondary,
            0.0,
        );
        ui.add_space(4.0);

        let backends = [
            (SearchBackend::Auto, "自动（Everything 优先）"),
            (SearchBackend::WindowsSearch, "Windows 搜索索引"),
            (SearchBackend::BuiltIn, "仅内置并行"),
        ];
        if let Some(choice) = segmented_row(ui, t, w, "search-seg", &backends, self.settings.search_backend) {
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
            "按 Windows 隐藏属性（FILE_ATTRIBUTE_HIDDEN）控制显隐，不再误藏 .git / .env",
            self.show_hidden,
        ) {
            self.command(Command::ToggleHidden);
        }
        if toggle_card(
            ui,
            t,
            w,
            "set-system",
            "显示受保护的操作系统文件",
            "显示同时具有隐藏和系统属性的文件（如 desktop.ini、pagefile.sys）",
            self.show_system,
        ) {
            self.command(Command::ToggleSystemFiles);
        }
    }
}

fn paint_settings_shortcuts(ui: &mut egui::Ui, t: &Theme, w: f32) {
    section_title(ui, t, "常用快捷键一览");
    const ROWS: [(&str, &str); 20] = [
        ("Space", "快速预览 (Quick Look) 选中项目"),
        ("Ctrl + Shift + E", "开关双栏分屏视图"),
        ("Ctrl + Shift + O", "吸收当前已打开的 Windows 资源管理器窗口为标签页"),
        ("Ctrl + Shift + G", "跳转到剪贴板中复制的本地路径"),
        ("Ctrl + ,", "打开设置中心"),
        ("Ctrl + Alt + E", "全局呼出 Lumen（在桌面协同中开启）"),
        ("Ctrl + T / Ctrl + W", "新建 / 关闭当前标签页"),
        ("Ctrl + Shift + T", "重新打开最近关闭的标签页"),
        ("Ctrl + 1..9", "快速切换到第 1..9 个标签页"),
        ("Ctrl + N", "打开新的独立窗口"),
        ("Alt + ← / → / ↑", "后退 / 前进 / 上一级目录"),
        ("Ctrl + L / Alt + D / F4", "聚焦地址栏（支持输入 cmd / pwsh / %TEMP%）"),
        ("Ctrl + F / Ctrl + E / F3", "聚焦搜索框（支持 ext: size: date: type:）"),
        ("F2", "重命名（多选时弹出批量重命名；编辑时 Tab 跳下一项）"),
        ("Ctrl + Shift + N", "新建文件夹"),
        ("Ctrl + Z / Ctrl + Y", "撤销（含回收站还原）/ 重做"),
        ("Ctrl + V", "粘贴文件，或将剪贴板截图保存为 PNG"),
        ("Shift + 右键 / Shift + F10", "弹出完整 Windows 经典 Shell 菜单"),
        ("Alt + Enter", "打开 Windows 原生属性弹窗"),
        ("Ctrl + H / Ctrl + Shift + D", "切换隐藏文件 / 切换浅色深色主题"),
    ];
    for (keys, desc) in ROWS {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 26.0), Sense::hover());
        widgets::text_left(
            ui,
            ui.painter(),
            Rect::from_min_size(rect.min, Vec2::new(165.0, rect.height())),
            keys,
            theme::mono(11.5),
            t.accent,
            4.0,
        );
        widgets::text_left(
            ui,
            ui.painter(),
            Rect::from_min_max(Pos2::new(rect.left() + 170.0, rect.top()), rect.max),
            desc,
            theme::font(12.0),
            t.text_secondary,
            0.0,
        );
    }
}

fn section_title(ui: &mut egui::Ui, t: &Theme, title: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::hover());
    widgets::text_left(ui, ui.painter(), rect, title, theme::semibold(12.5), t.text, 0.0);
    ui.add_space(4.0);
}

fn segmented_row<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    t: &Theme,
    w: f32,
    salt: &str,
    options: &[(T, &str)],
    current: T,
) -> Option<T> {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 34.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(6), t.surface_alt);
    ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let cell_w = (rect.width() - 6.0) / options.len().max(1) as f32;
    let active_idx = options.iter().position(|(v, _)| *v == current).unwrap_or(0);

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

    let mut picked = None;
    for (i, (val, label)) in options.iter().enumerate() {
        let cell = Rect::from_min_size(
            Pos2::new(rect.left() + 3.0 + i as f32 * cell_w, rect.top() + 3.0),
            Vec2::new(cell_w, rect.height() - 6.0),
        );
        let id = egui::Id::unique((salt, i));
        let resp = ui.interact(cell, id, Sense::click());
        let active = *val == current;
        let hover = widgets::hover_t(ui.ctx(), id, resp.hovered());
        if !active && hover > 0.0 {
            ui.painter().rect_filled(cell, CornerRadius::same(4), widgets::fade(t.hover_strong, hover));
        }
        let color = if active { Color32::WHITE } else { t.text };
        let g = widgets::galley(ui, label, theme::font(12.0), color, cell.width() - 8.0);
        widgets::galley_at(ui.painter(), cell.center() - g.size() * 0.5, g, color);
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() && !active {
            picked = Some(*val);
        }
    }
    picked
}

fn action_card(
    ui: &mut egui::Ui,
    t: &Theme,
    w: f32,
    salt: &str,
    title: &str,
    subtitle: &str,
    button_label: &str,
) -> bool {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 60.0), Sense::hover());
    let id = egui::Id::unique(salt);
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

    let btn_w = 96.0;
    let text_right = draw.right() - btn_w - 20.0;
    widgets::text_left(
        ui,
        ui.painter(),
        Rect::from_min_max(Pos2::new(draw.left() + 12.0, draw.top() + 8.0), Pos2::new(text_right, draw.top() + 28.0)),
        title,
        theme::semibold(12.5),
        t.text,
        0.0,
    );
    widgets::text_left(
        ui,
        ui.painter(),
        Rect::from_min_max(
            Pos2::new(draw.left() + 12.0, draw.top() + 28.0),
            Pos2::new(text_right, draw.bottom() - 6.0),
        ),
        subtitle,
        theme::font(11.0),
        t.text_tertiary,
        0.0,
    );

    let btn =
        Rect::from_center_size(Pos2::new(draw.right() - btn_w * 0.5 - 12.0, draw.center().y), Vec2::new(btn_w, 28.0));
    ui.painter().rect_filled(btn, CornerRadius::same(5), t.accent);
    let bg = widgets::galley(ui, button_label, theme::semibold(11.5), Color32::WHITE, btn_w - 8.0);
    widgets::galley_at(ui.painter(), btn.center() - bg.size() * 0.5, bg, Color32::WHITE);

    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.add_space(6.0);
    resp.clicked()
}

fn toggle_card(ui: &mut egui::Ui, t: &Theme, w: f32, salt: &str, title: &str, subtitle: &str, checked: bool) -> bool {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 54.0), Sense::hover());
    let id = egui::Id::unique(salt);
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
    widgets::text_left(
        ui,
        ui.painter(),
        Rect::from_min_max(Pos2::new(draw.left() + 12.0, draw.top() + 6.0), Pos2::new(text_right, draw.top() + 26.0)),
        title,
        theme::semibold(12.5),
        t.text,
        0.0,
    );
    widgets::text_left(
        ui,
        ui.painter(),
        Rect::from_min_max(
            Pos2::new(draw.left() + 12.0, draw.top() + 26.0),
            Pos2::new(text_right, draw.bottom() - 6.0),
        ),
        subtitle,
        theme::font(11.0),
        t.text_tertiary,
        0.0,
    );

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
    let knob_col = spring::mix_color(t.text_secondary, Color32::WHITE, s);
    ui.painter().rect_filled(knob_rect, CornerRadius::same(6), knob_col);

    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.add_space(6.0);
    resp.clicked()
}
