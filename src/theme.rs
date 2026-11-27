//! Design system: Fluent-inspired tokens, typography and egui style.

use std::sync::Arc;

use egui::epaint::text::{HintingTarget, SmoothHinting, VariationCoords};
use egui::{
    Color32, FontData, FontDefinitions, FontFamily, FontId, FontTweak, Stroke, TextStyle, Theme as EguiTheme,
    ThemePreference, Vec2,
};

pub const FONT_UI: &str = "ui";
pub const FONT_SEMIBOLD: &str = "ui-semibold";
pub const FONT_BOLD: &str = "ui-bold";
pub const FONT_ICONS: &str = "icons";
pub const FONT_MONO: &str = "mono";

// ---- metrics ---------------------------------------------------------------

pub const TITLEBAR_H: f32 = 40.0;
pub const TAB_H: f32 = 34.0;
pub const COMMAND_H: f32 = 52.0;
pub const STATUS_H: f32 = 28.0;
pub const NAV_W: f32 = 252.0;
pub const PANE_GAP: f32 = 8.0;
pub const RADIUS_CARD: u8 = 8;
pub const RADIUS_CTRL: u8 = 6;
pub const RADIUS_ROW: u8 = 4;

// ---- typography helpers ----------------------------------------------------

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FONT_SEMIBOLD.into()))
}
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FONT_BOLD.into()))
}
pub fn icon(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FONT_ICONS.into()))
}
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

// ---- colors ----------------------------------------------------------------

const WCAG_AA_TEXT: f32 = 4.5;
const DARK_INK: Color32 = Color32::from_rgb(12, 16, 20);
const LIGHT_INK: Color32 = Color32::WHITE;

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

fn lighten(c: Color32, t: f32) -> Color32 {
    mix(c, Color32::WHITE, t)
}

fn srgb_to_linear(channel: u8) -> f32 {
    let c = channel as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Relative luminance per WCAG 2.1 (sRGB linearized), in `[0.0, 1.0]`.
pub fn luminance(c: Color32) -> f32 {
    0.2126 * srgb_to_linear(c.r()) + 0.7152 * srgb_to_linear(c.g()) + 0.0722 * srgb_to_linear(c.b())
}

/// Contrast ratio per WCAG 2.1, in `[1.0, 21.0]`.
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let la = luminance(a);
    let lb = luminance(b);
    let (l1, l2) = if la >= lb { (la, lb) } else { (lb, la) };
    (l1 + 0.05) / (l2 + 0.05)
}

