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
    bg: hex(0x131518),
    panel: hex(0x1b1d21),
    panel_2: hex(0x26292e),
    border: hex(0x2c2f35),
    text: hex(0xe3e6ea),
    // at least 4.5:1 on the panels
    muted: hex(0x969da7),
    accent: hex(0x4c9bff),
    ok_green: hex(0x5cc47a),
    error: hex(0xff7a7a),
    error_soft: hex(0xff8a8a),
    finish: hex(0x2c8a4f),
    active_text: hex(0xb9d8ff),
    toast: hex(0x24272c),
    toast_err: hex(0x3d2324),
    toast_err_border: hex(0x6e3436),
    header: hex(0x1b1d21),
    tabbar: hex(0x161819),
    window: rgba(0x1e2125, 0xfa),
    label_bg: rgba(0x1b1d21, 0xe0),
    sel_fill: rgba(0x4c9bff, 0x2e),
    active_fill: rgba(0x4c9bff, 0x29),
    active_stroke: rgba(0x4c9bff, 0x00),
    rollbar: hex(0x50565f),
};

const LIGHT_PAL: Palette = Palette {
    bg: hex(0xffffff),
    panel: hex(0xffffff),
    panel_2: hex(0xf0f2f5),
    border: hex(0xe2e5e9),
    text: hex(0x1f2329),
    // at least 4.5:1 on the panels
    muted: hex(0x636b77),
    accent: hex(0x1a6fd9),
    ok_green: hex(0x1f8a4c),
    error: hex(0xd03a3a),
    error_soft: hex(0xc8413f),
    finish: hex(0x22894c),
    active_text: hex(0x0f5bb5),
    toast: hex(0xffffff),
    toast_err: hex(0xfdeeee),
    toast_err_border: hex(0xeab1b1),
    header: hex(0xffffff),
    tabbar: hex(0xf6f7f9),
    window: rgba(0xffffff, 0xfc),
    label_bg: rgba(0xffffff, 0xeb),
    sel_fill: rgba(0x1a6fd9, 0x21),
    active_fill: rgba(0x1a6fd9, 0x1c),
    active_stroke: rgba(0x1a6fd9, 0x00),
    rollbar: hex(0xc3c8ce),
};

const DARK_VP: ViewPalette = ViewPalette {
    background: hex(0x26292e),
    background_2: hex(0x1b1d21),
    body: hex(0xa4b2c1),
    edge: hex(0x1c2026),
    hover: hex(0xffb547),
    // selection: a cyan blue, apart from error red, the amber hover and the sketch blue
    picked: hex(0x2ed3ff),
    plane: hex(0x6f8fb3),
    sketch: hex(0x56b6ff),
    construction: hex(0x7d8a99),
    geometry: hex(0xe8ecf0),
    under_defined: hex(0x8cc4ff),
    preview: hex(0x7fd3c8),
    snap: hex(0xffe066),
    fixed: hex(0xff7a7a),
    dim: hex(0xffd27f),
    grid_minor: hex(0x2c3035),
    grid_major: hex(0x3a3f46),
    axis_x: hex(0xe05555),
    axis_y: hex(0x55c26a),
    axis_z: hex(0x5588ee),
};

