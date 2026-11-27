//! Status bar with item counts, non-overlapping compact status pills, and view controls.

use egui::{CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::{Command, LumenApp};
use crate::core::ViewMode;
use crate::core::format_size;
use crate::theme::{self};
use crate::ui::{icons, widgets};

const PILL_GAP: f32 = 6.0;
const LEFT_PILL_CLEARANCE: f32 = 12.0;

#[derive(Clone, Copy)]
struct StatusLabelInput<'a> {
    total: usize,
    selected: usize,
    total_size: u64,
    selected_size: u64,
    loading: bool,
    is_this_pc: bool,
    search_running: bool,
    search_active: bool,
    search_backend: &'a str,
}

fn format_status_label(input: StatusLabelInput<'_>) -> String {
    let mut label = if input.search_running {
        format!("正在搜索 ({})…  已找到 {} 个", input.search_backend, input.total)
    } else if input.search_active {
        format!("{} 个搜索结果 ({})", input.total, input.search_backend)
    } else if input.loading {
        format!("正在读取… {} 个项目", input.total)
    } else {
        format!("{} 个项目", input.total)
    };

    if input.selected > 0 {
        if input.selected_size > 0 {
            label.push_str(&format!("，已选择 {} 个，{}", input.selected, format_size(input.selected_size)));
        } else {
            label.push_str(&format!("，已选择 {} 个", input.selected));
        }
    } else if !input.search_running && !input.search_active && !input.loading && !input.is_this_pc {
        label.push_str(&format!("，{}", format_size(input.total_size)));
    }
    label
}

fn format_op_pill_text(label: &str, percent: u8, active_jobs: usize) -> String {
    let queued = active_jobs.saturating_sub(1);
    match (percent > 0, queued > 0) {
        (true, true) => format!("{label} ({percent}% · +{queued}排队)"),
        (true, false) => format!("{label} ({percent}%)"),
        (false, true) => format!("{label} (+{queued}排队)"),
        (false, false) => label.to_owned(),
    }
}

fn format_op_tooltip(label: &str, percent: u8, active_jobs: usize) -> String {
    let queued = active_jobs.saturating_sub(1);
    let mut lines = vec![label.to_owned()];
    if percent > 0 {
        lines.push(format!("当前进度：{percent}%"));
    }
    if queued > 0 {
        lines.push(format!("队列中还有 {queued} 项任务（共 {active_jobs} 项进行/排队）"));
    } else {
        lines.push("正在后台执行文件操作（不阻塞界面）".to_owned());
    }
    lines.join("\n")
}

#[derive(Clone, Copy, Debug)]
struct PillBudgetSpec {
    min_w: f32,
    desired_w: f32,
}

/// Allocates right-to-left horizontal widths for prioritized status pills inside
/// `[min_left, max_right]` so pills coexist cleanly without ever overlapping the
/// left status text. Returns `Some((left, width))` for each placed pill.
fn allocate_pill_rects(min_left: f32, max_right: f32, gap: f32, specs: &[PillBudgetSpec]) -> Vec<Option<(f32, f32)>> {
    let total_budget = (max_right - min_left).max(0.0);
    let mut fit_count = 0usize;
    let mut min_sum = 0.0f32;
    for (idx, spec) in specs.iter().enumerate() {
        let extra_gap = if idx > 0 { gap } else { 0.0 };
        if min_sum + extra_gap + spec.min_w <= total_budget {
            min_sum += extra_gap + spec.min_w;
            fit_count += 1;
        } else {
            break;
        }
    }

    let mut out = vec![None; specs.len()];
    if fit_count == 0 {
        return out;
    }

    let gaps_total = gap * fit_count.saturating_sub(1) as f32;
    let desired_sum: f32 = specs[..fit_count].iter().map(|s| s.desired_w.max(s.min_w)).sum();
    let widths: Vec<f32> = if desired_sum + gaps_total <= total_budget {
        specs[..fit_count].iter().map(|s| s.desired_w.max(s.min_w)).collect()
    } else {
        let surplus = (total_budget - min_sum).max(0.0);
        let want_extra: f32 = specs[..fit_count].iter().map(|s| (s.desired_w - s.min_w).max(0.0)).sum();
        specs[..fit_count]
            .iter()
            .map(|s| {
                let extra =
                    if want_extra > 0.0 { surplus * ((s.desired_w - s.min_w).max(0.0) / want_extra) } else { 0.0 };
                s.min_w + extra
            })
            .collect()
    };

    let mut cursor_right = max_right;
    for (idx, width) in widths.into_iter().enumerate() {
        let left = (cursor_right - width).max(min_left);
        out[idx] = Some((left, (cursor_right - left).max(0.0)));
        cursor_right = left - gap;
    }
    out
}

