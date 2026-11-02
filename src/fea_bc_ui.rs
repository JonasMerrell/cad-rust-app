//! The boundary conditions of a study, laid out as crackh3d's FE panel does them: one list of cards (a
//! colour swatch per type, the name, the type and its faces, then buttons to pick faces, move, duplicate
//! and remove), the open card's fields in a grid with units, an "Add BC" menu of supports and loads,
//! a restraint check, and in the viewport every BC's faces in its type's colour with small glyphs
//! (ground marks, symmetry bars, springs, arrows). A child of `fea_ui`.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, RichText, Stroke, Ui};
use glam::DVec3;

use super::{edit, index, study, trim};
use crate::app::App;
use crate::fea::{self, LoadKind, Study, SupportKind};
use crate::fea_adv::BcType;
use crate::icons;
use crate::render::{LineInst, SceneData};
use crate::theme;

// ---------------------------------------------------------------- colours and kinds

/// crackh3d's BC colours (fixed, displacement, symmetry, spring, pressure, traction, force, remote).
fn palette() -> [Color32; 8] {
    let c = |r: f32, g: f32, b: f32| Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8);
    if theme::is_dark() {
        [c(0.66, 0.71, 0.80), c(0.25, 0.80, 0.90), c(0.35, 0.85, 0.50), c(0.75, 0.55, 0.95), c(0.95, 0.40, 0.40), c(0.98, 0.62, 0.25), c(0.95, 0.45, 0.75), c(0.90, 0.75, 0.25)]
    } else {
        [c(0.36, 0.40, 0.47), c(0.02, 0.50, 0.62), c(0.13, 0.55, 0.27), c(0.50, 0.28, 0.75), c(0.80, 0.16, 0.18), c(0.86, 0.40, 0.05), c(0.75, 0.15, 0.50), c(0.62, 0.45, 0.02)]
    }
}

/// The colour of a BC type (a `Kind::key` or a `BcType` key).
pub(super) fn color(key: &str) -> Color32 {
    let i = match key {
        "fixed" => 0,
        "displacement" => 1,
        "symmetry" | "roller" => 2,
        "spring" => 3,
        "pressure" => 4,
        "traction" | "gravity" => 5,
        "force" | "point_force" => 6,
        _ => 7,
    };
    palette()[i]
}

/// A Simple-mode BC type: a support or a load.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Support(SupportKind),
    Load(LoadKind),
}

impl Kind {
    const ALL: [Kind; 5] = [Kind::Support(SupportKind::Fixed), Kind::Support(SupportKind::Roller), Kind::Load(LoadKind::Force), Kind::Load(LoadKind::Pressure), Kind::Load(LoadKind::Gravity)];
    fn key(self) -> &'static str {
        match self {
            Kind::Support(SupportKind::Fixed) => "fixed",
            Kind::Support(SupportKind::Roller) => "roller",
            Kind::Load(LoadKind::Force) => "force",
            Kind::Load(LoadKind::Pressure) => "pressure",
            Kind::Load(LoadKind::Gravity) => "gravity",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Kind::Support(k) => k.label(),
            Kind::Load(k) => k.label(),
        }
    }
    fn tip(self) -> &'static str {
        match self {
            Kind::Support(SupportKind::Fixed) => "The faces cannot move at all.",
            Kind::Support(SupportKind::Roller) => "The faces slide but cannot move along their normals (also a symmetry plane).",
            Kind::Load(LoadKind::Force) => "A total force (N) spread evenly over the faces.",
            Kind::Load(LoadKind::Pressure) => "A pressure (MPa) pushing into the faces.",
            Kind::Load(LoadKind::Gravity) => "An acceleration of the whole part (in g), such as its own weight.",
        }
    }
    fn support(self) -> bool {
        matches!(self, Kind::Support(_))
    }
}

/// Advanced BC kinds by group, as crackh3d's Add BC menu lists them.
const ADV_SUPPORTS: [&str; 5] = ["fixed", "displacement", "symmetry", "spring", "remote_rigid"];
const ADV_LOADS: [&str; 5] = ["pressure", "traction", "force", "remote_distributing", "point_force"];

fn adv_tip(key: &str) -> &'static str {
    match key {
        "fixed" => "u = 0 on the faces.",
        "displacement" => "Prescribed displacement per component; an unticked component is free.",
        "symmetry" => "No motion along the faces' normals (a symmetry plane; inclined and curved faces too).",
        "spring" => "An elastic foundation: traction = -k u, per unit area along x, y, z.",
        "remote_rigid" => "The faces move as a rigid body with a reference point; each translation and rotation is prescribed, or free under the force and moment.",
        "pressure" => "A pressure pushing into the faces (negative pulls).",
        "traction" => "A traction vector: force per area, the same at every point of the faces.",
        "force" => "A total force on the faces, spread as a uniform traction F / area.",
        "remote_distributing" => "A force and moment about a reference point, spread over the faces by area, with no stiffness added.",
        "point_force" => "A force on the mesh node nearest a point (a singular stress there).",
        _ => "",
    }
}

// ---------------------------------------------------------------- face geometry

/// Area, centroid and glyph sample points of every face the study's parts have (global face numbers).
#[derive(Default)]
pub(super) struct Geo {
    faces: HashMap<u32, FaceGeo>,
    pub(super) diag: f64,
    pub(super) centre: DVec3,
}

#[derive(Default)]
struct FaceGeo {
    area: f64,
    /// Area-weighted sum of triangle centroids.
    moment: DVec3,
    /// (centroid, outward normal) of the largest triangles, spread over the face.
    samples: Vec<(DVec3, DVec3)>,
    /// Every triangle's (centroid, normal, area).
    tris: Vec<(DVec3, DVec3, f64)>,
    /// Up to a few dozen of the face's vertices with the normal there, for the restraint check (a flat
    /// face of two triangles still gives its corners, so it holds rotations too).
    pts: Vec<(DVec3, DVec3)>,
}

impl Geo {
    fn area(&self, ids: &[u32]) -> f64 {
        ids.iter().filter_map(|f| self.faces.get(f)).map(|g| g.area).sum()
    }
    fn centroid(&self, ids: &[u32]) -> Option<DVec3> {
        let (a, m) = ids.iter().filter_map(|f| self.faces.get(f)).fold((0.0, DVec3::ZERO), |(a, m), g| (a + g.area, m + g.moment));
        (a > 0.0).then(|| m / a)
    }
    fn samples(&self, ids: &[u32], per_face: usize) -> Vec<(DVec3, DVec3)> {
        ids.iter().filter_map(|f| self.faces.get(f)).flat_map(|g| g.samples.iter().take(per_face).copied()).collect()
    }
}

