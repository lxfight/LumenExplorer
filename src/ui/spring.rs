//! Tailored motion choreography system (springs + cubic ease-out reveals).
//!
//! Different UI elements obey different physical metaphors rather than sharing
//! one monotonous bounce:
//! - `JELLY`: tactile micro-controls (toggle switches, toast bubbles) — lively Q弹 overshoot
//! - `MAGNETIC`: sliding pills & selection bars — crisp magnetic snap with gentle overshoot
//! - `MODAL`: large overlays (Quick Look, Settings) — poised float with subtle settle
//! - `PANEL`: structural panes & tabs (Details, Split View, Tab Strip) — critically damped, 0% overshoot
//! - `SCROLL`: inertial file-list scroll — smooth critically damped deceleration, 0% rebound
//! - `ease_out_reveal`: menus & folder entry — crisp WinUI 3 cubic ease-out unfold, 0% wobble
//!
//! Settled animations snap cleanly to rest and clear the activity flag so idle CPU stays 0%.

use std::sync::atomic::{AtomicBool, Ordering};

use egui::{Color32, Pos2, Rect, Vec2};

#[derive(Clone, Copy, Debug)]
pub struct SpringConfig {
    /// Spring stiffness `k`.
    pub stiffness: f32,
    /// Viscous damping `c`. Critically damped at `c = 2 * sqrt(k)`.
    pub damping: f32,
}

/// Playful Q弹 bounce (ζ ≈ 0.52, ~15% overshoot): toggle switches & toast entrance.
pub const JELLY: SpringConfig = SpringConfig { stiffness: 410.0, damping: 21.0 };
/// Crisp magnetic snap (ζ ≈ 0.66, ~6.5% overshoot): sliding pills, nav rail, button press.
pub const MAGNETIC: SpringConfig = SpringConfig { stiffness: 520.0, damping: 30.0 };
/// Poised modal lift (ζ ≈ 0.76, ~2.5% gentle settle): Quick Look & Settings modals.
pub const MODAL: SpringConfig = SpringConfig { stiffness: 460.0, damping: 32.5 };
/// Architectural critically-damped glide (ζ ≈ 1.00, 0% overshoot): panes & tab strip.
pub const PANEL: SpringConfig = SpringConfig { stiffness: 380.0, damping: 39.0 };
/// Silky damped inertial scroll (ζ ≈ 1.02, 0% rebound): virtualized file lists.
pub const SCROLL: SpringConfig = SpringConfig { stiffness: 260.0, damping: 33.0 };