enum StatusPill {
    Op { text: String, tooltip: String, percent: u8 },
    Update { text: String, tooltip: String, progress: Option<f32>, ready: bool },
    Clipboard { text: String, path: std::path::PathBuf },
}

impl LumenApp {
    pub(crate) fn paint_status_bar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let t = self.theme.clone();

        if self.settings.mica_backdrop {
            painter.rect_filled(rect, CornerRadius::ZERO, t.mica_status_bg);
            painter.line_segment(
                [Pos2::new(rect.left(), rect.top()), Pos2::new(rect.right(), rect.top())],
                Stroke::new(1.0, t.mica_subtle_border),
            );
        }

        // Compute right-hand view switcher and details toggle first so we know exact middle bounds.
        let models: [(ViewMode, &str, &str); 4] = [
            (ViewMode::Details, icons::LIST_BULLETS, "详细信息视图"),
            (ViewMode::List, icons::LIST, "列表视图"),
            (ViewMode::Tiles, icons::GRID, "平铺视图"),
            (ViewMode::Icons, icons::VIEW, "大图标视图"),
        ];
        let switcher_w = 4.0 * 27.0 + 4.0;
        let switcher = Rect::from_min_size(
            Pos2::new(rect.right() - switcher_w - 42.0, rect.center().y - 11.0),
            Vec2::new(switcher_w, 22.0),
        );
        let pills_max_right = switcher.left() - 10.0;

