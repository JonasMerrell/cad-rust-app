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
    look().map(|l| l.dark).unwrap_or_else(|| DARK.load(Ordering::Relaxed))
}

/// A trial look picked with `CAD_STYLE=<name>`, overriding the light and dark palettes.
pub struct Look {
    pub name: &'static str,
    pub dark: bool,
    pub pal: Palette,
    pub vp: ViewPalette,
    pub input: Color32,
    pub input_hover: Color32,
    pub button: Color32,
    pub radius: u8,
    pub font: f32,
}

fn look() -> Option<&'static Look> {
    static L: std::sync::OnceLock<Option<&'static Look>> = std::sync::OnceLock::new();
    *L.get_or_init(|| {
        let n = std::env::var("CAD_STYLE").ok()?;
        LOOKS.iter().find(|l| l.name.eq_ignore_ascii_case(&n))
    })
}

#[derive(Clone, Copy, PartialEq)]
pub enum TabStyle {
    /// The active tab joins the panel above with an accent line (current).
    Raised,
    /// Rounded pills on the bar.
    Pill,
    /// Square cells with dividers.
    Box,
    /// A folder tab with a border around the active one.
    Folder,
    /// Text only, the active one underlined.
    Underline,
}

/// A trial shape language picked with `CAD_SHAPE=<name>`: corners, borders, tabs and panels.
pub struct Shape {
    pub name: &'static str,
    /// Buttons, inputs, menus.
    pub radius: u8,
    /// Feature tree and list rows.
    pub row_radius: u8,
    pub tab: TabStyle,
    /// Outline idle buttons.
    pub button_stroke: bool,
    /// Fill idle buttons.
    pub button_fill: bool,
    /// Gap around panels drawn as floating cards on a canvas (0 = flush panels).
    pub gap: i8,
    pub card_radius: u8,
    /// Each toolbar group sits on its own rounded chip.
    pub chips: bool,
    /// Selected rows get an accent bar on the left instead of a tint only.
    pub sel_bar: bool,
    /// Row and button height.
    pub height: f32,
    /// Panel separator lines.
    pub separators: bool,
    /// Popups and dialogs.
    pub window_radius: u8,
    /// Hairline around cards.
    pub card_border: bool,
    /// Soft drop shadow under cards.
    pub card_shadow: bool,
    /// Canvas colour behind cards, light and dark (0 = derived from the palette).
    pub canvas: [u32; 2],
}

const CURRENT_SHAPE: Shape =
    Shape { name: "current", radius: 5, row_radius: 5, tab: TabStyle::Raised, button_stroke: false, button_fill: false, gap: 0, card_radius: 0, chips: false, sel_bar: false, height: 26.0, separators: true, window_radius: 10, card_border: false, card_shadow: false, canvas: [0, 0] };

const SHAPES: &[Shape] = &[
    Shape { name: "soft", radius: 8, row_radius: 8, tab: TabStyle::Pill, button_stroke: false, button_fill: false, gap: 6, card_radius: 12, chips: true, sel_bar: false, height: 28.0, separators: false, window_radius: 14, card_border: true, card_shadow: false, canvas: [0, 0] },
    Shape { name: "soft-elevated", radius: 8, row_radius: 8, tab: TabStyle::Pill, button_stroke: false, button_fill: false, gap: 8, card_radius: 14, chips: true, sel_bar: false, height: 28.0, separators: false, window_radius: 14, card_border: false, card_shadow: true, canvas: [0xe3e6ea, 0x0b0c0e] },
    Shape { name: "soft-round", radius: 14, row_radius: 14, tab: TabStyle::Pill, button_stroke: false, button_fill: false, gap: 8, card_radius: 18, chips: true, sel_bar: false, height: 28.0, separators: false, window_radius: 18, card_border: true, card_shadow: false, canvas: [0xe6ecf4, 0x0f131a] },
    Shape { name: "crisp", radius: 0, row_radius: 0, tab: TabStyle::Box, button_stroke: true, button_fill: false, gap: 0, card_radius: 0, chips: false, sel_bar: true, height: 26.0, separators: true, window_radius: 0, card_border: false, card_shadow: false, canvas: [0, 0] },
    Shape { name: "classic", radius: 3, row_radius: 2, tab: TabStyle::Folder, button_stroke: true, button_fill: true, gap: 0, card_radius: 0, chips: false, sel_bar: false, height: 26.0, separators: true, window_radius: 4, card_border: false, card_shadow: false, canvas: [0, 0] },
    Shape { name: "minimal", radius: 6, row_radius: 0, tab: TabStyle::Underline, button_stroke: false, button_fill: false, gap: 0, card_radius: 0, chips: false, sel_bar: true, height: 30.0, separators: false, window_radius: 10, card_border: false, card_shadow: false, canvas: [0, 0] },
];