/// Blends `color` minimally toward `target` until `contrast_ratio(result, against) >= min_ratio`.
fn adjust_toward_contrast(color: Color32, target: Color32, against: Color32, min_ratio: f32) -> Color32 {
    if contrast_ratio(color, against) >= min_ratio {
        return color;
    }
    if contrast_ratio(target, against) < min_ratio {
        return target;
    }
    let mut lo = 0.0f32;
    let mut hi = 1.0f32;
    for _ in 0..18 {
        let mid = (lo + hi) * 0.5;
        if contrast_ratio(mix(color, target, mid), against) >= min_ratio {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let candidate = mix(color, target, hi);
    if contrast_ratio(candidate, against) >= min_ratio { candidate } else { target }
}

/// Picks the higher-contrast foreground ink (`WHITE` or `DARK_INK`) for `base`,
/// and if even that ink is below WCAG AA (4.5:1), adaptively darkens or lightens
/// `base` until `contrast_ratio(text_on_accent, accent) >= 4.5`.
fn resolve_accent_pair(base: Color32) -> (Color32, Color32) {
    let light_cr = contrast_ratio(LIGHT_INK, base);
    let dark_cr = contrast_ratio(DARK_INK, base);
    if light_cr >= dark_cr {
        let accent = adjust_toward_contrast(base, Color32::BLACK, LIGHT_INK, WCAG_AA_TEXT);
        (accent, LIGHT_INK)
    } else {
        let accent = adjust_toward_contrast(base, Color32::WHITE, DARK_INK, WCAG_AA_TEXT);
        (accent, DARK_INK)
    }
}

fn accent_from_abgr(abgr: u32) -> Color32 {
    Color32::from_rgb((abgr & 0xFF) as u8, ((abgr >> 8) & 0xFF) as u8, ((abgr >> 16) & 0xFF) as u8)
}

#[derive(Clone)]
pub struct Theme {
    pub dark: bool,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub accent_strong: Color32,
    /// Accent color adjusted to meet WCAG AA (>= 4.5:1) when rendered as text on surfaces.
    pub accent_text: Color32,
    pub text_on_accent: Color32,

    /// Content surface (file list card).
    pub surface: Color32,
    pub surface_alt: Color32,
    pub surface_raised: Color32,
    pub popup: Color32,

    /// Frosted backing tokens for Windows 11 DWM Mica mode so chrome, sidebar,
    /// status bar and subtle dividers stay legible over high-contrast wallpapers.
    pub mica_tint: Color32,
    pub mica_chrome_bg: Color32,
    pub mica_sidebar_bg: Color32,
    pub mica_status_bg: Color32,
    pub mica_subtle_border: Color32,

    pub text: Color32,
    pub text_secondary: Color32,
    pub text_tertiary: Color32,
    pub text_disabled: Color32,

    pub hover: Color32,
    pub hover_strong: Color32,
    pub pressed: Color32,
    pub selected: Color32,
    pub selected_hover: Color32,
    pub selected_bar: Color32,

    pub border: Color32,
    pub border_strong: Color32,
    pub divider: Color32,
    pub danger: Color32,
    pub scrollbar_hover: Color32,
    pub shadow: Color32,
    pub bg_top: Color32,
    pub bg_bottom: Color32,
    pub glow: Color32,
}

impl Theme {
    pub fn new(dark: bool, accent_abgr: u32) -> Self {
        let raw = accent_from_abgr(accent_abgr);
        if dark {
            let surface = Color32::from_rgb(0x20, 0x20, 0x20);
            let surface_alt = Color32::from_rgb(0x1B, 0x1B, 0x1B);
            let surface_raised = Color32::from_rgb(0x2A, 0x2A, 0x2A);
            let lifted = if luminance(raw) < 0.22 { lighten(raw, 0.42) } else { raw };
            let (accent, text_on_accent) = resolve_accent_pair(lifted);
            let accent_text = adjust_toward_contrast(accent, Color32::WHITE, surface_raised, WCAG_AA_TEXT);
            Self {
                dark,
                accent,
                accent_soft: Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 38),
                accent_strong: lighten(accent, 0.18),
                accent_text,
                text_on_accent,
                surface,
                surface_alt,
                surface_raised,
                popup: Color32::from_rgb(0x2C, 0x2C, 0x2C),
                mica_tint: Color32::from_rgba_unmultiplied(24, 25, 30, 175),
                mica_chrome_bg: Color32::from_rgba_unmultiplied(28, 29, 34, 188),
                mica_sidebar_bg: Color32::from_rgba_unmultiplied(30, 31, 36, 198),
                mica_status_bg: Color32::from_rgba_unmultiplied(26, 27, 32, 192),
                mica_subtle_border: Color32::from_rgba_unmultiplied(255, 255, 255, 34),
                text: Color32::from_rgb(0xF2, 0xF2, 0xF2),
                text_secondary: Color32::from_rgb(0xC2, 0xC2, 0xC2),
                text_tertiary: Color32::from_rgb(0x9A, 0x9A, 0x9A),
                text_disabled: Color32::from_rgb(0x67, 0x67, 0x67),
                hover: Color32::from_rgba_unmultiplied(255, 255, 255, 14),
                hover_strong: Color32::from_rgba_unmultiplied(255, 255, 255, 24),
                pressed: Color32::from_rgba_unmultiplied(255, 255, 255, 10),
                selected: Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 54),
                selected_hover: Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 66),
                selected_bar: accent,
                border: Color32::from_rgba_unmultiplied(255, 255, 255, 22),
                border_strong: Color32::from_rgba_unmultiplied(255, 255, 255, 38),
                divider: Color32::from_rgba_unmultiplied(255, 255, 255, 16),
                danger: Color32::from_rgb(0xFF, 0x99, 0xA4),
                scrollbar_hover: Color32::from_rgba_unmultiplied(255, 255, 255, 110),
                shadow: Color32::from_rgba_unmultiplied(0, 0, 0, 120),
                bg_top: Color32::from_rgb(0x22, 0x23, 0x28),
                bg_bottom: Color32::from_rgb(0x18, 0x19, 0x1D),
                glow: Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 26),
            }
        } else {
            let surface = Color32::from_rgb(0xFA, 0xFA, 0xFA);
            let surface_alt = Color32::from_rgb(0xF3, 0xF3, 0xF3);
            let surface_raised = Color32::WHITE;
            let bg_bottom = Color32::from_rgb(0xEE, 0xEE, 0xF2);
            let base = if luminance(raw) > 0.75 { mix(raw, Color32::BLACK, 0.25) } else { raw };
            let (accent, text_on_accent) = resolve_accent_pair(base);
            let accent_text = adjust_toward_contrast(accent, Color32::BLACK, bg_bottom, WCAG_AA_TEXT);
            Self {
                dark,
                accent,
                accent_soft: Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 26),
                accent_strong: mix(accent, Color32::BLACK, 0.12),
                accent_text,
                text_on_accent,
                surface,
                surface_alt,
                surface_raised,
                popup: Color32::from_rgb(0xF9, 0xF9, 0xF9),
                mica_tint: Color32::from_rgba_unmultiplied(246, 247, 250, 185),
                mica_chrome_bg: Color32::from_rgba_unmultiplied(248, 249, 252, 198),
                mica_sidebar_bg: Color32::from_rgba_unmultiplied(250, 251, 253, 210),
                mica_status_bg: Color32::from_rgba_unmultiplied(246, 247, 250, 200),
                mica_subtle_border: Color32::from_rgba_unmultiplied(0, 0, 0, 34),
                text: Color32::from_rgb(0x1A, 0x1A, 0x1A),
                text_secondary: Color32::from_rgb(0x52, 0x52, 0x52),
                text_tertiary: Color32::from_rgb(0x6E, 0x6E, 0x6E),
                text_disabled: Color32::from_rgb(0x9E, 0x9E, 0x9E),
                hover: Color32::from_rgba_unmultiplied(0, 0, 0, 10),
                hover_strong: Color32::from_rgba_unmultiplied(0, 0, 0, 18),
                pressed: Color32::from_rgba_unmultiplied(0, 0, 0, 8),
                selected: Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 30),
                selected_hover: Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 44),
                selected_bar: accent,
                border: Color32::from_rgba_unmultiplied(0, 0, 0, 24),
                border_strong: Color32::from_rgba_unmultiplied(0, 0, 0, 45),
                divider: Color32::from_rgba_unmultiplied(0, 0, 0, 14),
                danger: Color32::from_rgb(0xC4, 0x2B, 0x1C),
                scrollbar_hover: Color32::from_rgba_unmultiplied(0, 0, 0, 100),
                shadow: Color32::from_rgba_unmultiplied(0, 0, 0, 70),
                bg_top: Color32::from_rgb(0xF7, 0xF7, 0xF9),
                bg_bottom,
                glow: Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 18),
            }
        }
    }
}