        // Collect all active right-hand pills (non-mutually-exclusive, prioritized right-to-left).
        let mut pills: Vec<StatusPill> = Vec::with_capacity(3);
        if let Some(op) = self.op_worker.progress() {
            let text = format_op_pill_text(&op.label, op.percent, op.active_jobs);
            let tooltip = format_op_tooltip(&op.label, op.percent, op.active_jobs);
            pills.push(StatusPill::Op { text, tooltip, percent: op.percent });
        }
        if let Some((text, tooltip, progress, ready)) = match &self.update_state {
            crate::shell::updater::UpdateState::Available(info) => Some((
                format!("新版本 v{}", info.version),
                format!("发现 Lumen v{}，点击查看更新详情", info.version),
                None,
                false,
            )),
            crate::shell::updater::UpdateState::Downloading { info, downloaded, total } => {
                let pct = if *total > 0 { (*downloaded as f32 / *total as f32).clamp(0.0, 1.0) } else { 0.0 };
                let tip = if *total > 0 {
                    format!(
                        "正在下载 Lumen v{} ({} / {})，点击查看详情",
                        info.version,
                        format_size(*downloaded),
                        format_size(*total)
                    )
                } else {
                    format!("正在下载 Lumen v{} (已下载 {})，点击查看详情", info.version, format_size(*downloaded))
                };
                Some((format!("更新 v{} ({:.0}%)", info.version, pct * 100.0), tip, Some(pct), false))
            }
            crate::shell::updater::UpdateState::Verifying { info } => Some((
                format!("校验 v{}…", info.version),
                format!("正在校验 Lumen v{} 安装包 SHA-256 摘要…", info.version),
                Some(1.0),
                false,
            )),
            crate::shell::updater::UpdateState::ReadyToInstall { info, .. } => Some((
                format!("安装 v{}", info.version),
                format!("Lumen v{} 已校验就绪，点击立即安装并重启", info.version),
                None,
                true,
            )),
            _ => None,
        } {
            pills.push(StatusPill::Update { text, tooltip, progress, ready });
        }
        if let Some(clip_path) = self.clipboard_path.clone()
            && self.current_dir().as_deref() != Some(clip_path.as_path())
        {
            let name = clip_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| clip_path.to_string_lossy().into_owned());
            pills.push(StatusPill::Clipboard { text: format!("跳转: {name}"), path: clip_path });
        }

        // Left-side status text with exact rendered right-edge tracking so right pills never overlap.
        let (total, selected, total_size, selected_size) = self.status_metrics();
        let label = format_status_label(StatusLabelInput {
            total,
            selected,
            total_size,
            selected_size,
            loading: self.active().loading,
            is_this_pc: self.active().is_this_pc(),
            search_running: self.search.running,
            search_active: self.search_active(),
            search_backend: self.search.engine.active_backend_label(),
        });
        let text_left = rect.left() + 14.0;
        let total_mid = (pills_max_right - text_left).max(0.0);
        let left_max_w = if pills.is_empty() { total_mid } else { (total_mid * 0.52).max(140.0).min(total_mid) };
        let natural_left_w = widgets::ellipsis_size(ui, &label, theme::font(11.5)).x;
        let left_galley = widgets::galley(ui, &label, theme::font(11.5), t.text_secondary, left_max_w);
        let left_size = left_galley.size();
        let left_pos = Pos2::new(text_left, rect.center().y - left_size.y * 0.5);
        widgets::galley_at(&painter, left_pos, left_galley, t.text_secondary);
        if natural_left_w > left_max_w + 1.0 {
            let left_rect =
                Rect::from_min_size(Pos2::new(text_left, rect.top()), Vec2::new(left_size.x, rect.height()));
            let _ = ui.interact(left_rect, egui::Id::unique("status-left-label"), Sense::hover()).on_hover_text(&label);
        }
        let left_text_right = text_left + left_size.x;
        let pills_min_left = left_text_right + LEFT_PILL_CLEARANCE;

        // Measure and allocate non-overlapping right-to-left widths for all active pills.
        if !pills.is_empty() && pills_max_right > pills_min_left {
            let specs: Vec<PillBudgetSpec> = pills
                .iter()
                .map(|pill| match pill {
                    StatusPill::Op { text, .. } => {
                        let w = widgets::ellipsis_size(ui, text, theme::font(10.5)).x + 18.0;
                        PillBudgetSpec { min_w: 88.0, desired_w: w.clamp(108.0, 224.0) }
                    }
                    StatusPill::Update { text, .. } => {
                        let w = widgets::ellipsis_size(ui, text, theme::font(11.0)).x + 18.0;
                        PillBudgetSpec { min_w: 68.0, desired_w: w.clamp(76.0, 178.0) }
                    }
                    StatusPill::Clipboard { text, .. } => {
                        let w = widgets::ellipsis_size(ui, text, theme::font(11.0)).x + 32.0;
                        PillBudgetSpec { min_w: 76.0, desired_w: w.clamp(84.0, 200.0) }
                    }
                })
                .collect();

            let slots = allocate_pill_rects(pills_min_left, pills_max_right, PILL_GAP, &specs);
            for (pill, slot) in pills.into_iter().zip(slots) {
                let Some((pill_left, pill_w)) = slot else { continue };
                match pill {
                    StatusPill::Op { text, tooltip, percent } => {
                        let pill_rect =
                            Rect::from_min_size(Pos2::new(pill_left, rect.center().y - 9.5), Vec2::new(pill_w, 19.0));
                        let oid = egui::Id::unique("op-progress-pill");
                        let oresp = ui.interact(pill_rect, oid, Sense::hover());
                        let hover = widgets::hover_t(&ctx, oid, oresp.hovered());
                        painter.rect_filled(pill_rect, CornerRadius::same(4), t.surface_raised);
                        let fill_w = (pill_rect.width() * (percent as f32 / 100.0)).clamp(0.0, pill_rect.width());
                        if fill_w > 0.0 {
                            painter.rect_filled(
                                Rect::from_min_size(pill_rect.min, Vec2::new(fill_w, pill_rect.height())),
                                CornerRadius::same(4),
                                t.accent_soft,
                            );
                        }
                        let border_col = if hover > 0.0 {
                            t.border_strong
                        } else if self.settings.mica_backdrop {
                            t.mica_subtle_border
                        } else {
                            t.border
                        };
                        painter.rect_stroke(
                            pill_rect,
                            CornerRadius::same(4),
                            Stroke::new(1.0, border_col),
                            egui::StrokeKind::Inside,
                        );
                        widgets::text_left(
                            ui,
                            &painter,
                            pill_rect.shrink2(Vec2::new(8.0, 0.0)),
                            &text,
                            theme::font(10.5),
                            t.text,
                            0.0,
                        );
                        let _ = oresp.on_hover_text(tooltip);
                    }
                    StatusPill::Update { text, tooltip, progress, ready } => {
                        let pill_rect =
                            Rect::from_min_size(Pos2::new(pill_left, rect.center().y - 10.0), Vec2::new(pill_w, 20.0));
                        let uid = egui::Id::unique("update-status-pill");
                        let uresp = ui.interact(pill_rect, uid, Sense::click());
                        let hover = widgets::hover_t(&ctx, uid, uresp.hovered());
                        if ready {
                            painter.rect_filled(pill_rect, CornerRadius::same(4), t.accent);
                            if hover > 0.0 {
                                painter.rect_stroke(
                                    pill_rect,
                                    CornerRadius::same(4),
                                    Stroke::new(1.0, widgets::fade(t.text_on_accent, 0.35 * hover)),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            widgets::text_left(
                                ui,
                                &painter,
                                pill_rect.shrink2(Vec2::new(8.0, 0.0)),
                                &text,
                                theme::semibold(11.0),
                                t.text_on_accent,
                                0.0,
                            );
                        } else {
                            painter.rect_filled(
                                pill_rect,
                                CornerRadius::same(4),
                                widgets::fade(t.accent_soft, 0.78 + hover * 0.22),
                            );
                            if let Some(pct) = progress {
                                let fw = (pill_rect.width() * pct).clamp(0.0, pill_rect.width());
                                painter.rect_filled(
                                    Rect::from_min_size(pill_rect.min, Vec2::new(fw, pill_rect.height())),
                                    CornerRadius::same(4),
                                    widgets::fade(t.accent, 0.22),
                                );
                            }
                            widgets::text_left(
                                ui,
                                &painter,
                                pill_rect.shrink2(Vec2::new(8.0, 0.0)),
                                &text,
                                theme::font(11.0),
                                t.accent_text,
                                0.0,
                            );
                        }
                        if uresp.hovered() {
                            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        let clicked = uresp.clicked();
                        let _ = uresp.on_hover_text(tooltip);
                        if clicked {
                            if ready {
                                self.command(Command::InstallUpdate);
                            } else {
                                self.settings_tab = crate::ui::settings_ui::SettingsTab::Updates;
                                self.settings_open = true;
                            }
                        }
                    }
                    StatusPill::Clipboard { text, path } => {
                        let pill_rect =
                            Rect::from_min_size(Pos2::new(pill_left, rect.center().y - 10.0), Vec2::new(pill_w, 20.0));
                        let jump_rect =
                            Rect::from_min_max(pill_rect.min, Pos2::new(pill_rect.right() - 20.0, pill_rect.bottom()));
                        let close_rect =
                            Rect::from_min_max(Pos2::new(pill_rect.right() - 20.0, pill_rect.top()), pill_rect.max);
                        let jid = egui::Id::unique("clip-jump-pill");
                        let cid = egui::Id::unique("clip-jump-dismiss");
                        let jresp = ui.interact(jump_rect, jid, Sense::click());
                        let cresp = ui.interact(close_rect, cid, Sense::click());
                        let hover = widgets::hover_t(&ctx, jid, jresp.hovered());
                        painter.rect_filled(
                            pill_rect,
                            CornerRadius::same(4),
                            widgets::fade(t.accent_soft, 0.75 + hover * 0.25),
                        );
                        widgets::text_left(
                            ui,
                            &painter,
                            jump_rect.shrink2(Vec2::new(7.0, 0.0)),
                            &text,
                            theme::font(11.0),
                            t.accent_text,
                            0.0,
                        );
                        widgets::icon_at(&painter, close_rect.center(), icons::CANCEL, 7.5, t.text_tertiary);
                        if jresp.hovered() || cresp.hovered() {
                            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        let j_clicked = jresp.clicked();
                        let c_clicked = cresp.clicked();
                        let _ = jresp.on_hover_text(format!("跳转到剪贴板路径: {} (Ctrl+Shift+G)", path.display()));
                        let _ = cresp.on_hover_text("忽略此剪贴板路径");
                        if c_clicked {
                            self.clipboard_path_dismissed = Some(path.to_string_lossy().to_ascii_lowercase());
                            self.clipboard_path = None;
                        } else if j_clicked {
                            self.command(Command::JumpClipboardPath);
                        }
                    }
                }
            }
        }

        // View switcher.
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
            if response.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let clicked = response.clicked();
            let _ = response.on_hover_text(*tip);
            if clicked {
                self.set_view(*mode);
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
            if self.details_visible { t.accent_text } else { t.text_tertiary },
        );
        if response.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let clicked = response.clicked();
        let details_tip = if self.details_visible { "隐藏详细信息窗格" } else { "显示详细信息窗格" };
        let _ = response.on_hover_text(details_tip);
        if clicked {
            self.command(Command::ToggleDetails);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_pc_status_hides_zero_byte_total() {
        let label = format_status_label(StatusLabelInput {
            total: 4,
            selected: 0,
            total_size: 0,
            selected_size: 0,
            loading: false,
            is_this_pc: true,
            search_running: false,
            search_active: false,
            search_backend: "",
        });
        assert_eq!(label, "4 个项目");
        assert!(!label.contains("0 B"));
    }

    #[test]
    fn search_mode_shows_selected_file_size() {
        let active_label = format_status_label(StatusLabelInput {
            total: 42,
            selected: 3,
            total_size: 0,
            selected_size: 2 * 1024 * 1024,
            loading: false,
            is_this_pc: false,
            search_running: false,
            search_active: true,
            search_backend: "Everything",
        });
        assert_eq!(active_label, format!("42 个搜索结果 (Everything)，已选择 3 个，{}", format_size(2 * 1024 * 1024)));

        let running_label = format_status_label(StatusLabelInput {
            total: 10,
            selected: 1,
            total_size: 0,
            selected_size: 4096,
            loading: false,
            is_this_pc: false,
            search_running: true,
            search_active: true,
            search_backend: "内置并行",
        });
        assert_eq!(running_label, format!("正在搜索 (内置并行)…  已找到 10 个，已选择 1 个，{}", format_size(4096)));
    }

    #[test]
    fn op_pill_and_tooltip_always_show_queued_count_even_with_nonzero_progress() {
        let text = format_op_pill_text("正在复制 5 个项目…", 45, 3);
        assert!(text.contains("45%"));
        assert!(text.contains("+2排队"));

        let tip = format_op_tooltip("正在复制 5 个项目…", 45, 3);
        assert!(tip.contains("正在复制 5 个项目…"));
        assert!(tip.contains("45%"));
        assert!(tip.contains("还有 2 项任务"));
    }

    #[test]
    fn right_pills_coexist_and_never_overlap_left_text_at_820px() {
        // Simulate 820px window: left text ends at 340px -> min_left = 352px, max_right = 656px (304px budget).
        let specs = [
            PillBudgetSpec { min_w: 88.0, desired_w: 160.0 },
            PillBudgetSpec { min_w: 68.0, desired_w: 110.0 },
            PillBudgetSpec { min_w: 76.0, desired_w: 140.0 },
        ];
        let min_left = 352.0;
        let max_right = 656.0;
        let slots = allocate_pill_rects(min_left, max_right, PILL_GAP, &specs);
        assert!(slots.iter().all(|s| s.is_some()), "all 3 compact pills should coexist in 304px budget");
        let mut prev_left = max_right + PILL_GAP;
        for (slot, spec) in slots.iter().zip(&specs) {
            let (left, width) = slot.unwrap();
            assert!(left >= min_left - 0.01, "pill left {left} overlapped min_left {min_left}");
            assert!(width >= spec.min_w - 0.01, "pill width {width} below min_w {}", spec.min_w);
            assert!(left + width <= prev_left - PILL_GAP + 0.01, "pills overlapped each other");
            prev_left = left;
        }

        // Tighter budget (170px) fits top-2 priority pills without overlapping min_left.
        let tight = allocate_pill_rects(486.0, 656.0, PILL_GAP, &specs);
        assert!(tight[0].is_some());
        assert!(tight[1].is_some());
        assert!(tight[2].is_none());
        assert!(tight[1].unwrap().0 >= 486.0 - 0.01);
    }
}