pub fn shape() -> &'static Shape {
    static S: std::sync::OnceLock<&'static Shape> = std::sync::OnceLock::new();
    S.get_or_init(|| std::env::var("CAD_SHAPE").ok().and_then(|n| SHAPES.iter().find(|s| s.name.eq_ignore_ascii_case(&n))).unwrap_or(&CURRENT_SHAPE))
}

/// A trial layout picked with `CAD_LAYOUT=floating|sidetools`; "current" otherwise.
pub fn layout() -> &'static str {
    static L: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    L.get_or_init(|| match std::env::var("CAD_LAYOUT").unwrap_or_default().as_str() {
        "floating" => "floating",
        "sidetools" => "sidetools",
        _ => "current",
    })
}

/// Behind floating panel cards.
pub fn canvas() -> Color32 {
    let c = shape().canvas[is_dark() as usize];
    if c != 0 {
        return hex(c);
    }
    if is_dark() { lerp(pal().bg, Color32::BLACK, 0.25) } else { lerp(pal().panel_2, pal().border, 0.35) }
}

/// A panel frame: flush, or a floating card for shapes with a gap.
pub fn panel_frame(fill: Color32, m: egui::Margin) -> egui::Frame {
    let sh = shape();
    let f = egui::Frame::new().fill(fill).inner_margin(m);
    if sh.gap == 0 {
        return f;
    }
    let mut f = f.outer_margin(egui::Margin::same(sh.gap)).corner_radius(sh.card_radius);
    if sh.card_border {
        f = f.stroke(card_stroke());
    }
    if sh.card_shadow {
        f = f.shadow(egui::Shadow { offset: [0, 2], blur: 10, spread: 0, color: Color32::from_black_alpha(if is_dark() { 90 } else { 22 }) });
    }
    f
}

pub fn card_stroke() -> Stroke {
    Stroke::new(1.0, if is_dark() { lerp(pal().border, pal().panel, 0.2) } else { lerp(pal().border, pal().panel, 0.3) })
}

/// Round the corners of a flush area (the viewport) by painting the canvas over them.
pub fn round_corners(painter: &egui::Painter, rect: egui::Rect, r: f32) {
    let col = canvas();
    let mut m = egui::Mesh::default();
    let n = 8;
    for (corner, centre, a0) in [
        (rect.left_top(), rect.left_top() + egui::vec2(r, r), std::f32::consts::PI),
        (rect.right_top(), rect.right_top() + egui::vec2(-r, r), 1.5 * std::f32::consts::PI),
        (rect.right_bottom(), rect.right_bottom() + egui::vec2(-r, -r), 0.0),
        (rect.left_bottom(), rect.left_bottom() + egui::vec2(r, -r), 0.5 * std::f32::consts::PI),
    ] {
        let base = m.vertices.len() as u32;
        m.colored_vertex(corner, col);
        for i in 0..=n {
            let a = a0 + std::f32::consts::FRAC_PI_2 * i as f32 / n as f32;
            m.colored_vertex(centre + r * egui::vec2(a.cos(), a.sin()), col);
        }
        for i in 0..n {
            m.add_triangle(base, base + 1 + i, base + 2 + i);
        }
    }
    painter.add(m);
    if shape().card_border {
        painter.rect_stroke(rect, r, card_stroke(), egui::StrokeKind::Inside);
    }
}

