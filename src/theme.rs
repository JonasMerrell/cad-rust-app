//! Colours and egui styling. Two palettes: a light one modelled on Onshape
//! (the default) and the original dark one from cad-app.html's CSS variables.
//! Colours are read through functions so the whole UI follows a theme switch.

use std::sync::atomic::{AtomicBool, Ordering};

use egui::{Color32, CornerRadius, Stroke, Visuals};

pub const fn hex(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

const fn rgba(c: u32, a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(((c >> 16) as u8 as u32 * a as u32 / 255) as u8, ((c >> 8) as u8 as u32 * a as u32 / 255) as u8, (c as u8 as u32 * a as u32 / 255) as u8, a)
}

static DARK: AtomicBool = AtomicBool::new(false);

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

pub struct Palette {
    pub bg: Color32,
    pub panel: Color32,
    pub panel_2: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub ok_green: Color32,
    pub error: Color32,
    pub error_soft: Color32,
    pub finish: Color32,
    pub active_text: Color32,
    pub toast: Color32,
    pub toast_err: Color32,
    pub toast_err_border: Color32,
    /// Top header strip (document name, menus).
    pub header: Color32,
    /// Bottom tab bar background and inactive tabs.
    pub tabbar: Color32,
    /// Floating dialogs over the viewport.
    pub window: Color32,
    /// Background behind text labels drawn in the viewport.
    pub label_bg: Color32,
    pub sel_fill: Color32,
    pub active_fill: Color32,
    pub active_stroke: Color32,
    /// Feature tree rollback bar.
    pub rollbar: Color32,
}

pub struct ViewPalette {
    pub background: Color32,
    /// Bottom of the background gradient.
    pub background_2: Color32,
    pub body: Color32,
    pub edge: Color32,
    pub hover: Color32,
    pub picked: Color32,
    pub plane: Color32,
    pub sketch: Color32,
    pub construction: Color32,
    pub geometry: Color32,
    pub under_defined: Color32,
    pub preview: Color32,
    pub snap: Color32,
    pub fixed: Color32,
    pub dim: Color32,
    pub grid_minor: Color32,
    pub grid_major: Color32,
    pub axis_x: Color32,
    pub axis_y: Color32,
    pub axis_z: Color32,
}

const DARK_PAL: Palette = Palette {
    bg: hex(0x15181c),
    panel: hex(0x1f242b),
    panel_2: hex(0x262c34),
    border: hex(0x323a44),
    text: hex(0xdfe3e8),
    muted: hex(0x8a94a1),
    accent: hex(0x4ea1ff),
    ok_green: hex(0x6fcf86),
    error: hex(0xff7a7a),
    error_soft: hex(0xff8a8a),
    finish: hex(0x2f7d4a),
    active_text: hex(0xcfe5ff),
    toast: hex(0x2a3442),
    toast_err: hex(0x4a2626),
    toast_err_border: hex(0x7a3a3a),
    header: hex(0x181c21),
    tabbar: hex(0x15181c),
    window: rgba(0x1f242b, 0xf7),
    label_bg: rgba(0x15181c, 0xd9),
    sel_fill: rgba(0x4ea1ff, 0x33),
    active_fill: rgba(0x4ea1ff, 0x2e),
    active_stroke: rgba(0x4ea1ff, 0x80),
    rollbar: hex(0x5a6573),
};

const LIGHT_PAL: Palette = Palette {
    bg: hex(0xffffff),
    panel: hex(0xffffff),
    panel_2: hex(0xebeef2),
    border: hex(0xd6dadf),
    text: hex(0x24292f),
    muted: hex(0x6a737d),
    accent: hex(0x1a73d9),
    ok_green: hex(0x23884a),
    error: hex(0xd03a3a),
    error_soft: hex(0xc8413f),
    finish: hex(0x2f8a50),
    active_text: hex(0x0f5bb5),
    toast: hex(0xffffff),
    toast_err: hex(0xfdecec),
    toast_err_border: hex(0xe9a3a3),
    header: hex(0xf3f4f6),
    tabbar: hex(0xe4e6e9),
    window: rgba(0xffffff, 0xf7),
    label_bg: rgba(0xffffff, 0xe6),
    sel_fill: rgba(0x1a73d9, 0x26),
    active_fill: rgba(0x1a73d9, 0x1f),
    active_stroke: rgba(0x1a73d9, 0x80),
    rollbar: hex(0xb4b9bf),
};

const DARK_VP: ViewPalette = ViewPalette {
    background: hex(0x20252c),
    background_2: hex(0x1a1e24),
    body: hex(0x9aa9ba),
    edge: hex(0x1c2026),
    hover: hex(0xffb547),
    // selection stands apart from blue sketch geometry and the amber hover
    picked: hex(0xff6a3d),
    plane: hex(0x6f8fb3),
    sketch: hex(0x56b6ff),
    construction: hex(0x7d8a99),
    geometry: hex(0xe8ecf0),
    under_defined: hex(0x8cc4ff),
    preview: hex(0x7fd3c8),
    snap: hex(0xffe066),
    fixed: hex(0xff7a7a),
    dim: hex(0xffd27f),
    grid_minor: hex(0x2b313a),
    grid_major: hex(0x39414d),
    axis_x: hex(0xe05555),
    axis_y: hex(0x55c26a),
    axis_z: hex(0x5588ee),
};

const LIGHT_VP: ViewPalette = ViewPalette {
    background: hex(0xffffff),
    background_2: hex(0xf1f4f8),
    body: hex(0x7a90a3),
    edge: hex(0x1b2630),
    hover: hex(0xf3a73b),
    picked: hex(0xe8451c),
    plane: hex(0x8fb2d9),
    sketch: hex(0x1a73d9),
    construction: hex(0x8a96a3),
    geometry: hex(0x1d2733),
    under_defined: hex(0x2f86e8),
    preview: hex(0x139c8f),
    snap: hex(0xd99a00),
    fixed: hex(0xd64545),
    dim: hex(0xb36b00),
    grid_minor: hex(0xedf0f4),
    grid_major: hex(0xd8dee6),
    axis_x: hex(0xd23c3c),
    axis_y: hex(0x2f9e4a),
    axis_z: hex(0x2f6fdc),
};

pub fn pal() -> &'static Palette {
    if is_dark() { &DARK_PAL } else { &LIGHT_PAL }
}