static ENABLED: AtomicBool = AtomicBool::new(true);
static FRAME_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Default)]
struct SpringState {
    value: f32,
    velocity: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct RevealState {
    elapsed: f32,
}

/// Must be called once at the start of every `LumenApp::frame`.
pub fn begin_frame(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    FRAME_ACTIVE.store(false, Ordering::Relaxed);
}

/// True if any spring or reveal was still moving during this frame.
pub fn is_animating() -> bool {
    FRAME_ACTIVE.load(Ordering::Relaxed)
}

/// Steps a single spring `(value, velocity)` toward `target` over `dt` seconds.
/// Returns `true` if still in motion.
fn integrate(state: &mut SpringState, target: f32, dt: f32, cfg: SpringConfig, pos_eps: f32) -> bool {
    const MAX_SUBSTEP: f32 = 1.0 / 160.0;
    let mut rem = dt.clamp(1.0 / 240.0, 0.05);
    while rem > 0.0 {
        let step = rem.min(MAX_SUBSTEP);
        let accel = cfg.stiffness * (target - state.value) - cfg.damping * state.velocity;
        state.velocity += accel * step;
        state.value += state.velocity * step;
        rem -= step;
    }
    let vel_eps = (pos_eps * 15.0).max(0.02);
    if (state.value - target).abs() <= pos_eps && state.velocity.abs() <= vel_eps {
        state.value = target;
        state.velocity = 0.0;
        false
    } else {
        true
    }
}

/// Advances a named spring stored in egui temp memory and returns `(value, velocity)`.
pub fn animate_vel(
    ctx: &egui::Context,
    id: egui::Id,
    target: f32,
    initial: f32,
    cfg: SpringConfig,
    pos_eps: f32,
) -> (f32, f32) {
    if !ENABLED.load(Ordering::Relaxed) {
        ctx.data_mut(|d| d.insert_temp(id, SpringState { value: target, velocity: 0.0 }));
        return (target, 0.0);
    }
    let dt = ctx.input(|i| i.unstable_dt);
    let mut state =
        ctx.data_mut(|d| d.get_temp::<SpringState>(id).unwrap_or(SpringState { value: initial, velocity: 0.0 }));
    let moving = integrate(&mut state, target, dt, cfg, pos_eps);
    if moving {
        FRAME_ACTIVE.store(true, Ordering::Relaxed);
    }
    ctx.data_mut(|d| d.insert_temp(id, state));
    (state.value, state.velocity)
}

/// Advances a normalized (0..1) or pixel spring toward `target`.
pub fn animate(ctx: &egui::Context, id: egui::Id, target: f32, initial: f32, cfg: SpringConfig) -> f32 {
    let eps = if target.abs() > 8.0 || initial.abs() > 8.0 { 0.15 } else { 0.0015 };
    animate_vel(ctx, id, target, initial, cfg, eps).0
}

/// Smooth WinUI 3 cubic ease-out (`0.0 -> 1.0`, zero overshoot) for menus and view entry.
pub fn ease_out_reveal(ctx: &egui::Context, id: egui::Id, duration_secs: f32) -> f32 {
    if !ENABLED.load(Ordering::Relaxed) || duration_secs <= 0.0 {
        return 1.0;
    }
    let dt = ctx.input(|i| i.unstable_dt).clamp(1.0 / 240.0, 0.05);
    let mut state = ctx.data_mut(|d| d.get_temp::<RevealState>(id).unwrap_or(RevealState { elapsed: 0.0 }));
    if state.elapsed < duration_secs {
        state.elapsed = (state.elapsed + dt).min(duration_secs);
        if state.elapsed < duration_secs {
            FRAME_ACTIVE.store(true, Ordering::Relaxed);
        }
        ctx.data_mut(|d| d.insert_temp(id, state));
    }
    let u = (state.elapsed / duration_secs).clamp(0.0, 1.0);
    let inv = 1.0 - u;
    1.0 - inv * inv * inv
}

/// Locks a spring directly to `value` with zero velocity (used while dragging).
pub fn set_immediate(ctx: &egui::Context, id: egui::Id, value: f32) {
    ctx.data_mut(|d| d.insert_temp(id, SpringState { value, velocity: 0.0 }));
}

/// Clears cached spring or reveal state so the next open replays cleanly.
pub fn clear(ctx: &egui::Context, id: egui::Id) {
    ctx.data_mut(|d| {
        d.remove_temp::<SpringState>(id);
        d.remove_temp::<RevealState>(id);
    });
}

/// Scales `rect` around `origin` by `scale`.
pub fn scale_rect_around(rect: Rect, origin: Pos2, scale: f32) -> Rect {
    let min = origin + (rect.min - origin) * scale;
    let max = origin + (rect.max - origin) * scale;
    Rect::from_min_max(min, max)
}

/// Linearly interpolates between two `Color32`s with `t` clamped to `[0, 1]`.
pub fn mix_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let inv = 1.0 - t;
    Color32::from_rgba_unmultiplied(
        (a.r() as f32 * inv + b.r() as f32 * t).round() as u8,
        (a.g() as f32 * inv + b.g() as f32 * t).round() as u8,
        (a.b() as f32 * inv + b.b() as f32 * t).round() as u8,
        (a.a() as f32 * inv + b.a() as f32 * t).round() as u8,
    )
}

/// Helper to offset a rect vertically by `dy`.
pub fn slide_y(rect: Rect, dy: f32) -> Rect {
    rect.translate(Vec2::new(0.0, dy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jelly_overshoots_while_panel_stays_critically_damped() {
        let mut jelly = SpringState { value: 0.0, velocity: 0.0 };
        let mut jelly_peak = 0.0f32;
        let mut jelly_settled = false;
        for _ in 0..120 {
            let moving = integrate(&mut jelly, 1.0, 1.0 / 60.0, JELLY, 0.0015);
            jelly_peak = jelly_peak.max(jelly.value);
            if !moving {
                jelly_settled = true;
                break;
            }
        }
        assert!(jelly_peak > 1.08, "JELLY should visibly bounce (>1.08), got {jelly_peak}");
        assert!(jelly_settled);

        let mut panel = SpringState { value: 0.0, velocity: 0.0 };
        let mut panel_peak = 0.0f32;
        for _ in 0..120 {
            let moving = integrate(&mut panel, 1.0, 1.0 / 60.0, PANEL, 0.0015);
            panel_peak = panel_peak.max(panel.value);
            if !moving {
                break;
            }
        }
        assert!(panel_peak <= 1.005, "PANEL must stay critically damped with ~0% overshoot, got {panel_peak}");
    }

    #[test]
    fn disabled_mode_snaps_immediately_without_marking_active() {
        let ctx = egui::Context::default();
        begin_frame(false);
        let val = animate(&ctx, egui::Id::unique("test-snap"), 100.0, 0.0, JELLY);
        assert_eq!(val, 100.0);
        assert_eq!(ease_out_reveal(&ctx, egui::Id::unique("test-rev"), 0.12), 1.0);
        assert!(!is_animating());
    }
}