// ---- font installation -----------------------------------------------------

fn font_dir() -> String {
    let root = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    format!("{root}\\Fonts\\")
}

/// Maps a system font read-only and hands out its bytes for the process lifetime.
///
/// System fonts are large (Microsoft YaHei alone is 19MB + 16MB bold); reading them
/// into the heap would add ~36MB of private pages that never get trimmed. A mapping
/// shares the OS file cache with every other process using the font and can be
/// paged out under pressure. Handle and mapping are intentionally leaked: epaint
/// keeps the slice alive for the whole run, so unmapping is never correct here.
fn map_font(name: &str) -> Option<&'static [u8]> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_SHARE_READ, OPEN_EXISTING,
    };
    use windows::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_READ, MapViewOfFile, PAGE_READONLY};
    use windows::core::PCWSTR;

    let path = format!("{}{}", font_dir(), name);
    let wide: Vec<u16> = std::ffi::OsStr::new(&path).encode_wide().chain(Some(0)).collect();
    unsafe {
        let file = CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .ok()?;
        let mapping = match CreateFileMappingW(file, None, PAGE_READONLY, 0, 0, PCWSTR::null()) {
            Ok(mapping) => mapping,
            Err(_) => {
                let _ = CloseHandle(file);
                return None;
            }
        };
        let length = std::fs::metadata(&path).ok()?.len() as usize;
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
        let pointer = view.Value as *const u8;
        if pointer.is_null() {
            let _ = CloseHandle(mapping);
            let _ = CloseHandle(file);
            return None;
        }
        // The file handle can go: the mapping keeps the pages alive.
        let _ = CloseHandle(file);
        Some(std::slice::from_raw_parts(pointer, length))
    }
}