/// The geometry of the study's faces, cached until the study or its parts change.
pub(super) fn geo(app: &mut App) -> Arc<Geo> {
    let key = app.fea.picks_key;
    if let Some((k, g)) = &app.fea.geo {
        if *k == key {
            return g.clone();
        }
    }
    let Some(s) = study(app).cloned() else { return Arc::default() };
    let bodies = app.studio_bodies(s.studio);
    let globals = super::body_faces(bodies, app.studio_doc(s.studio), &s);
    let mut g = Geo::default();
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for (b, gl) in bodies.iter().zip(&globals) {
        let Some((_, base)) = gl else { continue };
        for (ti, t) in b.indices.chunks(3).enumerate() {
            let v = |i: u32| DVec3::new(b.positions[3 * i as usize] as f64, b.positions[3 * i as usize + 1] as f64, b.positions[3 * i as usize + 2] as f64);
            let (a, bb, c) = (v(t[0]), v(t[1]), v(t[2]));
            lo = lo.min(a).min(bb).min(c);
            hi = hi.max(a).max(bb).max(c);
            let cr = (bb - a).cross(c - a);
            let area = 0.5 * cr.length();
            let gf = base + b.tri_face.get(ti).copied().unwrap_or(0);
            let f = g.faces.entry(gf).or_default();
            let ctr = (a + bb + c) / 3.0;
            f.area += area;
            f.moment += ctr * area;
            f.tris.push((ctr, cr.normalize_or_zero(), area));
            if f.pts.len() < 48 {
                for p in [a, bb, c] {
                    f.pts.push((p, cr.normalize_or_zero()));
                }
            }
        }
    }
    for f in g.faces.values_mut() {
        // the largest triangles first, then every k-th of those (crackh3d's samples)
        let mut ts: Vec<&(DVec3, DVec3, f64)> = f.tris.iter().collect();
        ts.sort_by(|x, y| y.2.total_cmp(&x.2));
        let per = 8;
        let keep = ts.len().min(per * 4).max(1).min(ts.len());
        let step = (keep / per).max(1);
        f.samples = ts[..keep].iter().step_by(step).take(per).map(|t| (t.0, t.1)).collect();
    }
    if lo.x.is_finite() {
        g.diag = (hi - lo).length();
        g.centre = (lo + hi) * 0.5;
    }
    let g = Arc::new(g);
    app.fea.geo = Some((key, g.clone()));
    g
}

fn faces_of(app: &App, id: u64) -> Vec<u32> {
    app.fea.picks.iter().find(|p| p.0 == id).map(|p| p.1.clone()).unwrap_or_default()
}

/// The BC (support, load or advanced BC) a face belongs to, the last one listed winning.
pub(super) fn owner(app: &App, s: &Study, face: u32) -> Option<(u64, String)> {
    let named: Vec<(u64, &str)> = if s.mode == fea::SimMode::Advanced {
        s.advanced.bcs.iter().map(|b| (b.id, b.name.as_str())).chain(s.advanced.periodic.iter().map(|p| (p.id, p.name.as_str()))).collect()
    } else {
        s.supports.iter().map(|b| (b.id, b.name.as_str())).chain(s.loads.iter().map(|b| (b.id, b.name.as_str()))).collect()
    };
    named.into_iter().rev().find(|(id, _)| app.fea.picks.iter().any(|p| (p.0 & !super::SIDE_B) == *id && p.1.contains(&face))).map(|(id, n)| (id, n.to_string()))
}

// ---------------------------------------------------------------- restraint check

/// Which rigid-body motions nothing holds (crackh3d's `free_rigid_modes` on the faces' triangles).
fn free_modes(app: &mut App, s: &Study) -> Vec<String> {
    use crackh3d_fe::fe::bcs::{free_rigid_modes, Restraint};
    let g = geo(app);
    let mut r = vec![];
    fn on(g: &Geo, r: &mut Vec<Restraint>, ids: &[u32], dirs: &dyn Fn(DVec3) -> Vec<DVec3>) {
        for f in ids.iter().filter_map(|f| g.faces.get(f)) {
            // a few points per face are enough: the check only needs where and which way
            for (p, n) in &f.pts {
                for d in dirs(*n) {
                    r.push(Restraint { point: p.to_array(), dir: d.to_array(), rotation: false });
                }
            }
        }
    }
    let xyz = |_: DVec3| vec![DVec3::X, DVec3::Y, DVec3::Z];
    if s.mode == fea::SimMode::Advanced {
        for b in s.advanced.bcs.iter().filter(|b| !b.suppressed) {
            let ids = faces_of(app, b.id);
            match &b.bc {
                BcType::Fixed => on(&g, &mut r, &ids, &xyz),
                BcType::Symmetry => on(&g, &mut r, &ids, &|n| vec![n]),
                BcType::Displacement { u } => {
                    let u = *u;
                    on(&g, &mut r, &ids, &move |_| [DVec3::X, DVec3::Y, DVec3::Z].into_iter().zip(u).filter(|(_, c)| c.is_some()).map(|(d, _)| d).collect())
                }
                BcType::Spring { k } => {
                    let k = *k;
                    on(&g, &mut r, &ids, &move |_| [DVec3::X, DVec3::Y, DVec3::Z].into_iter().zip(k).filter(|(_, c)| *c > 0.0).map(|(d, _)| d).collect())
                }
                BcType::RemoteRigid { point, u, rot, .. } if !ids.is_empty() => {
                    for (k, d) in [DVec3::X, DVec3::Y, DVec3::Z].into_iter().enumerate() {
                        if u[k].is_some() {
                            r.push(Restraint { point: *point, dir: d.to_array(), rotation: false });
                        }
                        if rot[k].is_some() {
                            r.push(Restraint { point: *point, dir: d.to_array(), rotation: true });
                        }
                    }
                }
                _ => {}
            }
        }
    } else {
        for sp in s.supports.iter().filter(|b| !b.suppressed) {
            let ids = faces_of(app, sp.id);
            match sp.kind {
                SupportKind::Fixed => on(&g, &mut r, &ids, &xyz),
                SupportKind::Roller => on(&g, &mut r, &ids, &|n| vec![n]),
            }
        }
    }
    free_rigid_modes(&r)
}

/// "Fully held" in green, or the free rigid-body motions: red for a static study, which cannot solve
/// them, muted for a modal or transient one, which then runs free-free.
fn restraint_chip(app: &mut App, ui: &mut Ui, s: &Study) {
    let free = free_modes(app, s);
    let tip = "Which rigid-body motions (translations along and rotations about x, y, z) the supports leave free, from the picked faces alone.";
    let (text, col) = if free.is_empty() {
        ("Fully held".to_string(), theme::ok_green())
    } else if free.len() == 6 && s.mode == fea::SimMode::Simple && s.analysis != fea::Analysis::Static {
        ("Free-free: nothing holds the part (fine for vibration)".to_string(), theme::muted())
    } else if s.mode == fea::SimMode::Simple && s.analysis != fea::Analysis::Static {
        (format!("⚠  Free: {}", free.join("; ")), theme::muted())
    } else {
        (format!("⚠  Free: {}", free.join("; ")), theme::error_soft())
    };
    ui.horizontal_wrapped(|ui| {
        if free.is_empty() {
            ui.add(icons::image("check", 14.0).tint(col));
        }
        ui.label(RichText::new(text).color(col).strong().size(11.5)).on_hover_text(tip);
    });
}

// ---------------------------------------------------------------- widgets

fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).color(theme::muted()).size(11.5)
}

/// A 2-column grid: the label (with its unit, muted, under it) and the field. The sidebar is too narrow
/// for crackh3d's third unit column next to three x, y, z fields.
fn grid(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([6.0, 6.0]).min_col_width(20.0).show(ui, add);
}

fn label(ui: &mut Ui, text: &str, tip: &str) {
    label_u(ui, text, "", tip);
}

/// A row's label, with its unit after it in small muted text.
fn label_u(ui: &mut Ui, text: &str, u: &str, tip: &str) {
    let r = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.label(RichText::new(text).color(theme::text()).size(12.0));
            if !u.is_empty() {
                ui.label(RichText::new(u).color(theme::muted()).size(10.0));
            }
        })
        .response;
    if !tip.is_empty() {
        r.on_hover_text(tip);
    }
}