const LIGHT_VP: ViewPalette = ViewPalette {
    background: hex(0xfcfcfd),
    background_2: hex(0xeef1f5),
    body: hex(0x8a9eb1),
    edge: hex(0x1b2630),
    hover: hex(0xf3a73b),
    picked: hex(0x009ee0),
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

/// Panels are cards floating on this canvas, with this gap around them.
pub const CARD_GAP: i8 = 8;
pub const CARD_RADIUS: u8 = 14;

/// Behind the panel cards.
pub fn canvas() -> Color32 {
    if is_dark() { hex(0x0b0c0e) } else { hex(0xe3e6ea) }
}

/// A panel drawn as a card: rounded, with a soft shadow, inset from its neighbours by `CARD_GAP`.
pub fn card(fill: Color32, inner: egui::Margin) -> egui::Frame {
    egui::Frame::new().fill(fill).inner_margin(inner).outer_margin(egui::Margin::same(CARD_GAP)).corner_radius(CARD_RADIUS).shadow(card_shadow())
}

pub fn card_shadow() -> egui::Shadow {
    egui::Shadow { offset: [0, 2], blur: 10, spread: 0, color: Color32::from_black_alpha(if is_dark() { 90 } else { 22 }) }
}

/// Round the corners of a flush area (the viewport) by painting the canvas over them.
pub fn round_corners(painter: &egui::Painter, rect: egui::Rect, r: f32) {
    use std::f32::consts::{FRAC_PI_2, PI};
    let col = canvas();
    let mut m = egui::Mesh::default();
    let n = 8;
    for (corner, centre, a0) in [
        (rect.left_top(), rect.left_top() + egui::vec2(r, r), PI),
        (rect.right_top(), rect.right_top() + egui::vec2(-r, r), 1.5 * PI),
        (rect.right_bottom(), rect.right_bottom() + egui::vec2(-r, -r), 0.0),
        (rect.left_bottom(), rect.left_bottom() + egui::vec2(r, -r), FRAC_PI_2),
    ] {
        let base = m.vertices.len() as u32;
        m.colored_vertex(corner, col);
        for i in 0..=n {
            let a = a0 + FRAC_PI_2 * i as f32 / n as f32;
            m.colored_vertex(centre + r * egui::vec2(a.cos(), a.sin()), col);
        }
        for i in 0..n {
            m.add_triangle(base, base + 1 + i, base + 2 + i);
        }
    }
    painter.add(m);
}

/// Switch between the light and dark palettes and restyle egui.
pub fn set_dark(ctx: &egui::Context, dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
    apply(ctx);
}

/// Inter for the UI text (Regular for body, Medium as the `medium()` family),
/// with egui's bundled fonts kept as fallbacks for symbols.
pub fn install_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};
    let mut f = FontDefinitions::default();
    f.font_data.insert("inter".into(), std::sync::Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Inter-Regular.ttf"))));
    f.font_data.insert("inter-medium".into(), std::sync::Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Inter-Medium.ttf"))));
    let fallbacks = f.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    f.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "inter".into());
    let mut medium = vec!["inter-medium".to_string()];
    medium.extend(fallbacks);
    f.families.insert(FontFamily::Name("medium".into()), medium);
    ctx.set_fonts(f);
}

/// The Medium weight of the UI font, for titles and the document name.
pub fn medium() -> egui::FontFamily {
    egui::FontFamily::Name("medium".into())
}

/// Background of text fields and combo boxes.
pub fn input_fill() -> Color32 {
    if is_dark() { pal().bg } else { hex(0xf3f4f6) }
}

/// Make combo boxes in `ui` look like filled input fields rather than flat buttons.
pub fn field_style(ui: &mut egui::Ui) {
    let w = &mut ui.visuals_mut().widgets;
    w.inactive.weak_bg_fill = input_fill();
    w.hovered.weak_bg_fill = if is_dark() { hex(0x202328) } else { hex(0xeceef1) };
}

pub fn apply(ctx: &egui::Context) {
    let p = pal();
    let dark = is_dark();
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.muted);
    v.panel_fill = p.panel;
    v.window_fill = p.window;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(8);
    // inputs are filled, not outlined; focus draws the accent ring
    let input = input_fill();
    v.extreme_bg_color = input;
    v.text_edit_bg_color = Some(input);
    v.faint_bg_color = p.panel_2;
    v.code_bg_color = p.panel_2;
    v.selection.bg_fill = if dark { rgba(0x4c9bff, 0x45) } else { rgba(0x1a6fd9, 0x38) };
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;
    let shadow = if dark { 110 } else { 28 };
    v.window_shadow = egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(shadow) };
    v.popup_shadow = egui::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(shadow) };
    v.striped = false;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;

    let r = CornerRadius::same(8);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    // separators stay faint; the cards and the canvas between them do the dividing
    w.noninteractive.bg_stroke = Stroke::new(1.0, lerp(p.border, p.panel, 0.55));
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;
    w.inactive.bg_fill = if dark { hex(0x2a2d33) } else { hex(0xe9ecf0) };
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
    w.active.bg_fill = if dark { hex(0x30343a) } else { hex(0xe6e9ed) };
    w.active.weak_bg_fill = w.active.bg_fill;
    w.active.bg_stroke = Stroke::NONE;
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;
    w.open.bg_fill = p.panel_2;
    w.open.weak_bg_fill = p.panel_2;
    w.open.bg_stroke = Stroke::NONE;
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = r;
    let theme = if dark { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_theme(if dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
    ctx.set_visuals_of(theme, v);

    ctx.style_mut_of(theme, |s| {
        s.spacing.item_spacing = egui::vec2(6.0, 5.0);
        s.spacing.button_padding = egui::vec2(6.0, 4.0);
        s.spacing.interact_size.y = 28.0;
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.window_margin = egui::Margin::same(10);
        s.spacing.combo_width = 120.0;
        s.spacing.icon_width = 14.0;
        s.spacing.icon_width_inner = 8.0;
        s.spacing.scroll = egui::style::ScrollStyle::floating();
        use egui::{FontId, TextStyle};
        s.text_styles.insert(TextStyle::Heading, FontId::new(16.0, medium()));
        s.text_styles.insert(TextStyle::Body, FontId::proportional(12.5));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(12.5));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(11.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(12.0));
    });
}
