//! Status bar with item counts and view controls.

use egui::{CornerRadius, Pos2, Rect, Sense, Vec2};

use crate::app::{Command, LumenApp};
use crate::core::ViewMode;
use crate::core::format_size;
use crate::theme::{self};
use crate::ui::{icons, widgets};

impl LumenApp {
    pub(crate) fn paint_status_bar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let t = self.theme.clone();
        let (total, selected, total_size, selected_size) = self.status_metrics();
        let mut label = if self.active().loading {
            format!("正在读取… {total} 个项目")
        } else {
            format!("{total} 个项目")
        };
        if selected > 0 {
            if selected_size > 0 {
                label.push_str(&format!("，已选择 {selected} 个，{}", format_size(selected_size)));
            } else {
                label.push_str(&format!("，已选择 {selected} 个"));
            }
        } else if !self.active().loading {
            label.push_str(&format!("，{}", format_size(total_size)));
        }
        if self.search.running {
            label = format!("正在搜索 ({})…  已找到 {total} 个", self.search.engine.active_backend_label());
        } else if self.search_active() {
            label = format!("{total} 个搜索结果 ({})", self.search.engine.active_backend_label());
            if selected > 0 {
                label.push_str(&format!("，已选择 {selected} 个"));
            }
        }
        widgets::text_left(
            ui,
            &painter,
            Rect::from_min_size(
                Pos2::new(rect.left() + 14.0, rect.top()),
                Vec2::new(rect.width() * 0.48, rect.height()),
            ),
            &label,
            theme::font(11.5),
            t.text_secondary,
            0.0,
        );

