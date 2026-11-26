//! Small painting primitives shared by every surface.

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align2, Color32, CornerRadius, FontId, Galley, Painter, Pos2, Rect, Stroke, StrokeKind, Ui, Vec2};

use crate::theme;

/// Snap a logical position to the physical pixel grid so glyphs are not
/// resampled at fractional offsets.
pub fn snap_point(ctx: &egui::Context, pos: Pos2) -> Pos2 {
    let pp = ctx.pixels_per_point();
    Pos2::new((pos.x * pp).round() / pp, (pos.y * pp).round() / pp)
}

pub fn galley_at(painter: &Painter, pos: Pos2, galley: Arc<Galley>, color: Color32) {
    painter.galley(snap_point(painter.ctx(), pos), galley, color);
}

pub fn galley(ui: &Ui, text: &str, font: FontId, color: Color32, max_w: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), TextFormat { font_id: font, color, ..Default::default() });
    job.wrap = TextWrapping { max_width: max_w, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    ui.ctx().fonts_mut(|f| f.layout_job(job))
}

/// Like [`galley`] but allows wrapping over `max_rows` lines (used for paths).
pub fn galley_wrap(ui: &Ui, text: &str, font: FontId, color: Color32, max_w: f32, max_rows: usize) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), TextFormat { font_id: font, color, ..Default::default() });
    job.wrap = TextWrapping { max_width: max_w, max_rows, break_anywhere: true, overflow_character: Some('…') };
    ui.ctx().fonts_mut(|fonts| fonts.layout_job(job))
}

pub fn text_left(ui: &Ui, painter: &Painter, rect: Rect, text: &str, font: FontId, color: Color32, offset_x: f32) {
    let width = (rect.width() - offset_x).max(0.0);
    let g = galley(ui, text, font, color, width);
    painter.galley(
        snap_point(ui.ctx(), Pos2::new(rect.left() + offset_x, rect.center().y - g.size().y * 0.5)),
        g,
        color,
    );
}

pub fn text_right(ui: &Ui, painter: &Painter, rect: Rect, text: &str, font: FontId, color: Color32) {
    let g = galley(ui, text, font, color, rect.width());
    painter.galley(
        snap_point(ui.ctx(), Pos2::new(rect.right() - g.size().x, rect.center().y - g.size().y * 0.5)),
        g,
        color,
    );
}

pub fn icon_at(painter: &Painter, center: Pos2, code: &str, size: f32, color: Color32) {
    painter.text(snap_point(painter.ctx(), center), Align2::CENTER_CENTER, code, theme::icon(size), color);
}

pub fn hover_t(ctx: &egui::Context, id: egui::Id, hovered: bool) -> f32 {
    ctx.animate_bool_with_time(id, hovered, 0.13)
}

pub fn stroke_rect(painter: &Painter, rect: Rect, radius: u8, stroke: Stroke) {
    painter.rect_stroke(rect, CornerRadius::same(radius), stroke, StrokeKind::Inside);
}

pub fn separator_v(painter: &Painter, x: f32, center_y: f32, height: f32, color: Color32) {
    painter.line_segment(
        [Pos2::new(x, center_y - height * 0.5), Pos2::new(x, center_y + height * 0.5)],
        Stroke::new(1.0, color),
    );
}

pub fn ellipsis_size(ui: &Ui, text: &str, font: FontId) -> Vec2 {
    let g = galley(ui, text, font, Color32::WHITE, f32::INFINITY);
    g.size()
}

pub fn fade(color: Color32, t: f32) -> Color32 {
    let a = (color.a() as f32 * t.clamp(0.0, 1.0)) as u8;
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}
/// Vertical two-colour gradient covering `rect`.
pub fn vertical_gradient(painter: &Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(egui::Shape::mesh(mesh));
}

/// Soft radial glow used as a subtle brand accent in the chrome.
pub fn radial_glow(painter: &Painter, center: Pos2, radius: f32, color: Color32) {
    if color.a() == 0 {
        return;
    }
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, color);
    let segments = 24;
    for i in 0..=segments {
        let angle = i as f32 / segments as f32 * std::f32::consts::TAU;
        let p = center + Vec2::new(angle.cos(), angle.sin()) * radius;
        mesh.colored_vertex(p, Color32::TRANSPARENT);
    }
    for i in 0..segments {
        mesh.add_triangle(0, 1 + i, 2 + i);
    }
    painter.add(egui::Shape::mesh(mesh));
}