fn read_font(name: &str) -> Option<&'static [u8]> {
    map_font(name).or_else(|| {
        let bytes = std::fs::read(format!("{}{}", font_dir(), name)).ok()?;
        Some(&*Box::leak(bytes.into_boxed_slice()))
    })
}

/// Microsoft YaHei ships as a TTC; face 1 is the UI variant with the tighter
/// vertical metrics Windows itself uses for Chinese UI text.
fn cjk_face_index(bytes: &[u8]) -> u32 {
    if bytes.starts_with(b"ttcf") && bytes.len() >= 12 {
        let count = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        (count >= 2) as u32
    } else {
        0
    }
}

/// Crisp-text baseline for every face: no fractional-x binning and horizontal
/// stem grid-fitting, so hinted strokes land on the pixel grid at any DPI.
fn base_tweak() -> FontTweak {
    FontTweak {
        subpixel_binning: Some(false),
        hinting_target: HintingTarget::Smooth(SmoothHinting { preserve_linear_metrics: false, ..Default::default() }),
        ..Default::default()
    }
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    // One shared blob per font file: the variable font is registered under three
    // family names with different `wght` coordinates, and copying its bytes per
    // weight would keep ~30MB of identical data resident.
    let push = |fonts: &mut FontDefinitions, key: &str, bytes: &'static [u8], weight: Option<f32>| {
        let mut data = FontData::from_static(bytes);
        data.index = if key.starts_with("cjk") { cjk_face_index(data.bytes()) } else { 0 };
        data.tweak = base_tweak();
        if let Some(weight) = weight {
            data.tweak.coords = VariationCoords::new([(b"wght", weight)]);
        }
        fonts.font_data.insert(key.to_owned(), Arc::new(data));
    };

    let variable = read_font("SegUIVar.ttf");
    match &variable {
        Some(bytes) => {
            push(&mut fonts, FONT_UI, bytes, Some(400.0));
            push(&mut fonts, FONT_SEMIBOLD, bytes, Some(600.0));
            push(&mut fonts, FONT_BOLD, bytes, Some(700.0));
        }
        None => {
            if let Some(bytes) = read_font("segoeui.ttf") {
                push(&mut fonts, FONT_UI, bytes, None);
            }
            if let Some(bytes) = read_font("segoeuib.ttf") {
                push(&mut fonts, FONT_SEMIBOLD, bytes, None);
                push(&mut fonts, FONT_BOLD, bytes, None);
            }
        }
    }
    if let Some(bytes) = read_font("SegoeIcons.ttf").or_else(|| read_font("segmdl2.ttf")) {
        push(&mut fonts, FONT_ICONS, bytes, None);
    }
    if let Some(bytes) = read_font("CascadiaMono.ttf").or_else(|| read_font("consola.ttf")) {
        push(&mut fonts, FONT_MONO, bytes, None);
    }

    // CJK: Microsoft YaHei matches Segoe UI stroke weight and has real bold
    // glyphs; DengXian reads thin in UI chrome.
    let cjk = read_font("msyh.ttc").or_else(|| read_font("msyh.ttf")).or_else(|| read_font("Deng.ttf"));
    let cjk_bold = read_font("msyhbd.ttc").or_else(|| read_font("msyhbd.ttf")).or_else(|| read_font("Dengb.ttf"));
    if let Some(bytes) = cjk {
        push(&mut fonts, "cjk", bytes, None);
    }
    if let Some(bytes) = cjk_bold {
        push(&mut fonts, "cjk-bold", bytes, None);
    }

    let mut latin = Vec::new();
    if fonts.font_data.contains_key(FONT_UI) {
        latin.push(FONT_UI.to_owned());
    }
    if fonts.font_data.contains_key("cjk") {
        latin.push("cjk".to_owned());
    }
    if latin.is_empty() {
        latin.push("Ubuntu-Light".to_owned());
    }

    let bold_cjk = fonts
        .font_data
        .contains_key("cjk-bold")
        .then(|| "cjk-bold".to_owned())
        .or_else(|| fonts.font_data.contains_key("cjk").then(|| "cjk".to_owned()));

    let mut bold_family = Vec::new();
    if fonts.font_data.contains_key(FONT_BOLD) {
        bold_family.push(FONT_BOLD.to_owned());
    }
    if let Some(face) = &bold_cjk {
        bold_family.push(face.clone());
    }
    if bold_family.is_empty() {
        bold_family = latin.clone();
    }

    let mut semibold_family = Vec::new();
    if fonts.font_data.contains_key(FONT_SEMIBOLD) {
        semibold_family.push(FONT_SEMIBOLD.to_owned());
    }
    if let Some(face) = &bold_cjk {
        semibold_family.push(face.clone());
    }
    if semibold_family.is_empty() {
        semibold_family = bold_family.clone();
    }

    fonts.families.insert(FontFamily::Name(FONT_SEMIBOLD.into()), semibold_family);
    fonts.families.insert(FontFamily::Name(FONT_BOLD.into()), bold_family);
    if fonts.font_data.contains_key(FONT_ICONS) {
        fonts.families.insert(FontFamily::Name(FONT_ICONS.into()), vec![FONT_ICONS.to_owned()]);
    } else {
        fonts.families.insert(FontFamily::Name(FONT_ICONS.into()), latin.clone());
    }
    fonts.families.insert(FontFamily::Proportional, latin.clone());

    if fonts.font_data.contains_key(FONT_MONO) {
        let mut mono_family = vec![FONT_MONO.to_owned()];
        if fonts.font_data.contains_key("cjk") {
            mono_family.push("cjk".to_owned());
        }
        fonts.families.insert(FontFamily::Monospace, mono_family);
    } else if fonts.font_data.contains_key("cjk") {
        fonts.families.entry(FontFamily::Monospace).or_default().push("cjk".to_owned());
    }

    ctx.set_fonts(fonts);
}

