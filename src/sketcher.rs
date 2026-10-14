//! Sketch mode: drawing tools, selection, constraints, dimensions and their
//! on-screen presentation.

use std::collections::HashSet;

use egui::{Color32, Key, Pos2, RichText, Stroke, Ui};
use glam::{DVec2, Vec3};

use crate::app::{App, ToastKind};
use crate::doc::{Document, FeatId, Op, OriginPlane, Ref};
use crate::expr::{self, Env, Kind};
use crate::icons;
use crate::regions::{self, find_regions, Region};
use crate::render::SceneData;
use crate::sketch::{arc_angles, Constraint, Curve, Dim, Frame, Geom, Id, SolveStatus, Sketch};
use crate::theme::{self, to_rgba, vp, MUTED, TEXT};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Point,
    Line,
    Rect,
    CenterRect,
    Circle,
    Arc,
    Arc3,
    Slot,
    Polygon,
    Ellipse,
    Spline,
    Text,
    Trim,
    Extend,
    Fillet2d,
    Offset,
    Project,
    Dimension,
}

/// Sketch tool buttons, as `km` in the original.
pub const TOOLS: &[(Tool, &str, &str, Option<&str>)] = &[
    (Tool::Select, "select", "Select", Some("S")),
    (Tool::Point, "point", "Point", Some("P")),
    (Tool::Line, "line", "Line", Some("L")),
    (Tool::Rect, "rect", "Rectangle", Some("R")),
    (Tool::CenterRect, "centerRect", "Center rectangle", None),
    (Tool::Circle, "circle", "Circle", Some("C")),
    (Tool::Arc, "arc", "Center arc", Some("A")),
    (Tool::Arc3, "arc3", "3-point arc", None),
    (Tool::Slot, "slot", "Slot", None),
    (Tool::Polygon, "polygon", "Polygon", None),
    (Tool::Ellipse, "ellipse", "Ellipse", None),
    (Tool::Spline, "spline", "Spline", None),
    (Tool::Text, "text", "Text", None),
    (Tool::Trim, "trim", "Trim", Some("K")),
    (Tool::Extend, "extend", "Extend", None),
    (Tool::Fillet2d, "fillet2d", "Sketch fillet", None),
    (Tool::Offset, "offset", "Offset (select curves first)", Some("O")),
    (Tool::Project, "project", "Use / project model edges", Some("U")),
    (Tool::Dimension, "dimension", "Dimension", Some("D")),
];