fn unit(ui: &mut Ui, _: &str) {
    ui.end_row();
}

const VEC_W: f32 = 46.0;

fn num(v: &mut f64, speed: f64) -> egui::DragValue<'_> {
    egui::DragValue::new(v).speed(speed).max_decimals(6).custom_formatter(|v, _| trim(v))
}

fn vec3(ui: &mut Ui, v: &mut [f64; 3], speed: f64, tip: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        for (k, x) in v.iter_mut().enumerate() {
            ui.add_sized([VEC_W, 20.0], num(x, speed).prefix(["x ", "y ", "z "][k])).on_hover_text(tip);
        }
    });
}

fn row_vec3(ui: &mut Ui, name: &str, tip: &str, v: &mut [f64; 3], u: &str, speed: f64) {
    label_u(ui, name, u, tip);
    vec3(ui, v, speed, tip);
    unit(ui, u);
}

fn row_f64(ui: &mut Ui, name: &str, tip: &str, v: &mut f64, u: &str, speed: f64) {
    label_u(ui, name, u, tip);
    ui.add_sized([VEC_W * 1.6, 20.0], num(v, speed)).on_hover_text(tip);
    unit(ui, u);
}

/// Three optional components: a checkbox each (ticked: prescribed) and its value, or "free".
fn row_opt3(ui: &mut Ui, name: &str, tip: &str, v: &mut [Option<f64>; 3], u: &str) {
    label_u(ui, name, u, tip);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 1.0;
        ui.spacing_mut().icon_spacing = 0.0;
        for (k, x) in v.iter_mut().enumerate() {
            let axis = ["x", "y", "z"][k];
            if tick(ui, x.is_some()).on_hover_text(format!("{axis}: ticked = prescribed, unticked = free")).clicked() {
                *x = if x.is_some() { None } else { Some(0.0) };
            }
            match x {
                Some(val) => {
                    ui.add_sized([VEC_W - 16.0, 20.0], num(val, 0.001).prefix(format!("{axis} ")).custom_formatter(|v, _| trim(v))).on_hover_text(format!("{axis}: {tip}"));
                }
                None => {
                    ui.add_sized([VEC_W - 16.0, 20.0], egui::Label::new(RichText::new(format!("{axis} free")).color(theme::muted()).size(10.5)));
                }
            }
        }
    });
    unit(ui, u);
}

/// A compact checkbox (the sidebar has no room for egui's, three to a row).
fn tick(ui: &mut Ui, on: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(14.0, 20.0), egui::Sense::click());
    let r = egui::Rect::from_center_size(rect.center(), egui::vec2(12.0, 12.0));
    let p = ui.painter();
    if on {
        p.rect_filled(r, 3.0, theme::accent());
        icons::image("check", 11.0).tint(Color32::WHITE).paint_at(ui, egui::Rect::from_center_size(r.center(), egui::vec2(11.0, 11.0)));
    } else {
        p.rect(r, 3.0, theme::input_fill(), Stroke::new(1.0, if resp.hovered() { theme::text() } else { theme::border() }), egui::StrokeKind::Inside);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small icon button; `on` draws it pressed.
fn icon_button(ui: &mut Ui, icon: &str, tip: &str, on: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
    let resp = resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand);
    if on {
        ui.painter().rect_filled(rect, 5.0, theme::lerp(theme::window(), theme::accent(), 0.22));
    } else if resp.hovered() {
        ui.painter().rect(rect, 5.0, ui.visuals().widgets.hovered.weak_bg_fill, Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
    }
    let col = if on { theme::accent() } else if resp.hovered() { theme::text() } else { theme::muted() };
    icons::image(icon, 14.0).tint(col).paint_at(ui, egui::Rect::from_center_size(rect.center(), egui::vec2(14.0, 14.0)));
    resp
}

/// The "Pick faces" / "Picking - click faces" button of an open card.
fn pick_button(ui: &mut Ui, on: bool) -> egui::Response {
    let text = if on { "Picking – click faces" } else { "Pick faces" };
    let col = if on { Color32::WHITE } else { theme::text() };
    let mut b = egui::Button::image_and_text(icons::image("select", 13.0).tint(col), RichText::new(text).color(col).size(12.0));
    if on {
        b = b.fill(theme::accent());
    }
    ui.add(b).on_hover_text("Then click faces in the 3D view to add or remove them. Click again (or Esc) to stop.")
}

/// What a card's header asked for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Toggle,
    Pick,
    PickPoint,
    Up,
    Down,
    Dup,
    Del,
    Suppress,
}

struct Head<'a> {
    id: u64,
    key: &'a str,
    name: &'a str,
    detail: String,
    suppressed: bool,
    /// Picks faces (else a point, or nothing for gravity).
    faces: bool,
    point: bool,
    first: bool,
    last: bool,
}

/// One BC as a card: the header (caret, swatch, name, type · faces, buttons) and, when open, `body`.
fn card(app: &App, ui: &mut Ui, h: &Head, body: impl FnOnce(&mut Ui)) -> Option<(Act, u64)> {
    let mut act = None;
    let act = &mut act;
    let open = app.fea.open == Some(h.id);
    let picking = app.fea.sel == Some(h.id);
    let point_picking = app.fea.point_pick == Some(h.id);
    let stroke = if picking || point_picking { Stroke::new(1.5, theme::accent()) } else { Stroke::new(1.0, theme::border()) };
    let frame = egui::Frame::new().fill(theme::window()).stroke(stroke).corner_radius(8).inner_margin(egui::Margin::symmetric(6, 4));
    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        let mut title_rect = egui::Rect::NOTHING;
        ui.horizontal(|ui| {
            // the buttons first (right to left), then the title in what is left, truncated
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if icon_button(ui, "delete", "Remove", false).clicked() {
                    *act = Some((Act::Del, h.id));
                }
                if icon_button(ui, "copy", "Duplicate (same faces and values)", false).clicked() {
                    *act = Some((Act::Dup, h.id));
                }
                if !h.last && icon_button(ui, "down", "Move down", false).clicked() {
                    *act = Some((Act::Down, h.id));
                }
                if !h.first && icon_button(ui, "up", "Move up", false).clicked() {
                    *act = Some((Act::Up, h.id));
                }
                if h.point {
                    if icon_button(ui, "pin", "Pick this BC's point in the 3D view", point_picking).clicked() {
                        *act = Some((Act::PickPoint, h.id));
                    }
                } else if h.faces && icon_button(ui, "select", "Pick this BC's faces in the 3D view", picking).clicked() {
                    *act = Some((Act::Pick, h.id));
                }
                title_rect = ui
                    .with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        ui.add(icons::image(if open { "chevronDown" } else { "chevronRight" }, 12.0).tint(theme::muted()));
                        let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, 3.0, if h.suppressed { theme::muted() } else { color(h.key) });
                        let mut name = RichText::new(h.name).color(theme::text()).size(12.5).family(theme::medium());
                        if h.suppressed {
                            name = name.color(theme::muted()).strikethrough();
                        }
                        ui.add(egui::Label::new(name).truncate());
                        ui.add(egui::Label::new(muted(&h.detail)).truncate());
                    })
                    .response
                    .rect;
            });
        });
        let r = ui.interact(title_rect, ui.id().with(("bc_hdr", h.id)), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        if r.clicked() {
            *act = Some((Act::Toggle, h.id));
        }
        r.context_menu(|ui| {
            if ui.button(if h.suppressed { "Unsuppress" } else { "Suppress" }).clicked() {
                *act = Some((Act::Suppress, h.id));
                ui.close();
            }
            if ui.button(RichText::new("Delete").color(theme::error_soft())).clicked() {
                *act = Some((Act::Del, h.id));
                ui.close();
            }
        });
        if open {
            ui.add_space(4.0);
            body(ui);
            ui.add_space(2.0);
        }
    });
    ui.add_space(4.0);
    *act
}