const LOOKS: &[Look] = &[
    // A: dark charcoal, warm orange accent, light aluminium parts
    Look {
        name: "graphite",
        dark: true,
        pal: Palette {
            bg: hex(0x0f1012),
            panel: hex(0x17181b),
            panel_2: hex(0x222327),
            border: hex(0x2a2b30),
            text: hex(0xe9e9eb),
            muted: hex(0x9b9ca3),
            accent: hex(0xff8a3d),
            active_text: hex(0xffc39a),
            header: hex(0x121315),
            tabbar: hex(0x111214),
            window: rgba(0x1b1c20, 0xfa),
            label_bg: rgba(0x17181b, 0xe6),
            sel_fill: rgba(0xff8a3d, 0x2e),
            active_fill: rgba(0xff8a3d, 0x26),
            active_stroke: rgba(0xff8a3d, 0x00),
            rollbar: hex(0x55575e),
            ..DARK_PAL
        },
        vp: ViewPalette {
            background: hex(0x2b2d31),
            background_2: hex(0x131416),
            body: hex(0xd4d7dc),
            edge: hex(0x0c0d0f),
            hover: hex(0xffb36b),
            picked: hex(0x46c8ff),
            plane: hex(0x8a8f99),
            grid_minor: hex(0x26282c),
            grid_major: hex(0x35383d),
            ..DARK_VP
        },
        input: hex(0x0f1012),
        input_hover: hex(0x1c1d21),
        button: hex(0x26272b),
        radius: 6,
        font: 12.5,
    },
    // B: warm paper white, quiet greys, pale parts, blue accent
    Look {
        name: "paper",
        dark: false,
        pal: Palette {
            bg: hex(0xfbfbfa),
            panel: hex(0xf6f6f4),
            panel_2: hex(0xeaeae6),
            border: hex(0xe1e1dc),
            text: hex(0x1c1c1a),
            muted: hex(0x66665f),
            accent: hex(0x2563eb),
            active_text: hex(0x1d4ed8),
            header: hex(0xf6f6f4),
            tabbar: hex(0xefefeb),
            window: rgba(0xffffff, 0xfc),
            label_bg: rgba(0xffffff, 0xf0),
            sel_fill: rgba(0x2563eb, 0x1f),
            active_fill: rgba(0x2563eb, 0x1a),
            active_stroke: rgba(0x2563eb, 0x00),
            rollbar: hex(0xc9c9c2),
            ..LIGHT_PAL
        },
        vp: ViewPalette {
            background: hex(0xffffff),
            background_2: hex(0xf0f0ec),
            body: hex(0xdde2e8),
            edge: hex(0x2b3038),
            plane: hex(0xa9bfdc),
            grid_minor: hex(0xf0f0ec),
            grid_major: hex(0xdedfd9),
            ..LIGHT_VP
        },
        input: hex(0xffffff),
        input_hover: hex(0xf2f2ef),
        button: hex(0xe6e6e1),
        radius: 8,
        font: 13.0,
    },
    // C: engineering blueprint, navy and cyan, white edges
    Look {
        name: "blueprint",
        dark: true,
        pal: Palette {
            bg: hex(0x081527),
            panel: hex(0x0d1e36),
            panel_2: hex(0x152c4d),
            border: hex(0x1d3961),
            text: hex(0xe4eeff),
            muted: hex(0x8ea6c8),
            accent: hex(0x38c6f4),
            active_text: hex(0xa6e8ff),
            header: hex(0x0a1930),
            tabbar: hex(0x08152a),
            window: rgba(0x10233f, 0xfa),
            label_bg: rgba(0x0d1e36, 0xe6),
            sel_fill: rgba(0x38c6f4, 0x2e),
            active_fill: rgba(0x38c6f4, 0x26),
            active_stroke: rgba(0x38c6f4, 0x00),
            rollbar: hex(0x3d5f8f),
            ..DARK_PAL
        },
        vp: ViewPalette {
            background: hex(0x13325c),
            background_2: hex(0x0a1b35),
            body: hex(0x7fa9d6),
            edge: hex(0xe8f3ff),
            hover: hex(0xffd166),
            picked: hex(0x7cf0ff),
            plane: hex(0x5d8ccc),
            grid_minor: hex(0x1a3d6b),
            grid_major: hex(0x2a5590),
            ..DARK_VP
        },
        input: hex(0x081527),
        input_hover: hex(0x112642),
        button: hex(0x173156),
        radius: 3,
        font: 12.5,
    },
    // D: mid-tone cool slate, soft contrast, frost accent
    Look {
        name: "slate",
        dark: true,
        pal: Palette {
            bg: hex(0x2b303b),
            panel: hex(0x343a46),
            panel_2: hex(0x3e4552),
            border: hex(0x464e5c),
            text: hex(0xeceff4),
            muted: hex(0xaab2c0),
            accent: hex(0x88c0d0),
            active_text: hex(0xc4e4ee),
            header: hex(0x2f3541),
            tabbar: hex(0x2b303b),
            window: rgba(0x373d4a, 0xfa),
            label_bg: rgba(0x343a46, 0xe6),
            sel_fill: rgba(0x88c0d0, 0x33),
            active_fill: rgba(0x88c0d0, 0x2b),
            active_stroke: rgba(0x88c0d0, 0x00),
            rollbar: hex(0x6a7486),
            ..DARK_PAL
        },
        vp: ViewPalette {
            background: hex(0x4a5263),
            background_2: hex(0x2e3440),
            body: hex(0xdfe5ee),
            edge: hex(0x242933),
            hover: hex(0xebcb8b),
            picked: hex(0x8fd4ff),
            plane: hex(0x81a1c1),
            grid_minor: hex(0x434b5a),
            grid_major: hex(0x515a6b),
            ..DARK_VP
        },
        input: hex(0x2b303b),
        input_hover: hex(0x3a404d),
        button: hex(0x434a58),
        radius: 7,
        font: 12.5,
    },
    // E: warm workshop sand, ink text, safety orange, square corners
    Look {
        name: "workshop",
        dark: false,
        pal: Palette {
            bg: hex(0xf5f2eb),
            panel: hex(0xede8dd),
            panel_2: hex(0xe1d9c9),
            border: hex(0xd3c9b5),
            text: hex(0x221f1a),
            muted: hex(0x655e50),
            accent: hex(0xd9531e),
            active_text: hex(0xa83c10),
            header: hex(0xe6e0d2),
            tabbar: hex(0xe6e0d2),
            window: rgba(0xf8f5ef, 0xfc),
            label_bg: rgba(0xf8f5ef, 0xf0),
            sel_fill: rgba(0xd9531e, 0x24),
            active_fill: rgba(0xd9531e, 0x1f),
            active_stroke: rgba(0xd9531e, 0x00),
            rollbar: hex(0xbdb19a),
            ..LIGHT_PAL
        },
        vp: ViewPalette {
            background: hex(0xf8f6f1),
            background_2: hex(0xe3ddd0),
            body: hex(0xc3c8cd),
            edge: hex(0x221f1a),
            hover: hex(0xf0a020),
            picked: hex(0x0f8fd6),
            plane: hex(0xb7a98c),
            grid_minor: hex(0xebe6db),
            grid_major: hex(0xd6cdbb),
            ..LIGHT_VP
        },
        input: hex(0xfbf9f5),
        input_hover: hex(0xf3efe7),
        button: hex(0xdcd3c1),
        radius: 2,
        font: 12.5,
    },
];

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
    if let Some(l) = look() {
        return &l.pal;
    }
    if is_dark() { &DARK_PAL } else { &LIGHT_PAL }
}