pub fn vpal() -> &'static ViewPalette {
    if is_dark() { &DARK_VP } else { &LIGHT_VP }
}

macro_rules! getters {
    ($src:ident: $($f:ident),*) => { $(pub fn $f() -> Color32 { $src().$f })* };
}
getters!(pal: bg, panel, panel_2, border, text, muted, accent, ok_green, error, error_soft, finish, active_text, toast, toast_err, toast_err_border, header, tabbar, window, label_bg, sel_fill, active_fill, active_stroke, rollbar);

/// Viewport palette.
pub mod vp {
    use super::vpal;
    use egui::Color32;
    getters!(vpal: background, background_2, body, edge, hover, picked, plane, sketch, construction, geometry, under_defined, preview, snap, fixed, dim, grid_minor, grid_major, axis_x, axis_y, axis_z);
}

pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

pub fn to_rgba(c: Color32, a: f32) -> [f32; 4] {
    [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, a]
}

/// Switch between the light and dark palettes and restyle egui.
pub fn set_dark(ctx: &egui::Context, dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
    apply(ctx);
}

pub fn apply(ctx: &egui::Context) {
    let p = pal();
    let dark = is_dark();
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.override_text_color = Some(p.text);
    v.panel_fill = p.panel;
    v.window_fill = p.window;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(7);
    v.extreme_bg_color = if dark { p.bg } else { hex(0xffffff) }; // text edits / combo boxes
    v.text_edit_bg_color = Some(if dark { p.bg } else { hex(0xffffff) });
    v.faint_bg_color = p.panel_2;
    v.selection.bg_fill = if dark { rgba(0x4ea1ff, 0x40) } else { rgba(0x1a73d9, 0x40) };
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;
    if !dark {
        v.window_shadow = egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(40) };
        v.popup_shadow = egui::Shadow { offset: [0, 3], blur: 10, spread: 0, color: Color32::from_black_alpha(36) };
    }

    let r = CornerRadius::same(4);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.bg_fill = if dark { p.bg } else { hex(0xf3f4f6) };
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;
    w.hovered.bg_fill = p.panel_2;
    w.hovered.weak_bg_fill = p.panel_2;
    w.hovered.bg_stroke = Stroke::NONE;
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;
    w.active.bg_fill = p.panel_2;
    w.active.weak_bg_fill = p.panel_2;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;
    w.open.bg_fill = if dark { p.bg } else { hex(0xffffff) };
    w.open.weak_bg_fill = w.open.bg_fill;
    w.open.bg_stroke = Stroke::new(1.0, p.accent);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = CornerRadius::same(4);
    let theme = if dark { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_theme(if dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
    ctx.set_visuals_of(theme, v);

    ctx.style_mut_of(theme, |s| {
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(6.0, 4.0);
        s.spacing.interact_size.y = 24.0;
        use egui::{FontId, TextStyle};
        s.text_styles.insert(TextStyle::Body, FontId::proportional(13.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(12.5));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(11.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(12.0));
    });
}