/// The Faces row of an open card: the pick button, Clear, and what is picked.
fn faces_rows(ui: &mut Ui, picking: bool, n: usize, area: f64, act: &mut Option<(Act, u64)>, clear: &mut bool, id: u64) {
    label(ui, "Faces", "The faces this BC acts on. Pick them in the 3D view.");
    ui.horizontal(|ui| {
        if pick_button(ui, picking).clicked() {
            *act = Some((Act::Pick, id));
        }
        if n > 0 && ui.button(RichText::new("Clear").size(12.0)).on_hover_text("Remove every face from this BC.").clicked() {
            *clear = true;
        }
    });
    unit(ui, "");
    ui.label("");
    let a = if area > 0.0 { format!("  ·  {} mm²", super::sig(area)) } else { String::new() };
    ui.label(muted(format!("{}{a}", super::faces_text(n))));
    unit(ui, "");
}

/// Runs a header action on the study and the panel state.
fn apply(app: &mut App, a: Act, id: u64) {
    match a {
        Act::Toggle => {
            app.fea.open = if app.fea.open == Some(id) { None } else { Some(id) };
            if app.fea.open.is_none() && app.fea.sel == Some(id) {
                app.fea.sel = None;
            }
            if app.fea.open.is_some() {
                app.fea.show_results = false;
            }
        }
        Act::Pick => {
            app.fea.sel = if app.fea.sel == Some(id) { None } else { Some(id) };
            app.fea.point_pick = None;
            app.fea.open = Some(id);
            app.fea.side_b = false;
            app.fea.show_results = app.fea.sel.is_none() && app.fea.open.is_none();
        }
        Act::PickPoint => {
            app.fea.point_pick = if app.fea.point_pick == Some(id) { None } else { Some(id) };
            app.fea.sel = None;
            app.fea.open = Some(id);
            app.fea.show_results = false;
        }
        Act::Del => {
            edit(app, |s| {
                s.supports.retain(|x| x.id != id);
                s.loads.retain(|x| x.id != id);
                s.advanced.bcs.retain(|x| x.id != id);
            });
            for v in [&mut app.fea.sel, &mut app.fea.open, &mut app.fea.point_pick] {
                if *v == Some(id) {
                    *v = None;
                }
            }
        }
        Act::Suppress => edit(app, |s| {
            if let Some(x) = s.support_mut(id) {
                x.suppressed = !x.suppressed;
            }
            if let Some(x) = s.load_mut(id) {
                x.suppressed = !x.suppressed;
            }
            if let Some(x) = s.advanced.bc_mut(id) {
                x.suppressed = !x.suppressed;
            }
        }),
        Act::Up | Act::Down => edit(app, |s| {
            fn mv<T>(v: &mut [T], i: usize, up: bool) {
                let j = if up { i.wrapping_sub(1) } else { i + 1 };
                if j < v.len() {
                    v.swap(i, j);
                }
            }
            let up = a == Act::Up;
            if let Some(i) = s.supports.iter().position(|x| x.id == id) {
                mv(&mut s.supports, i, up);
            } else if let Some(i) = s.loads.iter().position(|x| x.id == id) {
                mv(&mut s.loads, i, up);
            } else if let Some(i) = s.advanced.bcs.iter().position(|x| x.id == id) {
                mv(&mut s.advanced.bcs, i, up);
            }
        }),
        Act::Dup => {
            let mut new = None;
            edit(app, |s| new = duplicate(s, id));
            if new.is_some() {
                app.fea.open = new;
                app.fea.sel = None;
            }
        }
    }
    app.fea.scene_dirty = true;
}

/// A copy of a BC under a fresh id and name, right after it.
fn duplicate(s: &mut Study, id: u64) -> Option<u64> {
    let names: Vec<String> = s.supports.iter().map(|x| x.name.clone()).chain(s.loads.iter().map(|x| x.name.clone())).chain(s.advanced.bcs.iter().map(|x| x.name.clone())).collect();
    let fresh = |n: &str| {
        let base = n.trim_end_matches(|c: char| c.is_ascii_digit()).trim_end();
        (2..).map(|k| format!("{base} {k}")).find(|c| !names.contains(c)).unwrap()
    };
    if let Some(i) = s.supports.iter().position(|x| x.id == id) {
        let mut b = s.supports[i].clone();
        b.name = fresh(&b.name);
        let nid = s.add_support(b.kind);
        s.supports.pop();
        b.id = nid;
        s.supports.insert(i + 1, b);
        return Some(nid);
    }
    if let Some(i) = s.loads.iter().position(|x| x.id == id) {
        let mut b = s.loads[i].clone();
        b.name = fresh(&b.name);
        let nid = s.add_load(b.kind);
        s.loads.pop();
        b.id = nid;
        s.loads.insert(i + 1, b);
        return Some(nid);
    }
    if let Some(i) = s.advanced.bcs.iter().position(|x| x.id == id) {
        let mut b = s.advanced.bcs[i].clone();
        b.name = fresh(&b.name);
        let nid = s.add_adv_bc(b.bc.key())?;
        s.advanced.bcs.pop();
        b.id = nid;
        s.advanced.bcs.insert(i + 1, b);
        return Some(nid);
    }
    None
}

/// Arms picking for a BC just added (or its point), and opens its card.
fn added(app: &mut App, id: u64, faces: bool, point: bool) {
    app.fea.open = Some(id);
    app.fea.sel = faces.then_some(id);
    app.fea.point_pick = point.then_some(id);
    app.fea.side_b = false;
    app.fea.show_results = false;
    app.fea.scene_dirty = true;
}

fn callout(ui: &mut Ui) {
    ui.add_space(2.0);
    ui.label(muted(
        "Click a BC's pointer button (or Pick faces), then click faces in the view to add or remove them; Esc stops. With nothing armed, clicking a face opens the BC that owns it.",
    ));
}

// ---------------------------------------------------------------- Simple mode

const DIRS: [(&str, [f64; 3]); 6] =
    [("−Z (down)", [0.0, 0.0, -1.0]), ("+Z", [0.0, 0.0, 1.0]), ("−X", [-1.0, 0.0, 0.0]), ("+X", [1.0, 0.0, 0.0]), ("−Y", [0.0, -1.0, 0.0]), ("+Y", [0.0, 1.0, 0.0])];

fn type_combo_simple(ui: &mut Ui, id: u64, cur: Kind) -> Option<Kind> {
    let mut out = None;
    egui::ComboBox::from_id_salt(("fea-bc-type", id)).width(128.0).selected_text(cur.label()).show_ui(ui, |ui| {
        for (title, sup) in [("Supports", true), ("Loads", false)] {
            ui.label(muted(title));
            for k in Kind::ALL.into_iter().filter(|k| k.support() == sup) {
                if ui.selectable_label(k == cur, k.label()).on_hover_text(k.tip()).clicked() && k != cur {
                    out = Some(k);
                }
            }
            if sup {
                ui.separator();
            }
        }
    });
    out
}