pub fn vpal() -> &'static ViewPalette {
    if let Some(l) = look() {
        return &l.vp;
    }
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
    if let Some(l) = look() {
        return l.input;
    }
    if is_dark() { pal().bg } else { hex(0xf3f4f6) }
}

/// Make combo boxes in `ui` look like filled input fields rather than flat buttons.
pub fn field_style(ui: &mut egui::Ui) {
    let w = &mut ui.visuals_mut().widgets;
    w.inactive.weak_bg_fill = input_fill();
    w.hovered.weak_bg_fill = look().map(|l| l.input_hover).unwrap_or(if is_dark() { hex(0x202328) } else { hex(0xeceef1) });
}

pub fn apply(ctx: &egui::Context) {
    let p = pal();
    let dark = is_dark();
    let font = look().map(|l| l.font).unwrap_or(12.5);
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.muted);
    v.panel_fill = p.panel;
    v.window_fill = p.window;
    v.window_stroke = Stroke::new(1.0, p.border);
    let sh = shape();
    v.window_corner_radius = CornerRadius::same(sh.window_radius);
    v.menu_corner_radius = CornerRadius::same(sh.window_radius.min(8).max(sh.radius));
    // inputs are filled, not outlined; focus draws the accent ring
    let input = input_fill();
    v.extreme_bg_color = input;
    v.text_edit_bg_color = Some(input);
    v.faint_bg_color = p.panel_2;
    v.code_bg_color = p.panel_2;
    v.selection.bg_fill = if let Some(l) = look() { Color32::from_rgba_unmultiplied(l.pal.accent.r(), l.pal.accent.g(), l.pal.accent.b(), 0x45) } else if dark { rgba(0x4c9bff, 0x45) } else { rgba(0x1a6fd9, 0x38) };
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;
    let shadow = if dark { 110 } else { 28 };
    v.window_shadow = egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(shadow) };
    v.popup_shadow = egui::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(shadow) };
    v.striped = false;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;

    let r = CornerRadius::same(if sh.name == "current" { look().map(|l| l.radius).unwrap_or(5) } else { sh.radius });
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;
    w.inactive.bg_fill = look().map(|l| l.button).unwrap_or(if dark { hex(0x2a2d33) } else { hex(0xe9ecf0) });
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
    w.active.bg_fill = look().map(|l| lerp(l.button, l.pal.text, 0.08)).unwrap_or(if dark { hex(0x30343a) } else { hex(0xe6e9ed) });
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
    if sh.button_stroke {
        w.inactive.bg_stroke = Stroke::new(1.0, lerp(p.border, p.muted, 0.25));
        w.hovered.bg_stroke = Stroke::new(1.0, lerp(p.border, p.muted, 0.6));
        w.active.bg_stroke = Stroke::new(1.0, p.accent);
        w.open.bg_stroke = w.hovered.bg_stroke;
    }
    if sh.button_fill {
        w.inactive.weak_bg_fill = if dark { lerp(p.panel, Color32::WHITE, 0.07) } else { lerp(p.panel_2, p.border, 0.2) };
        w.hovered.weak_bg_fill = if dark { lerp(p.panel, Color32::WHITE, 0.12) } else { lerp(p.panel_2, p.border, 0.7) };
        w.inactive.bg_stroke = Stroke::new(1.0, lerp(p.border, p.muted, 0.45));
    }
    if !sh.separators {
        w.noninteractive.bg_stroke = Stroke::new(1.0, lerp(p.border, p.panel, 0.55));
    }
    let theme = if dark { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_theme(if dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
    ctx.set_visuals_of(theme, v);

    ctx.style_mut_of(theme, |s| {
        s.spacing.item_spacing = egui::vec2(6.0, 5.0);
        s.spacing.button_padding = egui::vec2(6.0, 4.0);
        s.spacing.interact_size.y = sh.height;
        if sh.height > 28.0 {
            s.spacing.item_spacing = egui::vec2(8.0, 7.0);
            s.spacing.button_padding = egui::vec2(6.0, 5.0);
        }
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.window_margin = egui::Margin::same(10);
        s.spacing.combo_width = 120.0;
        s.spacing.icon_width = 14.0;
        s.spacing.icon_width_inner = 8.0;
        s.spacing.scroll = egui::style::ScrollStyle::floating();
        use egui::{FontId, TextStyle};
        s.text_styles.insert(TextStyle::Heading, FontId::new(16.0, medium()));
        s.text_styles.insert(TextStyle::Body, FontId::proportional(font));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(font));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(11.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(12.0));
    });
}
