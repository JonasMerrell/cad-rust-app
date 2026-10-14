//! Colours and egui styling, taken from cad-app.html's CSS variables and the
//! viewport palette (`vm` in the original bundle).

use egui::{Color32, CornerRadius, Stroke, Visuals};

pub const fn hex(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

// CSS :root
pub const BG: Color32 = hex(0x15181c);
pub const PANEL: Color32 = hex(0x1f242b);
pub const PANEL_2: Color32 = hex(0x262c34);
pub const BORDER: Color32 = hex(0x323a44);
pub const TEXT: Color32 = hex(0xdfe3e8);
pub const MUTED: Color32 = hex(0x8a94a1);
pub const ACCENT: Color32 = hex(0x4ea1ff);
pub const OK_GREEN: Color32 = hex(0x6fcf86);
pub const ERROR: Color32 = hex(0xff7a7a);
pub const ERROR_SOFT: Color32 = hex(0xff8a8a);
pub const FINISH: Color32 = hex(0x2f7d4a);
pub const FINISH_HOVER: Color32 = hex(0x379457);
pub const ACTIVE_TEXT: Color32 = hex(0xcfe5ff);
pub const TOAST: Color32 = hex(0x2a3442);
pub const TOAST_ERR: Color32 = hex(0x4a2626);

/// Viewport palette.
pub mod vp {
    use super::hex;
    use egui::Color32;
    pub const BACKGROUND: Color32 = hex(0x20252c);
    pub const BODY: Color32 = hex(0x9aa9ba);
    pub const EDGE: Color32 = hex(0x1c2026);
    pub const HOVER: Color32 = hex(0xffb547);
    pub const PICKED: Color32 = hex(0x4ea1ff);
    pub const PLANE: Color32 = hex(0x6f8fb3);
    pub const SKETCH: Color32 = hex(0x56b6ff);
    pub const CONSTRUCTION: Color32 = hex(0x7d8a99);
    pub const GEOMETRY: Color32 = hex(0xe8ecf0);
    pub const UNDER_DEFINED: Color32 = hex(0x8cc4ff);
    pub const PREVIEW: Color32 = hex(0x7fd3c8);
    pub const SNAP: Color32 = hex(0xffe066);
    pub const FIXED: Color32 = hex(0xff7a7a);
    pub const DIM: Color32 = hex(0xffd27f);
    pub const GRID_MINOR: Color32 = hex(0x2b313a);
    pub const GRID_MAJOR: Color32 = hex(0x39414d);
    pub const AXIS_X: Color32 = hex(0xe05555);
    pub const AXIS_Y: Color32 = hex(0x55c26a);
    pub const AXIS_Z: Color32 = hex(0x5588ee);
}

pub fn to_rgba(c: Color32, a: f32) -> [f32; 4] {
    [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, a]
}

pub fn apply(ctx: &egui::Context) {
    let mut v = Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.panel_fill = PANEL;
    v.window_fill = Color32::from_rgba_unmultiplied(0x1f, 0x24, 0x2b, 247);
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(7);
    v.extreme_bg_color = BG; // text edits / combo boxes
    v.faint_bg_color = PANEL_2;
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(0x4e, 0xa1, 0xff, 0x40);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;

    let r = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = PANEL;
    w.noninteractive.weak_bg_fill = PANEL;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.bg_fill = BG;
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = r;
    w.hovered.bg_fill = PANEL_2;
    w.hovered.weak_bg_fill = PANEL_2;
    w.hovered.bg_stroke = Stroke::NONE;
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;
    w.active.bg_fill = PANEL_2;
    w.active.weak_bg_fill = PANEL_2;
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.active.fg_stroke = Stroke::new(1.0, TEXT);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;
    w.open.bg_fill = BG;
    w.open.weak_bg_fill = BG;
    w.open.bg_stroke = Stroke::new(1.0, ACCENT);
    w.open.corner_radius = CornerRadius::same(5);
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);

    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(7.0, 4.0);
        s.spacing.interact_size.y = 24.0;
        use egui::{FontId, TextStyle};
        s.text_styles.insert(TextStyle::Body, FontId::proportional(13.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(12.5));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(11.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(12.0));
    });
}