/// Constraint buttons (`w` in the original).
pub const CONSTRAINTS: &[(&str, &str, Option<&str>, &str)] = &[
    ("coincident", "Coincident", Some("I"), "two points, or a point and a curve"),
    ("horizontal", "Horizontal", Some("H"), "lines, or two points"),
    ("vertical", "Vertical", Some("V"), "lines, or two points"),
    ("parallel", "Parallel", None, "two or more lines"),
    ("perpendicular", "Perpendicular", None, "two lines"),
    ("tangent", "Tangent", Some("T"), "a line or arc/circle and an arc/circle"),
    ("equal", "Equal", Some("E"), "lines, or circles/arcs"),
    ("midpoint", "Midpoint", Some("M"), "a point and a line"),
    ("concentric", "Concentric", None, "two circles/arcs"),
    ("fix", "Fix", Some("X"), "points or curves"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sel {
    Point(Id),
    Curve(Id),
    Constraint(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Snap {
    Point(Id),
    Curve(Id),
    Origin,
    Grid,
}

pub enum Drag {
    /// Moving points; `start` is the sketch cursor where the drag began.
    Geometry { points: Vec<(Id, DVec2)>, start: DVec2, radius: Option<(Id, f64)> },
    /// Moving a dimension label.
    Label { idx: usize, start: DVec2, offset: [f64; 2] },
    Box { start: Pos2 },
}

pub struct SketchSession {
    pub feature: FeatId,
    pub before: Document,
    pub is_new: bool,
    pub undo: Vec<Document>,
    pub redo: Vec<Document>,
    pub tool: Tool,
    pub construction: bool,
    pub grid_snap: bool,
    pub sel: HashSet<Sel>,
    pub hover: Option<Sel>,
    pub drag: Option<Drag>,
    /// Points placed so far by the active tool.
    pub clicks: Vec<DVec2>,
    /// The previous point of a line chain.
    pub chain: Option<Id>,
    pub cursor: Option<DVec2>,
    pub snap: Option<Snap>,
    pub status: SolveStatus,
    pub regions: Vec<Region>,
    pub polygon_sides: u32,
    pub fillet_radius: f64,
    pub offset_dist: f64,
    pub dim_pending: Vec<Sel>,
    /// (constraint index, text being typed)
    pub dim_edit: Option<(usize, String)>,
    pub box_end: Option<Pos2>,
}

// ---------------------------------------------------------------- access

fn sketch_of(doc: &Document, id: FeatId) -> Option<(&Sketch, Frame)> {
    match doc.feature(id).map(|f| &f.op) {
        Some(Op::Sketch { sketch, frame, .. }) => Some((sketch, *frame)),
        _ => None,
    }
}

fn sketch_mut(doc: &mut Document, id: FeatId) -> Option<&mut Sketch> {
    match doc.feature_mut(id).map(|f| &mut f.op) {
        Some(Op::Sketch { sketch, .. }) => Some(sketch),
        _ => None,
    }
}

impl App {
    fn sk(&self) -> (&Sketch, Frame) {
        let id = self.sketch.as_ref().map(|s| s.feature).unwrap_or(0);
        sketch_of(&self.doc, id).expect("sketch session without sketch")
    }

    fn sk_mut(&mut self) -> &mut Sketch {
        let id = self.sketch.as_ref().map(|s| s.feature).unwrap_or(0);
        sketch_mut(&mut self.doc, id).expect("sketch session without sketch")
    }

    /// Undo step inside the sketch session.
    fn sk_checkpoint(&mut self) {
        let snap = self.doc.clone();
        if let Some(s) = &mut self.sketch {
            s.undo.push(snap);
            s.redo.clear();
        }
    }

    pub fn start_sketch(&mut self) {
        let plane = self.picks.iter().find(|r| r.is_planar() || matches!(r, Ref::FeaturePlane { .. })).cloned();
        let plane = match plane {
            Some(Ref::FeaturePlane { feature }) => match self.doc.feature(feature).map(|f| &f.op) {
                // picking a sketch in the tree sketches on its plane
                Some(Op::Sketch { plane, .. }) => plane.clone(),
                _ => Ref::FeaturePlane { feature },
            },
            Some(p) => p,
            None => {
                self.flash("Sketching on the Top plane (select a plane or planar face first to choose another)", ToastKind::Info);
                Ref::OriginPlane { plane: OriginPlane::Top }
            }
        };
        let frame = match &plane {
            Ref::OriginPlane { plane } => plane.frame(),
            Ref::FeaturePlane { feature } => match self.result.frames.get(feature) {
                Some(f) => *f,
                None => {
                    self.flash("That plane has not been built yet", ToastKind::Error);
                    return;
                }
            },
            Ref::Face { point, normal } => crate::kernel::plane_frame((*point).into(), (*normal).into()),
            _ => Frame::TOP,
        };
        let before = self.doc.clone();
        self.checkpoint();
        let name = self.doc.next_name("sketch");
        let id = self.doc.insert(name, Op::Sketch { plane, sketch: Sketch::default(), frame });
        self.picks.clear();
        self.open_session(id, before, true);
    }

    pub fn edit_sketch(&mut self, id: FeatId) {
        let before = self.doc.clone();
        self.checkpoint();
        self.picks.clear();
        self.open_session(id, before, false);
    }

    fn open_session(&mut self, id: FeatId, before: Document, is_new: bool) {
        self.hover = None;
        self.sketch = Some(SketchSession {
            feature: id,
            before,
            is_new,
            undo: vec![],
            redo: vec![],
            tool: if is_new { Tool::Line } else { Tool::Select },
            construction: false,
            grid_snap: true,
            sel: HashSet::new(),
            hover: None,
            drag: None,
            clicks: vec![],
            chain: None,
            cursor: None,
            snap: None,
            status: SolveStatus::default(),
            regions: vec![],
            polygon_sides: 6,
            fillet_radius: 2.0,
            offset_dist: 2.0,
            dim_pending: vec![],
            dim_edit: None,
            box_end: None,
        });
        if let Some((_, fr)) = sketch_of(&self.doc, id) {
            view_normal(self, &fr);
        }
        self.after_sketch_edit();
    }

    /// Re-solve the sketch, refresh its regions and the model.
    pub fn after_sketch_edit(&mut self) {
        self.solve_sketch(&[]);
        self.changed();
    }

    fn solve_sketch(&mut self, hold: &[Id]) {
        let (vars, _) = self.doc.var_values();
        let units = self.doc.units;
        let Some(sess) = &self.sketch else { return };
        let fid = sess.feature;
        let Some(sk) = sketch_mut(&mut self.doc, fid) else { return };
        let env = Env { vars: &vars, units };
        let st = sk.solve(&env, hold);
        let regions = find_regions(sk);
        if let Some(s) = &mut self.sketch {
            s.status = st;
            s.regions = regions;
        }
        self.scene_dirty = true;
    }
}

fn view_normal(app: &mut App, fr: &Frame) {
    app.camera.set_orientation(fr.normal().as_vec3(), fr.y.map(|v| v as f32).into());
    app.camera.target = glam::DVec3::from(fr.origin).as_vec3();
    let (sk, _) = app.sk_opt().unwrap_or((None, *fr));
    if let Some((lo, hi)) = sk.and_then(|s| s.bounds()) {
        let c = (lo + hi) / 2.0;
        app.camera.target = fr.to_world(c).as_vec3();
        app.camera.half_height = (((hi - lo).length() as f32) * 0.75).max(30.0);
    }
    app.scene_dirty = true;
}

impl App {
    fn sk_opt(&self) -> Option<(Option<&Sketch>, Frame)> {
        let id = self.sketch.as_ref()?.feature;
        sketch_of(&self.doc, id).map(|(s, f)| (Some(s), f))
    }
}

pub fn finish(app: &mut App) {
    let Some(sess) = app.sketch.take() else { return };
    let empty = sketch_of(&app.doc, sess.feature).map(|(s, _)| s.curves.is_empty() && s.points.is_empty()).unwrap_or(true);
    if empty && sess.is_new {
        app.doc = sess.before;
        app.undo.pop();
    }
    app.changed();
}

pub fn cancel(app: &mut App) {
    let Some(sess) = app.sketch.take() else { return };
    app.doc = sess.before;
    app.undo.pop();
    app.changed();
}

// ---------------------------------------------------------------- toolbar

pub fn toolbar(app: &mut App, ui: &mut Ui) {
    let (tool, construction, grid, has_sel) = {
        let s = app.sketch.as_ref().unwrap();
        (s.tool, s.construction, s.grid_snap, !s.sel.is_empty())
    };
    group(ui, |ui| {
        for (t, icon, label, key) in TOOLS {
            let tip = match key {
                Some(k) => format!("{label} ({k})"),
                None => label.to_string(),
            };
            let short = label.split(" (").next().unwrap_or(label).replace("Use / project model edges", "Project");
            if app.tool_button(ui, icon, &short, &tip, false, tool == *t, true).clicked() {
                set_tool(app, *t);
            }
        }
        let s = app.sketch.as_mut().unwrap();
        match tool {
            Tool::Polygon => {
                let mut n = s.polygon_sides as i32;
                if ui.add(egui::DragValue::new(&mut n).range(3..=64)).on_hover_text("Polygon sides").changed() {
                    s.polygon_sides = n as u32;
                }
            }
            Tool::Fillet2d => {
                ui.add(egui::DragValue::new(&mut s.fillet_radius).range(0.01..=1e5).speed(0.1)).on_hover_text("Fillet radius");
            }
            Tool::Offset => {
                ui.add(egui::DragValue::new(&mut s.offset_dist).range(0.001..=1e5).speed(0.1)).on_hover_text("Offset distance");
            }
            _ => {}
        }
        if app.tool_button(ui, "mirrorSketch", "Mirror", "Mirror selection across a selected line", false, false, true).clicked() {
            mirror_selection(app);
        }
        if app.tool_button(ui, "construction", "Construct", "Construction geometry (Q): toggles selected curves, or the mode for new ones", false, construction, true).clicked() {
            toggle_construction(app);
        }
    });
    group(ui, |ui| {
        for (key, label, k, help) in CONSTRAINTS {
            let tip = format!("{label}{}: select {help}", k.map(|k| format!(" ({k})")).unwrap_or_default());
            if app.tool_button(ui, key, label, &tip, false, false, true).clicked() {
                constrain(app, key);
            }
        }
    });
    group(ui, |ui| {
        for (icon, label, tip) in [
            ("photo", "Photo", "Add a photo to trace (camera or file)"),
            ("ruler", "Scale photo", "Scale photo: click two points a known distance apart"),
            ("move", "Move photo", "Move photo"),
            ("wand", "Trace", "Trace: click an object in the photo to outline it (magic wand / edge detect)"),
        ] {
            if app.tool_button(ui, icon, label, tip, false, false, icon == "photo").clicked() {
                app.flash("Photo tracing isn't in the Rust version yet", ToastKind::Info);
            }
        }
    });
    group(ui, |ui| {
        if app.tool_button(ui, "delete", "Delete", "Delete (Del)", false, false, has_sel).clicked() {
            delete_selection(app);
        }
        if app.tool_button(ui, "grid", "Grid", "Snap to grid (G)", false, grid, true).clicked() {
            let s = app.sketch.as_mut().unwrap();
            s.grid_snap = !s.grid_snap;
        }
        if app.tool_button(ui, "normal", "Normal to", "View normal to sketch (N)", false, false, true).clicked() {
            let (_, fr) = app.sk();
            view_normal(app, &fr);
        }
    });
}

fn group(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 24.0), egui::Sense::hover());
    ui.painter().vline(rect.center().x, rect.y_range(), Stroke::new(1.0, theme::BORDER));
    ui.add_space(6.0);
    ui.spacing_mut().item_spacing.x = 1.0;
    add(ui);
    ui.spacing_mut().item_spacing.x = 8.0;
}

pub fn set_tool(app: &mut App, t: Tool) {
    if matches!(t, Tool::Ellipse | Tool::Spline | Tool::Text) {
        app.flash(format!("{t:?} isn't in the Rust version yet"), ToastKind::Info);
        return;
    }
    if t == Tool::Offset {
        let has = app.sketch.as_ref().map(|s| s.sel.iter().any(|x| matches!(x, Sel::Curve(_)))).unwrap_or(false);
        if !has {
            app.flash("Select the curves to offset first, then click the side to offset to", ToastKind::Info);
        }
    }
    let s = app.sketch.as_mut().unwrap();
    s.tool = t;
    s.clicks.clear();
    s.chain = None;
    s.dim_pending.clear();
    app.scene_dirty = true;
}

pub fn status_text(app: &App, s: &SketchSession) -> String {
    let base = match s.tool {
        Tool::Select => "Click to select, drag to move, drag on empty space to box-select; double-click a dimension to edit",
        Tool::Point => "Click to place points",
        Tool::Line => "Click to place line points; Esc or right-click ends the chain",
        Tool::Rect => "Click two opposite corners",
        Tool::CenterRect => "Click the centre, then a corner",
        Tool::Circle => "Click the centre, then a point on the circle",
        Tool::Arc => "Click the centre, the start, then the end (counter-clockwise)",
        Tool::Arc3 => "Click the start, the end, then a point on the arc",
        Tool::Slot => "Click the two end centres, then the width",
        Tool::Polygon => "Click the centre, then a corner",
        Tool::Trim => "Click the piece of a curve to remove",
        Tool::Extend => "Click near the end of a line to extend it to the next curve",
        Tool::Fillet2d => "Click a corner where two lines meet",
        Tool::Offset => "Click on the side to offset the selected curves to",
        Tool::Project => "Click model edges to bring them into the sketch",
        Tool::Dimension => "Click a line, circle or arc (then where to put it), or two items to dimension between",
        _ => "",
    };
    let dof = if let Some(e) = &s.status.error {
        format!(" · {e}")
    } else if s.status.dof == 0 && !app.sk().0.points.is_empty() {
        " · fully defined".to_string()
    } else if s.status.dof > 0 {
        format!(" · {} degrees of freedom", s.status.dof)
    } else {
        String::new()
    };
    format!("{base}{dof}")
}

// ---------------------------------------------------------------- keyboard

pub fn shortcuts(app: &mut App, ctx: &egui::Context) {
    let editing_dim = app.sketch.as_ref().map(|s| s.dim_edit.is_some()).unwrap_or(false);
    if editing_dim {
        return;
    }
    let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
    let shift = crate::app::key_mods(ctx).1;
    if pressed(Key::Escape) {
        let s = app.sketch.as_mut().unwrap();
        if !s.clicks.is_empty() || s.chain.is_some() || !s.dim_pending.is_empty() {
            s.clicks.clear();
            s.chain = None;
            s.dim_pending.clear();
        } else if s.tool != Tool::Select {
            s.tool = Tool::Select;
        } else {
            s.sel.clear();
        }
        app.scene_dirty = true;
        return;
    }
    if pressed(Key::Enter) {
        finish(app);
        return;
    }
    if pressed(Key::Delete) || pressed(Key::Backspace) {
        delete_selection(app);
        return;
    }
    if shift {
        return;
    }
    let sel_empty = app.sketch.as_ref().map(|s| s.sel.is_empty()).unwrap_or(true);
    // with a selection, letters apply constraints; otherwise they pick tools
    if !sel_empty {
        for (key, _, k, _) in CONSTRAINTS {
            if let Some(k) = k {
                if let Some(kk) = Key::from_name(k) {
                    if pressed(kk) {
                        constrain(app, key);
                        return;
                    }
                }
            }
        }
    }
    for (t, _, _, k) in TOOLS {
        if let Some(k) = k {
            if let Some(kk) = Key::from_name(k) {
                if pressed(kk) {
                    set_tool(app, *t);
                    return;
                }
            }
        }
    }
    if pressed(Key::Q) {
        toggle_construction(app);
    }
    if pressed(Key::G) {
        let s = app.sketch.as_mut().unwrap();
        s.grid_snap = !s.grid_snap;
    }
    if pressed(Key::N) {
        let (_, fr) = app.sk();
        view_normal(app, &fr);
    }
}

// ---------------------------------------------------------------- input

fn grid_step(app: &App) -> f64 {
    // a round number of mm close to 20 px
    let target = (app.camera.wpp() * 20.0) as f64;
    let units = app.doc.units.scale();
    let t = target / units;
    let p = 10f64.powf(t.log10().floor());
    let m = t / p;
    let step = if m < 2.0 { 2.0 } else if m < 5.0 { 5.0 } else { 10.0 };
    step * p * units
}

/// Snap the cursor (sketch coordinates) to points, curves, the origin or the grid.
fn snap(app: &App, s: Pos2, rect: egui::Rect, raw: DVec2, exclude: &HashSet<Id>) -> (DVec2, Option<Snap>) {
    let (sk, fr) = app.sk();
    let to_screen = |p: DVec2| {
        let v = app.camera.project(fr.to_world(p).as_vec3());
        Pos2::new(rect.min.x + v.x, rect.min.y + v.y)
    };
    let mut best: Option<(f32, DVec2, Snap)> = None;
    for p in &sk.points {
        if exclude.contains(&p.id) {
            continue;
        }
        let d = (to_screen(p.v()) - s).length();
        if d < 9.0 && best.as_ref().map(|b| d < b.0).unwrap_or(true) {
            best = Some((d, p.v(), Snap::Point(p.id)));
        }
    }
    if let Some((_, p, sn)) = best {
        return (p, Some(sn));
    }
    let o = (to_screen(DVec2::ZERO) - s).length();
    if o < 9.0 {
        return (DVec2::ZERO, Some(Snap::Origin));
    }
    let wpp = app.camera.wpp() as f64;
    for c in &sk.curves {
        if Sketch::curve_points(&c.geom).iter().any(|p| exclude.contains(p)) {
            continue;
        }
        let d = sk.curve_distance(c, raw);
        if d / wpp < 6.0 {
            if let Some(u) = regions::param_at(sk, c.id, raw) {
                return (regions::point_at(sk, c.id, u), Some(Snap::Curve(c.id)));
            }
        }
    }
    let sess = app.sketch.as_ref().unwrap();
    if sess.grid_snap {
        let g = grid_step(app);
        return ((raw / g).round() * g, Some(Snap::Grid));
    }
    (raw, None)
}

fn sketch_cursor(app: &App, s: Pos2, rect: egui::Rect) -> Option<DVec2> {
    let (_, fr) = app.sk();
    let (o, d) = app.camera.ray(glam::Vec2::new(s.x - rect.min.x, s.y - rect.min.y));
    fr.ray_hit(o.as_dvec3(), d.as_dvec3())
}

/// What is under the cursor in sketch mode (points before curves before dimensions).
fn hit(app: &App, s: Pos2, rect: egui::Rect, raw: DVec2) -> Option<Sel> {
    let (sk, fr) = app.sk();
    let to_screen = |p: DVec2| {
        let v = app.camera.project(fr.to_world(p).as_vec3());
        Pos2::new(rect.min.x + v.x, rect.min.y + v.y)
    };
    let mut best: Option<(f32, Sel)> = None;
    for p in &sk.points {
        let d = (to_screen(p.v()) - s).length();
        if d < 8.0 && best.as_ref().map(|b| d < b.0).unwrap_or(true) {
            best = Some((d, Sel::Point(p.id)));
        }
    }
    if let Some((_, h)) = best {
        return Some(h);
    }
    // dimension labels and constraint glyphs
    for (i, r) in label_rects(app, sk, &fr, rect) {
        if r.expand(2.0).contains(s) {
            return Some(Sel::Constraint(i));
        }
    }
    let wpp = app.camera.wpp() as f64;
    let mut best: Option<(f64, Sel)> = None;
    for c in &sk.curves {
        let d = sk.curve_distance(c, raw) / wpp;
        if d < 6.0 && best.as_ref().map(|b| d < b.0).unwrap_or(true) {
            best = Some((d, Sel::Curve(c.id)));
        }
    }
    best.map(|b| b.1)
}

pub fn viewport_input(app: &mut App, ui: &Ui, resp: &egui::Response, rect: egui::Rect) {
    if app.nav.is_some() {
        return;
    }
    let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p));
    let Some(s) = pointer.or(ui.input(|i| i.pointer.latest_pos())) else { return };
    let Some(raw) = sketch_cursor(app, s, rect) else { return };
    let tool = app.sketch.as_ref().unwrap().tool;
    let shift = ui.input(|i| i.modifiers.shift || i.modifiers.command);

    // hover
    let h = if pointer.is_some() { hit(app, s, rect, raw) } else { None };
    let exclude: HashSet<Id> = match &app.sketch.as_ref().unwrap().drag {
        Some(Drag::Geometry { points, .. }) => points.iter().map(|p| p.0).collect(),
        _ => HashSet::new(),
    };
    let (snapped, snp) = if tool == Tool::Select { (raw, None) } else { snap(app, s, rect, raw, &exclude) };
    {
        let sess = app.sketch.as_mut().unwrap();
        if sess.hover != h || sess.cursor != Some(snapped) || sess.snap != snp {
            sess.hover = h;
            sess.cursor = Some(snapped);
            sess.snap = snp;
            app.scene_dirty = true;
        }
    }

    // right click ends a chain / cancels the tool step
    if resp.secondary_clicked() {
        let sess = app.sketch.as_mut().unwrap();
        if !sess.clicks.is_empty() || sess.chain.is_some() {
            sess.clicks.clear();
            sess.chain = None;
            app.context_menu = None;
            app.scene_dirty = true;
        }
    }

    match tool {
        Tool::Select => select_input(app, ui, resp, rect, s, raw, h, shift),
        _ => {
            if resp.clicked_by(egui::PointerButton::Primary) {
                if resp.double_clicked() && tool == Tool::Line {
                    let sess = app.sketch.as_mut().unwrap();
                    sess.chain = None;
                    sess.clicks.clear();
                    return;
                }
                tool_click(app, tool, snapped, snp, h, raw);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn select_input(app: &mut App, ui: &Ui, resp: &egui::Response, rect: egui::Rect, s: Pos2, raw: DVec2, h: Option<Sel>, shift: bool) {
    // start drag
    if resp.drag_started_by(egui::PointerButton::Primary) {
        let press = ui.input(|i| i.pointer.press_origin()).unwrap_or(s);
        let raw0 = sketch_cursor(app, press, rect).unwrap_or(raw);
        let h0 = hit(app, press, rect, raw0);
        let (sk, _) = app.sk();
        let drag = match h0 {
            Some(Sel::Point(p)) => Some(Drag::Geometry { points: vec![(p, sk.pv(p))], start: raw0, radius: None }),
            Some(Sel::Curve(c)) => {
                let curve = sk.curve(c).unwrap();
                match curve.geom {
                    // dragging a circle's rim changes its radius
                    Geom::Circle { c: cc, r } if !shift => Some(Drag::Geometry { points: vec![(cc, sk.pv(cc))], start: raw0, radius: Some((c, r)) }),
                    _ => {
                        let mut pts: Vec<Id> = Sketch::curve_points(&curve.geom);
                        // dragging a selected curve drags the whole selection
                        let sess = app.sketch.as_ref().unwrap();
                        if sess.sel.contains(&Sel::Curve(c)) {
                            for x in &sess.sel {
                                match x {
                                    Sel::Curve(id) => pts.extend(sk.curve(*id).map(|c| Sketch::curve_points(&c.geom)).unwrap_or_default()),
                                    Sel::Point(id) => pts.push(*id),
                                    _ => {}
                                }
                            }
                        }
                        pts.sort();
                        pts.dedup();
                        Some(Drag::Geometry { points: pts.iter().map(|p| (*p, sk.pv(*p))).collect(), start: raw0, radius: None })
                    }
                }
            }
            Some(Sel::Constraint(i)) if sk.constraints[i].dim().is_some() => {
                let off = sk.constraints[i].dim().unwrap().offset;
                Some(Drag::Label { idx: i, start: raw0, offset: off })
            }
            _ => Some(Drag::Box { start: press }),
        };
        if matches!(drag, Some(Drag::Geometry { .. }) | Some(Drag::Label { .. })) {
            app.sk_checkpoint();
        }
        app.sketch.as_mut().unwrap().drag = drag;
    }
    let dragging = ui.input(|i| i.pointer.primary_down());
    let drag = app.sketch.as_mut().unwrap().drag.take();
    match drag {
        Some(Drag::Geometry { points, start, radius }) => {
            let delta = raw - start;
            if let Some((cid, r0)) = radius {
                let (sk, _) = app.sk();
                let c = points[0].1;
                let r = (raw - c).length().max(1e-6);
                let _ = r0;
                let _ = sk;
                if let Some(Curve { geom: Geom::Circle { r: rr, .. }, .. }) = app.sk_mut().curves.iter_mut().find(|x| x.id == cid) {
                    *rr = r;
                }
                // the radius parameter isn't a point; hold the centre
                app.solve_sketch(&[points[0].0]);
            } else {
                let sk = app.sk_mut();
                for (id, p0) in &points {
                    if let Some(p) = sk.point_mut(*id) {
                        p.x = p0.x + delta.x;
                        p.y = p0.y + delta.y;
                    }
                }
                let hold: Vec<Id> = points.iter().map(|p| p.0).collect();
                app.solve_sketch(&hold);
            }
            if dragging {
                app.sketch.as_mut().unwrap().drag = Some(Drag::Geometry { points, start, radius });
            } else {
                app.changed();
            }
        }
        Some(Drag::Label { idx, start, offset }) => {
            let d = raw - start;
            if let Some(dim) = app.sk_mut().constraints.get_mut(idx).and_then(|c| c.dim_mut()) {
                dim.offset = [offset[0] + d.x, offset[1] + d.y];
            }
            app.scene_dirty = true;
            if dragging {
                app.sketch.as_mut().unwrap().drag = Some(Drag::Label { idx, start, offset });
            }
        }
        Some(Drag::Box { start }) => {
            if dragging {
                let sess = app.sketch.as_mut().unwrap();
                sess.box_end = Some(s);
                sess.drag = Some(Drag::Box { start });
            } else {
                // box select: window (left→right) needs full containment
                let r = egui::Rect::from_two_pos(start, s);
                let (sk, fr) = app.sk();
                let to_screen = |p: DVec2| {
                    let v = app.camera.project(fr.to_world(p).as_vec3());
                    Pos2::new(rect.min.x + v.x, rect.min.y + v.y)
                };
                let mut new: Vec<Sel> = vec![];
                for p in &sk.points {
                    if r.contains(to_screen(p.v())) {
                        new.push(Sel::Point(p.id));
                    }
                }
                for c in &sk.curves {
                    let pts = sk.tessellate(c, 2.0);
                    let inside = pts.iter().filter(|p| r.contains(to_screen(**p))).count();
                    if inside == pts.len() || (s.x < start.x && inside > 0) {
                        new.push(Sel::Curve(c.id));
                    }
                }
                let sess = app.sketch.as_mut().unwrap();
                if !shift {
                    sess.sel.clear();
                }
                sess.sel.extend(new);
                sess.box_end = None;
                app.scene_dirty = true;
            }
        }
        None => {}
    }

    if resp.double_clicked() {
        if let Some(Sel::Constraint(i)) = h {
            open_dim_edit(app, i);
        }
        return;
    }
    if resp.clicked_by(egui::PointerButton::Primary) {
        let sess = app.sketch.as_mut().unwrap();
        match h {
            Some(x) => {
                if shift || true {
                    // like the original: clicking adds to the selection, clicking again removes
                    if !sess.sel.remove(&x) {
                        sess.sel.insert(x);
                    }
                }
            }
            None => sess.sel.clear(),
        }
        app.scene_dirty = true;
    }
}

fn open_dim_edit(app: &mut App, i: usize) {
    let (sk, _) = app.sk();
    if let Some(d) = sk.constraints.get(i).and_then(|c| c.dim()) {
        let text = d.expr.clone();
        app.sketch.as_mut().unwrap().dim_edit = Some((i, text));
    }
}

// ---------------------------------------------------------------- creation

/// Use an existing point when snapped to one, else create one (adding an
/// on-curve or origin constraint for curve/origin snaps).
fn point_at(app: &mut App, p: DVec2, snp: Option<Snap>) -> Id {
    match snp {
        Some(Snap::Point(id)) => id,
        Some(Snap::Curve(c)) => {
            let sk = app.sk_mut();
            let id = sk.add_point(p);
            let k = match sk.curve(c).map(|x| &x.geom) {
                Some(Geom::Line { .. }) => Constraint::PointOnLine { p: id, l: c },
                _ => Constraint::PointOnCircle { p: id, c },
            };
            sk.constraints.push(k);
            id
        }
        Some(Snap::Origin) => {
            let sk = app.sk_mut();
            let id = sk.add_point(DVec2::ZERO);
            sk.constraints.push(Constraint::Fix { p: id, x: 0.0, y: 0.0 });
            id
        }
        _ => app.sk_mut().add_point(p),
    }
}

fn hv_infer(app: &App, a: DVec2, b: DVec2) -> Option<bool> {
    let d = b - a;
    if d.length() < 1e-9 {
        return None;
    }
    let ang = d.y.atan2(d.x).to_degrees().abs();
    let tol = 2.5;
    if ang < tol || (180.0 - ang) < tol {
        Some(true)
    } else if (ang - 90.0).abs() < tol {
        Some(false)
    } else {
        let _ = app;
        None
    }
}

fn tool_click(app: &mut App, tool: Tool, p: DVec2, snp: Option<Snap>, h: Option<Sel>, raw: DVec2) {
    let construction = app.sketch.as_ref().unwrap().construction;
    match tool {
        Tool::Point => {
            app.sk_checkpoint();
            point_at(app, p, snp);
            app.after_sketch_edit();
        }
        Tool::Line => {
            let chain = app.sketch.as_ref().unwrap().chain;
            app.sk_checkpoint();
            match chain {
                None => {
                    let id = point_at(app, p, snp);
                    app.sketch.as_mut().unwrap().chain = Some(id);
                    app.solve_sketch(&[]);
                }
                Some(a) => {
                    let pa = app.sk().0.pv(a);
                    let mut q = p;
                    let hv = if matches!(snp, Some(Snap::Point(_))) { None } else { hv_infer(app, pa, p) };
                    if let Some(h) = hv {
                        if h {
                            q.y = pa.y
                        } else {
                            q.x = pa.x
                        }
                    }
                    if (q - pa).length() < 1e-9 {
                        return;
                    }
                    let b = point_at(app, q, snp);
                    let sk = app.sk_mut();
                    let l = sk.add_curve(Geom::Line { a, b }, construction);
                    match hv {
                        Some(true) => sk.constraints.push(Constraint::Horizontal { l }),
                        Some(false) => sk.constraints.push(Constraint::Vertical { l }),
                        None => {}
                    }
                    // closing onto an existing point ends the chain
                    let close = matches!(snp, Some(Snap::Point(_)));
                    app.sketch.as_mut().unwrap().chain = if close { None } else { Some(b) };
                    app.after_sketch_edit();
                }
            }
        }
        Tool::Rect | Tool::CenterRect | Tool::Circle | Tool::Polygon => {
            let sess = app.sketch.as_mut().unwrap();
            sess.clicks.push(p);
            if sess.clicks.len() == 1 {
                // remember what the first click snapped to
                app.scene_dirty = true;
                FIRST_SNAP.with(|f| f.set(snp));
                return;
            }
            let a = sess.clicks[0];
            sess.clicks.clear();
            let first_snap = FIRST_SNAP.with(|f| f.get());
            app.sk_checkpoint();
            match tool {
                Tool::Rect => make_rect(app, a, p, first_snap, snp, construction, false),
                Tool::CenterRect => make_rect(app, a, p, first_snap, snp, construction, true),
                Tool::Circle => {
                    let c = point_at(app, a, first_snap);
                    let r = (p - a).length().max(1e-6);
                    let sk = app.sk_mut();
                    let ci = sk.add_curve(Geom::Circle { c, r }, construction);
                    if let Some(Snap::Point(pid)) = snp {
                        sk.constraints.push(Constraint::PointOnCircle { p: pid, c: ci });
                    }
                }
                _ => make_polygon(app, a, p, first_snap, construction),
            }
            app.after_sketch_edit();
        }
        Tool::Arc | Tool::Arc3 | Tool::Slot => {
            let sess = app.sketch.as_mut().unwrap();
            sess.clicks.push(p);
            if sess.clicks.len() == 1 {
                FIRST_SNAP.with(|f| f.set(snp));
            }
            if sess.clicks.len() == 2 {
                SECOND_SNAP.with(|f| f.set(snp));
            }
            if sess.clicks.len() < 3 {
                app.scene_dirty = true;
                return;
            }
            let pts = std::mem::take(&mut sess.clicks);
            let (s1, s2) = (FIRST_SNAP.with(|f| f.get()), SECOND_SNAP.with(|f| f.get()));
            app.sk_checkpoint();
            match tool {
                Tool::Arc => make_center_arc(app, pts[0], pts[1], pts[2], s1, s2, snp, construction),
                Tool::Arc3 => make_arc3(app, pts[0], pts[1], pts[2], s1, s2, construction),
                _ => make_slot(app, pts[0], pts[1], pts[2], construction),
            }
            app.after_sketch_edit();
        }
        Tool::Trim => {
            if let Some(Sel::Curve(c)) = h {
                app.sk_checkpoint();
                trim(app, c, raw);
                app.after_sketch_edit();
            }
        }
        Tool::Extend => {
            if let Some(Sel::Curve(c)) = h {
                app.sk_checkpoint();
                extend(app, c, raw);
                app.after_sketch_edit();
            }
        }
        Tool::Fillet2d => {
            let pid = match (h, snp) {
                (Some(Sel::Point(p)), _) | (_, Some(Snap::Point(p))) => Some(p),
                _ => None,
            };
            if let Some(pid) = pid {
                app.sk_checkpoint();
                let r = app.sketch.as_ref().unwrap().fillet_radius * app.doc.units.scale();
                if let Err(e) = fillet_corner(app, pid, r) {
                    app.sketch.as_mut().unwrap().undo.pop();
                    app.flash(e, ToastKind::Error);
                }
                app.after_sketch_edit();
            }
        }
        Tool::Offset => {
            let d = app.sketch.as_ref().unwrap().offset_dist * app.doc.units.scale();
            app.sk_checkpoint();
            offset_selection(app, raw, d);
            app.after_sketch_edit();
        }
        Tool::Project => project_at(app, raw),
        Tool::Dimension => dimension_click(app, h, raw),
        _ => {}
    }
}

thread_local! {
    static FIRST_SNAP: std::cell::Cell<Option<Snap>> = const { std::cell::Cell::new(None) };
    static SECOND_SNAP: std::cell::Cell<Option<Snap>> = const { std::cell::Cell::new(None) };
}

fn make_rect(app: &mut App, a: DVec2, b: DVec2, sa: Option<Snap>, sb: Option<Snap>, construction: bool, centered: bool) {
    let (p0, p2) = if centered { (a - (b - a), b) } else { (a, b) };
    if (p2 - p0).x.abs() < 1e-9 || (p2 - p0).y.abs() < 1e-9 {
        return;
    }
    let corners = [p0, DVec2::new(p2.x, p0.y), p2, DVec2::new(p0.x, p2.y)];
    let ids: Vec<Id> = corners
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let sn = if !centered && i == 0 { sa } else if i == 2 { sb } else { None };
            point_at(app, *c, sn)
        })
        .collect();
    let sk = app.sk_mut();
    let mut lines = vec![];
    for i in 0..4 {
        lines.push(sk.add_curve(Geom::Line { a: ids[i], b: ids[(i + 1) % 4] }, construction));
    }
    sk.constraints.push(Constraint::Horizontal { l: lines[0] });
    sk.constraints.push(Constraint::Horizontal { l: lines[2] });
    sk.constraints.push(Constraint::Vertical { l: lines[1] });
    sk.constraints.push(Constraint::Vertical { l: lines[3] });
    if centered {
        let c = point_at(app, a, sa);
        let sk = app.sk_mut();
        let d = sk.add_curve(Geom::Line { a: ids[0], b: ids[2] }, true);
        sk.constraints.push(Constraint::Midpoint { p: c, l: d });
    }
}

fn make_polygon(app: &mut App, c: DVec2, v: DVec2, sc: Option<Snap>, construction: bool) {
    let n = app.sketch.as_ref().unwrap().polygon_sides.max(3) as usize;
    let r = (v - c).length();
    if r < 1e-9 {
        return;
    }
    let a0 = (v - c).to_angle();
    let cid = point_at(app, c, sc);
    let sk = app.sk_mut();
    let circ = sk.add_curve(Geom::Circle { c: cid, r }, true);
    let ids: Vec<Id> = (0..n).map(|i| sk.add_point(c + DVec2::from_angle(a0 + i as f64 / n as f64 * std::f64::consts::TAU) * r)).collect();
    let mut lines = vec![];
    for i in 0..n {
        lines.push(sk.add_curve(Geom::Line { a: ids[i], b: ids[(i + 1) % n] }, construction));
        sk.constraints.push(Constraint::PointOnCircle { p: ids[i], c: circ });
    }
    for i in 1..n {
        sk.constraints.push(Constraint::Equal { a: lines[0], b: lines[i] });
    }
}

#[allow(clippy::too_many_arguments)]
fn make_center_arc(app: &mut App, c: DVec2, a: DVec2, e: DVec2, sc: Option<Snap>, sa: Option<Snap>, se: Option<Snap>, construction: bool) {
    let r = (a - c).length();
    if r < 1e-9 {
        return;
    }
    let e = c + (e - c).normalize_or_zero() * r;
    let ccw = (a - c).perp_dot(e - c) >= 0.0;
    let cid = point_at(app, c, sc);
    let aid = point_at(app, a, sa);
    let eid = point_at(app, e, se);
    let sk = app.sk_mut();
    let (s, t) = if ccw { (aid, eid) } else { (eid, aid) };
    sk.add_curve(Geom::Arc { c: cid, a: s, b: t }, construction);
}

fn make_arc3(app: &mut App, a: DVec2, b: DVec2, m: DVec2, sa: Option<Snap>, sb: Option<Snap>, construction: bool) {
    let Some(c) = circumcenter(a, b, m) else { return };
    let aid = point_at(app, a, sa);
    let bid = point_at(app, b, sb);
    let sk = app.sk_mut();
    let cid = sk.add_point(c);
    // pick the direction that passes through m
    let (t0, t1) = arc_angles(c, a, b);
    let mut tm = (m - c).to_angle();
    while tm < t0 {
        tm += std::f64::consts::TAU;
    }
    let (s, e) = if tm <= t1 { (aid, bid) } else { (bid, aid) };
    sk.add_curve(Geom::Arc { c: cid, a: s, b: e }, construction);
}

fn circumcenter(a: DVec2, b: DVec2, c: DVec2) -> Option<DVec2> {
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if d.abs() < 1e-12 {
        return None;
    }
    let (a2, b2, c2) = (a.length_squared(), b.length_squared(), c.length_squared());
    Some(DVec2::new((a2 * (b.y - c.y) + b2 * (c.y - a.y) + c2 * (a.y - b.y)) / d, (a2 * (c.x - b.x) + b2 * (a.x - c.x) + c2 * (b.x - a.x)) / d))
}

fn make_slot(app: &mut App, c1: DVec2, c2: DVec2, w: DVec2, construction: bool) {
    let d = c2 - c1;
    if d.length() < 1e-9 {
        return;
    }
    let n = d.perp().normalize();
    let r = ((w - c1).dot(n)).abs().max(1e-3);
    let sk = app.sk_mut();
    let a = sk.add_point(c1);
    let b = sk.add_point(c2);
    let p1 = sk.add_point(c1 + n * r);
    let p2 = sk.add_point(c2 + n * r);
    let p3 = sk.add_point(c2 - n * r);
    let p4 = sk.add_point(c1 - n * r);
    let l1 = sk.add_curve(Geom::Line { a: p1, b: p2 }, construction);
    let l2 = sk.add_curve(Geom::Line { a: p3, b: p4 }, construction);
    let a1 = sk.add_curve(Geom::Arc { c: b, a: p3, b: p2 }, construction);
    let a2 = sk.add_curve(Geom::Arc { c: a, a: p1, b: p4 }, construction);
    let axis = sk.add_curve(Geom::Line { a, b }, true);
    for (l, ar) in [(l1, a1), (l1, a2), (l2, a1), (l2, a2)] {
        sk.constraints.push(Constraint::Tangent { a: l, b: ar, inner: false });
    }
    sk.constraints.push(Constraint::Equal { a: a1, b: a2 });
    sk.constraints.push(Constraint::Parallel { a: axis, b: l1 });
}

// ---------------------------------------------------------------- editing tools

fn trim(app: &mut App, cid: Id, at: DVec2) {
    let (sk, _) = app.sk();
    let Some(c) = sk.curve(cid).cloned() else { return };
    let splits = regions::split_params(sk, cid);
    let Some(u) = regions::param_at(sk, cid, at) else { return };
    let k = splits.iter().position(|s| *s > u).unwrap_or(splits.len() - 1).max(1);
    let (u0, u1) = (splits[k - 1], splits[k]);
    let at_start = u0 <= 1e-9;
    let at_end = u1 >= 1.0 - 1e-9;
    let p0 = regions::point_at(sk, cid, u0);
    let p1 = regions::point_at(sk, cid, u1);
    let is_circle = matches!(c.geom, Geom::Circle { .. });
    let sk = app.sk_mut();
    if (at_start && at_end && !is_circle) || (is_circle && splits.len() <= 3) {
        sk.delete(&[cid].into(), &HashSet::new());
        return;
    }
    let mut old_points = Sketch::curve_points(&c.geom);
    let set = |sk: &mut Sketch, g: Geom| {
        if let Some(x) = sk.curves.iter_mut().find(|x| x.id == cid) {
            x.geom = g;
        }
    };
    match c.geom {
        Geom::Line { a, b } => {
            if at_start {
                let m = sk.add_point(p1);
                set(sk, Geom::Line { a: m, b });
            } else if at_end {
                let m = sk.add_point(p0);
                set(sk, Geom::Line { a, b: m });
            } else {
                let m0 = sk.add_point(p0);
                let m1 = sk.add_point(p1);
                set(sk, Geom::Line { a, b: m0 });
                sk.add_curve(Geom::Line { a: m1, b }, c.construction);
            }
        }
        Geom::Circle { c: cc, .. } => {
            // keep the part from u1 around to u0
            let a = sk.add_point(p1);
            let b = sk.add_point(p0);
            set(sk, Geom::Arc { c: cc, a, b });
        }
        Geom::Arc { c: cc, a, b } => {
            if at_start {
                let m = sk.add_point(p1);
                set(sk, Geom::Arc { c: cc, a: m, b });
            } else if at_end {
                let m = sk.add_point(p0);
                set(sk, Geom::Arc { c: cc, a, b: m });
            } else {
                let m0 = sk.add_point(p0);
                let m1 = sk.add_point(p1);
                set(sk, Geom::Arc { c: cc, a, b: m0 });
                sk.add_curve(Geom::Arc { c: cc, a: m1, b }, c.construction);
            }
        }
    }
    // drop endpoints nothing uses any more
    let used: HashSet<Id> = sk.curves.iter().flat_map(|c| Sketch::curve_points(&c.geom)).chain(sk.constraints.iter().flat_map(|k| k.refs())).collect();
    old_points.retain(|p| !used.contains(p));
    sk.points.retain(|p| !old_points.contains(&p.id));
    merge_close_points(app);
}

/// Merge coincident points created by trimming so the result stays connected.
fn merge_close_points(app: &mut App) {
    let sk = app.sk_mut();
    let pts: Vec<(Id, DVec2)> = sk.points.iter().map(|p| (p.id, p.v())).collect();
    for i in 0..pts.len() {
        for j in 0..i {
            if (pts[i].1 - pts[j].1).length() < 1e-7 && sk.point(pts[i].0).is_some() && sk.point(pts[j].0).is_some() {
                sk.merge_point(pts[i].0, pts[j].0);
            }
        }
    }
}

fn extend(app: &mut App, cid: Id, at: DVec2) {
    let (sk, _) = app.sk();
    let Some(Geom::Line { a, b }) = sk.curve(cid).map(|c| c.geom.clone()) else {
        return;
    };
    let (pa, pb) = (sk.pv(a), sk.pv(b));
    // extend the end nearer the click
    let (fixed, moving, pm) = if (at - pa).length() < (at - pb).length() { (pb, a, pa) } else { (pa, b, pb) };
    let d = (pm - fixed).normalize();
    let len = (pm - fixed).length();
    let hits = regions::ray_hits(sk, fixed, d, cid);
    let next = hits.into_iter().filter(|t| *t > len + 1e-6).fold(f64::INFINITY, f64::min);
    if next.is_finite() {
        if let Some(p) = app.sk_mut().point_mut(moving) {
            let q = fixed + d * next;
            p.x = q.x;
            p.y = q.y;
        }
    } else {
        app.flash("Nothing to extend to", ToastKind::Info);
    }
}

fn fillet_corner(app: &mut App, pid: Id, r: f64) -> Result<(), String> {
    let (sk, _) = app.sk();
    let lines: Vec<(Id, Id)> = sk
        .curves
        .iter()
        .filter_map(|c| match c.geom {
            Geom::Line { a, b } if a == pid => Some((c.id, b)),
            Geom::Line { a, b } if b == pid => Some((c.id, a)),
            _ => None,
        })
        .collect();
    if lines.len() != 2 {
        return Err("A sketch fillet needs a corner where exactly two lines meet".into());
    }
    let p = sk.pv(pid);
    let (l1, o1) = lines[0];
    let (l2, o2) = lines[1];
    let u = (sk.pv(o1) - p).normalize();
    let v = (sk.pv(o2) - p).normalize();
    let half = u.angle_to(v).abs() / 2.0;
    if half < 1e-3 || (std::f64::consts::FRAC_PI_2 - half).abs() > std::f64::consts::FRAC_PI_2 - 1e-3 {
        return Err("Those lines are parallel".into());
    }
    let t = r / half.tan();
    if t > (sk.pv(o1) - p).length() || t > (sk.pv(o2) - p).length() {
        return Err("The fillet radius is too large for those lines".into());
    }
    let bis = (u + v).normalize();
    let c = p + bis * (r / half.sin());
    let (q1, q2) = (p + u * t, p + v * t);
    let sk = app.sk_mut();
    let n1 = sk.add_point(q1);
    let n2 = sk.add_point(q2);
    let cid = sk.add_point(c);
    // re-point the two lines at the new tangent points
    for c in &mut sk.curves {
        if c.id == l1 || c.id == l2 {
            if let Geom::Line { a, b } = &mut c.geom {
                let n = if c.id == l1 { n1 } else { n2 };
                if *a == pid {
                    *a = n
                } else if *b == pid {
                    *b = n
                }
            }
        }
    }
    sk.constraints.retain(|k| !k.refs().contains(&pid));
    sk.points.retain(|x| x.id != pid);
    let ccw = (q1 - c).perp_dot(q2 - c) > 0.0;
    let arc = if ccw { sk.add_curve(Geom::Arc { c: cid, a: n1, b: n2 }, false) } else { sk.add_curve(Geom::Arc { c: cid, a: n2, b: n1 }, false) };
    sk.constraints.push(Constraint::Tangent { a: l1, b: arc, inner: false });
    sk.constraints.push(Constraint::Tangent { a: l2, b: arc, inner: false });
    sk.constraints.push(Constraint::PointOnLine { p: n1, l: l1 });
    Ok(())
}

fn offset_selection(app: &mut App, side: DVec2, d: f64) {
    let ids: Vec<Id> = app.sketch.as_ref().unwrap().sel.iter().filter_map(|s| if let Sel::Curve(c) = s { Some(*c) } else { None }).collect();
    if ids.is_empty() {
        app.flash("Select the curves to offset first", ToastKind::Info);
        return;
    }
    let sk = app.sk().0.clone();
    let mut out = sk.clone();
    for id in ids {
        let Some(c) = sk.curve(id) else { continue };
        match c.geom {
            Geom::Line { a, b } => {
                let (pa, pb) = (sk.pv(a), sk.pv(b));
                let n = (pb - pa).perp().normalize();
                let s = if (side - pa).dot(n) >= 0.0 { 1.0 } else { -1.0 };
                let na = out.add_point(pa + n * d * s);
                let nb = out.add_point(pb + n * d * s);
                let l = out.add_curve(Geom::Line { a: na, b: nb }, c.construction);
                out.constraints.push(Constraint::Parallel { a: id, b: l });
                out.constraints.push(Constraint::PointLineDistance { p: na, l: id, dim: Dim { expr: expr::trim(d / app.doc.units.scale(), 4), offset: [0.0; 2] } });
            }
            Geom::Circle { c: cc, r } => {
                let s = if (side - sk.pv(cc)).length() > r { 1.0 } else { -1.0 };
                out.add_curve(Geom::Circle { c: cc, r: (r + d * s).max(1e-3) }, c.construction);
            }
            Geom::Arc { c: cc, a, b } => {
                let ctr = sk.pv(cc);
                let r = (sk.pv(a) - ctr).length();
                let s = if (side - ctr).length() > r { 1.0 } else { -1.0 };
                let nr = (r + d * s).max(1e-3);
                let na = out.add_point(ctr + (sk.pv(a) - ctr).normalize() * nr);
                let nb = out.add_point(ctr + (sk.pv(b) - ctr).normalize() * nr);
                out.add_curve(Geom::Arc { c: cc, a: na, b: nb }, c.construction);
            }
        }
    }
    *app.sk_mut() = out;
}

fn mirror_selection(app: &mut App) {
    let sess = app.sketch.as_ref().unwrap();
    let (sk, _) = app.sk();
    let lines: Vec<Id> = sess.sel.iter().filter_map(|s| if let Sel::Curve(c) = s { Some(*c) } else { None }).filter(|c| matches!(sk.curve(*c).map(|x| &x.geom), Some(Geom::Line { .. }))).collect();
    let Some(&axis) = lines.iter().max_by_key(|c| sk.curve(**c).map(|x| x.construction as u8).unwrap_or(0)) else {
        app.flash("Select the curves to mirror and a line to mirror across", ToastKind::Info);
        return;
    };
    let Some(Geom::Line { a, b }) = sk.curve(axis).map(|c| c.geom.clone()) else { return };
    let (pa, pb) = (sk.pv(a), sk.pv(b));
    let d = (pb - pa).normalize();
    let refl = |p: DVec2| {
        let v = p - pa;
        pa + d * (2.0 * v.dot(d)) - v
    };
    let sel: Vec<Sel> = sess.sel.iter().copied().collect();
    let mut out = sk.clone();
    let mut map = std::collections::HashMap::new();
    let mut new_point = |out: &mut Sketch, id: Id| -> Id {
        *map.entry(id).or_insert_with(|| {
            let p = sk.pv(id);
            // points on the axis map to themselves
            if crate::sketch::seg_dist(p, pa - d * 1e6, pa + d * 1e6) < 1e-9 { id } else { out.add_point(refl(p)) }
        })
    };
    for s in sel {
        if let Sel::Curve(id) = s {
            if id == axis {
                continue;
            }
            let Some(c) = sk.curve(id) else { continue };
            let g = match c.geom {
                Geom::Line { a, b } => Geom::Line { a: new_point(&mut out, a), b: new_point(&mut out, b) },
                Geom::Circle { c, r } => Geom::Circle { c: new_point(&mut out, c), r },
                // mirroring reverses orientation
                Geom::Arc { c, a, b } => Geom::Arc { c: new_point(&mut out, c), a: new_point(&mut out, b), b: new_point(&mut out, a) },
            };
            out.add_curve(g, c.construction);
        }
    }
    app.sk_checkpoint();
    *app.sk_mut() = out;
    app.after_sketch_edit();
}

fn toggle_construction(app: &mut App) {
    let curves: Vec<Id> = app.sketch.as_ref().unwrap().sel.iter().filter_map(|s| if let Sel::Curve(c) = s { Some(*c) } else { None }).collect();
    if curves.is_empty() {
        let s = app.sketch.as_mut().unwrap();
        s.construction = !s.construction;
        return;
    }
    app.sk_checkpoint();
    let sk = app.sk_mut();
    for c in &mut sk.curves {
        if curves.contains(&c.id) {
            c.construction = !c.construction;
        }
    }
    app.after_sketch_edit();
}

fn delete_selection(app: &mut App) {
    let sel: Vec<Sel> = app.sketch.as_ref().unwrap().sel.iter().copied().collect();
    if sel.is_empty() {
        return;
    }
    app.sk_checkpoint();
    let ids: HashSet<Id> = sel.iter().filter_map(|s| match s {
        Sel::Point(p) => Some(*p),
        Sel::Curve(c) => Some(*c),
        _ => None,
    }).collect();
    let cons: HashSet<usize> = sel.iter().filter_map(|s| if let Sel::Constraint(i) = s { Some(*i) } else { None }).collect();
    app.sk_mut().delete(&ids, &cons);
    app.sketch.as_mut().unwrap().sel.clear();
    app.after_sketch_edit();
}

fn project_at(app: &mut App, _raw: DVec2) {
    let Some(pos) = app.viewport_rect.contains(app.last_pointer()).then(|| app.last_pointer()) else { return };
    let local = glam::Vec2::new(pos.x - app.viewport_rect.min.x, pos.y - app.viewport_rect.min.y);
    let cam = app.camera.clone();
    let (_, fr) = app.sk();
    let mut best: Option<(f32, Vec<[f32; 3]>, bool)> = None;
    for b in &app.result.bodies {
        for e in &b.edges {
            let pts: Vec<glam::Vec2> = e.pts.iter().map(|p| cam.project(Vec3::from(*p))).collect();
            for w in pts.windows(2) {
                let d = w[1] - w[0];
                let t = if d.length_squared() < 1e-9 { 0.0 } else { ((local - w[0]).dot(d) / d.length_squared()).clamp(0.0, 1.0) };
                let dist = (w[0] + d * t - local).length();
                if dist < 7.0 && best.as_ref().map(|b| dist < b.0).unwrap_or(true) {
                    best = Some((dist, e.pts.clone(), e.linear));
                }
            }
        }
    }
    let Some((_, pts, linear)) = best else {
        app.flash("Click a model edge to project it", ToastKind::Info);
        return;
    };
    app.sk_checkpoint();
    let local: Vec<DVec2> = pts.iter().map(|p| fr.to_local(glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64))).collect();
    let sk = app.sk_mut();
    let fix = |sk: &mut Sketch, p: DVec2| {
        let id = sk.add_point(p);
        sk.constraints.push(Constraint::Fix { p: id, x: p.x, y: p.y });
        id
    };
    if linear || local.len() == 2 {
        let a = fix(sk, local[0]);
        let b = fix(sk, *local.last().unwrap());
        sk.add_curve(Geom::Line { a, b }, false);
    } else if let Some(c) = circumcenter(local[0], local[local.len() / 3], local[2 * local.len() / 3]) {
        let r = (local[0] - c).length();
        let closed = (local[0] - *local.last().unwrap()).length() < 1e-6;
        let cid = fix(sk, c);
        if closed {
            let ci = sk.add_curve(Geom::Circle { c: cid, r }, false);
            sk.constraints.push(Constraint::Radius { c: ci, dim: Dim { expr: format!("{r} mm"), offset: [r * 0.7, r * 0.7] } });
        } else {
            let a = fix(sk, local[0]);
            let b = fix(sk, *local.last().unwrap());
            let mid = local[local.len() / 2];
            let (t0, t1) = arc_angles(c, local[0], *local.last().unwrap());
            let mut tm = (mid - c).to_angle();
            while tm < t0 {
                tm += std::f64::consts::TAU;
            }
            if tm <= t1 {
                sk.add_curve(Geom::Arc { c: cid, a, b }, false);
            } else {
                sk.add_curve(Geom::Arc { c: cid, a: b, b: a }, false);
            }
        }
    } else {
        let ids: Vec<Id> = local.iter().map(|p| fix(sk, *p)).collect();
        for w in ids.windows(2) {
            sk.add_curve(Geom::Line { a: w[0], b: w[1] }, false);
        }
    }
    app.after_sketch_edit();
}

impl App {
    fn last_pointer(&self) -> Pos2 {
        LAST_POINTER.with(|p| p.get())
    }
}

thread_local! {
    static LAST_POINTER: std::cell::Cell<Pos2> = const { std::cell::Cell::new(Pos2::ZERO) };
}

// ---------------------------------------------------------------- constraints

pub fn constrain(app: &mut App, kind: &str) {
    let sel: Vec<Sel> = app.sketch.as_ref().unwrap().sel.iter().copied().collect();
    let (sk, _) = app.sk();
    let pts: Vec<Id> = sel.iter().filter_map(|s| if let Sel::Point(p) = s { Some(*p) } else { None }).collect();
    let curves: Vec<&Curve> = sel.iter().filter_map(|s| if let Sel::Curve(c) = s { sk.curve(*c) } else { None }).collect();
    let lines: Vec<Id> = curves.iter().filter(|c| matches!(c.geom, Geom::Line { .. })).map(|c| c.id).collect();
    let circles: Vec<Id> = curves.iter().filter(|c| !matches!(c.geom, Geom::Line { .. })).map(|c| c.id).collect();
    let n = pts.len() + curves.len();
    let (label, help) = CONSTRAINTS.iter().find(|c| c.0 == kind).map(|c| (c.1, c.3)).unwrap_or(("", ""));
    let err = format!("{label} needs {help}.");
    let mut add: Vec<Constraint> = vec![];
    let mut merges: Vec<(Id, Id)> = vec![];
    match kind {
        "coincident" => {
            if pts.len() == 2 && n == 2 {
                merges.push((pts[1], pts[0]));
            } else if pts.len() == 1 && curves.len() == 1 {
                let c = curves[0];
                add.push(match c.geom {
                    Geom::Line { .. } => Constraint::PointOnLine { p: pts[0], l: c.id },
                    _ => Constraint::PointOnCircle { p: pts[0], c: c.id },
                });
            } else {
                return app.flash(err, ToastKind::Error);
            }
        }
        "horizontal" | "vertical" => {
            let h = kind == "horizontal";
            if !lines.is_empty() && lines.len() == n {
                for l in &lines {
                    add.push(if h { Constraint::Horizontal { l: *l } } else { Constraint::Vertical { l: *l } });
                }
            } else if pts.len() == 2 && n == 2 {
                add.push(if h { Constraint::HorizontalPts { a: pts[0], b: pts[1] } } else { Constraint::VerticalPts { a: pts[0], b: pts[1] } });
            } else {
                return app.flash(err, ToastKind::Error);
            }
        }
        "parallel" => {
            if lines.len() >= 2 && lines.len() == n {
                for l in &lines[1..] {
                    add.push(Constraint::Parallel { a: lines[0], b: *l });
                }
            } else {
                return app.flash(err, ToastKind::Error);
            }
        }
        "perpendicular" => {
            if lines.len() == 2 && n == 2 {
                add.push(Constraint::Perpendicular { a: lines[0], b: lines[1] });
            } else {
                return app.flash(err, ToastKind::Error);
            }
        }
        "tangent" => {
            if curves.len() == 2 && n == 2 && !circles.is_empty() {
                let (a, b) = (curves[0].id, curves[1].id);
                let inner = if circles.len() == 2 {
                    let r = |id: Id| -> (DVec2, f64) {
                        match sk.curve(id).unwrap().geom {
                            Geom::Circle { c, r } => (sk.pv(c), r),
                            Geom::Arc { c, a, .. } => (sk.pv(c), (sk.pv(a) - sk.pv(c)).length()),
                            _ => (DVec2::ZERO, 0.0),
                        }
                    };
                    let ((c1, r1), (c2, r2)) = (r(a), r(b));
                    (c1 - c2).length() < (r1 - r2).abs() + (r1 + r2 - (r1 - r2).abs()) / 2.0
                } else {
                    false
                };
                add.push(Constraint::Tangent { a, b, inner });
            } else {
                return app.flash(err, ToastKind::Error);
            }
        }
        "equal" => {
            if lines.len() >= 2 && lines.len() == n {
                for l in &lines[1..] {
                    add.push(Constraint::Equal { a: lines[0], b: *l });
                }
            } else if circles.len() >= 2 && circles.len() == n {
                for c in &circles[1..] {
                    add.push(Constraint::Equal { a: circles[0], b: *c });
                }
            } else {
                return app.flash(err, ToastKind::Error);
            }
        }
        "midpoint" => {
            if pts.len() == 1 && lines.len() == 1 && n == 2 {
                add.push(Constraint::Midpoint { p: pts[0], l: lines[0] });
            } else {
                return app.flash(err, ToastKind::Error);
            }
        }
        "concentric" => {
            if circles.len() == 2 && n == 2 {
                let c = |id: Id| Sketch::curve_points(&sk.curve(id).unwrap().geom)[0];
                merges.push((c(circles[1]), c(circles[0])));
            } else {
                return app.flash(err, ToastKind::Error);
            }
        }
        "fix" => {
            if n == 0 {
                return app.flash(err, ToastKind::Error);
            }
            let mut ids: Vec<Id> = pts.clone();
            for c in &curves {
                ids.extend(Sketch::curve_points(&c.geom));
            }
            ids.sort();
            ids.dedup();
            for p in ids {
                let v = sk.pv(p);
                add.push(Constraint::Fix { p, x: v.x, y: v.y });
            }
            for c in &curves {
                if let Geom::Circle { r, .. } = c.geom {
                    add.push(Constraint::Radius { c: c.id, dim: Dim { expr: format!("{} mm", expr::trim(r, 6)), offset: [r * 0.7, r * 0.7] } });
                }
            }
        }
        _ => return,
    }
    app.sk_checkpoint();
    let sk = app.sk_mut();
    for (from, to) in merges {
        sk.merge_point(from, to);
    }
    sk.constraints.extend(add);
    app.sketch.as_mut().unwrap().sel.clear();
    app.after_sketch_edit();
    if let Some(e) = app.sketch.as_ref().unwrap().status.error.clone() {
        app.flash(e, ToastKind::Error);
    }
}

// ---------------------------------------------------------------- dimensions

fn dimension_click(app: &mut App, h: Option<Sel>, raw: DVec2) {
    let sk = app.sk().0.clone();
    let sess = app.sketch.as_mut().unwrap();
    let h = h.filter(|x| !matches!(x, Sel::Constraint(_)));
    match h {
        Some(x) if !sess.dim_pending.contains(&x) => {
            sess.dim_pending.push(x);
            if sess.dim_pending.len() < 2 {
                // a lone circle/arc gets its dimension straight away
                if let Sel::Curve(c) = x {
                    if !matches!(sk.curve(c).map(|c| &c.geom), Some(Geom::Line { .. })) {
                        create_dimension(app, raw);
                    }
                }
                app.scene_dirty = true;
                return;
            }
            create_dimension(app, raw);
        }
        _ => {
            // click in empty space (or on the same item again) places a single-item dimension
            if !sess.dim_pending.is_empty() {
                create_dimension(app, raw);
            }
        }
    }
    app.scene_dirty = true;
}

fn create_dimension(app: &mut App, at: DVec2) {
    let pending = std::mem::take(&mut app.sketch.as_mut().unwrap().dim_pending);
    let (sk, _) = app.sk();
    let u = app.doc.units;
    let fmt_len = |v: f64| expr::trim(v / u.scale(), 3);
    let mk = |v: String| Dim { expr: v, offset: [0.0, 0.0] };
    let line = |id: Id| match sk.curve(id).map(|c| c.geom.clone()) {
        Some(Geom::Line { a, b }) => Some((a, b)),
        _ => None,
    };
    let c = match pending.as_slice() {
        [Sel::Curve(l)] if line(*l).is_some() => {
            let (a, b) = line(*l).unwrap();
            Constraint::Length { l: *l, dim: mk(fmt_len((sk.pv(b) - sk.pv(a)).length())) }
        }
        [Sel::Curve(c)] => match sk.curve(*c).map(|x| x.geom.clone()) {
            Some(Geom::Circle { r, .. }) => Constraint::Diameter { c: *c, dim: mk(fmt_len(2.0 * r)) },
            Some(Geom::Arc { c: cc, a, .. }) => Constraint::Radius { c: *c, dim: mk(fmt_len((sk.pv(a) - sk.pv(cc)).length())) },
            _ => return,
        },
        [Sel::Point(a), Sel::Point(b)] => {
            let d = sk.pv(*b) - sk.pv(*a);
            // place the label to pick horizontal/vertical/aligned
            let mid = (sk.pv(*a) + sk.pv(*b)) / 2.0;
            let off = at - mid;
            if off.y.abs() > off.x.abs() * 2.0 && d.x.abs() > 1e-9 {
                Constraint::AxisDistance { a: *a, b: *b, axis: 0, dim: mk(fmt_len(d.x.abs())) }
            } else if off.x.abs() > off.y.abs() * 2.0 && d.y.abs() > 1e-9 {
                Constraint::AxisDistance { a: *a, b: *b, axis: 1, dim: mk(fmt_len(d.y.abs())) }
            } else {
                Constraint::Distance { a: *a, b: *b, dim: mk(fmt_len(d.length())) }
            }
        }
        [Sel::Point(p), Sel::Curve(l)] | [Sel::Curve(l), Sel::Point(p)] if line(*l).is_some() => {
            let (a, b) = line(*l).unwrap();
            let (pa, pb) = (sk.pv(a), sk.pv(b));
            let dist = ((pb - pa).perp_dot(sk.pv(*p) - pa) / (pb - pa).length()).abs();
            Constraint::PointLineDistance { p: *p, l: *l, dim: mk(fmt_len(dist)) }
        }
        [Sel::Curve(l1), Sel::Curve(l2)] if line(*l1).is_some() && line(*l2).is_some() => {
            let (a, b) = line(*l1).unwrap();
            let (c, d) = line(*l2).unwrap();
            let u1 = (sk.pv(b) - sk.pv(a)).normalize();
            let u2 = (sk.pv(d) - sk.pv(c)).normalize();
            if u1.perp_dot(u2).abs() < 1e-6 {
                // parallel lines: distance between them
                let dist = (u1.perp_dot(sk.pv(c) - sk.pv(a))).abs();
                Constraint::PointLineDistance { p: c, l: *l1, dim: mk(fmt_len(dist)) }
            } else {
                let ang = u1.angle_to(u2).to_degrees();
                Constraint::Angle { a: *l1, b: *l2, dim: mk(expr::trim(ang, 3)) }
            }
        }
        [Sel::Point(p), Sel::Curve(c)] | [Sel::Curve(c), Sel::Point(p)] => {
            // point to circle centre
            let cc = Sketch::curve_points(&sk.curve(*c).unwrap().geom)[0];
            Constraint::Distance { a: *p, b: cc, dim: mk(fmt_len((sk.pv(cc) - sk.pv(*p)).length())) }
        }
        [Sel::Curve(c1), Sel::Curve(c2)] => {
            let a = Sketch::curve_points(&sk.curve(*c1).unwrap().geom)[0];
            let b = Sketch::curve_points(&sk.curve(*c2).unwrap().geom)[0];
            Constraint::Distance { a, b, dim: mk(fmt_len((sk.pv(b) - sk.pv(a)).length())) }
        }
        _ => {
            app.flash("Pick a line, circle or arc, or two items to dimension between", ToastKind::Info);
            return;
        }
    };
    app.sk_checkpoint();
    let anchor = dim_anchor(app.sk().0, &c).unwrap_or(at);
    let mut c = c;
    if let Some(d) = c.dim_mut() {
        let off = at - anchor;
        d.offset = [off.x, off.y];
    }
    app.sk_mut().constraints.push(c);
    let idx = app.sk().0.constraints.len() - 1;
    app.after_sketch_edit();
    open_dim_edit(app, idx);
}

/// Default anchor point of a dimension (labels are drawn at anchor + offset).
fn dim_anchor(sk: &Sketch, c: &Constraint) -> Option<DVec2> {
    use Constraint::*;
    let line_mid = |l: Id| match sk.curve(l).map(|c| &c.geom) {
        Some(Geom::Line { a, b }) => Some((sk.pv(*a) + sk.pv(*b)) / 2.0),
        _ => None,
    };
    let center = |c: Id| sk.curve(c).map(|x| sk.pv(Sketch::curve_points(&x.geom)[0]));
    match c {
        Length { l, .. } => line_mid(*l),
        Distance { a, b, .. } | AxisDistance { a, b, .. } => Some((sk.pv(*a) + sk.pv(*b)) / 2.0),
        PointLineDistance { p, .. } => Some(sk.pv(*p)),
        Radius { c, .. } | Diameter { c, .. } => center(*c),
        Angle { a, b, .. } => {
            let (Some(Geom::Line { a: p, b: q }), Some(Geom::Line { a: r, b: s })) = (sk.curve(*a).map(|c| &c.geom), sk.curve(*b).map(|c| &c.geom)) else {
                return None;
            };
            line_intersection(sk.pv(*p), sk.pv(*q), sk.pv(*r), sk.pv(*s)).or(line_mid(*a))
        }
        _ => None,
    }
}

fn line_intersection(a: DVec2, b: DVec2, c: DVec2, d: DVec2) -> Option<DVec2> {
    let r = b - a;
    let s = d - c;
    let den = r.perp_dot(s);
    if den.abs() < 1e-12 {
        return None;
    }
    Some(a + r * ((c - a).perp_dot(s) / den))
}

// ---------------------------------------------------------------- drawing

fn rgba(c: Color32, a: f32) -> [f32; 4] {
    to_rgba(c, a)
}

/// The active sketch's 3D lines, grid and regions.
pub fn scene(app: &App, sess: &SketchSession, s: &mut SceneData) {
    let Some((sk, fr)) = sketch_of(&app.doc, sess.feature) else { return };
    let w = |p: DVec2| fr.to_world(p).as_vec3();
    // grid around the view centre
    let g = grid_step(app);
    let centre = fr.to_local(app.camera.target.as_dvec3());
    let half = (app.camera.half_height as f64 * app.camera.aspect() as f64).max(app.camera.half_height as f64) * 1.5;
    let n = ((half / g).ceil() as i64).min(400);
    let (cx, cy) = ((centre.x / g).round() as i64, (centre.y / g).round() as i64);
    for i in -n..=n {
        for (axis, k) in [(0, cx + i), (1, cy + i)] {
            let v = k as f64 * g;
            let major = k % 5 == 0;
            let col = if major { vp::GRID_MAJOR } else { vp::GRID_MINOR };
            let (a, b) = if axis == 0 {
                (DVec2::new(v, (cy - n) as f64 * g), DVec2::new(v, (cy + n) as f64 * g))
            } else {
                (DVec2::new((cx - n) as f64 * g, v), DVec2::new((cx + n) as f64 * g, v))
            };
            s.line(w(a), w(b), 1.0, rgba(col, 1.0));
        }
    }
    // sketch axes through the origin
    s.polyline(&[w(DVec2::new(-half * 4.0, 0.0)), w(DVec2::new(half * 4.0, 0.0))], 1.0, rgba(vp::AXIS_X, 0.45), false);
    s.polyline(&[w(DVec2::new(0.0, -half * 4.0)), w(DVec2::new(0.0, half * 4.0))], 1.0, rgba(vp::AXIS_Y, 0.45), false);

    for r in &sess.regions {
        for t in &r.tris {
            s.tri(true, t.map(|p| w(p) + fr.normal().as_vec3() * 0.005), rgba(vp::SKETCH, 0.10));
        }
    }
    let st = &sess.status;
    for c in &sk.curves {
        let sel = sess.sel.contains(&Sel::Curve(c.id));
        let hov = sess.hover == Some(Sel::Curve(c.id)) || sess.dim_pending.contains(&Sel::Curve(c.id));
        let col = if sel {
            vp::PICKED
        } else if hov {
            vp::HOVER
        } else if c.construction {
            vp::CONSTRUCTION
        } else if st.fixed_curves.contains(&c.id) {
            vp::GEOMETRY
        } else {
            vp::UNDER_DEFINED
        };
        let pts: Vec<Vec3> = sk.tessellate(c, (app.camera.wpp() * 3.0) as f64).iter().map(|p| w(*p)).collect();
        if c.construction {
            crate::viewport::dashed(s, &pts, if sel || hov { 2.0 } else { 1.3 }, rgba(col, 1.0), app.camera.wpp() * 6.0);
        } else {
            s.polyline(&pts, if sel || hov { 2.8 } else { 2.0 }, rgba(col, 1.0), true);
        }
    }
    for p in &sk.points {
        let sel = sess.sel.contains(&Sel::Point(p.id));
        let hov = sess.hover == Some(Sel::Point(p.id)) || sess.dim_pending.contains(&Sel::Point(p.id));
        let col = if sel {
            vp::PICKED
        } else if hov {
            vp::HOVER
        } else if st.fixed_points.contains(&p.id) {
            vp::GEOMETRY
        } else {
            vp::UNDER_DEFINED
        };
        s.point(w(p.v()), if sel || hov { 8.0 } else { 5.5 }, rgba(col, 1.0));
    }
    // origin
    s.point(w(DVec2::ZERO), 6.0, rgba(vp::FIXED, 0.9));

    // tool preview
    if let Some(c) = sess.cursor {
        let prev = rgba(vp::PREVIEW, 1.0);
        let mut poly = |pts: Vec<DVec2>| s.polyline(&pts.iter().map(|p| w(*p)).collect::<Vec<_>>(), 1.6, prev, true);
        match sess.tool {
            Tool::Line => {
                if let Some(a) = sess.chain {
                    poly(vec![sk.pv(a), c]);
                }
            }
            Tool::Rect if sess.clicks.len() == 1 => {
                let a = sess.clicks[0];
                poly(vec![a, DVec2::new(c.x, a.y), c, DVec2::new(a.x, c.y), a]);
            }
            Tool::CenterRect if sess.clicks.len() == 1 => {
                let m = sess.clicks[0];
                let a = m - (c - m);
                poly(vec![a, DVec2::new(c.x, a.y), c, DVec2::new(a.x, c.y), a]);
            }
            Tool::Circle if sess.clicks.len() == 1 => {
                let m = sess.clicks[0];
                let r = (c - m).length();
                poly((0..=64).map(|i| m + DVec2::from_angle(i as f64 / 64.0 * std::f64::consts::TAU) * r).collect());
            }
            Tool::Polygon if sess.clicks.len() == 1 => {
                let m = sess.clicks[0];
                let r = (c - m).length();
                let a0 = (c - m).to_angle();
                let n = sess.polygon_sides as usize;
                poly((0..=n).map(|i| m + DVec2::from_angle(a0 + i as f64 / n as f64 * std::f64::consts::TAU) * r).collect());
            }
            Tool::Arc if !sess.clicks.is_empty() => {
                let m = sess.clicks[0];
                if sess.clicks.len() == 1 {
                    poly(vec![m, c]);
                } else {
                    let a = sess.clicks[1];
                    let r = (a - m).length();
                    let e = m + (c - m).normalize_or_zero() * r;
                    let ccw = (a - m).perp_dot(e - m) >= 0.0;
                    let (s0, s1) = if ccw { (a, e) } else { (e, a) };
                    let (t0, t1) = arc_angles(m, s0, s1);
                    poly((0..=48).map(|i| m + DVec2::from_angle(t0 + (t1 - t0) * i as f64 / 48.0) * r).collect());
                }
            }
            Tool::Arc3 if !sess.clicks.is_empty() => {
                if sess.clicks.len() == 1 {
                    poly(vec![sess.clicks[0], c]);
                } else if let Some(m) = circumcenter(sess.clicks[0], sess.clicks[1], c) {
                    let (a, b) = (sess.clicks[0], sess.clicks[1]);
                    let r = (a - m).length();
                    let (t0, t1) = arc_angles(m, a, b);
                    let mut tm = (c - m).to_angle();
                    while tm < t0 {
                        tm += std::f64::consts::TAU;
                    }
                    let (t0, t1) = if tm <= t1 { (t0, t1) } else { arc_angles(m, b, a) };
                    poly((0..=48).map(|i| m + DVec2::from_angle(t0 + (t1 - t0) * i as f64 / 48.0) * r).collect());
                }
            }
            Tool::Slot if !sess.clicks.is_empty() => {
                if sess.clicks.len() == 1 {
                    poly(vec![sess.clicks[0], c]);
                } else {
                    let (c1, c2) = (sess.clicks[0], sess.clicks[1]);
                    let n = (c2 - c1).perp().normalize_or_zero();
                    let r = (c - c1).dot(n).abs();
                    let base = (c2 - c1).to_angle();
                    let mut pts = vec![];
                    for i in 0..=24 {
                        pts.push(c2 + DVec2::from_angle(base - std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * i as f64 / 24.0) * r);
                    }
                    for i in 0..=24 {
                        pts.push(c1 + DVec2::from_angle(base + std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * i as f64 / 24.0) * r);
                    }
                    pts.push(pts[0]);
                    poly(pts);
                }
            }
            _ => {}
        }
        if sess.tool != Tool::Select {
            let col = match sess.snap {
                Some(Snap::Point(_)) | Some(Snap::Origin) => vp::SNAP,
                Some(Snap::Curve(_)) => vp::HOVER,
                _ => vp::PREVIEW,
            };
            s.point(w(c), 6.0, rgba(col, 1.0));
        }
    }
}

/// Screen rectangles of dimension labels and constraint glyphs (for picking).
fn label_rects(app: &App, sk: &Sketch, fr: &Frame, rect: egui::Rect) -> Vec<(usize, egui::Rect)> {
    let mut out = vec![];
    for (i, c) in sk.constraints.iter().enumerate() {
        if let Some(p) = label_pos(app, sk, fr, rect, c, i) {
            let size = if c.dim().is_some() { egui::vec2(48.0, 18.0) } else { egui::vec2(16.0, 16.0) };
            out.push((i, egui::Rect::from_center_size(p, size)));
        }
    }
    out
}

fn to_screen(app: &App, fr: &Frame, rect: egui::Rect, p: DVec2) -> Pos2 {
    let v = app.camera.project(fr.to_world(p).as_vec3());
    Pos2::new(rect.min.x + v.x, rect.min.y + v.y)
}

fn label_pos(app: &App, sk: &Sketch, fr: &Frame, rect: egui::Rect, c: &Constraint, i: usize) -> Option<Pos2> {
    if let Some(d) = c.dim() {
        let a = dim_anchor(sk, c)?;
        return Some(to_screen(app, fr, rect, a + DVec2::from(d.offset)));
    }
    // glyphs sit next to the first referenced curve's midpoint (or point)
    let r = c.refs();
    let base = r.iter().find_map(|id| sk.curve(*id).map(|cv| sk.tessellate(cv, 1.0)).map(|pts| pts[pts.len() / 2])).or_else(|| r.iter().find_map(|id| sk.point(*id).map(|p| p.v())))?;
    let s = to_screen(app, fr, rect, base);
    // spread glyphs on the same spot
    let k = sk.constraints[..i].iter().filter(|o| o.dim().is_none() && o.refs().first() == r.first()).count();
    Some(s + egui::vec2(12.0 + 15.0 * k as f32, -12.0))
}

/// Dimension annotations (also drawn on the model when the sketch isn't being edited).
pub fn draw_dims(app: &App, sk: &Sketch, fr: &Frame, painter: &egui::Painter, rect: egui::Rect, sess: Option<&SketchSession>, ui: Option<&Ui>) {
    let glyphs = ui.is_some();
    let u = app.doc.units;
    let (vars, _) = app.doc.var_values();
    let env = Env { vars: &vars, units: u };
    let ts = |p: DVec2| to_screen(app, fr, rect, p);
    for (i, c) in sk.constraints.iter().enumerate() {
        let sel = sess.map(|s| s.sel.contains(&Sel::Constraint(i))).unwrap_or(false);
        let hov = sess.map(|s| s.hover == Some(Sel::Constraint(i))).unwrap_or(false);
        let bad = sess.map(|s| s.status.bad_dims.contains(&i)).unwrap_or(false);
        let col = if sel {
            vp::PICKED
        } else if hov {
            vp::HOVER
        } else if bad {
            vp::FIXED
        } else {
            vp::DIM
        };
        let stroke = Stroke::new(1.0, col.gamma_multiply(0.9));
        let Some(d) = c.dim() else {
            if glyphs {
                if let Some(p) = label_pos(app, sk, fr, rect, c, i) {
                    let r = egui::Rect::from_center_size(p, egui::vec2(15.0, 15.0));
                    painter.rect_filled(r, 3.0, Color32::from_rgba_unmultiplied(0x15, 0x18, 0x1c, 0xc0));
                    let tint = if sel { vp::PICKED } else if hov { vp::HOVER } else { MUTED };
                    if let Some(ui) = ui {
                        icons::image(c.icon(), 13.0).tint(tint).paint_at(ui, r.shrink(1.0));
                    }
                }
            }
            continue;
        };
        let Some(anchor) = dim_anchor(sk, c) else { continue };
        let lp = ts(anchor + DVec2::from(d.offset));
        let value = expr::eval(&d.expr, if c.is_angle() { Kind::Angle } else { Kind::Length }, &env);
        let shown = match value {
            Ok(v) if c.is_angle() => format!("{}°", expr::trim(v, 2)),
            Ok(v) => {
                let s = u.fmt(v);
                match c {
                    Constraint::Diameter { .. } => format!("⌀{s}"),
                    Constraint::Radius { .. } => format!("R{s}"),
                    _ => s,
                }
            }
            Err(_) => d.expr.clone(),
        };
        let text = if d.expr.trim().parse::<f64>().is_ok() || value.is_err() { shown } else { format!("{} = {shown}", d.expr.trim()) };
        // leader / extension lines
        use Constraint::*;
        match c {
            Length { l, .. } => {
                if let Some(Geom::Line { a, b }) = sk.curve(*l).map(|c| &c.geom) {
                    dim_linear(painter, ts(sk.pv(*a)), ts(sk.pv(*b)), lp, stroke);
                }
            }
            Distance { a, b, .. } => dim_linear(painter, ts(sk.pv(*a)), ts(sk.pv(*b)), lp, stroke),
            AxisDistance { a, b, axis, .. } => {
                let (pa, pb) = (sk.pv(*a), sk.pv(*b));
                let lw = anchor + DVec2::from(d.offset);
                let (qa, qb) = if *axis == 0 { (DVec2::new(pa.x, lw.y), DVec2::new(pb.x, lw.y)) } else { (DVec2::new(lw.x, pa.y), DVec2::new(lw.x, pb.y)) };
                painter.line_segment([ts(pa), ts(qa)], stroke);
                painter.line_segment([ts(pb), ts(qb)], stroke);
                arrow_line(painter, ts(qa), ts(qb), stroke);
            }
            PointLineDistance { p, l, .. } => {
                if let Some(Geom::Line { a, b }) = sk.curve(*l).map(|c| &c.geom) {
                    let (pa, pb, pp) = (sk.pv(*a), sk.pv(*b), sk.pv(*p));
                    let dd = (pb - pa).normalize();
                    let foot = pa + dd * (pp - pa).dot(dd);
                    arrow_line(painter, ts(pp), ts(foot), stroke);
                    painter.line_segment([ts(foot), ts(pa)], Stroke::new(0.8, col.gamma_multiply(0.5)));
                    painter.line_segment([ts((pp + foot) / 2.0), lp], Stroke::new(0.8, col.gamma_multiply(0.6)));
                }
            }
            Radius { c: cid, .. } | Diameter { c: cid, .. } => {
                let ctr = anchor;
                let r = match sk.curve(*cid).map(|x| &x.geom) {
                    Some(Geom::Circle { r, .. }) => *r,
                    Some(Geom::Arc { c, a, .. }) => (sk.pv(*a) - sk.pv(*c)).length(),
                    _ => 0.0,
                };
                let dir = DVec2::from(d.offset).normalize_or(DVec2::X);
                let rim = ctr + dir * r;
                let start = if matches!(c, Diameter { .. }) { ctr - dir * r } else { ctr };
                arrow_line(painter, ts(start), ts(rim), stroke);
                painter.line_segment([ts(rim), lp], stroke);
            }
            Angle { a, b, .. } => {
                if let (Some(Geom::Line { a: p, b: q }), Some(Geom::Line { a: r, b: s })) = (sk.curve(*a).map(|c| &c.geom), sk.curve(*b).map(|c| &c.geom)) {
                    let u1 = (sk.pv(*q) - sk.pv(*p)).normalize();
                    let u2 = (sk.pv(*s) - sk.pv(*r)).normalize();
                    let rad = DVec2::from(d.offset).length().max(app.camera.wpp() as f64 * 30.0);
                    let (t0, mut t1) = (u1.to_angle(), u2.to_angle());
                    while t1 < t0 {
                        t1 += std::f64::consts::TAU;
                    }
                    if t1 - t0 > std::f64::consts::PI {
                        t1 -= std::f64::consts::TAU;
                    }
                    let pts: Vec<Pos2> = (0..=24).map(|i| ts(anchor + DVec2::from_angle(t0 + (t1 - t0) * i as f64 / 24.0) * rad)).collect();
                    painter.add(egui::Shape::line(pts, stroke));
                }
            }
            _ => {}
        }
        let editing = sess.and_then(|s| s.dim_edit.as_ref()).map(|e| e.0 == i).unwrap_or(false);
        if !editing {
            let font = egui::FontId::proportional(12.0);
            let g = painter.layout_no_wrap(text, font, col);
            let r = egui::Rect::from_center_size(lp, g.size() + egui::vec2(8.0, 2.0));
            painter.rect_filled(r, 3.0, Color32::from_rgba_unmultiplied(0x20, 0x25, 0x2c, 0xe0));
            painter.galley(r.min + egui::vec2(4.0, 1.0), g, col);
        }
    }
}

fn dim_linear(painter: &egui::Painter, a: Pos2, b: Pos2, label: Pos2, stroke: Stroke) {
    let d = b - a;
    if d.length() < 1e-3 {
        return;
    }
    let n = egui::vec2(-d.y, d.x).normalized();
    let off = (label - a).dot(n);
    let (qa, qb) = (a + n * off, b + n * off);
    painter.line_segment([a + n * off.signum() * 3.0, qa + n * off.signum() * 4.0], stroke);
    painter.line_segment([b + n * off.signum() * 3.0, qb + n * off.signum() * 4.0], stroke);
    arrow_line(painter, qa, qb, stroke);
}

fn arrow_line(painter: &egui::Painter, a: Pos2, b: Pos2, stroke: Stroke) {
    painter.line_segment([a, b], stroke);
    let d = (b - a).normalized();
    if !d.x.is_finite() {
        return;
    }
    let n = egui::vec2(-d.y, d.x);
    for (p, s) in [(a, 1.0), (b, -1.0)] {
        let tip = [p, p + d * 7.0 * s + n * 3.0, p + d * 7.0 * s - n * 3.0];
        painter.add(egui::Shape::convex_polygon(tip.to_vec(), stroke.color, Stroke::NONE));
    }
}

/// Labels, glyphs, box selection, the cursor read-out and the dimension editor.
pub fn overlay(app: &mut App, sess: &SketchSession, ui: &Ui, painter: &egui::Painter, rect: egui::Rect) {
    if let Some(p) = painter.ctx().pointer_latest_pos() {
        LAST_POINTER.with(|c| c.set(p));
    }
    // other sketches' dimensions stay visible
    let Some((sk, fr)) = sketch_of(&app.doc, sess.feature) else { return };
    let sk = sk.clone();
    draw_dims(app, &sk, &fr, painter, rect, Some(sess), Some(ui));

    if let (Some(Drag::Box { start }), Some(end)) = (&sess.drag, sess.box_end) {
        let r = egui::Rect::from_two_pos(*start, end);
        let crossing = end.x < start.x;
        painter.rect(r, 0.0, theme::ACCENT.gamma_multiply(0.08), Stroke::new(1.0, theme::ACCENT), egui::StrokeKind::Inside);
        if crossing {
            painter.text(r.left_top() + egui::vec2(4.0, 4.0), egui::Align2::LEFT_TOP, "crossing", egui::FontId::proportional(10.0), theme::ACCENT);
        }
    }

    // cursor label: size of what's being drawn
    if let (Some(c), Some(p)) = (sess.cursor, painter.ctx().pointer_hover_pos()) {
        if rect.contains(p) {
            let u = app.doc.units;
            let text = match sess.tool {
                Tool::Line => sess.chain.map(|a| u.fmt((c - sk.pv(a)).length())),
                Tool::Rect if sess.clicks.len() == 1 => {
                    let d = c - sess.clicks[0];
                    Some(format!("{} × {}", u.fmt(d.x.abs()), u.fmt(d.y.abs())))
                }
                Tool::CenterRect if sess.clicks.len() == 1 => {
                    let d = c - sess.clicks[0];
                    Some(format!("{} × {}", u.fmt(2.0 * d.x.abs()), u.fmt(2.0 * d.y.abs())))
                }
                Tool::Circle | Tool::Polygon if sess.clicks.len() == 1 => Some(format!("⌀ {}", u.fmt(2.0 * (c - sess.clicks[0]).length()))),
                Tool::Arc if sess.clicks.len() >= 1 => Some(format!("R {}", u.fmt((sess.clicks.last().copied().unwrap_or(c) - sess.clicks[0]).length().max((c - sess.clicks[0]).length())))),
                Tool::Select => None,
                _ => None,
            };
            if let Some(t) = text {
                crate::viewport::label_box(painter, p + egui::vec2(16.0, 20.0), &t, TEXT);
            }
        }
    }

    // dimension editor
    let edit = app.sketch.as_ref().and_then(|s| s.dim_edit.clone());
    if let Some((i, mut text)) = edit {
        let Some(c) = sk.constraints.get(i) else {
            app.sketch.as_mut().unwrap().dim_edit = None;
            return;
        };
        let Some(a) = dim_anchor(&sk, c) else { return };
        let pos = to_screen(app, &fr, rect, a + DVec2::from(c.dim().map(|d| d.offset).unwrap_or_default()));
        let angle = c.is_angle();
        let (vars, _) = app.doc.var_values();
        let units = app.doc.units;
        let mut done = None;
        egui::Area::new(egui::Id::new("dim-edit")).order(egui::Order::Foreground).pivot(egui::Align2::CENTER_CENTER).fixed_pos(pos).show(painter.ctx(), |ui| {
            egui::Frame::new().fill(theme::BG).stroke(Stroke::new(1.0, theme::ACCENT)).corner_radius(4).inner_margin(2).show(ui, |ui| {
                let ok = expr::eval(&text, if angle { Kind::Angle } else { Kind::Length }, &Env { vars: &vars, units }).is_ok();
                let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(90.0).text_color(if ok { TEXT } else { theme::ERROR_SOFT }));
                if !r.has_focus() && !r.lost_focus() {
                    r.request_focus();
                }
                if ui.input(|i| i.key_pressed(Key::Enter)) && ok {
                    done = Some(true);
                } else if ui.input(|i| i.key_pressed(Key::Escape)) || r.lost_focus() && !ui.input(|i| i.key_pressed(Key::Enter)) {
                    done = Some(false);
                }
                ui.label(RichText::new(if angle { "°" } else { units.label() }).color(MUTED).size(11.0));
            });
        });
        match done {
            Some(true) => {
                app.sk_checkpoint();
                if let Some(d) = app.sk_mut().constraints.get_mut(i).and_then(|c| c.dim_mut()) {
                    d.expr = text.trim().to_string();
                }
                app.sketch.as_mut().unwrap().dim_edit = None;
                app.after_sketch_edit();
                if let Some(e) = app.sketch.as_ref().unwrap().status.error.clone() {
                    app.flash(format!("{e}: that value can't be met"), ToastKind::Error);
                }
            }
            Some(false) => app.sketch.as_mut().unwrap().dim_edit = None,
            None => app.sketch.as_mut().unwrap().dim_edit = Some((i, text)),
        }
    }
}