/// Changes a support or load's type, moving it between the lists when it changes group.
fn retype_simple(s: &mut Study, id: u64, to: Kind) {
    let rename = |name: &str, from: &str, to: &str| if name.starts_with(from) { name.replacen(from, to, 1) } else { name.to_string() };
    if let Some(i) = s.supports.iter().position(|x| x.id == id) {
        match to {
            Kind::Support(k) => {
                let sp = &mut s.supports[i];
                sp.name = rename(&sp.name, sp.kind.label(), k.label());
                sp.kind = k;
            }
            Kind::Load(k) => {
                let sp = s.supports.remove(i);
                let value = match k {
                    LoadKind::Force => 100.0,
                    LoadKind::Pressure => 0.1,
                    LoadKind::Gravity => 1.0,
                };
                let name = rename(&sp.name, sp.kind.label(), k.label());
                s.loads.push(fea::Load { id, name, kind: k, faces: sp.faces, value, normal: k == LoadKind::Pressure, suppressed: sp.suppressed, ..Default::default() });
            }
        }
    } else if let Some(i) = s.loads.iter().position(|x| x.id == id) {
        match to {
            Kind::Load(k) => {
                let l = &mut s.loads[i];
                l.name = rename(&l.name, l.kind.label(), k.label());
                if l.kind != k {
                    l.value = match k {
                        LoadKind::Force => 100.0,
                        LoadKind::Pressure => 0.1,
                        LoadKind::Gravity => 1.0,
                    };
                }
                l.normal = k == LoadKind::Pressure || (k == LoadKind::Force && l.normal);
                l.kind = k;
            }
            Kind::Support(k) => {
                let l = s.loads.remove(i);
                let name = rename(&l.name, l.kind.label(), k.label());
                s.supports.push(fea::Support { id, name, kind: k, faces: l.faces, suppressed: l.suppressed });
            }
        }
    }
}

/// The Simple mode's "Boundary conditions" section.
pub(super) fn simple(app: &mut App, ui: &mut Ui) {
    let Some(si) = index(app) else { return };
    let geo = geo(app);
    let s0 = app.ws.sims[si].clone();
    let mut act = None;
    let mut retype: Option<(u64, Kind)> = None;
    let mut clear: Option<u64> = None;
    if s0.supports.is_empty() && s0.loads.is_empty() {
        ui.label(muted("None yet: add a support and a load."));
    }
    let ns = s0.supports.len();
    for (i, sp) in s0.supports.iter().enumerate() {
        let ids = faces_of(app, sp.id);
        let h = Head {
            id: sp.id,
            key: Kind::Support(sp.kind).key(),
            name: &sp.name,
            detail: format!("{} · {}", sp.kind.label(), super::faces_text(sp.faces.len())),
            suppressed: sp.suppressed,
            faces: true,
            point: false,
            first: i == 0,
            last: i + 1 == ns,
        };
        let picking = app.fea.sel == Some(sp.id);
        let mut cl = false;
        let mut name = sp.name.clone();
        let head = card(app, ui, &h, |ui| {
            grid(ui, ("fea-bc", sp.id), |ui| {
                label(ui, "Name", "Shown in the reactions and in messages.");
                ui.add(egui::TextEdit::singleline(&mut name).desired_width(128.0));
                unit(ui, "");
                label(ui, "Type", Kind::Support(sp.kind).tip());
                if let Some(k) = type_combo_simple(ui, sp.id, Kind::Support(sp.kind)) {
                    retype = Some((sp.id, k));
                }
                unit(ui, "");
                faces_rows(ui, picking, sp.faces.len(), geo.area(&ids), &mut act, &mut cl, sp.id);
            });
        });
        act = head.or(act);
        if cl {
            clear = Some(sp.id);
        }
        if name != sp.name {
            if let Some(x) = app.ws.sims[si].support_mut(sp.id) {
                x.name = name;
            }
        }
    }
    let nl = s0.loads.len();
    for (i, l) in s0.loads.iter().enumerate() {
        let ids = faces_of(app, l.id);
        let v = format!("{}{}", trim(l.value), l.kind.unit());
        let gravity = l.kind == LoadKind::Gravity;
        let h = Head {
            id: l.id,
            key: Kind::Load(l.kind).key(),
            name: &l.name,
            detail: if gravity { format!("{} · {v}", l.kind.label()) } else { format!("{} {v} · {}", l.kind.label(), super::faces_text(l.faces.len())) },
            suppressed: l.suppressed,
            faces: !gravity,
            point: false,
            first: i == 0,
            last: i + 1 == nl,
        };
        let picking = app.fea.sel == Some(l.id);
        let mut cl = false;
        let mut q = l.clone();
        let head = card(app, ui, &h, |ui| {
            grid(ui, ("fea-bc", l.id), |ui| {
                label(ui, "Name", "Shown in the reactions and in messages.");
                ui.add(egui::TextEdit::singleline(&mut q.name).desired_width(128.0));
                unit(ui, "");
                label(ui, "Type", Kind::Load(l.kind).tip());
                if let Some(k) = type_combo_simple(ui, l.id, Kind::Load(l.kind)) {
                    retype = Some((l.id, k));
                }
                unit(ui, "");
                let (name, speed, u, tip) = match l.kind {
                    LoadKind::Force => ("Force", 1.0, "N", "The total force, spread evenly over the faces' area."),
                    LoadKind::Pressure => ("Pressure", 0.01, "MPa", "Pushes into the faces (negative pulls). 1 MPa = 10 bar = 145 psi."),
                    LoadKind::Gravity => ("Acceleration", 0.05, "g", "Acts on the whole part: 1 g is its own weight."),
                };
                row_f64(ui, name, tip, &mut q.value, u, speed);
                if l.kind != LoadKind::Pressure {
                    label(ui, "Direction", "Which way the force or acceleration acts (part coordinates).");
                    let text = if q.normal {
                        "Into the faces".to_string()
                    } else {
                        DIRS.iter().find(|d| d.1 == q.direction).map(|d| d.0.to_string()).unwrap_or_else(|| "Custom".into())
                    };
                    egui::ComboBox::from_id_salt(("fea-dir", l.id)).width(128.0).selected_text(text).show_ui(ui, |ui| {
                        if l.kind == LoadKind::Force && ui.selectable_label(q.normal, "Into the faces").clicked() {
                            q.normal = true;
                        }
                        for (n, d) in DIRS {
                            if ui.selectable_label(!q.normal && q.direction == d, n).clicked() {
                                q.normal = false;
                                q.direction = d;
                            }
                        }
                    });
                    unit(ui, "");
                    if !q.normal {
                        row_vec3(ui, "", "Direction vector (it is normalised).", &mut q.direction, "", 0.01);
                    }
                    if l.kind == LoadKind::Force && !q.normal {
                        let a = geo.area(&ids);
                        if a > 0.0 {
                            ui.label("");
                            ui.label(muted(format!("= {} MPa over {} mm²", super::sig(q.value.abs() / a), super::sig(a))));
                            unit(ui, "");
                        }
                    }
                }
                if !gravity {
                    faces_rows(ui, picking, l.faces.len(), geo.area(&ids), &mut act, &mut cl, l.id);
                }
            });
        });
        act = head.or(act);
        if cl {
            clear = Some(l.id);
        }
        if q != *l {
            if let Some(x) = app.ws.sims[si].load_mut(l.id) {
                *x = q;
            }
        }
    }
    if let Some((id, k)) = retype {
        retype_simple(&mut app.ws.sims[si], id, k);
        app.fea.open = Some(id);
        app.fea.sel = None;
        app.fea.scene_dirty = true;
    }
    if let Some(id) = clear {
        if let Some(f) = app.ws.sims[si].faces_mut(id) {
            f.clear();
        }
    }
    if let Some((a, id)) = act {
        apply(app, a, id);
    }
    // Add BC
    let mut add = None;
    ui.menu_button(RichText::new("+  Add BC").size(12.5).family(theme::medium()), |ui| {
        for (title, sup) in [("Supports", true), ("Loads", false)] {
            ui.label(muted(title));
            for k in Kind::ALL.into_iter().filter(|k| k.support() == sup) {
                if ui.button(k.label()).on_hover_text(k.tip()).clicked() {
                    add = Some(k);
                    ui.close();
                }
            }
            if sup {
                ui.separator();
            }
        }
    })
    .response
    .on_hover_text("Add a boundary condition; its faces are picked next.");
    if let Some(k) = add {
        let mut id = 0;
        edit(app, |s| {
            id = match k {
                Kind::Support(k) => s.add_support(k),
                Kind::Load(k) => s.add_load(k),
            }
        });
        added(app, id, k != Kind::Load(LoadKind::Gravity), false);
    }
    ui.add_space(4.0);
    if let Some(s) = study(app).cloned() {
        restraint_chip(app, ui, &s);
    }
    callout(ui);
}