        // Live non-blocking background file-operation progress pill, or clipboard quick-jump pill.
        let pill_right = rect.right() - 166.0;
        if let Some(op) = self.op_worker.progress() {
            let pill_w = 220.0;
            if pill_right - pill_w > rect.left() + 200.0 {
                let pill =
                    Rect::from_min_size(Pos2::new(pill_right - pill_w, rect.center().y - 9.5), Vec2::new(pill_w, 19.0));
                painter.rect_filled(pill, CornerRadius::same(4), t.surface_raised);
                let fill_w = (pill.width() * (op.percent as f32 / 100.0)).clamp(0.0, pill.width());
                if fill_w > 0.0 {
                    painter.rect_filled(
                        Rect::from_min_size(pill.min, Vec2::new(fill_w, pill.height())),
                        CornerRadius::same(4),
                        t.accent_soft,
                    );
                }
                let text = if op.percent > 0 {
                    format!("{} ({}%)", op.label, op.percent)
                } else if op.active_jobs > 1 {
                    format!("{} [{}项排队]", op.label, op.active_jobs)
                } else {
                    op.label
                };
                widgets::text_left(
                    ui,
                    &painter,
                    pill.shrink2(Vec2::new(8.0, 0.0)),
                    &text,
                    theme::font(10.5),
                    t.text,
                    0.0,
                );
            }
        } else if let Some(clip_path) = self.clipboard_path.clone()
            && self.current_dir().as_deref() != Some(clip_path.as_path())
        {
            let name = clip_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| clip_path.to_string_lossy().into_owned());
            let tip_text = format!("跳转到剪贴板: {name}");
            let g = widgets::galley(ui, &tip_text, theme::font(11.0), t.accent, 210.0);
            let pill_w = (g.size().x + 34.0).min((pill_right - rect.left() - 210.0).max(0.0));
            if pill_w > 84.0 {
                let pill = Rect::from_min_size(
                    Pos2::new(pill_right - pill_w, rect.center().y - 10.0),
                    Vec2::new(pill_w, 20.0),
                );
                let jump_rect = Rect::from_min_max(pill.min, Pos2::new(pill.right() - 20.0, pill.bottom()));
                let close_rect = Rect::from_min_max(Pos2::new(pill.right() - 20.0, pill.top()), pill.max);
                let jid = egui::Id::unique("clip-jump-pill");
                let jresp = ui.interact(jump_rect, jid, Sense::click());
                let cresp = ui.interact(close_rect, egui::Id::unique("clip-jump-dismiss"), Sense::click());
                let hover = widgets::hover_t(&ctx, jid, jresp.hovered());
                painter.rect_filled(pill, CornerRadius::same(4), widgets::fade(t.accent_soft, 0.75 + hover * 0.25));
                widgets::text_left(
                    ui,
                    &painter,
                    jump_rect.shrink2(Vec2::new(7.0, 0.0)),
                    &tip_text,
                    theme::font(11.0),
                    t.accent,
                    0.0,
                );
                widgets::icon_at(&painter, close_rect.center(), icons::CANCEL, 7.5, t.text_tertiary);
                if jresp.hovered() || cresp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if jresp.hovered() {
                    let _ = jresp.clone().on_hover_text(format!("{} (Ctrl+Shift+G)", clip_path.display()));
                }
                if cresp.clicked() {
                    self.clipboard_path_dismissed = Some(clip_path.to_string_lossy().to_ascii_lowercase());
                    self.clipboard_path = None;
                } else if jresp.clicked() {
                    self.command(Command::JumpClipboardPath);
                }
            }
        }

        // View switcher.
        let models: [(ViewMode, &str, &str); 4] = [
            (ViewMode::Details, icons::LIST_BULLETS, "详细信息"),
            (ViewMode::List, icons::LIST, "列表"),
            (ViewMode::Tiles, icons::GRID, "平铺"),
            (ViewMode::Icons, icons::VIEW, "大图标"),
        ];
        let switcher_w = 4.0 * 27.0 + 4.0;
        let switcher = Rect::from_min_size(
            Pos2::new(rect.right() - switcher_w - 42.0, rect.center().y - 11.0),
            Vec2::new(switcher_w, 22.0),
        );
        painter.rect_filled(switcher, CornerRadius::same(4), widgets::fade(t.hover, 0.55));
        let current = self.active().view;
        let active_idx = models.iter().position(|(m, _, _)| *m == current).unwrap_or(0);
        let pill_slot = painter.add(egui::Shape::Noop);
        let target_x = switcher.left() + 2.0 + active_idx as f32 * 27.0;
        let (pill_x, pill_v) = crate::ui::spring::animate_vel(
            &ctx,
            egui::Id::unique("viewmode-pill-x"),
            target_x,
            target_x,
            crate::ui::spring::MAGNETIC,
            0.1,
        );
        let stretch = (pill_v.abs() * 0.008).min(6.0);
        let pill_rect = Rect::from_min_size(
            Pos2::new(
                (pill_x - stretch * 0.5).clamp(switcher.left() + 1.0, switcher.right() - 26.0),
                switcher.top() + 2.0,
            ),
            Vec2::new(26.0 + stretch, 18.0),
        );
        painter.set(pill_slot, egui::Shape::rect_filled(pill_rect, CornerRadius::same(3), t.surface));
        for (index, (mode, code, tip)) in models.iter().enumerate() {
            let cell = Rect::from_min_size(
                Pos2::new(switcher.left() + 2.0 + index as f32 * 27.0, switcher.top() + 2.0),
                Vec2::new(26.0, 18.0),
            );
            let id = egui::Id::unique(("viewmode", index));
            let response = ui.interact(cell, id, Sense::click());
            let hover = widgets::hover_t(&ctx, id, response.hovered());
            let active = current == *mode;
            if !active && hover > 0.0 {
                painter.rect_filled(cell, CornerRadius::same(3), widgets::fade(t.hover_strong, hover));
            }
            widgets::icon_at(&painter, cell.center(), code, 11.5, if active { t.text } else { t.text_tertiary });
            if response.clicked() {
                self.set_view(*mode);
            }
            if response.hovered() {
                response.on_hover_text(*tip);
            }
        }

        // Details pane toggle.
        let toggle = Rect::from_center_size(Pos2::new(rect.right() - 22.0, rect.center().y), Vec2::splat(24.0));
        let id = egui::Id::unique("status-details");
        let response = ui.interact(toggle, id, Sense::click());
        let hover = widgets::hover_t(&ctx, id, response.hovered());
        if self.details_visible {
            painter.rect_filled(toggle, CornerRadius::same(4), t.hover);
        } else if hover > 0.0 {
            painter.rect_filled(toggle, CornerRadius::same(4), widgets::fade(t.hover, hover));
        }
        widgets::icon_at(
            &painter,
            toggle.center(),
            icons::INFO,
            12.0,
            if self.details_visible { t.accent } else { t.text_tertiary },
        );
        if response.clicked() {
            self.command(Command::ToggleDetails);
        }
    }
}