// ---- egui style ------------------------------------------------------------

pub fn apply_style(ctx: &egui::Context, theme: &Theme) {
    ctx.set_theme(if theme.dark { ThemePreference::Dark } else { ThemePreference::Light });

    let kind = if theme.dark { EguiTheme::Dark } else { EguiTheme::Light };
    let mut style = (*ctx.style_of(kind)).clone();
    let body = font(13.0);
    style.text_styles = [
        (TextStyle::Small, font(11.0)),
        (TextStyle::Body, body),
        (TextStyle::Button, font(13.0)),
        (TextStyle::Heading, semibold(15.0)),
        (TextStyle::Monospace, mono(12.5)),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(10.0, 5.0);
    style.spacing.interact_size = Vec2::new(28.0, 28.0);
    style.spacing.icon_width = 16.0;
    style.spacing.scroll.bar_width = 9.0;
    style.spacing.scroll.bar_inner_margin = 2.0;
    style.spacing.scroll.floating = true;

    let mut visuals = if theme.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    visuals.override_text_color = Some(theme.text);
    visuals.panel_fill = Color32::TRANSPARENT;
    visuals.window_fill = theme.popup;
    visuals.window_stroke = Stroke::new(1.0, theme.border);
    visuals.window_corner_radius = egui::CornerRadius::same(RADIUS_CARD);
    visuals.menu_corner_radius = egui::CornerRadius::same(RADIUS_CARD);
    visuals.popup_shadow = egui::epaint::Shadow { offset: [0, 4], blur: 18, spread: 0, color: theme.shadow };
    visuals.window_shadow = visuals.popup_shadow;
    visuals.extreme_bg_color = theme.surface_alt;
    visuals.faint_bg_color = theme.hover;
    visuals.selection.bg_fill = theme.selected;
    visuals.selection.stroke = Stroke::new(1.0, theme.accent);
    visuals.hyperlink_color = theme.accent_text;
    visuals.text_cursor.stroke = Stroke::new(1.5, theme.text);
    visuals.widgets.noninteractive.bg_fill = theme.surface;
    visuals.widgets.noninteractive.weak_bg_fill = theme.surface;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, theme.border);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, theme.text_secondary);
    visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(RADIUS_CTRL);
    visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.weak_bg_fill = theme.hover;
    visuals.widgets.inactive.bg_stroke = Stroke::NONE;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, theme.text);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(RADIUS_CTRL);
    visuals.widgets.hovered.bg_fill = theme.hover_strong;
    visuals.widgets.hovered.weak_bg_fill = theme.hover_strong;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, theme.border);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, theme.text);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(RADIUS_CTRL);
    visuals.widgets.active.bg_fill = theme.pressed;
    visuals.widgets.active.weak_bg_fill = theme.pressed;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, theme.border);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, theme.text);
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(RADIUS_CTRL);
    visuals.widgets.open.bg_fill = theme.hover;
    visuals.widgets.open.weak_bg_fill = theme.hover;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, theme.border);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, theme.text);
    visuals.widgets.open.corner_radius = egui::CornerRadius::same(RADIUS_CTRL);
    visuals.striped = true;

    style.visuals = visuals;
    ctx.set_style_of(kind, style);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abgr(r: u8, g: u8, b: u8) -> u32 {
        (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
    }

    #[test]
    fn wcag_luminance_and_contrast_match_spec() {
        assert!((luminance(Color32::BLACK) - 0.0).abs() < 1e-6);
        assert!((luminance(Color32::WHITE) - 1.0).abs() < 1e-6);
        assert!((contrast_ratio(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 1e-3);
        assert!((contrast_ratio(Color32::WHITE, Color32::BLACK) - 21.0).abs() < 1e-3);
        assert!((contrast_ratio(Color32::from_rgb(0x00, 0xCC, 0x6A), Color32::WHITE) - 2.13).abs() < 0.05);
    }

    #[test]
    fn theme_accents_meet_wcag_aa_in_light_and_dark() {
        let accents = [
            ("win_blue", abgr(0x00, 0x78, 0xD4)),
            ("mint", abgr(0x00, 0xCC, 0x6A)),
            ("gold", abgr(0xFF, 0xB9, 0x00)),
            ("mid_gray", abgr(0x77, 0x77, 0x77)),
            ("mid_orange", abgr(0xD9, 0x6B, 0x27)),
            ("dark_red", abgr(0xA8, 0x00, 0x00)),
            ("pure_black", abgr(0x00, 0x00, 0x00)),
            ("pure_white", abgr(0xFF, 0xFF, 0xFF)),
        ];
        for dark in [false, true] {
            for (name, color) in accents {
                let t = Theme::new(dark, color);
                let on_accent = contrast_ratio(t.text_on_accent, t.accent);
                assert!(
                    on_accent >= WCAG_AA_TEXT,
                    "dark={dark} accent={name}: text_on_accent contrast {on_accent:.2} < 4.5"
                );
                let on_surface = contrast_ratio(t.accent_text, t.surface);
                assert!(
                    on_surface >= WCAG_AA_TEXT,
                    "dark={dark} accent={name}: accent_text on surface contrast {on_surface:.2} < 4.5"
                );
                let on_alt = contrast_ratio(t.accent_text, t.surface_alt);
                assert!(
                    on_alt >= WCAG_AA_TEXT,
                    "dark={dark} accent={name}: accent_text on surface_alt contrast {on_alt:.2} < 4.5"
                );
                assert!(t.mica_chrome_bg.a() > 160 && t.mica_sidebar_bg.a() > 180 && t.mica_status_bg.a() > 170);
                assert!(t.mica_subtle_border.a() >= 30);
            }
        }
    }

    #[test]
    fn install_fonts_registers_icons_and_monospace_cjk_fallback() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let mut output = ctx.run_pass(egui::RawInput::default(), |ctx| {
            let families = ctx.fonts(|fonts| {
                let mono = fonts.families().into_iter().any(|f| f == FontFamily::Monospace);
                let icons = fonts.families().into_iter().any(|f| f == FontFamily::Name(FONT_ICONS.into()));
                (mono, icons)
            });
            assert!(families.0, "Monospace family must be registered");
            assert!(families.1, "Icons family must be registered");
        });
        output.textures_delta.clear();
    }
}