// ---------------------------------------------------------------- Advanced mode

fn type_combo_adv(ui: &mut Ui, id: u64, cur: &str) -> Option<&'static str> {
    let mut out = None;
    egui::ComboBox::from_id_salt(("fea-adv-type", id)).width(128.0).selected_text(BcType::label_of(cur)).show_ui(ui, |ui| {
        for (title, keys) in [("Supports", ADV_SUPPORTS), ("Loads", ADV_LOADS)] {
            ui.label(muted(title));
            for k in keys {
                if ui.selectable_label(k == cur, BcType::label_of(k)).on_hover_text(adv_tip(k)).clicked() && k != cur {
                    out = Some(k);
                }
            }
            if title == "Supports" {
                ui.separator();
            }
        }
    });
    out
}

fn short(b: &BcType) -> String {
    match b {
        BcType::RemoteRigid { .. } => "Remote, rigid".into(),
        BcType::RemoteDistributing { .. } => "Remote, distributing".into(),
        _ => b.label().split(" (").next().unwrap_or("").to_string(),
    }
}

fn fmt_pt(p: &[f64; 3]) -> String {
    format!("at ({}, {}, {})", trim(p[0]), trim(p[1]), trim(p[2]))
}

/// The point a BC acts at or is coupled to.
fn point_mut(b: &mut BcType) -> Option<&mut [f64; 3]> {
    match b {
        BcType::RemoteRigid { point, .. } | BcType::RemoteDistributing { point, .. } => Some(point),
        BcType::PointForce { at, .. } => Some(at),
        _ => None,
    }
}

/// Puts a BC's point where the user clicked (point picking).
pub(super) fn set_point(app: &mut App, id: u64, p: [f64; 3]) {
    edit(app, |s| {
        if let Some(b) = s.advanced.bc_mut(id) {
            if let Some(pt) = point_mut(&mut b.bc) {
                *pt = p.map(|x| (x * 100.0).round() / 100.0);
            }
        }
    });
    app.fea.point_pick = None;
    app.fea.scene_dirty = true;
}

/// The Advanced mode's "Boundary conditions" section.
pub(super) fn advanced(app: &mut App, ui: &mut Ui) {
    let Some(si) = index(app) else { return };
    let geo = geo(app);
    let s0 = app.ws.sims[si].clone();
    let mut act = None;
    let mut retype: Option<(u64, &str)> = None;
    if s0.advanced.bcs.is_empty() {
        ui.label(muted("None yet: add a support and a load."));
    }
    let n = s0.advanced.bcs.len();
    for (i, b) in s0.advanced.bcs.iter().enumerate() {
        let ids = faces_of(app, b.id);
        let on_faces = b.bc.on_faces();
        let target = match &b.bc {
            BcType::PointForce { at, .. } => fmt_pt(at),
            _ => super::faces_text(b.faces.len()),
        };
        let h = Head {
            id: b.id,
            key: b.bc.key(),
            name: &b.name,
            detail: format!("{} · {target}", short(&b.bc)),
            suppressed: b.suppressed,
            faces: on_faces,
            point: !on_faces,
            first: i == 0,
            last: i + 1 == n,
        };
        let picking = app.fea.sel == Some(b.id);
        let point_picking = app.fea.point_pick == Some(b.id);
        let centroid = geo.centroid(&ids);
        let area = geo.area(&ids);
        let mut q = b.clone();
        let mut cl = false;
        let head = card(app, ui, &h, |ui| {
            grid(ui, ("fea-adv-bc", b.id), |ui| {
                label(ui, "Name", "Shown in the reactions and in messages.");
                ui.add(egui::TextEdit::singleline(&mut q.name).desired_width(128.0));
                unit(ui, "");
                label(ui, "Type", adv_tip(b.bc.key()));
                if let Some(k) = type_combo_adv(ui, b.id, b.bc.key()) {
                    retype = Some((b.id, k));
                }
                unit(ui, "");
                let has_point = point_mut(&mut q.bc).is_some();
                match &mut q.bc {
                    BcType::Fixed | BcType::Symmetry => {}
                    BcType::Displacement { u } => row_opt3(ui, "u", "Prescribed displacement per component; an unticked component is free.", u, "mm"),
                    BcType::Pressure { p } => row_f64(ui, "p", "Pressure on the faces, pushing along the inward normal (positive presses in, negative pulls).", p, "MPa", 0.01),
                    BcType::Traction { t } => row_vec3(ui, "t", "Traction vector: force per area, the same at every point of the faces.", t, "MPa", 0.01),
                    BcType::Force { f } => {
                        row_vec3(ui, "F", "Total force on the faces, spread as a uniform traction F / area.", f, "N", 1.0);
                        if area > 0.0 {
                            let mag = DVec3::from(*f).length();
                            ui.label("");
                            ui.label(muted(format!("= {} MPa over {} mm²", super::sig(mag / area), super::sig(area))));
                            unit(ui, "");
                        }
                    }
                    BcType::Spring { k } => row_vec3(ui, "k", "Foundation stiffness per unit area along x, y, z: traction = -k u.", k, "N/mm³", 0.1),
                    BcType::RemoteRigid { force, moment, u, rot, .. } => {
                        row_vec3(ui, "Force", "Force applied at the reference point.", force, "N", 1.0);
                        row_vec3(ui, "Moment", "Moment applied at the reference point.", moment, "N·mm", 10.0);
                        row_opt3(ui, "u", "Displacement: prescribed displacement of the reference point; unticked = free (loaded by Force).", u, "mm");
                        row_opt3(ui, "θ", "Rotation: prescribed rotation of the reference point about x, y, z (small angles); unticked = free (loaded by Moment).", rot, "rad");
                    }
                    BcType::RemoteDistributing { force, moment, .. } => {
                        row_vec3(ui, "Force", "Force applied at the reference point.", force, "N", 1.0);
                        row_vec3(ui, "Moment", "Moment applied at the reference point.", moment, "N·mm", 10.0);
                    }
                    BcType::PointForce { f, .. } => row_vec3(ui, "F", "Concentrated force on the node, N.", f, "N", 1.0),
                }
                if has_point {
                    let is_ref = !matches!(q.bc, BcType::PointForce { .. });
                    let (name, tip) = if is_ref {
                        ("Point", "The reference point the faces couple to, and that the moment is taken about. Type it, pick it on the part, or put it at the faces' centroid.")
                    } else {
                        ("Point", "Where the force acts: the mesh node nearest this point. Type the coordinates, or pick it in the 3D view.")
                    };
                    label_u(ui, name, "mm", tip);
                    if let Some(p) = point_mut(&mut q.bc) {
                        vec3(ui, p, 0.5, tip);
                    }
                    unit(ui, "mm");
                    ui.label("");
                    ui.horizontal(|ui| {
                        if icon_button(ui, "pin", "Pick point: click the part in the 3D view (Esc cancels).", point_picking).clicked() {
                            act = Some((Act::PickPoint, b.id));
                        }
                        if is_ref {
                            let r = icon_button(ui, "target", "Place it at the centroid of this BC's faces (area-weighted).", false);
                            if r.clicked() {
                                if let (Some(c), Some(p)) = (centroid, point_mut(&mut q.bc)) {
                                    *p = c.to_array().map(|x| (x * 1e4).round() / 1e4);
                                }
                            }
                        }
                    });
                    unit(ui, "");
                }
                if on_faces {
                    faces_rows(ui, picking, b.faces.len(), area, &mut act, &mut cl, b.id);
                }
            });
        });
        act = head.or(act);
        if cl {
            q.faces.clear();
        }
        if q != *b {
            if let Some(x) = app.ws.sims[si].advanced.bc_mut(b.id) {
                *x = q;
            }
        }
    }
    if let Some((id, k)) = retype {
        if let (Some(b), Some(new)) = (app.ws.sims[si].advanced.bc_mut(id), BcType::new(k)) {
            let old = BcType::label_of(b.bc.key()).split(" (").next().unwrap_or("").to_string();
            let to = BcType::label_of(k).split(" (").next().unwrap_or("");
            if b.name.starts_with(&old) {
                b.name = b.name.replacen(&old, to, 1);
            }
            // keep the point when both have one
            let mut new = new;
            if let (Some(p), Some(q)) = (point_mut(&mut b.bc).copied(), point_mut(&mut new)) {
                *q = p;
            }
            b.bc = new;
            if !b.bc.on_faces() {
                b.faces.clear();
            }
        }
        app.fea.open = Some(id);
        app.fea.sel = None;
        app.fea.point_pick = None;
        app.fea.scene_dirty = true;
    }
    if let Some((a, id)) = act {
        apply(app, a, id);
    }
    let mut add = None;
    ui.menu_button(RichText::new("+  Add BC").size(12.5).family(theme::medium()), |ui| {
        for (title, keys) in [("Supports", ADV_SUPPORTS), ("Loads", ADV_LOADS)] {
            ui.label(muted(title));
            for k in keys {
                if ui.button(BcType::label_of(k)).on_hover_text(adv_tip(k)).clicked() {
                    add = Some(k);
                    ui.close();
                }
            }
            if title == "Supports" {
                ui.separator();
            }
        }
    })
    .response
    .on_hover_text("Add a boundary condition; its faces are picked next.");
    if let Some(k) = add {
        let mut id = None;
        let c = geo.centre.to_array().map(|x| (x * 1e3).round() / 1e3);
        edit(app, |s| {
            id = s.add_adv_bc(k);
            // a new point starts at the middle of the parts
            if let Some(b) = id.and_then(|id| s.advanced.bc_mut(id)) {
                if let Some(p) = point_mut(&mut b.bc) {
                    *p = c;
                }
            }
        });
        if let Some(id) = id {
            let point = k == "point_force";
            added(app, id, !point, point);
        }
    }
    ui.add_space(4.0);
    if let Some(s) = study(app).cloned() {
        restraint_chip(app, ui, &s);
    }
    // body force
    ui.add_space(4.0);
    let mut g = s0.advanced.gravity;
    grid(ui, "fea-adv-body-force", |ui| {
        label_u(ui, "Body force", "g", "An acceleration of the whole part, in g along x, y, z: (0, 0, -1) is its own weight.");
        ui.horizontal(|ui| {
            let mut on = g.is_some();
            if ui.checkbox(&mut on, "").on_hover_text("Unticked: none.").changed() {
                g = if on { Some([0.0, 0.0, -1.0]) } else { None };
            }
            match &mut g {
                Some(v) => vec3(ui, v, 0.05, "Acceleration in g."),
                None => {
                    ui.label(muted("none"));
                }
            }
        });
        unit(ui, "g");
    });
    if g != s0.advanced.gravity {
        app.ws.sims[si].advanced.gravity = g;
    }
    callout(ui);
}

// ---------------------------------------------------------------- viewport

fn seg(sd: &mut SceneData, a: DVec3, b: DVec3, c: [f32; 4]) {
    sd.lines_top.push(LineInst { a: a.as_vec3().into(), b: b.as_vec3().into(), width: 1.6, bias: 0.0, color: c });
}

fn perp(n: DVec3) -> (DVec3, DVec3) {
    let t = if n.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    let u = n.cross(t).normalize_or_zero();
    (u, n.cross(u))
}

/// An arrow from `a` to `b` with a 4-line head.
fn arrow(sd: &mut SceneData, a: DVec3, b: DVec3, c: [f32; 4]) {
    seg(sd, a, b, c);
    let d = b - a;
    let l = d.length();
    if l <= 0.0 {
        return;
    }
    let (u, v) = perp(d / l);
    let back = b - d * 0.28;
    for w in [u, -u, v, -v] {
        seg(sd, b, back + w * l * 0.12, c);
    }
}

/// A support's ground mark: a triangle with its tip on the face and a hatched base.
fn ground(sd: &mut SceneData, x: DVec3, n: DVec3, s: f64, c: [f32; 4]) {
    let (u, _) = perp(n);
    let base = x + n * s * 0.6;
    let (p, q) = (base + u * s * 0.35, base - u * s * 0.35);
    seg(sd, x, p, c);
    seg(sd, x, q, c);
    seg(sd, p, q, c);
    for k in 0..4 {
        let t = -0.35 + 0.7 * (k as f64 + 0.5) / 4.0;
        let a = base + u * s * t;
        seg(sd, a, a + n * s * 0.18 + u * s * 0.12, c);
    }
}

/// A symmetry (roller) mark: a short stem along the normal and two bars.
fn symmetry(sd: &mut SceneData, x: DVec3, n: DVec3, s: f64, c: [f32; 4]) {
    let (u, _) = perp(n);
    seg(sd, x, x + n * s * 0.45, c);
    for h in [0.45, 0.6] {
        let m = x + n * s * h;
        seg(sd, m - u * s * 0.25, m + u * s * 0.25, c);
    }
}

/// A spring: a zigzag along the normal with a ground bar at its end.
fn spring(sd: &mut SceneData, x: DVec3, n: DVec3, s: f64, c: [f32; 4]) {
    let (u, _) = perp(n);
    let mut prev = x;
    let k = 6;
    for i in 1..=k {
        let along = x + n * s * 0.7 * (i as f64 / k as f64);
        let side = if i == k { 0.0 } else if i % 2 == 1 { 0.15 } else { -0.15 };
        let p = along + u * s * side;
        seg(sd, prev, p, c);
        prev = p;
    }
    seg(sd, prev - u * s * 0.25, prev + u * s * 0.25, c);
}

/// A point as a small diamond.
fn diamond(sd: &mut SceneData, p: DVec3, s: f64, c: [f32; 4]) {
    let r = s * 0.18;
    let q = [p + DVec3::X * r, p + DVec3::Y * r, p - DVec3::X * r, p - DVec3::Y * r];
    for k in 0..4 {
        seg(sd, q[k], q[(k + 1) % 4], c);
        seg(sd, q[k], p + DVec3::Z * r, c);
        seg(sd, q[k], p - DVec3::Z * r, c);
    }
}

/// The face colour of each BC (by global face) for the setup view.
pub(super) fn tints(app: &App, s: &Study) -> HashMap<u32, Color32> {
    let mut tint = HashMap::new();
    let mut put = |id: u64, key: &str| {
        for f in faces_of(app, id) {
            tint.insert(f, color(key));
        }
    };
    if s.mode == fea::SimMode::Advanced {
        for b in s.advanced.bcs.iter().filter(|x| !x.suppressed) {
            put(b.id, b.bc.key());
        }
    } else {
        for sp in s.supports.iter().filter(|x| !x.suppressed) {
            put(sp.id, Kind::Support(sp.kind).key());
        }
        for l in s.loads.iter().filter(|x| !x.suppressed) {
            put(l.id, Kind::Load(l.kind).key());
        }
    }
    tint
}

/// Every BC's glyphs, in its type's colour (the armed one in the pick colour, with more of them).
pub(super) fn glyphs(app: &App, s: &Study, geo: &Geo, sd: &mut SceneData) {
    let size = 0.06 * geo.diag.max(1e-3);
    let rgba = |key: &str, id: u64| {
        let c = if app.fea.sel == Some(id) || app.fea.point_pick == Some(id) { theme::vp::picked() } else { color(key) };
        theme::to_rgba(c, 1.0)
    };
    let per = |id: u64| if app.fea.open == Some(id) || app.fea.sel == Some(id) { 8 } else { 5 };
    if s.mode == fea::SimMode::Advanced {
        for b in s.advanced.bcs.iter().filter(|x| !x.suppressed) {
            let c = rgba(b.bc.key(), b.id);
            let pts = geo.samples(&faces_of(app, b.id), per(b.id));
            match &b.bc {
                BcType::Fixed => pts.iter().for_each(|(x, n)| ground(sd, *x, *n, size, c)),
                BcType::Symmetry => pts.iter().for_each(|(x, n)| symmetry(sd, *x, *n, size, c)),
                BcType::Spring { .. } => pts.iter().for_each(|(x, n)| spring(sd, *x, *n, size, c)),
                BcType::Pressure { p } => {
                    for (x, n) in &pts {
                        if *p >= 0.0 {
                            arrow(sd, *x + *n * size, *x, c);
                        } else {
                            arrow(sd, *x, *x + *n * size, c);
                        }
                    }
                }
                BcType::Traction { t: v } | BcType::Force { f: v } => {
                    let d = DVec3::from(*v).normalize_or_zero();
                    if d != DVec3::ZERO {
                        pts.iter().for_each(|(x, _)| arrow(sd, *x, *x + d * size, c));
                    }
                }
                BcType::Displacement { u } => {
                    let d = DVec3::from(u.map(|x| x.unwrap_or(0.0))).normalize_or_zero();
                    for (x, n) in &pts {
                        if d != DVec3::ZERO {
                            arrow(sd, *x, *x + d * size, c);
                        }
                        if u.iter().any(|x| *x == Some(0.0)) {
                            ground(sd, *x, *n, size * 0.7, c);
                        }
                    }
                }
                BcType::RemoteRigid { point, force, .. } | BcType::RemoteDistributing { point, force, .. } => {
                    let p = DVec3::from(*point);
                    for (x, _) in &pts {
                        seg(sd, p, *x, c);
                    }
                    diamond(sd, p, size, c);
                    let d = DVec3::from(*force).normalize_or_zero();
                    if d != DVec3::ZERO {
                        arrow(sd, p, p + d * size * 1.4, c);
                    }
                }
                BcType::PointForce { at, f } => {
                    let p = DVec3::from(*at);
                    diamond(sd, p, size, c);
                    let d = DVec3::from(*f).normalize_or_zero();
                    if d != DVec3::ZERO {
                        arrow(sd, p - d * size * 1.4, p, c);
                    }
                }
            }
        }
        return;
    }
    for sp in s.supports.iter().filter(|x| !x.suppressed) {
        let c = rgba(Kind::Support(sp.kind).key(), sp.id);
        for (x, n) in geo.samples(&faces_of(app, sp.id), per(sp.id)) {
            match sp.kind {
                SupportKind::Fixed => ground(sd, x, n, size, c),
                SupportKind::Roller => symmetry(sd, x, n, size, c),
            }
        }
    }
    for l in s.loads.iter().filter(|x| !x.suppressed) {
        let c = rgba(Kind::Load(l.kind).key(), l.id);
        let dir = DVec3::from(l.direction).normalize_or_zero();
        if l.kind == LoadKind::Gravity {
            let p = geo.centre;
            arrow(sd, p - dir * size * 1.5, p, c);
            continue;
        }
        let into = l.normal || l.kind == LoadKind::Pressure;
        let push = l.kind != LoadKind::Pressure || l.value >= 0.0;
        for (x, n) in geo.samples(&faces_of(app, l.id), per(l.id)) {
            if into {
                if push {
                    arrow(sd, x + n * size, x, c);
                } else {
                    arrow(sd, x, x + n * size, c);
                }
            } else if dir != DVec3::ZERO {
                arrow(sd, x - dir * size, x, c);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retyping_moves_a_bc_between_supports_and_loads() {
        let mut s = Study::default();
        let id = s.add_support(SupportKind::Fixed);
        s.supports[0].faces.push(fea::FacePick { point: [1.0, 0.0, 0.0], normal: [1.0, 0.0, 0.0] });
        retype_simple(&mut s, id, Kind::Load(LoadKind::Pressure));
        assert!(s.supports.is_empty());
        let l = &s.loads[0];
        assert_eq!((l.id, l.kind, l.faces.len(), l.name.as_str()), (id, LoadKind::Pressure, 1, "Pressure 1"));
        assert!(l.normal);
        retype_simple(&mut s, id, Kind::Support(SupportKind::Roller));
        assert!(s.loads.is_empty());
        assert_eq!((s.supports[0].kind, s.supports[0].faces.len()), (SupportKind::Roller, 1));
    }

    #[test]
    fn duplicate_goes_right_after_with_a_fresh_id_and_name() {
        let mut s = Study::default();
        let a = s.add_load(LoadKind::Force);
        let b = s.add_load(LoadKind::Force);
        s.loads[0].value = 42.0;
        let c = duplicate(&mut s, a).unwrap();
        assert!(c != a && c != b);
        let names: Vec<&str> = s.loads.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Force 1", "Force 3", "Force 2"]);
        assert_eq!(s.loads[1].value, 42.0);
        let d = s.add_adv_bc("remote_rigid").unwrap();
        let e = duplicate(&mut s, d).unwrap();
        assert_eq!(s.advanced.bcs[1].id, e);
        assert_eq!(s.advanced.bcs[1].bc.key(), "remote_rigid");
    }
}
