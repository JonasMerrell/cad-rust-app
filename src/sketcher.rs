//! Sketch mode: drawing tools, selection, constraints, dimensions and their
//! on-screen presentation.

use std::collections::HashSet;

use egui::{Color32, Key, Pos2, RichText, Stroke, Ui};
use glam::{DVec2, Vec3};

use crate::app::{App, ToastKind};
use crate::doc::{Document, FeatId, Op, Ref};
use crate::expr::{self, Env, Kind};
use crate::icons;
use crate::regions::{self, Region, find_regions};
use crate::render::SceneData;
use crate::sketch::{Constraint, Curve, Dim, Frame, Geom, Id, Sketch, SolveStatus, arc_angles};
use crate::sketch_infer::{self, Dir};
use crate::theme::{self, to_rgba, vp};

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
    TangentArc,
    Polygon,
    Ellipse,
    EllipticalArc,
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
    (Tool::TangentArc, "arc", "Tangent arc", None),
    (Tool::Slot, "slot", "Slot", None),
    (Tool::Polygon, "polygon", "Polygon", None),
    (Tool::Ellipse, "ellipse", "Ellipse", None),
    (Tool::EllipticalArc, "ellipticalArc", "Elliptical arc", None),
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
    ("symmetric", "Symmetric", None, "two points and the line between them"),
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
    /// Midpoint of a sketch line.
    Mid(Id),
    /// A model vertex or edge midpoint, projected onto the sketch plane.
    Model,
}

pub enum Drag {
    /// Moving points; `start` is the sketch cursor where the drag began.
    Geometry {
        points: Vec<(Id, DVec2)>,
        start: DVec2,
        radius: Option<(Id, f64)>,
    },
    /// Moving a dimension label.
    Label {
        idx: usize,
        start: DVec2,
        offset: [f64; 2],
    },
    Box {
        start: Pos2,
    },
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
    /// The order things were clicked into the selection (may hold items no longer selected).
    pub order: Vec<Sel>,
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
    pub text_height: f64,
    /// Text being typed for the Text tool: (baseline start, text).
    pub text_edit: Option<(DVec2, String)>,
    /// Index of the text item being edited (None = new text).
    pub text_editing: Option<usize>,
    /// Sketch pattern popup: (circular?, count, spacing x / angle, spacing y).
    pub pattern: Option<(bool, u32, f64, f64)>,
    /// A size typed while drawing (line length, "w, h" for rectangles, circle diameter).
    pub typed: Option<String>,
    /// Model corner or edge midpoint under the cursor (sketch coordinates), when no sketch geometry is.
    pub model_hover: Option<DVec2>,
    /// Constraints that stopped the last edit, shown highlighted until the next one.
    pub conflicts: Vec<usize>,
    /// The direction the line being drawn is locked to (and the constraint it will get).
    pub infer: Option<Dir>,
    /// Points the cursor lines up with (dashed guides).
    pub guides: Vec<DVec2>,
    /// Lines the cursor recently passed over: a new line can be parallel or perpendicular to them.
    pub woken: Vec<Id>,
}

impl SketchSession {
    /// The selection in click order (box-selected items last).
    pub fn ordered_sel(&self) -> Vec<Sel> {
        let mut out: Vec<Sel> = vec![];
        for x in &self.order {
            if self.sel.contains(x) && !out.contains(x) {
                out.push(*x);
            }
        }
        let mut rest: Vec<Sel> = self.sel.iter().filter(|x| !out.contains(x)).copied().collect();
        rest.sort_by_key(|x| match *x {
            Sel::Point(i) | Sel::Curve(i) => (0, i as usize),
            Sel::Constraint(i) => (1, i),
        });
        out.extend(rest);
        out
    }
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
            s.conflicts.clear();
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
                // ask for the plane; the next planar pick starts the sketch
                self.picks.retain(|r| r.is_planar());
                self.awaiting_sketch_plane = true;
                self.flash_info("Select a plane or flat face to sketch on (Esc cancels)", None);
                self.scene_dirty = true;
                return;
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
            Ref::Face { point, normal, .. } => crate::kernel::plane_frame((*point).into(), (*normal).into()),
            _ => Frame::TOP,
        };
        let before = self.doc.clone();
        self.checkpoint();
        let name = self.doc.next_name("sketch");
        let face = if let Ref::Face { point, .. } = &plane { Some(glam::DVec3::from(*point)) } else { None };
        let id = self.doc.insert(name, Op::Sketch { plane, sketch: Sketch::default(), frame });
        self.picks.clear();
        self.open_session(id, before, true);
        if let Some(p) = face {
            zoom_to_face(self, p, &frame);
        }
    }

    /// Bring a DXF or SVG drawing into the open sketch, or into a new sketch on the Top plane.
    pub fn import_drawing(&mut self, p: &std::path::Path) {
        let d = match crate::sketch_import::read(p) {
            Ok(d) => d,
            Err(e) => return self.flash(e, ToastKind::Error),
        };
        let n = if self.sketch.is_some() {
            self.sk_checkpoint();
            let n = crate::sketch_import::add_to_sketch(self.sk_mut(), &d, 1.0, DVec2::ZERO);
            self.after_sketch_edit();
            n
        } else {
            self.checkpoint();
            let mut sk = Sketch::default();
            let n = crate::sketch_import::add_to_sketch(&mut sk, &d, 1.0, DVec2::ZERO);
            let name = p.file_stem().map(|s| self.doc.unique_name(&s.to_string_lossy())).unwrap_or_else(|| self.doc.next_name("sketch"));
            let plane = Ref::OriginPlane { plane: crate::doc::OriginPlane::Top };
            self.doc.insert(name, Op::Sketch { plane, sketch: sk, frame: Frame::TOP });
            self.first_fit = true;
            self.changed();
            n
        };
        let skipped = if d.skipped.is_empty() { String::new() } else { format!(" (left out {})", crate::sketch_import::skipped_text(&d.skipped)) };
        self.flash(format!("Imported {n} curves from {}{skipped}", p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()), ToastKind::Info);
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
            order: vec![],
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
            text_height: 10.0,
            text_edit: None,
            text_editing: None,
            pattern: None,
            typed: None,
            model_hover: None,
            conflicts: vec![],
            infer: None,
            guides: vec![],
            woken: vec![],
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
        // text outlines follow their (possibly moved) anchor points
        if !sk.texts.is_empty() {
            sk.refresh_texts();
        }
        let regions = find_regions(sk);
        if let Some(s) = &mut self.sketch {
            s.status = st;
            s.regions = regions;
        }
        self.scene_dirty = true;
    }
}

/// Frame the model face through `p` (on the sketch plane `fr`), as a new sketch on a face starts.
fn zoom_to_face(app: &mut App, p: glam::DVec3, fr: &Frame) {
    let mut lo = DVec2::splat(f64::INFINITY);
    let mut hi = DVec2::splat(f64::NEG_INFINITY);
    for b in app.result.bodies.iter().filter(|b| !app.part_hidden(b.id)) {
        let v = |i: u32| glam::DVec3::new(b.positions[3 * i as usize] as f64, b.positions[3 * i as usize + 1] as f64, b.positions[3 * i as usize + 2] as f64);
        // the face whose triangle holds the point
        let on = |t: &[u32]| {
            let (a, b, c) = (v(t[0]), v(t[1]), v(t[2]));
            let n = (b - a).cross(c - a);
            if n.length() < 1e-12 || ((p - a).dot(n) / n.length()).abs() > 1e-4 {
                return false;
            }
            [(a, b), (b, c), (c, a)].iter().all(|(x, y)| (*y - *x).cross(p - *x).dot(n) >= -1e-9 * n.length())
        };
        let Some(fi) = b.indices.chunks(3).position(on).map(|t| b.tri_face[t]) else { continue };
        for (t, tri) in b.indices.chunks(3).enumerate() {
            if b.tri_face[t] == fi {
                for i in tri {
                    let q = fr.to_local(v(*i));
                    lo = lo.min(q);
                    hi = hi.max(q);
                }
            }
        }
        break;
    }
    if lo.x.is_finite() {
        app.camera.target = fr.to_world((lo + hi) / 2.0).as_vec3();
        let aspect = app.camera.aspect() as f64;
        app.camera.half_height = ((hi - lo).y.max((hi - lo).x / aspect.max(1e-3)) * 0.65).max(1.0) as f32;
        app.scene_dirty = true;
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

/// Discard the session's changes. Discarding real work is one undo step, so Ctrl+Z brings it back.
pub fn cancel(app: &mut App) {
    let Some(sess) = app.sketch.take() else { return };
    app.undo.pop();
    let has_work = sketch_of(&app.doc, sess.feature).map(|(s, _)| !s.curves.is_empty() || !s.points.is_empty()).unwrap_or(false);
    if !sess.undo.is_empty() && (has_work || !sess.is_new) {
        let edited = std::mem::replace(&mut app.doc, sess.before);
        app.undo.push(edited);
        app.redo.clear();
        app.flash(if sess.is_new { "Sketch discarded · Ctrl+Z brings it back" } else { "Sketch changes discarded · Ctrl+Z brings them back" }, ToastKind::Info);
    } else {
        app.doc = sess.before;
    }
    app.changed();
}

/// After undo or redo inside a sketch: in-progress tool steps (a line chain, clicks, dimension picks, an open
/// dimension box, a drag) refer to points and constraint indices of the old state, so drop them, and keep
/// only the selected items that still exist.
pub fn after_undo(app: &mut App) {
    let Some(sess) = app.sketch.as_ref() else { return };
    let Some((sk, _)) = sketch_of(&app.doc, sess.feature) else { return };
    let exists = |x: &Sel| match *x {
        Sel::Point(p) => sk.point(p).is_some(),
        Sel::Curve(c) => sk.curve(c).is_some(),
        Sel::Constraint(i) => i < sk.constraints.len(),
    };
    let sel: HashSet<Sel> = sess.sel.iter().copied().filter(|x| exists(x)).collect();
    let order: Vec<Sel> = sess.order.iter().copied().filter(|x| exists(x)).collect();
    let s = app.sketch.as_mut().unwrap();
    s.chain = None;
    s.clicks.clear();
    s.dim_pending.clear();
    s.dim_edit = None;
    s.drag = None;
    s.typed = None;
    s.conflicts.clear();
    s.sel = sel;
    s.order = order;
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
            Tool::Text => {
                ui.add(egui::DragValue::new(&mut s.text_height).range(0.01..=1e5).speed(0.1)).on_hover_text("Text height");
            }
            _ => {}
        }
        if app.tool_button(ui, "import", "Import", "Import a DXF or SVG drawing into the sketch", false, false, true).clicked() {
            if let Some(p) = app.open_file_dialog("DXF, SVG", &["dxf", "svg", "DXF", "SVG"]) {
                app.import_drawing(&p);
            }
        }
        if app.tool_button(ui, "linearPattern", "Linear pattern", "Linear pattern of the selection", false, false, has_sel).clicked() {
            app.sketch.as_mut().unwrap().pattern = Some((false, 3, 20.0, 0.0));
        }
        if app.tool_button(ui, "circularPattern", "Circular pattern", "Circular pattern of the selection about the origin (or a selected point)", false, false, has_sel).clicked() {
            app.sketch.as_mut().unwrap().pattern = Some((true, 6, 360.0, 0.0));
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
        if app.tool_button(ui, "delete", "Delete", "Delete (Del)", false, false, has_sel).clicked() {
            delete_selection(app);
        }
        if app.tool_button(ui, "grid", "Grid", "Snap to grid (G): the cursor catches grid lines only when it is close to them", false, grid, true).clicked() {
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
    ui.painter().vline(rect.center().x, rect.y_range(), Stroke::new(1.0, theme::border()));
    ui.add_space(6.0);
    ui.spacing_mut().item_spacing.x = 1.0;
    add(ui);
    ui.spacing_mut().item_spacing.x = 8.0;
}

pub fn set_tool(app: &mut App, t: Tool) {
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
        Tool::Line => "Click to place line points; Esc or right-click ends the chain. Hover a line to draw parallel or perpendicular to it; hold Shift to place freely",
        Tool::Rect => "Click two opposite corners",
        Tool::CenterRect => "Click the centre, then a corner",
        Tool::Circle => "Click the centre, then a point on the circle",
        Tool::Arc => "Click the centre, the start, then the end (counter-clockwise)",
        Tool::Arc3 => "Click the start, the end, then a point on the arc",
        Tool::Slot => "Click the two end centres, then the width",
        Tool::TangentArc => "Click the end of a line or arc, then where the tangent arc ends",
        Tool::Ellipse => "Click the centre, the end of the major axis, then the minor size",
        Tool::EllipticalArc => "Click the centre, the major axis end, the minor size, then the start and end (counter-clockwise)",
        Tool::Spline => "Click points along the curve; Enter, right-click or clicking the first point finishes",
        Tool::Text => "Click where the text starts, type, then press Enter",
        Tool::Polygon => "Click the centre, then a corner",
        Tool::Trim => "Click the piece of a curve to remove",
        Tool::Extend => "Click near the end of a line to extend it to the next curve",
        Tool::Fillet2d => "Click a corner where two lines meet",
        Tool::Offset => "Click on the side to offset the selected curves to",
        Tool::Project => "Click model edges to bring them into the sketch",
        Tool::Dimension => "Click a line, circle, arc or two items, then click where the dimension goes and type its value; click a dimension to change it",
    };
    let dof = if let Some(e) = &s.status.error {
        format!(" · {e}")
    } else if s.status.dof == 0 && !app.sk().0.points.is_empty() {
        " · fully defined".to_string()
    } else if s.status.dof > 0 {
        format!(" · {} degree{} of freedom", s.status.dof, if s.status.dof == 1 { "" } else { "s" })
    } else {
        String::new()
    };
    format!("{base}{dof}")
}

// ---------------------------------------------------------------- keyboard

pub fn shortcuts(app: &mut App, ctx: &egui::Context) {
    let editing_dim = app.sketch.as_ref().map(|s| s.dim_edit.is_some() || s.text_edit.is_some() || s.pattern.is_some()).unwrap_or(false);
    if editing_dim {
        return;
    }
    if typed_input(app, ctx) {
        return;
    }
    if crate::app::key_mods(ctx).0 && ctx.input(|i| i.key_pressed(Key::A)) {
        // select everything in the sketch
        let (sk, _) = app.sk();
        let all: Vec<Sel> = sk.curves.iter().map(|c| Sel::Curve(c.id)).chain(sk.points.iter().map(|p| Sel::Point(p.id))).collect();
        let s = app.sketch.as_mut().unwrap();
        s.sel = all.into_iter().collect();
        app.scene_dirty = true;
        return;
    }
    if crate::app::key_mods(ctx).0 {
        // other Ctrl keys are not the letters' tools
        return;
    }
    if crate::app::key_mods(ctx).1 && ctx.input(|i| i.key_pressed(Key::E)) {
        app.extrude_from_sketch();
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
        let spline_open = app.sketch.as_ref().map(|s| s.tool == Tool::Spline && s.clicks.len() >= 2).unwrap_or(false);
        if spline_open {
            finish_spline(app, false);
        } else {
            finish(app);
        }
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
    if pressed(Key::F) {
        app.fit_view();
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
    let step = if m < 2.0 {
        2.0
    } else if m < 5.0 {
        5.0
    } else {
        10.0
    };
    step * p * units
}

/// Snap the cursor (sketch coordinates) to points, curves, the origin or the grid.
/// Geometry snaps come first; with nothing under the cursor it lines up with points, and
/// only then (weakly, per axis) with the grid. Also returns the points it lines up with.
fn snap(app: &App, s: Pos2, rect: egui::Rect, raw: DVec2, exclude: &HashSet<Id>) -> (DVec2, Option<Snap>, Vec<DVec2>) {
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
        return (p, Some(sn), vec![]);
    }
    let o = (to_screen(DVec2::ZERO) - s).length();
    if o < 9.0 {
        return (DVec2::ZERO, Some(Snap::Origin), vec![]);
    }
    // midpoints of sketch lines
    for c in &sk.curves {
        if let Geom::Line { a, b } = c.geom {
            if exclude.contains(&a) || exclude.contains(&b) {
                continue;
            }
            let m = (sk.pv(a) + sk.pv(b)) / 2.0;
            let d = (to_screen(m) - s).length();
            if d < 8.0 && best.as_ref().map(|b| d < b.0).unwrap_or(true) {
                best = Some((d, m, Snap::Mid(c.id)));
            }
        }
    }
    // model corners and edge midpoints, projected onto the sketch plane
    for b in app.result.bodies.iter().filter(|b| !app.part_hidden(b.id)) {
        for e in &b.edges {
            let ends = [e.pts.first(), e.pts.last()].map(|p| p.map(|p| glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)));
            for w in ends.into_iter().flatten().chain(std::iter::once(glam::DVec3::from(e.mid))) {
                let m = fr.to_local(w);
                let d = (to_screen(m) - s).length();
                if d < 7.0 && best.as_ref().map(|b| d < b.0).unwrap_or(true) {
                    best = Some((d, m, Snap::Model));
                }
            }
        }
    }
    if let Some((_, p, sn)) = best {
        return (p, Some(sn), vec![]);
    }
    let wpp = app.camera.wpp() as f64;
    for c in &sk.curves {
        if Sketch::curve_points(&c.geom).iter().any(|p| exclude.contains(p)) {
            continue;
        }
        let d = sk.curve_distance(c, raw);
        if d / wpp < 6.0 {
            if let Some(u) = regions::param_at(sk, c.id, raw) {
                return (regions::point_at(sk, c.id, u), Some(Snap::Curve(c.id)), vec![]);
            }
        }
    }
    let sess = app.sketch.as_ref().unwrap();
    let grid = sess.grid_snap.then(|| grid_step(app));
    let inf = sketch_infer::free_point(sk, raw, wpp, grid, exclude);
    (inf.p, inf.grid.then_some(Snap::Grid), inf.align)
}

fn sketch_cursor(app: &App, s: Pos2, rect: egui::Rect) -> Option<DVec2> {
    let (_, fr) = app.sk();
    let (o, d) = app.camera.ray(glam::Vec2::new(s.x - rect.min.x, s.y - rect.min.y));
    fr.ray_hit(o.as_dvec3(), d.as_dvec3())
}

/// What is under the cursor in sketch mode: points, then dimension labels and glyphs, then curves.
/// While the Dimension tool has a pick pending, curves come before labels: the next pick is geometry.
fn hit(app: &App, s: Pos2, rect: egui::Rect, raw: DVec2) -> Option<Sel> {
    let geometry_first = app.sketch.as_ref().map(|x| x.tool == Tool::Dimension && !x.dim_pending.is_empty()).unwrap_or(false);
    let (sk, fr) = app.sk();
    let to_screen = |p: DVec2| {
        let v = app.camera.project(fr.to_world(p).as_vec3());
        Pos2::new(rect.min.x + v.x, rect.min.y + v.y)
    };
    let mut best: Option<(f32, Sel)> = None;
    // a pending dimension pick reaches a little further for points (circle centres sit under leaders)
    let reach = if geometry_first { 12.0 } else { 8.0 };
    for p in &sk.points {
        let d = (to_screen(p.v()) - s).length();
        if d < reach && best.as_ref().map(|b| d < b.0).unwrap_or(true) {
            best = Some((d, Sel::Point(p.id)));
        }
    }
    if let Some((_, h)) = best {
        return Some(h);
    }
    let curve = || {
        let wpp = app.camera.wpp() as f64;
        let mut best: Option<(f64, Sel)> = None;
        for c in &sk.curves {
            let d = sk.curve_distance(c, raw) / wpp;
            if d < 6.0 && best.as_ref().map(|b| d < b.0).unwrap_or(true) {
                best = Some((d, Sel::Curve(c.id)));
            }
        }
        best.map(|b| b.1)
    };
    if geometry_first {
        if let Some(c) = curve() {
            return Some(c);
        }
    }
    // dimension labels and constraint glyphs
    for (i, r) in label_rects(app, sk, &fr, rect) {
        if r.expand(2.0).contains(s) {
            return Some(Sel::Constraint(i));
        }
    }
    curve()
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
    let mut h = if pointer.is_some() { hit(app, s, rect, raw) } else { None };
    // model corners and edge midpoints can be picked too (to dimension or constrain to)
    let model_pt = if h.is_none() && pointer.is_some() && matches!(tool, Tool::Select | Tool::Dimension) { model_point_near(app, s, rect, tool == Tool::Dimension) } else { None };
    if app.sketch.as_ref().unwrap().model_hover != model_pt {
        app.sketch.as_mut().unwrap().model_hover = model_pt;
        app.scene_dirty = true;
    }
    if let (Some(p), true) = (model_pt, resp.clicked_by(egui::PointerButton::Primary)) {
        // becomes a fixed sketch point at the projected position (reusing one already there)
        app.sk_checkpoint();
        let existing = app.sk().0.points.iter().find(|q| (q.v() - p).length() < 1e-6).map(|q| q.id);
        let id = existing.unwrap_or_else(|| {
            let sk = app.sk_mut();
            let id = sk.add_point(p);
            sk.constraints.push(Constraint::Fix { p: id, x: p.x, y: p.y });
            id
        });
        h = Some(Sel::Point(id));
        app.after_sketch_edit();
    }
    let exclude: HashSet<Id> = match &app.sketch.as_ref().unwrap().drag {
        Some(Drag::Geometry { points, .. }) => points.iter().map(|p| p.0).collect(),
        _ => HashSet::new(),
    };
    // a single point being dragged shows what it would join when dropped
    let point_drag = matches!(&app.sketch.as_ref().unwrap().drag, Some(Drag::Geometry { points, radius: None, .. }) if points.len() == 1);
    let (mut snapped, snp, mut guides) = if point_drag {
        match snap(app, s, rect, raw, &exclude) {
            (p, sn @ Some(Snap::Point(_) | Snap::Curve(_) | Snap::Mid(_) | Snap::Origin), _) => (p, sn, vec![]),
            _ => (raw, None, vec![]),
        }
    } else if matches!(tool, Tool::Select | Tool::Dimension) {
        (raw, None, vec![])
    } else {
        match snap(app, s, rect, raw, &exclude) {
            // Shift places freely: no grid, alignment or direction
            (_, None | Some(Snap::Grid), _) if shift => (raw, None, vec![]),
            x => x,
        }
    };
    // lines passed over can be referenced (parallel / perpendicular) by the next line drawn
    if let (Tool::Line, Some(Sel::Curve(c))) = (tool, h) {
        if matches!(app.sk().0.curve(c).map(|x| &x.geom), Some(Geom::Line { .. })) {
            let w = &mut app.sketch.as_mut().unwrap().woken;
            if w.first() != Some(&c) {
                w.retain(|x| *x != c);
                w.insert(0, c);
                w.truncate(3);
            }
        }
    }
    // the line being drawn locks to horizontal, vertical, parallel or perpendicular
    let mut infer = None;
    let chain = app.sketch.as_ref().unwrap().chain;
    if let (Tool::Line, Some(a), None | Some(Snap::Grid), false) = (tool, chain, snp, shift) {
        let sess = app.sketch.as_ref().unwrap();
        let grid = sess.grid_snap.then(|| grid_step(app));
        let inf = sketch_infer::line_end(app.sk().0, a, raw, app.camera.wpp() as f64, grid, &sess.woken, &exclude);
        snapped = inf.p;
        guides = inf.align;
        infer = inf.dir;
    }
    {
        let sess = app.sketch.as_mut().unwrap();
        if sess.hover != h || sess.cursor != Some(snapped) || sess.snap != snp || sess.infer != infer || sess.guides != guides {
            sess.hover = h;
            sess.cursor = Some(snapped);
            sess.snap = snp;
            sess.infer = infer;
            sess.guides = guides;
            app.scene_dirty = true;
        }
    }

    // right click ends a chain / cancels the tool step
    if resp.secondary_clicked() {
        if tool == Tool::Spline && app.sketch.as_ref().unwrap().clicks.len() >= 2 {
            finish_spline(app, false);
            app.context_menu = None;
            return;
        }
        let sess = app.sketch.as_mut().unwrap();
        if !sess.clicks.is_empty() || sess.chain.is_some() || !sess.dim_pending.is_empty() {
            sess.clicks.clear();
            sess.chain = None;
            sess.dim_pending.clear();
            app.context_menu = None;
            app.scene_dirty = true;
        }
    }

    match tool {
        Tool::Select => select_input(app, ui, resp, rect, s, raw, h, shift),
        // dimension labels can be dragged without leaving the tool
        Tool::Dimension if label_drag(app, ui, resp, rect, raw) => {}
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
            } else if let (true, Some(drop)) = (points.len() == 1 && radius.is_none(), app.sketch.as_ref().unwrap().snap.and_then(drop_target)) {
                // dropping a point onto a point, curve, midpoint or the origin joins it there
                let before = app.doc.clone();
                let p = points[0].0;
                if sketch_infer::drop_point(app.sk_mut(), p, drop) {
                    app.solve_sketch(&[]);
                    if let Some(e) = app.sketch.as_ref().unwrap().status.error.clone() {
                        app.doc = before;
                        app.solve_sketch(&[]);
                        app.flash(format!("Can't join there: {e}"), ToastKind::Error);
                    } else {
                        let sess = app.sketch.as_mut().unwrap();
                        sess.sel.remove(&Sel::Point(p));
                        sess.snap = None;
                    }
                }
                app.changed();
            } else {
                app.changed();
            }
        }
        Some(Drag::Label { idx, start, offset }) => move_label(app, idx, start, offset, raw, dragging),
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
        // double-clicking text outlines edits the text
        if let Some(Sel::Curve(c)) = h {
            let found = {
                let (sk, _) = app.sk();
                sk.text_of(c).map(|i| (i, sk.pv(sk.texts[i].anchor), sk.texts[i].text.clone(), sk.texts[i].height))
            };
            if let Some((i, at, text, height)) = found {
                let scale = app.doc.units.scale();
                let sess = app.sketch.as_mut().unwrap();
                sess.text_edit = Some((at, text));
                sess.text_editing = Some(i);
                sess.text_height = height / scale;
            }
        }
        return;
    }
    if resp.clicked_by(egui::PointerButton::Primary) {
        let sess = app.sketch.as_mut().unwrap();
        match h {
            Some(x) => {
                if shift || true {
                    // like the original: clicking adds to the selection, clicking again removes
                    if sess.sel.remove(&x) {
                        sess.order.retain(|o| *o != x);
                    } else {
                        sess.sel.insert(x);
                        sess.order.retain(|o| *o != x);
                        sess.order.push(x);
                    }
                }
            }
            None => {
                sess.sel.clear();
                sess.order.clear();
            }
        }
        app.scene_dirty = true;
    }
}

fn move_label(app: &mut App, idx: usize, start: DVec2, offset: [f64; 2], raw: DVec2, dragging: bool) {
    let d = raw - start;
    if let Some(dim) = app.sk_mut().constraints.get_mut(idx).and_then(|c| c.dim_mut()) {
        dim.offset = [offset[0] + d.x, offset[1] + d.y];
    }
    app.scene_dirty = true;
    if dragging {
        app.sketch.as_mut().unwrap().drag = Some(Drag::Label { idx, start, offset });
    }
}

/// Starts or continues dragging a dimension label; true while one is being dragged.
fn label_drag(app: &mut App, ui: &Ui, resp: &egui::Response, rect: egui::Rect, raw: DVec2) -> bool {
    if resp.drag_started_by(egui::PointerButton::Primary) {
        let press = ui.input(|i| i.pointer.press_origin());
        let raw0 = press.and_then(|p| sketch_cursor(app, p, rect)).unwrap_or(raw);
        if let Some(Sel::Constraint(i)) = press.and_then(|p| hit(app, p, rect, raw0)) {
            if let Some(d) = app.sk().0.constraints[i].dim() {
                let offset = d.offset;
                app.sk_checkpoint();
                app.sketch.as_mut().unwrap().drag = Some(Drag::Label { idx: i, start: raw0, offset });
            }
        }
    }
    match app.sketch.as_mut().unwrap().drag.take() {
        Some(Drag::Label { idx, start, offset }) => {
            move_label(app, idx, start, offset, raw, ui.input(|i| i.pointer.primary_down()));
            true
        }
        other => {
            app.sketch.as_mut().unwrap().drag = other;
            false
        }
    }
}

/// A model vertex or edge midpoint near the cursor, projected onto the sketch plane
/// (or, with `origin`, the sketch origin, so dimensions can measure from it).
fn model_point_near(app: &App, s: Pos2, rect: egui::Rect, origin: bool) -> Option<DVec2> {
    let (_, fr) = app.sk();
    let to_screen = |p: DVec2| {
        let v = app.camera.project(fr.to_world(p).as_vec3());
        Pos2::new(rect.min.x + v.x, rect.min.y + v.y)
    };
    let mut best: Option<(f32, DVec2)> = None;
    if origin {
        let d = (to_screen(DVec2::ZERO) - s).length();
        if d < 8.0 {
            best = Some((d, DVec2::ZERO));
        }
    }
    for b in app.result.bodies.iter().filter(|b| !app.part_hidden(b.id)) {
        for e in &b.edges {
            let ends = [e.pts.first(), e.pts.last()].map(|p| p.map(|p| glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)));
            for w in ends.into_iter().flatten().chain(std::iter::once(glam::DVec3::from(e.mid))) {
                let m = fr.to_local(w);
                let d = (to_screen(m) - s).length();
                if d < 8.0 && best.map(|b| d < b.0).unwrap_or(true) {
                    best = Some((d, m));
                }
            }
        }
    }
    best.map(|b| b.1)
}

fn open_dim_edit(app: &mut App, i: usize) {
    let (sk, _) = app.sk();
    if sk.constraints.get(i).and_then(|c| c.dim()).map(|d| d.driven).unwrap_or(false) {
        app.flash("That's a reference dimension: it shows a size other constraints set. Delete one of those to drive it instead", ToastKind::Info);
        return;
    }
    if let Some(d) = sk.constraints.get(i).and_then(|c| c.dim()) {
        let text = d.expr.clone();
        app.sketch.as_mut().unwrap().dim_edit = Some((i, text));
    }
}

// ---------------------------------------------------------------- creation

/// Curves (other than lines) that a point can be constrained onto.
fn holds_points(g: &Geom) -> bool {
    matches!(g, Geom::Circle { .. } | Geom::Arc { .. } | Geom::Ellipse { .. } | Geom::EllipticalArc { .. })
}

/// What dropping a dragged point here joins it to.
fn drop_target(s: Snap) -> Option<sketch_infer::Drop> {
    use sketch_infer::Drop;
    match s {
        Snap::Point(p) => Some(Drop::Point(p)),
        Snap::Curve(c) => Some(Drop::Curve(c)),
        Snap::Mid(l) => Some(Drop::Mid(l)),
        Snap::Origin => Some(Drop::Origin),
        _ => None,
    }
}

/// Use an existing point when snapped to one, else create one (adding an
/// on-curve or origin constraint for curve/origin snaps).
fn point_at(app: &mut App, p: DVec2, snp: Option<Snap>) -> Id {
    match snp {
        Some(Snap::Point(id)) => id,
        Some(Snap::Curve(c)) => {
            let sk = app.sk_mut();
            let id = sk.add_point(p);
            // splines and outlines can't hold a point: it is only placed there
            match sk.curve(c).map(|x| &x.geom) {
                Some(Geom::Line { .. }) => sk.constraints.push(Constraint::PointOnLine { p: id, l: c }),
                Some(g) if holds_points(g) => sk.constraints.push(Constraint::PointOnCircle { p: id, c }),
                _ => {}
            }
            id
        }
        Some(Snap::Origin) => {
            let sk = app.sk_mut();
            let id = sk.add_point(DVec2::ZERO);
            sk.constraints.push(Constraint::Fix { p: id, x: 0.0, y: 0.0 });
            id
        }
        Some(Snap::Mid(l)) => {
            let sk = app.sk_mut();
            let id = sk.add_point(p);
            sk.constraints.push(Constraint::Midpoint { p: id, l });
            id
        }
        Some(Snap::Model) => {
            let sk = app.sk_mut();
            let id = sk.add_point(p);
            sk.constraints.push(Constraint::Fix { p: id, x: p.x, y: p.y });
            id
        }
        _ => app.sk_mut().add_point(p),
    }
}

/// Typing a size while drawing: digits (and expressions) collect in `typed`; Enter places
/// the line, rectangle or circle at that size along the cursor, with a dimension.
/// Returns true when the key presses were used.
fn typed_input(app: &mut App, ctx: &egui::Context) -> bool {
    let Some(sess) = app.sketch.as_ref() else { return false };
    let active = match sess.tool {
        Tool::Line => sess.chain.is_some(),
        Tool::Rect | Tool::CenterRect | Tool::Circle => sess.clicks.len() == 1,
        _ => false,
    };
    if !active {
        if sess.typed.is_some() {
            app.sketch.as_mut().unwrap().typed = None;
        }
        return false;
    }
    let (texts, enter, back, esc) = ctx.input(|i| {
        let t: String = i.events.iter().filter_map(|e| if let egui::Event::Text(t) = e { Some(t.clone()) } else { None }).collect();
        (t, i.key_pressed(Key::Enter), i.key_pressed(Key::Backspace), i.key_pressed(Key::Escape))
    });
    let mut typed = sess.typed.clone();
    let mut used = false;
    // start only on something number-like, so tool letters keep working
    let starts = |t: &str| t.chars().next().map(|c| c.is_ascii_digit() || c == '.' || c == '(').unwrap_or(false);
    if !texts.is_empty() && (typed.is_some() || starts(&texts)) {
        typed.get_or_insert_with(String::new).push_str(&texts);
        used = true;
    }
    if typed.is_some() {
        if back {
            let t = typed.as_mut().unwrap();
            t.pop();
            if t.is_empty() {
                typed = None;
            }
            used = true;
        } else if esc {
            typed = None;
            used = true;
        } else if enter {
            let t = typed.take().unwrap_or_default();
            app.sketch.as_mut().unwrap().typed = None;
            commit_typed(app, &t);
            return true;
        }
    }
    app.sketch.as_mut().unwrap().typed = typed;
    if used {
        app.scene_dirty = true;
    }
    used
}

fn commit_typed(app: &mut App, text: &str) {
    let (vars, _) = app.doc.var_values();
    let units = app.doc.units;
    let eval = |s: &str| crate::doc::eval(s.trim(), crate::expr::Kind::Length, &vars, units);
    let sess = app.sketch.as_ref().unwrap();
    let tool = sess.tool;
    let cursor = sess.cursor.unwrap_or_default();
    let construction = sess.construction;
    let dim = |expr: &str, off: [f64; 2]| Dim::new(expr.trim(), off);
    match tool {
        Tool::Line => {
            let Ok(len) = eval(text) else {
                app.flash(format!("\"{text}\" is not a length"), ToastKind::Error);
                return;
            };
            let a = sess.chain.unwrap();
            let pa = app.sk().0.pv(a);
            // the cursor already follows the inferred direction
            let dir = (cursor - pa).normalize_or(DVec2::X);
            let infer = sess.infer;
            if len <= 0.0 {
                return;
            }
            app.sk_checkpoint();
            let sk = app.sk_mut();
            let end = pa + dir * len;
            // landing on an existing point (closing a loop) joins it and ends the chain
            let existing = sk.points.iter().find(|q| q.id != a && (q.v() - end).length() < 1e-6 * len.max(1.0)).map(|q| q.id);
            let b = existing.unwrap_or_else(|| sk.add_point(end));
            let l = sk.add_curve(Geom::Line { a, b }, construction);
            if let Some(d) = infer {
                sk.constraints.push(d.constraint(l));
            }
            let n = dir.perp() * (len * 0.15).max(2.0);
            let length = Constraint::Length { l, dim: dim(text, [n.x, n.y]) };
            app.sketch.as_mut().unwrap().chain = if existing.is_some() { None } else { Some(b) };
            if existing.is_some() {
                // the closing side's length may already follow from the others: then it only measures
                app.solve_sketch(&[]);
                let dof = app.sketch.as_ref().unwrap().status.dof;
                app.sk_mut().constraints.push(length);
                app.solve_sketch(&[]);
                let st = &app.sketch.as_ref().unwrap().status;
                if st.error.is_none() && st.dof >= dof {
                    if let Some(d) = app.sk_mut().constraints.last_mut().and_then(|c| c.dim_mut()) {
                        d.driven = true;
                    }
                }
            } else {
                app.sk_mut().constraints.push(length);
            }
            app.after_sketch_edit();
        }
        Tool::Rect | Tool::CenterRect => {
            let Some((wt, ht)) = split_size(text) else { return };
            let (Ok(w), Ok(h)) = (eval(wt), eval(ht)) else {
                app.flash(format!("\"{text}\" is not a size (type width, height)"), ToastKind::Error);
                return;
            };
            let a = sess.clicks[0];
            let sx = if cursor.x < a.x { -1.0 } else { 1.0 };
            let sy = if cursor.y < a.y { -1.0 } else { 1.0 };
            let centered = tool == Tool::CenterRect;
            let b = if centered { a + DVec2::new(sx * w / 2.0, sy * h / 2.0) } else { a + DVec2::new(sx * w, sy * h) };
            let first = FIRST_SNAP.with(|f| f.get());
            app.sketch.as_mut().unwrap().clicks.clear();
            app.sk_checkpoint();
            let before = app.sk().0.curves.len();
            make_rect(app, a, b, first, None, construction, centered);
            let sk = app.sk_mut();
            let new: Vec<Id> = sk.curves[before..].iter().filter(|c| !c.construction || construction).map(|c| c.id).collect();
            if new.len() >= 2 {
                sk.constraints.push(Constraint::Length { l: new[0], dim: dim(wt, [0.0, -sy * 4.0]) });
                sk.constraints.push(Constraint::Length { l: new[1], dim: dim(ht, [sx * 4.0, 0.0]) });
            }
            app.after_sketch_edit();
        }
        Tool::Circle => {
            let Ok(d) = eval(text) else {
                app.flash(format!("\"{text}\" is not a diameter"), ToastKind::Error);
                return;
            };
            if d <= 0.0 {
                return;
            }
            let a = sess.clicks[0];
            let first = FIRST_SNAP.with(|f| f.get());
            app.sketch.as_mut().unwrap().clicks.clear();
            app.sk_checkpoint();
            let c = point_at(app, a, first);
            let sk = app.sk_mut();
            let ci = sk.add_curve(Geom::Circle { c, r: d / 2.0 }, construction);
            sk.constraints.push(Constraint::Diameter { c: ci, dim: dim(text, [d * 0.5, d * 0.5]) });
            app.after_sketch_edit();
        }
        _ => {}
    }
}

/// A typed rectangle size, "width, height" (or "w x h", "w × h", "w*h"), or one value for a square.
/// Only separators outside parentheses count, and an `x` only when it isn't part of a name, so
/// expressions such as `max(10, w)` or a variable `box_w` stay whole.
fn split_size(text: &str) -> Option<(&str, &str)> {
    let cs: Vec<(usize, char)> = text.char_indices().collect();
    let name = |c: char| c.is_alphanumeric() || c == '_';
    let find = |sep: &dyn Fn(usize, char) -> bool| -> Option<usize> {
        let mut depth = 0i32;
        for (k, &(_, c)) in cs.iter().enumerate() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ if depth == 0 && sep(k, c) => return Some(k),
                _ => {}
            }
        }
        None
    };
    let lone_x = |k: usize, c: char| {
        let before = k.checked_sub(1).map(|j| cs[j].1);
        let after = cs.get(k + 1).map(|x| x.1);
        c == 'x' && before.is_some_and(|b| !name(b) || b.is_ascii_digit()) && after.is_some_and(|a| !name(a) || a.is_ascii_digit())
    };
    let k = find(&|_, c| c == ',' || c == '×').or_else(|| find(&lone_x)).or_else(|| find(&|_, c| c == '*'));
    let (w, h) = match k {
        Some(k) => {
            let at = cs[k].0;
            (&text[..at], &text[at + cs[k].1.len_utf8()..])
        }
        None => (text, text),
    };
    match (w.trim(), h.trim()) {
        ("", "") => None,
        ("", x) | (x, "") => Some((x, x)),
        (w, h) => Some((w, h)),
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
                    // the hover already moved `p` onto the inferred direction
                    let mut infer = app.sketch.as_ref().unwrap().infer.filter(|_| matches!(snp, None | Some(Snap::Grid)));
                    // closing onto a point that lines up exactly (as the guides make it) is level / plumb too
                    if let (None, Some(Snap::Point(_))) = (infer, snp) {
                        let d = p - pa;
                        let eps = 1e-9 * d.length().max(1.0);
                        infer = if d.y.abs() < eps { Some(Dir::Horizontal) } else if d.x.abs() < eps { Some(Dir::Vertical) } else { None };
                    }
                    if (p - pa).length() < 1e-9 {
                        return;
                    }
                    let b = point_at(app, p, snp);
                    let sk = app.sk_mut();
                    let l = sk.add_curve(Geom::Line { a, b }, construction);
                    if let Some(d) = infer {
                        sk.constraints.push(d.constraint(l));
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
                _ => make_slot(app, pts[0], pts[1], pts[2], s1, s2, construction),
            }
            app.after_sketch_edit();
        }
        Tool::Ellipse => {
            let sess = app.sketch.as_mut().unwrap();
            sess.clicks.push(p);
            if sess.clicks.len() < 3 {
                app.scene_dirty = true;
                return;
            }
            let pts = std::mem::take(&mut sess.clicks);
            let u = pts[1] - pts[0];
            if u.length() < 1e-9 {
                return;
            }
            let rmin = ((pts[2] - pts[0]).perp_dot(u.normalize())).abs().max(1e-3);
            app.sk_checkpoint();
            let sk = app.sk_mut();
            let c = sk.add_point(pts[0]);
            let a = sk.add_point(pts[1]);
            sk.add_curve(Geom::Ellipse { c, a, rmin }, construction);
            app.after_sketch_edit();
        }
        Tool::EllipticalArc => {
            let sess = app.sketch.as_mut().unwrap();
            sess.clicks.push(p);
            if sess.clicks.len() < 5 {
                app.scene_dirty = true;
                return;
            }
            let pts = std::mem::take(&mut sess.clicks);
            let u = pts[1] - pts[0];
            if u.length() < 1e-9 {
                return;
            }
            let rmin = ((pts[2] - pts[0]).perp_dot(u.normalize())).abs().max(1e-3);
            let param = |q: DVec2| {
                let d = q - pts[0];
                (d.dot(u.perp().normalize()) / rmin).atan2(d.dot(u.normalize()) / u.length())
            };
            let t0 = param(pts[3]);
            let mut t1 = param(pts[4]);
            while t1 <= t0 {
                t1 += std::f64::consts::TAU;
            }
            app.sk_checkpoint();
            let sk = app.sk_mut();
            let c = sk.add_point(pts[0]);
            let a = sk.add_point(pts[1]);
            sk.add_curve(Geom::EllipticalArc { c, a, rmin, t0, t1 }, construction);
            app.after_sketch_edit();
        }
        Tool::Spline => {
            let wpp = app.camera.wpp() as f64;
            let sess = app.sketch.as_mut().unwrap();
            if sess.clicks.len() >= 3 && (p - sess.clicks[0]).length() < 8.0 * wpp {
                finish_spline(app, true);
                return;
            }
            sess.clicks.push(p);
            app.scene_dirty = true;
        }
        Tool::Text => {
            app.sketch.as_mut().unwrap().text_edit = Some((p, String::new()));
        }
        Tool::TangentArc => {
            let sess = app.sketch.as_mut().unwrap();
            match (sess.chain, snp) {
                (None, Some(Snap::Point(pid))) => {
                    if tangent_at(app.sk().0, pid).is_some() {
                        app.sketch.as_mut().unwrap().chain = Some(pid);
                    } else {
                        app.flash("Start a tangent arc at the end of a line or arc", ToastKind::Info);
                    }
                }
                (None, _) => app.flash("Start a tangent arc at the end of a line or arc", ToastKind::Info),
                (Some(start), _) => {
                    app.sk_checkpoint();
                    if let Err(e) = make_tangent_arc(app, start, p, snp, construction) {
                        app.sketch.as_mut().unwrap().undo.pop();
                        app.flash(e, ToastKind::Error);
                    }
                    app.sketch.as_mut().unwrap().chain = None;
                    app.after_sketch_edit();
                }
            }
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
            let sn = if !centered && i == 0 {
                sa
            } else if i == 2 {
                sb
            } else {
                None
            };
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

fn make_slot(app: &mut App, c1: DVec2, c2: DVec2, w: DVec2, s1: Option<Snap>, s2: Option<Snap>, construction: bool) {
    let d = c2 - c1;
    if d.length() < 1e-9 {
        return;
    }
    let n = d.perp().normalize();
    let r = ((w - c1).dot(n)).abs().max(1e-3);
    // the end centres join whatever the clicks snapped to
    let a = point_at(app, c1, s1);
    let b = point_at(app, c2, s2);
    if a == b {
        return;
    }
    let sk = app.sk_mut();
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

fn finish_spline(app: &mut App, closed: bool) {
    let pts = std::mem::take(&mut app.sketch.as_mut().unwrap().clicks);
    if pts.len() < 2 {
        return;
    }
    let construction = app.sketch.as_ref().unwrap().construction;
    app.sk_checkpoint();
    let sk = app.sk_mut();
    let ids: Vec<Id> = pts.iter().map(|p| sk.add_point(*p)).collect();
    sk.add_curve(Geom::Spline { pts: ids, closed }, construction);
    app.after_sketch_edit();
}

/// Unit tangent leaving point `pid` along the line or arc that ends there.
fn tangent_at(sk: &Sketch, pid: Id) -> Option<(DVec2, Id)> {
    for c in &sk.curves {
        match c.geom {
            Geom::Line { a, b } if a == pid || b == pid => {
                let (from, to) = if b == pid { (a, b) } else { (b, a) };
                return Some(((sk.pv(to) - sk.pv(from)).normalize(), c.id));
            }
            Geom::Arc { c: cc, a, b } if a == pid || b == pid => {
                let r = sk.pv(pid) - sk.pv(cc);
                // counter-clockwise arcs travel a -> b
                let t = if b == pid { r.perp() } else { -r.perp() };
                return Some((t.normalize(), c.id));
            }
            _ => {}
        }
    }
    None
}

fn make_tangent_arc(app: &mut App, start: Id, end: DVec2, snp: Option<Snap>, construction: bool) -> Result<(), String> {
    let (t, prev) = tangent_at(app.sk().0, start).ok_or("no curve ends there")?;
    let s = app.sk().0.pv(start);
    let chord = end - s;
    let n = t.perp();
    let denom = 2.0 * chord.dot(n);
    if denom.abs() < 1e-9 {
        return Err("that would be a straight line".into());
    }
    let r = chord.length_squared() / denom;
    let c = s + n * r;
    let eid = point_at(app, end, snp);
    let sk = app.sk_mut();
    let cid = sk.add_point(c);
    // going left of the tangent is counter-clockwise
    let arc = if r > 0.0 { sk.add_curve(Geom::Arc { c: cid, a: start, b: eid }, construction) } else { sk.add_curve(Geom::Arc { c: cid, a: eid, b: start }, construction) };
    sk.constraints.push(Constraint::Tangent { a: prev, b: arc, inner: false });
    Ok(())
}

fn apply_pattern(app: &mut App, circular: bool, count: u32, a: f64, b: f64) {
    let sel: Vec<Sel> = app.sketch.as_ref().unwrap().sel.iter().copied().collect();
    let sk = app.sk().0.clone();
    let units = app.doc.units.scale();
    let center_id = sel.iter().find_map(|s| if let Sel::Point(p) = s { Some(*p) } else { None }).filter(|_| circular);
    let center = center_id.map(|p| sk.pv(p)).unwrap_or(DVec2::ZERO);
    let curves: Vec<Id> = sel.iter().filter_map(|s| if let Sel::Curve(c) = s { Some(*c) } else { None }).collect();
    if curves.is_empty() {
        app.flash("Select the curves to pattern", ToastKind::Info);
        return;
    }
    let mut out = sk.clone();
    for k in 1..count.max(1) {
        let step = if (a - 360.0).abs() < 1e-9 { a / count as f64 } else { a / (count.max(2) - 1) as f64 };
        let deg = step * k as f64;
        let off = DVec2::new(a, b) * units * k as f64;
        let xf = |p: DVec2| -> DVec2 {
            if circular {
                center + DVec2::from_angle(deg.to_radians()).rotate(p - center)
            } else {
                p + off
            }
        };
        let mut map = std::collections::HashMap::new();
        let mut np = |out: &mut Sketch, id: Id| *map.entry(id).or_insert_with(|| out.add_point(xf(sk.pv(id))));
        for id in &curves {
            let Some(c) = sk.curve(*id) else { continue };
            let g = match c.geom {
                Geom::Line { a, b } => Geom::Line { a: np(&mut out, a), b: np(&mut out, b) },
                Geom::Circle { c, r } => Geom::Circle { c: np(&mut out, c), r },
                Geom::Arc { c, a, b } => Geom::Arc { c: np(&mut out, c), a: np(&mut out, a), b: np(&mut out, b) },
                Geom::Ellipse { c, a, rmin } => Geom::Ellipse { c: np(&mut out, c), a: np(&mut out, a), rmin },
                Geom::EllipticalArc { c, a, rmin, t0, t1 } => Geom::EllipticalArc { c: np(&mut out, c), a: np(&mut out, a), rmin, t0, t1 },
                Geom::Spline { ref pts, closed } => Geom::Spline { pts: pts.iter().map(|p| np(&mut out, *p)).collect(), closed },
                Geom::Poly { ref pts, closed } => Geom::Poly { pts: pts.iter().map(|p| xf(DVec2::from(*p)).into()).collect(), closed },
            };
            let linked = !matches!(g, Geom::Poly { .. });
            let copy = out.add_curve(g, c.construction);
            // keep the copy tied to the original, so resizing or moving it updates every copy
            if linked {
                out.constraints.push(Constraint::Pattern { a: *id, b: copy, center: center_id, angle: if circular { deg } else { 0.0 }, dx: if circular { 0.0 } else { off.x }, dy: if circular { 0.0 } else { off.y }, circular });
            }
        }
    }
    app.sk_checkpoint();
    *app.sk_mut() = out;
    app.after_sketch_edit();
}

fn create_text(app: &mut App, at: DVec2, text: &str) {
    let h = app.sketch.as_ref().unwrap().text_height * app.doc.units.scale();
    let editing = app.sketch.as_mut().unwrap().text_editing.take();
    app.sk_checkpoint();
    let sk = app.sk_mut();
    match editing {
        Some(i) if i < sk.texts.len() => {
            if text.trim().is_empty() {
                // empty text deletes it
                let anchor = sk.texts[i].anchor;
                sk.delete(&[anchor].into(), &HashSet::new());
            } else {
                sk.texts[i].text = text.to_string();
                sk.texts[i].height = h;
            }
        }
        _ => {
            if text.trim().is_empty() {
                return;
            }
            let anchor = sk.add_point(at);
            sk.texts.push(crate::sketch::TextItem { anchor, text: text.to_string(), height: h, curves: vec![] });
        }
    }
    sk.refresh_texts();
    app.after_sketch_edit();
}

// ---------------------------------------------------------------- editing tools

fn trim(app: &mut App, cid: Id, at: DVec2) {
    let (sk, _) = app.sk();
    let Some(c) = sk.curve(cid).cloned() else { return };
    let splits = regions::split_params(sk, cid);
    let Some(u) = regions::param_at(sk, cid, at) else { return };
    let is_circle = matches!(c.geom, Geom::Circle { .. });
    let (u0, u1) = trim_range(&splits, u, is_circle);
    let at_start = u0 <= 1e-9;
    let at_end = u1 >= 1.0 - 1e-9;
    let p0 = regions::point_at(sk, cid, u0);
    let p1 = regions::point_at(sk, cid, u1);
    let sk = app.sk_mut();
    if (at_start && at_end && !is_circle) || (is_circle && splits.len() <= 3) {
        sk.delete(&[cid].into(), &HashSet::new());
        return;
    }
    if !matches!(c.geom, Geom::Line { .. } | Geom::Circle { .. } | Geom::Arc { .. }) {
        trim_freeform(sk, &c, &splits, u0, u1);
        return;
    }
    let mut old_points = Sketch::curve_points(&c.geom);
    // a trimmed line is a different length: its length and midpoint constraints no longer apply
    if matches!(c.geom, Geom::Line { .. }) {
        sk.constraints.retain(|k| !matches!(k, Constraint::Length { l, .. } | Constraint::Midpoint { l, .. } if *l == cid));
    }
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
        _ => {}
    }
    // drop endpoints nothing uses any more
    let used: HashSet<Id> = sk.curves.iter().flat_map(|c| Sketch::curve_points(&c.geom)).chain(sk.constraints.iter().flat_map(|k| k.refs())).collect();
    old_points.retain(|p| !used.contains(p));
    sk.points.retain(|p| !old_points.contains(&p.id));
    merge_close_points(app);
}

/// The parameter range of the piece around `u` between neighbouring cuts in `splits` (sorted, 0 and 1
/// included). On a circle the seam at 0/1 isn't a cut: the piece across it runs from the last cut round
/// to the first (u0 > u1 then).
fn trim_range(splits: &[f64], u: f64, closed: bool) -> (f64, f64) {
    let k = splits.iter().position(|s| *s > u).unwrap_or(splits.len() - 1).max(1);
    let (u0, u1) = (splits[k - 1], splits[k]);
    if closed && splits.len() > 3 && (u0 <= 1e-9 || u1 >= 1.0 - 1e-9) {
        return (splits[splits.len() - 2], splits[1]);
    }
    (u0, u1)
}

/// Trim a piece out of an ellipse, elliptical arc, spline or outline. Ellipses stay
/// elliptical arcs; splines and outlines become fixed outlines.
fn trim_freeform(sk: &mut Sketch, c: &Curve, splits: &[f64], u0: f64, u1: f64) {
    let closed = matches!(c.geom, Geom::Ellipse { .. } | Geom::Spline { closed: true, .. } | Geom::Poly { closed: true, .. });
    let inner: Vec<f64> = splits.iter().copied().filter(|s| *s > 1e-9 && *s < 1.0 - 1e-9).collect();
    // pieces to keep, as parameter ranges (may run past 1 for closed curves)
    let keep: Vec<(f64, f64)> = if closed {
        if inner.is_empty() {
            vec![]
        } else if u0 <= 1e-9 || u1 >= 1.0 - 1e-9 {
            vec![(inner[0], *inner.last().unwrap())]
        } else {
            vec![(u1, u0 + 1.0)]
        }
    } else {
        let mut v = vec![];
        if u0 > 1e-9 {
            v.push((0.0, u0));
        }
        if u1 < 1.0 - 1e-9 {
            v.push((u1, 1.0));
        }
        v
    };
    let id = c.id;
    let pts = sk.tessellate(c, 0.5);
    sk.curves.retain(|x| x.id != id);
    for (a, b) in keep {
        if b - a < 1e-9 {
            continue;
        }
        let g = match c.geom {
            Geom::Ellipse { c: cc, a: aa, rmin } => Geom::EllipticalArc { c: cc, a: aa, rmin, t0: a * std::f64::consts::TAU, t1: b * std::f64::consts::TAU },
            Geom::EllipticalArc { c: cc, a: aa, rmin, t0, t1 } => Geom::EllipticalArc { c: cc, a: aa, rmin, t0: t0 + (t1 - t0) * a, t1: t0 + (t1 - t0) * b },
            _ => {
                // sample the polyline between the parameters (wrapping for closed curves)
                let n = pts.len() - 1;
                let at = |u: f64| -> DVec2 {
                    let u = u.rem_euclid(1.0 + 1e-12).min(1.0);
                    let f = u * n as f64;
                    let i = (f.floor() as usize).min(n - 1);
                    pts[i].lerp(pts[i + 1], f - i as f64)
                };
                let steps = (((b - a) * n as f64).ceil() as usize).max(2);
                let out: Vec<[f64; 2]> = (0..=steps).map(|k| at(a + (b - a) * k as f64 / steps as f64).into()).collect();
                Geom::Poly { pts: out, closed: false }
            }
        };
        sk.add_curve(g, c.construction);
    }
    // text pieces are no longer editable text
    if let Some(i) = sk.text_of(id) {
        sk.texts.remove(i);
    }
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
    // the corner point goes away: nothing else may be built on it (an arc's centre or end, a circle's centre)
    if sk.curves.iter().any(|c| !matches!(c.geom, Geom::Line { .. }) && Sketch::curve_points(&c.geom).contains(&pid)) || sk.texts.iter().any(|t| t.anchor == pid) {
        return Err("Other curves use that corner point, so it can't be rounded off".into());
    }
    let p = sk.pv(pid);
    let (l1, o1) = lines[0];
    let (l2, o2) = lines[1];
    let u = (sk.pv(o1) - p).normalize();
    let v = (sk.pv(o2) - p).normalize();
    let half = u.angle_to(v).abs() / 2.0;
    // folded back onto each other, or running straight on: no corner to round
    if half < 1e-3 || (std::f64::consts::FRAC_PI_2 - half).abs() < 1e-3 {
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
    // shortened lines: their length and midpoint constraints no longer apply (as in trim)
    sk.constraints.retain(|k| !k.refs().contains(&pid) && !matches!(k, Constraint::Length { l, .. } | Constraint::Midpoint { l, .. } if *l == l1 || *l == l2));
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
    let label = expr::trim(d / app.doc.units.scale(), 4);
    match offset_curves(app.sk().0, &ids, side, d, &label) {
        Ok(out) => *app.sk_mut() = out,
        Err(e) => app.flash(e, ToastKind::Error),
    }
}

/// One piece of an offset chain, in travel order.
#[derive(Clone, Copy, Debug)]
enum OffEl {
    Line { s: DVec2, e: DVec2 },
    /// Around the centre point `c`; `ccw` is the travel direction.
    Arc { c: Id, ctr: DVec2, s: DVec2, e: DVec2, ccw: bool },
}

impl OffEl {
    fn ends(&self) -> (DVec2, DVec2) {
        match *self {
            OffEl::Line { s, e } | OffEl::Arc { s, e, .. } => (s, e),
        }
    }

    /// Unit direction of travel at `q` (a point on it).
    fn tangent(&self, q: DVec2) -> DVec2 {
        match *self {
            OffEl::Line { s, e } => (e - s).normalize_or_zero(),
            OffEl::Arc { ctr, ccw, .. } => {
                let t = (q - ctr).normalize_or_zero().perp();
                if ccw { t } else { -t }
            }
        }
    }

    /// Moved `o` to the left of the direction of travel.
    fn offset(&self, o: f64) -> Result<OffEl, String> {
        Ok(match *self {
            OffEl::Line { s, e } => {
                let n = (e - s).perp().normalize_or_zero() * o;
                OffEl::Line { s: s + n, e: e + n }
            }
            OffEl::Arc { c, ctr, s, e, ccw } => {
                // left of a counter-clockwise arc is its centre
                let r = (s - ctr).length() + if ccw { -o } else { o };
                if r <= 1e-9 {
                    return Err("The offset is larger than an arc's radius".into());
                }
                OffEl::Arc { c, ctr, s: ctr + (s - ctr).normalize() * r, e: ctr + (e - ctr).normalize() * r, ccw }
            }
        })
    }

    /// Where the full lines / circles of two pieces cross.
    fn crossings(&self, other: &OffEl) -> Vec<DVec2> {
        let circle = |el: &OffEl| match *el {
            OffEl::Arc { ctr, s, .. } => Some((ctr, (s - ctr).length())),
            _ => None,
        };
        match (self, other) {
            (OffEl::Line { s: a, e: b }, OffEl::Line { s: c, e: d }) => line_intersection(*a, *b, *c, *d).into_iter().collect(),
            (OffEl::Line { s, e }, arc) | (arc, OffEl::Line { s, e }) => {
                let (c, r) = circle(arc).unwrap();
                let d = (*e - *s).normalize_or_zero();
                let foot = *s + d * (c - *s).dot(d);
                let h2 = r * r - (foot - c).length_squared();
                if h2 < 0.0 {
                    vec![]
                } else {
                    vec![foot - d * h2.sqrt(), foot + d * h2.sqrt()]
                }
            }
            (a, b) => {
                let ((c1, r1), (c2, r2)) = (circle(a).unwrap(), circle(b).unwrap());
                let dd = (c2 - c1).length();
                if dd < 1e-12 || dd > r1 + r2 || dd < (r1 - r2).abs() {
                    return vec![];
                }
                let x = (dd * dd + r1 * r1 - r2 * r2) / (2.0 * dd);
                let h = (r1 * r1 - x * x).max(0.0).sqrt();
                let u = (c2 - c1) / dd;
                vec![c1 + u * x + u.perp() * h, c1 + u * x - u.perp() * h]
            }
        }
    }
}

/// Selected lines and arcs that share end points, as chains: (curve, travelled backwards) and closed?
fn offset_chains(sk: &Sketch, ids: &[Id]) -> Vec<(Vec<(Id, bool)>, bool)> {
    let ends = |id: Id| match sk.curve(id).map(|c| &c.geom) {
        Some(Geom::Line { a, b }) | Some(Geom::Arc { a, b, .. }) => Some((*a, *b)),
        _ => None,
    };
    let mut ids: Vec<Id> = ids.iter().copied().filter(|c| ends(*c).is_some()).collect();
    ids.sort();
    let mut degree: std::collections::HashMap<Id, usize> = std::collections::HashMap::new();
    for c in &ids {
        let (a, b) = ends(*c).unwrap();
        *degree.entry(a).or_default() += 1;
        *degree.entry(b).or_default() += 1;
    }
    let joins = |p: Id| degree.get(&p) == Some(&2);
    let mut used: HashSet<Id> = HashSet::new();
    let mut out = vec![];
    // open chains start at a free end; what is left over are closed loops
    let starts: Vec<(Id, bool)> = ids
        .iter()
        .filter_map(|c| {
            let (a, b) = ends(*c).unwrap();
            if !joins(a) {
                Some((*c, false))
            } else if !joins(b) {
                Some((*c, true))
            } else {
                None
            }
        })
        .chain(ids.iter().map(|c| (*c, false)))
        .collect();
    for (first, rev) in starts {
        if used.contains(&first) {
            continue;
        }
        used.insert(first);
        let mut chain = vec![(first, rev)];
        let (a, b) = ends(first).unwrap();
        let start = if rev { b } else { a };
        let mut at = if rev { a } else { b };
        while joins(at) {
            let Some(&next) = ids.iter().find(|c| !used.contains(*c) && {
                let (p, q) = ends(**c).unwrap();
                p == at || q == at
            }) else {
                break;
            };
            used.insert(next);
            let (p, q) = ends(next).unwrap();
            let rev = q == at;
            chain.push((next, rev));
            at = if rev { p } else { q };
        }
        let closed = chain.len() > 1 && at == start;
        out.push((chain, closed));
    }
    out
}

/// The sketch with the curves `ids` offset by `d` towards `side`. Connected lines and arcs are offset as one
/// chain: corners that open up get an arc around the original corner, corners that overlap are trimmed to
/// where the pieces cross, and the chain carries one distance dimension (labelled `label`).
fn offset_curves(sk: &Sketch, ids: &[Id], side: DVec2, d: f64, label: &str) -> Result<Sketch, String> {
    let mut out = sk.clone();
    let chains = offset_chains(sk, ids);
    let chained: HashSet<Id> = chains.iter().flat_map(|c| c.0.iter().map(|x| x.0)).collect();
    for (chain, closed) in &chains {
        let construction = |id: Id| sk.curve(id).map(|c| c.construction).unwrap_or(false);
        // each piece in travel order, with the point it ends on
        let mut els: Vec<OffEl> = vec![];
        let mut joints: Vec<Id> = vec![];
        for &(id, rev) in chain {
            let (el, end) = match sk.curve(id).map(|c| c.geom.clone()) {
                Some(Geom::Line { a, b }) => {
                    let (s, e) = if rev { (b, a) } else { (a, b) };
                    (OffEl::Line { s: sk.pv(s), e: sk.pv(e) }, e)
                }
                Some(Geom::Arc { c, a, b }) => {
                    let (s, e) = if rev { (b, a) } else { (a, b) };
                    (OffEl::Arc { c, ctr: sk.pv(c), s: sk.pv(s), e: sk.pv(e), ccw: !rev }, e)
                }
                _ => unreachable!(),
            };
            els.push(el);
            joints.push(end);
        }
        // the clicked side, from the piece nearest the click: left of travel is +
        let near = (0..chain.len()).min_by(|x, y| {
            let dist = |i: usize| sk.curve(chain[i].0).map(|c| sk.curve_distance(c, side)).unwrap_or(f64::INFINITY);
            dist(*x).total_cmp(&dist(*y))
        });
        let Some(near) = near else { continue };
        let o = match els[near] {
            OffEl::Line { s, e } => {
                if (e - s).perp_dot(side - s) >= 0.0 {
                    d
                } else {
                    -d
                }
            }
            OffEl::Arc { ctr, s, ccw, .. } => {
                let inside = (side - ctr).length() < (s - ctr).length();
                if inside == ccw { d } else { -d }
            }
        };
        let mut new: Vec<OffEl> = els.iter().map(|e| e.offset(o)).collect::<Result<_, _>>()?;
        let n = els.len();
        let corners = if *closed { n } else { n - 1 };
        // per corner: the shared point where the pieces were trimmed, or None for a corner arc
        let mut shared: Vec<Option<DVec2>> = vec![];
        for k in 0..corners {
            let j = (k + 1) % n;
            let (e1, s2) = (new[k].ends().1, new[j].ends().0);
            if (e1 - s2).length() < 1e-9 {
                shared.push(Some(e1));
                continue;
            }
            let v = sk.pv(joints[k]);
            let turn = els[k].tangent(v).perp_dot(els[j].tangent(v));
            let mid = (e1 + s2) / 2.0;
            let cross = if turn * o > 0.0 { new[k].crossings(&new[j]).into_iter().min_by(|a, b| (*a - mid).length().total_cmp(&(*b - mid).length())) } else { None };
            shared.push(cross);
            if let Some(x) = cross {
                for (i, end) in [(k, true), (j, false)] {
                    new[i] = match new[i] {
                        OffEl::Line { s, e } => if end { OffEl::Line { s, e: x } } else { OffEl::Line { s: x, e } },
                        OffEl::Arc { c, ctr, s, e, ccw } => if end { OffEl::Arc { c, ctr, s, e: x, ccw } } else { OffEl::Arc { c, ctr, s: x, e, ccw } },
                    };
                }
            }
        }
        // points: shared at trimmed corners
        let mut start_id: Vec<Option<Id>> = vec![None; n];
        let mut end_id: Vec<Option<Id>> = vec![None; n];
        for k in 0..corners {
            if let Some(x) = shared[k] {
                let id = out.add_point(x);
                end_id[k] = Some(id);
                start_id[(k + 1) % n] = Some(id);
            }
        }
        let mut curve_ids = vec![];
        for i in 0..n {
            let (s, e) = new[i].ends();
            let a = *start_id[i].get_or_insert_with(|| out.add_point(s));
            let b = *end_id[i].get_or_insert_with(|| out.add_point(e));
            let (orig, _) = chain[i];
            let id = match new[i] {
                OffEl::Line { .. } => {
                    let l = out.add_curve(Geom::Line { a, b }, construction(orig));
                    out.constraints.push(Constraint::Parallel { a: orig, b: l });
                    l
                }
                OffEl::Arc { c, ccw, .. } => out.add_curve(if ccw { Geom::Arc { c, a, b } } else { Geom::Arc { c, a: b, b: a } }, construction(orig)),
            };
            curve_ids.push(id);
        }
        // corner arcs around the original corners, tangent to the lines they join
        let is_line = |i: usize| matches!(new[i], OffEl::Line { .. });
        // joints that were smooth (an arc and a line or arc meeting tangentially) stay smooth
        for k in 0..corners {
            let j = (k + 1) % n;
            if shared[k].is_some() && !(is_line(k) && is_line(j)) {
                let v = sk.pv(joints[k]);
                let (t1, t2) = (els[k].tangent(v), els[j].tangent(v));
                if t1.perp_dot(t2).abs() < 1e-6 && t1.dot(t2) > 0.0 {
                    out.constraints.push(Constraint::Tangent { a: curve_ids[k], b: curve_ids[j], inner: false });
                }
            }
        }
        let mut linked: Vec<usize> = (0..n).collect();
        fn root(l: &mut [usize], i: usize) -> usize {
            if l[i] != i {
                l[i] = root(l, l[i]);
            }
            l[i]
        }
        for k in 0..corners {
            if shared[k].is_some() {
                continue;
            }
            let j = (k + 1) % n;
            let (ea, sb) = (end_id[k].unwrap(), start_id[j].unwrap());
            let v = joints[k];
            let ccw = (out.pv(ea) - out.pv(v)).perp_dot(out.pv(sb) - out.pv(v)) > 0.0;
            let arc = out.add_curve(if ccw { Geom::Arc { c: v, a: ea, b: sb } } else { Geom::Arc { c: v, a: sb, b: ea } }, construction(chain[k].0));
            for i in [k, j] {
                if is_line(i) {
                    out.constraints.push(Constraint::Tangent { a: curve_ids[i], b: arc, inner: false });
                }
            }
            if is_line(k) && is_line(j) {
                let (rk, rj) = (root(&mut linked, k), root(&mut linked, j));
                linked[rk] = rj;
            }
        }
        // one distance dimension; lines not tied to it through corner arcs keep the same distance
        if let Some(first) = (0..n).find(|i| is_line(*i)) {
            let p0 = start_id[first].unwrap();
            out.constraints.push(Constraint::PointLineDistance { p: p0, l: chain[first].0, dim: Dim::new(label, [0.0; 2]) });
            let r0 = root(&mut linked, first);
            for i in (first + 1..n).filter(|i| is_line(*i)) {
                if root(&mut linked, i) != r0 {
                    out.constraints.push(Constraint::EqualDistance { p: start_id[i].unwrap(), l: chain[i].0, q: p0, m: chain[first].0 });
                    let ri = root(&mut linked, i);
                    linked[ri] = r0;
                }
            }
        }
    }
    for &id in ids.iter().filter(|id| !chained.contains(id)) {
        let Some(c) = sk.curve(id) else { continue };
        match c.geom {
            Geom::Circle { c: cc, r } => {
                let s = if (side - sk.pv(cc)).length() > r { 1.0 } else { -1.0 };
                out.add_curve(Geom::Circle { c: cc, r: (r + d * s).max(1e-3) }, c.construction);
            }
            _ => {
                // free-form: offset the polyline along its normals, to the clicked side
                let closed = matches!(c.geom, Geom::Ellipse { .. } | Geom::Spline { closed: true, .. } | Geom::Poly { closed: true, .. });
                let mut pts = sk.tessellate(c, 0.5);
                if closed && pts.len() > 2 {
                    pts.pop();
                }
                let n = pts.len();
                if n < 2 {
                    continue;
                }
                let normal = |i: usize| {
                    let (a, b) = if closed { (pts[(i + n - 1) % n], pts[(i + 1) % n]) } else { (pts[i.saturating_sub(1)], pts[(i + 1).min(n - 1)]) };
                    (b - a).perp().normalize_or_zero()
                };
                let j = (0..n).min_by(|&x, &y| (pts[x] - side).length().total_cmp(&(pts[y] - side).length())).unwrap_or(0);
                let sgn = if (side - pts[j]).dot(normal(j)) >= 0.0 { 1.0 } else { -1.0 };
                let off: Vec<[f64; 2]> = (0..n).map(|i| (pts[i] + normal(i) * d * sgn).into()).collect();
                out.add_curve(Geom::Poly { pts: off, closed }, c.construction);
            }
        }
    }
    Ok(out)
}

fn mirror_selection(app: &mut App) {
    let sess = app.sketch.as_ref().unwrap();
    let (sk, _) = app.sk();
    let ordered = sess.ordered_sel();
    let lines: Vec<Id> =
        ordered.iter().filter_map(|s| if let Sel::Curve(c) = s { Some(*c) } else { None }).filter(|c| matches!(sk.curve(*c).map(|x| &x.geom), Some(Geom::Line { .. }))).collect();
    let construction: Vec<Id> = lines.iter().copied().filter(|c| sk.curve(*c).map(|x| x.construction).unwrap_or(false)).collect();
    let clicked = sess.order.iter().find_map(|s| match s {
        Sel::Curve(c) if lines.contains(c) => Some(*c),
        _ => None,
    });
    // the mirror line: the only line, else the only construction line, else the first line clicked
    let axis = match (lines.as_slice(), construction.as_slice(), clicked) {
        ([], ..) => None,
        ([l], ..) => Some(*l),
        (_, [c], _) => Some(*c),
        (_, _, Some(c)) => Some(c),
        _ => {
            app.flash("Several lines are selected: click the mirror line first, then the curves to mirror", ToastKind::Info);
            return;
        }
    };
    let Some(axis) = axis else {
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
    let sel: Vec<Sel> = ordered;
    let mut out = sk.clone();
    let mut equal: Vec<(Id, Id)> = vec![];
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
                Geom::Ellipse { c, a, rmin } => Geom::Ellipse { c: new_point(&mut out, c), a: new_point(&mut out, a), rmin },
                // a reflection runs the parameter backwards
                Geom::EllipticalArc { c, a, rmin, t0, t1 } => Geom::EllipticalArc { c: new_point(&mut out, c), a: new_point(&mut out, a), rmin, t0: -t1, t1: -t0 },
                Geom::Spline { ref pts, closed } => Geom::Spline { pts: pts.iter().map(|p| new_point(&mut out, *p)).collect(), closed },
                Geom::Poly { ref pts, closed } => Geom::Poly { pts: pts.iter().rev().map(|p| refl(DVec2::from(*p)).into()).collect(), closed },
            };
            let round = matches!(c.geom, Geom::Circle { .. } | Geom::Arc { .. });
            let nid = out.add_curve(g, c.construction);
            if round {
                equal.push((id, nid));
            }
        }
    }
    // keep the copy linked: each point mirrors its original across the axis, round curves keep the same size
    for (&orig, &copy) in &map {
        if orig != copy {
            out.constraints.push(Constraint::Symmetric { a: orig, b: copy, l: axis });
        }
    }
    for (a, b) in equal {
        out.constraints.push(Constraint::Equal { a, b });
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
    let ids: HashSet<Id> = sel
        .iter()
        .filter_map(|s| match s {
            Sel::Point(p) => Some(*p),
            Sel::Curve(c) => Some(*c),
            _ => None,
        })
        .collect();
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
    let mut best: Option<(f32, Vec<[f32; 3]>, bool, Option<([f64; 3], [f64; 3], f64)>)> = None;
    for b in &app.result.bodies {
        for e in &b.edges {
            let pts: Vec<glam::Vec2> = e.pts.iter().map(|p| cam.project(Vec3::from(*p))).collect();
            for w in pts.windows(2) {
                let d = w[1] - w[0];
                let t = if d.length_squared() < 1e-9 { 0.0 } else { ((local - w[0]).dot(d) / d.length_squared()).clamp(0.0, 1.0) };
                let dist = (w[0] + d * t - local).length();
                if dist < 7.0 && best.as_ref().map(|b| dist < b.0).unwrap_or(true) {
                    best = Some((dist, e.pts.clone(), e.linear, e.circle));
                }
            }
        }
    }
    let Some((_, pts, linear, circle)) = best else {
        app.flash("Click a model edge to project it", ToastKind::Info);
        return;
    };
    app.sk_checkpoint();
    let local: Vec<DVec2> = pts.iter().map(|p| fr.to_local(glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64))).collect();
    let closed = local.len() > 2 && (local[0] - *local.last().unwrap()).length() < 1e-5;
    let n = fr.normal();
    let sk = app.sk_mut();
    let fix = |sk: &mut Sketch, p: DVec2| {
        let id = sk.add_point(p);
        sk.constraints.push(Constraint::Fix { p: id, x: p.x, y: p.y });
        id
    };
    // Circles and arcs project exactly from their true centre, axis and radius (the
    // display polyline is only an approximation, and stops short of the real edge).
    let done = 'exact: {
        if let Some((c3, ax, r)) = circle {
            let (c3, ax) = (glam::DVec3::from(c3), glam::DVec3::from(ax).normalize());
            let c = fr.to_local(c3);
            let tilt = ax.dot(n).abs();
            if tilt < 1e-6 {
                // seen edge-on: a straight line, along the circle's in-plane direction
                let d3 = ax.cross(n).normalize();
                let d = (fr.to_local(c3 + d3) - c).normalize();
                let (a, b) = if closed {
                    (c - d * r, c + d * r)
                } else {
                    let ts: Vec<f64> = local.iter().map(|p| (*p - c).dot(d)).collect();
                    let lo = ts.iter().cloned().fold(f64::INFINITY, f64::min).max(-r);
                    let hi = ts.iter().cloned().fold(f64::NEG_INFINITY, f64::max).min(r);
                    // snap to the exact extremes when the arc reaches them
                    let snap = |t: f64| if (t.abs() - r).abs() < 1e-2 * r.max(1.0) { r * t.signum() } else { t };
                    (c + d * snap(lo), c + d * snap(hi))
                };
                let a = fix(sk, a);
                let b = fix(sk, b);
                sk.add_curve(Geom::Line { a, b }, false);
                break 'exact true;
            }
            if closed && tilt < 1.0 - 1e-9 {
                // tilted circle: an ellipse with the full radius across the tilt
                let m3 = ax.cross(n).normalize();
                let m = (fr.to_local(c3 + m3) - c).normalize();
                let cid = fix(sk, c);
                let aid = fix(sk, c + m * r);
                sk.add_curve(Geom::Ellipse { c: cid, a: aid, rmin: r * tilt }, false);
                break 'exact true;
            }
            if closed {
                let cid = fix(sk, c);
                let ci = sk.add_curve(Geom::Circle { c: cid, r }, false);
                sk.constraints.push(Constraint::Radius { c: ci, dim: Dim::new(format!("{r} mm"), [r * 0.7, r * 0.7]) });
                break 'exact true;
            }
            if tilt > 1.0 - 1e-9 {
                let cid = fix(sk, c);
                let (p0, p1) = (c + (local[0] - c).normalize() * r, c + (*local.last().unwrap() - c).normalize() * r);
                let a = fix(sk, p0);
                let b = fix(sk, p1);
                let mid = local[local.len() / 2];
                let (t0, t1) = arc_angles(c, p0, p1);
                let mut tm = (mid - c).to_angle();
                while tm < t0 {
                    tm += std::f64::consts::TAU;
                }
                sk.add_curve(if tm <= t1 { Geom::Arc { c: cid, a, b } } else { Geom::Arc { c: cid, a: b, b: a } }, false);
                break 'exact true;
            }
        }
        // any other edge that lands on a straight line becomes one line between its ends
        let collinear = local.len() > 2 && {
            let (a, b) = (local[0], local.iter().cloned().max_by(|p, q| (*p - local[0]).length().total_cmp(&(*q - local[0]).length())).unwrap());
            let d = (b - a).normalize_or_zero();
            d != DVec2::ZERO && local.iter().all(|p| (*p - a).perp_dot(d).abs() < 1e-4)
        };
        if collinear {
            let d = (local.iter().cloned().max_by(|p, q| (*p - local[0]).length().total_cmp(&(*q - local[0]).length())).unwrap() - local[0]).normalize();
            let ts: Vec<f64> = local.iter().map(|p| (*p - local[0]).dot(d)).collect();
            let lo = ts.iter().cloned().fold(f64::INFINITY, f64::min);
            let hi = ts.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let a = fix(sk, local[0] + d * lo);
            let b = fix(sk, local[0] + d * hi);
            sk.add_curve(Geom::Line { a, b }, false);
            break 'exact true;
        }
        false
    };
    if done {
    } else if linear || local.len() == 2 {
        let a = fix(sk, local[0]);
        let b = fix(sk, *local.last().unwrap());
        sk.add_curve(Geom::Line { a, b }, false);
    } else if let Some(c) = circumcenter(local[0], local[local.len() / 3], local[2 * local.len() / 3]) {
        let r = (local[0] - c).length();
        let closed = (local[0] - *local.last().unwrap()).length() < 1e-6;
        let cid = fix(sk, c);
        if closed {
            let ci = sk.add_curve(Geom::Circle { c: cid, r }, false);
            sk.constraints.push(Constraint::Radius { c: ci, dim: Dim::new(format!("{r} mm"), [r * 0.7, r * 0.7]) });
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
    let sel: Vec<Sel> = app.sketch.as_ref().unwrap().ordered_sel();
    let (sk, _) = app.sk();
    let pts: Vec<Id> = sel.iter().filter_map(|s| if let Sel::Point(p) = s { Some(*p) } else { None }).collect();
    let curves: Vec<&Curve> = sel.iter().filter_map(|s| if let Sel::Curve(c) = s { sk.curve(*c) } else { None }).collect();
    let lines: Vec<Id> = curves.iter().filter(|c| matches!(c.geom, Geom::Line { .. })).map(|c| c.id).collect();
    // only circles and arcs: the solver has no tangency, equality or centre for ellipses, splines or outlines
    let circles: Vec<Id> = curves.iter().filter(|c| matches!(c.geom, Geom::Circle { .. } | Geom::Arc { .. })).map(|c| c.id).collect();
    let others = curves.iter().filter(|c| !matches!(c.geom, Geom::Line { .. } | Geom::Circle { .. } | Geom::Arc { .. })).count();
    let n = pts.len() + curves.len();
    let (label, help) = CONSTRAINTS.iter().find(|c| c.0 == kind).map(|c| (c.1, c.3)).unwrap_or(("", ""));
    let err = format!("{label} needs {help}.");
    let mut add: Vec<Constraint> = vec![];
    let mut merges: Vec<(Id, Id)> = vec![];
    match kind {
        "coincident" => {
            if pts.len() == 2 && n == 2 {
                return coincide_points(app, pts[0], pts[1]);
            } else if pts.len() == 1 && curves.len() == 1 {
                let c = curves[0];
                let k = match c.geom {
                    Geom::Line { .. } => Constraint::PointOnLine { p: pts[0], l: c.id },
                    ref g if holds_points(g) => Constraint::PointOnCircle { p: pts[0], c: c.id },
                    _ => return app.flash("A point can't be held on a spline or outline", ToastKind::Error),
                };
                return coincide_on_curve(app, pts[0], c.id, k);
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
            if curves.len() == 2 && n == 2 && !circles.is_empty() && others == 0 {
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
            // circles, arcs and ellipses have a centre point (outlines and splines have none)
            let centred: Vec<Id> = curves.iter().filter(|c| holds_points(&c.geom)).map(|c| c.id).collect();
            if centred.len() == 2 && n == 2 {
                let c = |id: Id| Sketch::curve_points(&sk.curve(id).unwrap().geom)[0];
                merges.push((c(centred[1]), c(centred[0])));
            } else {
                return app.flash(err, ToastKind::Error);
            }
        }
        "symmetric" => {
            if pts.len() == 2 && lines.len() == 1 && n == 3 {
                add.push(Constraint::Symmetric { a: pts[0], b: pts[1], l: lines[0] });
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
                    add.push(Constraint::Radius { c: c.id, dim: Dim::new(format!("{} mm", expr::trim(r, 6)), [r * 0.7, r * 0.7]) });
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
    let first = sk.constraints.len();
    sk.constraints.extend(add);
    app.sketch.as_mut().unwrap().sel.clear();
    app.after_sketch_edit();
    let st = &app.sketch.as_ref().unwrap().status;
    if let Some(e) = st.error.clone() {
        app.flash(e, ToastKind::Error);
    } else if st.unsupported.iter().any(|i| *i >= first) {
        app.flash(format!("{label} can't be applied to that geometry"), ToastKind::Error);
    }
}

/// Points of `group` that the sketch already holds in place.
fn held(app: &App, group: &HashSet<Id>) -> usize {
    let st = &app.sketch.as_ref().unwrap().status;
    group.iter().filter(|p| st.fixed_points.contains(p)).count()
}

/// Make two points one. The side held less (fewer fully defined points; the later pick on a tie)
/// moves to the other, and when it is separate geometry it moves as a whole instead of being stretched.
fn coincide_points(app: &mut App, first: Id, second: Id) {
    let (sk, _) = app.sk();
    let (g1, g2) = (sk.group_of(first), sk.group_of(second));
    let (keep, mover, keep_group, move_group) = if held(app, &g2) <= held(app, &g1) { (first, second, g1, g2) } else { (second, first, g2, g1) };
    let delta = sk.pv(keep) - sk.pv(mover);
    let separate = !keep_group.contains(&mover);
    let translate = separate && held(app, &move_group) == 0;
    app.sk_checkpoint();
    let sk = app.sk_mut();
    if translate {
        for p in &move_group {
            if let Some(q) = sk.point_mut(*p) {
                q.x += delta.x;
                q.y += delta.y;
            }
        }
    }
    sk.merge_point(mover, keep);
    let hold: Vec<Id> = if separate { keep_group.into_iter().collect() } else { vec![keep] };
    finish_constraint(app, &hold);
}

/// Put a point on a curve, moving whichever side is separate and held less as a whole.
fn coincide_on_curve(app: &mut App, p: Id, c: Id, k: Constraint) {
    let (sk, _) = app.sk();
    let (gp, gc) = (sk.group_of(p), sk.group_of(c));
    let pp = sk.pv(p);
    let target = match sk.curve(c).map(|x| &x.geom) {
        Some(Geom::Line { a, b }) => {
            let (pa, d) = (sk.pv(*a), (sk.pv(*b) - sk.pv(*a)).normalize_or_zero());
            pa + d * (pp - pa).dot(d)
        }
        Some(Geom::Circle { c: cc, r }) => sk.pv(*cc) + (pp - sk.pv(*cc)).normalize_or(DVec2::X) * *r,
        Some(Geom::Arc { c: cc, a, .. }) => sk.pv(*cc) + (pp - sk.pv(*cc)).normalize_or(DVec2::X) * (sk.pv(*a) - sk.pv(*cc)).length(),
        _ => regions::param_at(sk, c, pp).map(|u| regions::point_at(sk, c, u)).unwrap_or(pp),
    };
    let separate = gp.is_disjoint(&gc);
    let (hp, hc) = (held(app, &gp), held(app, &gc));
    let (moving, delta, hold) = match separate {
        true if hp <= hc && hp == 0 => (gp, target - pp, gc),
        true if hc == 0 => (gc, pp - target, gp),
        _ => (HashSet::new(), DVec2::ZERO, HashSet::new()),
    };
    app.sk_checkpoint();
    let sk = app.sk_mut();
    for id in &moving {
        if let Some(q) = sk.point_mut(*id) {
            q.x += delta.x;
            q.y += delta.y;
        }
    }
    sk.constraints.push(k);
    finish_constraint(app, &hold.into_iter().collect::<Vec<_>>());
}

fn finish_constraint(app: &mut App, hold: &[Id]) {
    let s = app.sketch.as_mut().unwrap();
    s.sel.clear();
    s.order.clear();
    if !hold.is_empty() {
        app.solve_sketch(hold);
    }
    app.after_sketch_edit();
    if let Some(e) = app.sketch.as_ref().unwrap().status.error.clone() {
        app.flash(e, ToastKind::Error);
    }
}

// ---------------------------------------------------------------- dimensions

/// Dimension tool clicks: pick one or two items, then click where the label goes.
/// Until then the dimension follows the cursor (see `pending_dimension`).
fn dimension_click(app: &mut App, h: Option<Sel>, raw: DVec2) {
    let pending = app.sketch.as_ref().unwrap().dim_pending.clone();
    // clicking an existing dimension (with nothing picked) edits it
    if let (Some(Sel::Constraint(i)), true) = (h, pending.is_empty()) {
        open_dim_edit(app, i);
        return;
    }
    match h.filter(|x| !matches!(x, Sel::Constraint(_))) {
        Some(x) if pending.len() < 2 && !pending.contains(&x) => {
            let mut next = pending;
            next.push(x);
            // a lone point is kept: it measures to the second pick
            if matches!(next.as_slice(), [Sel::Point(_)]) || build_dimension(app, &next, raw).is_some() {
                app.sketch.as_mut().unwrap().dim_pending = next;
            } else if next.len() == 1 {
                app.flash("Pick a line, circle, arc or point to dimension", ToastKind::Info);
            } else {
                app.flash("Those two can't be dimensioned together; click empty space to place the first one's dimension", ToastKind::Info);
            }
        }
        // a lone point has nothing to place yet
        _ if matches!(pending.as_slice(), [Sel::Point(_)]) => app.flash("Now pick a second point, or a line, circle or arc, to dimension the point to", ToastKind::Info),
        // a click in empty space (or on a picked item, or after two picks) places the dimension
        _ if !pending.is_empty() => create_dimension(app, raw),
        _ => {}
    }
    app.scene_dirty = true;
}

/// The dimension the picked items would get with its label at `at`, or None
/// when they can't be dimensioned (more items may still make it valid).
fn build_dimension(app: &App, pending: &[Sel], at: DVec2) -> Option<Constraint> {
    let (sk, _) = app.sk();
    let u = app.doc.units;
    let fmt_len = |v: f64| expr::trim(v / u.scale(), 3);
    let mk = |v: String| Dim::new(v, [0.0, 0.0]);
    let line = |id: Id| match sk.curve(id).map(|c| &c.geom) {
        Some(Geom::Line { a, b }) => Some((*a, *b)),
        _ => None,
    };
    let circular = |id: Id| matches!(sk.curve(id).map(|c| &c.geom), Some(Geom::Circle { .. } | Geom::Arc { .. }));
    let center = |id: Id| Sketch::curve_points(&sk.curve(id).unwrap().geom)[0];
    // horizontal, vertical or aligned distance between two points, from where the label goes
    let linear = |a: Id, b: Id, aligned: Constraint| {
        let d = sk.pv(b) - sk.pv(a);
        match linear_axis(sk.pv(a), sk.pv(b), at) {
            Some(0) => Constraint::AxisDistance { a, b, axis: 0, dim: mk(fmt_len(d.x.abs())) },
            Some(_) => Constraint::AxisDistance { a, b, axis: 1, dim: mk(fmt_len(d.y.abs())) },
            None => aligned,
        }
    };
    let c = match *pending {
        [Sel::Curve(l)] if line(l).is_some() => {
            let (a, b) = line(l).unwrap();
            linear(a, b, Constraint::Length { l, dim: mk(fmt_len((sk.pv(b) - sk.pv(a)).length())) })
        }
        [Sel::Curve(c)] => match sk.curve(c).map(|x| &x.geom) {
            Some(Geom::Circle { r, .. }) => Constraint::Diameter { c, dim: mk(fmt_len(2.0 * r)) },
            Some(Geom::Arc { c: cc, a, .. }) => Constraint::Radius { c, dim: mk(fmt_len((sk.pv(*a) - sk.pv(*cc)).length())) },
            _ => return None,
        },
        // a lone point waits for a second item
        [Sel::Point(_)] => return None,
        [Sel::Point(a), Sel::Point(b)] => linear(a, b, Constraint::Distance { a, b, dim: mk(fmt_len((sk.pv(b) - sk.pv(a)).length())) }),
        [Sel::Point(p), Sel::Curve(l)] | [Sel::Curve(l), Sel::Point(p)] if line(l).is_some() => point_line(sk, p, l, &fmt_len)?,
        [Sel::Curve(l1), Sel::Curve(l2)] if line(l1).is_some() && line(l2).is_some() => {
            let (a, b) = line(l1).unwrap();
            let (c, d) = line(l2).unwrap();
            let u1 = (sk.pv(b) - sk.pv(a)).normalize();
            let u2 = (sk.pv(d) - sk.pv(c)).normalize();
            if u1.perp_dot(u2).abs() < 1e-6 {
                // parallel lines: distance between them
                let dist = (u1.perp_dot(sk.pv(c) - sk.pv(a))).abs();
                Constraint::PointLineDistance { p: c, l: l1, dim: mk(fmt_len(dist)) }
            } else {
                // the label's quadrant picks which of the four angles between the lines is meant
                let x = line_intersection(sk.pv(a), sk.pv(b), sk.pv(c), sk.pv(d))?;
                let (v1, v2) = angle_wedge(u1, u2, at - x);
                let ang = v1.angle_to(v2).to_degrees();
                // stored as a positive angle from the first line to the second
                let (a, b) = if ang >= 0.0 { (l1, l2) } else { (l2, l1) };
                Constraint::Angle { a, b, dim: mk(expr::trim(ang.abs(), 3)) }
            }
        }
        // circle or arc centre to a line
        [Sel::Curve(c), Sel::Curve(l)] | [Sel::Curve(l), Sel::Curve(c)] if circular(c) && line(l).is_some() => point_line(sk, center(c), l, &fmt_len)?,
        [Sel::Point(p), Sel::Curve(c)] | [Sel::Curve(c), Sel::Point(p)] if circular(c) => {
            let cc = center(c);
            linear(p, cc, Constraint::Distance { a: p, b: cc, dim: mk(fmt_len((sk.pv(cc) - sk.pv(p)).length())) })
        }
        [Sel::Curve(c1), Sel::Curve(c2)] if circular(c1) && circular(c2) => {
            let (a, b) = (center(c1), center(c2));
            // concentric: the gap between them, measured from the smaller one out
            if a == b || (sk.pv(b) - sk.pv(a)).length() < 1e-9 {
                let radius = |id: Id| sk.measure(&Constraint::Radius { c: id, dim: mk(String::new()) }).unwrap_or(0.0);
                let (ra, rb) = (radius(c1), radius(c2));
                if (rb - ra).abs() < 1e-9 {
                    return None;
                }
                let (a, b, gap) = if ra < rb { (c1, c2, rb - ra) } else { (c2, c1, ra - rb) };
                Constraint::RadialDistance { a, b, dim: mk(fmt_len(gap)) }
            } else {
                    linear(a, b, Constraint::Distance { a, b, dim: mk(fmt_len((sk.pv(b) - sk.pv(a)).length())) })
            }
        }
        _ => return None,
    };
    let anchor = dim_anchor(sk, &c).unwrap_or(at);
    let mut c = c;
    if let Some(d) = c.dim_mut() {
        let off = at - anchor;
        d.offset = [off.x, off.y];
    }
    Some(c)
}

fn point_line(sk: &Sketch, p: Id, l: Id, fmt_len: &dyn Fn(f64) -> String) -> Option<Constraint> {
    let Some(Geom::Line { a, b }) = sk.curve(l).map(|c| &c.geom) else { return None };
    let (pa, pb) = (sk.pv(*a), sk.pv(*b));
    if p == *a || p == *b {
        return None;
    }
    let dist = ((pb - pa).perp_dot(sk.pv(p) - pa) / (pb - pa).length()).abs();
    Some(Constraint::PointLineDistance { p, l, dim: Dim::new(fmt_len(dist), [0.0; 2]) })
}

/// Which way a dimension between two points measures, from where its label goes:
/// above or below the pair it is horizontal (0), beside it vertical (1), otherwise aligned (None).
fn linear_axis(pa: DVec2, pb: DVec2, at: DVec2) -> Option<usize> {
    let d = pb - pa;
    let tol = d.length() * 1e-6;
    if d.x.abs() <= tol || d.y.abs() <= tol {
        // already horizontal or vertical: aligned is the same thing
        return None;
    }
    let (lo, hi) = (pa.min(pb), pa.max(pb));
    let in_x = at.x > lo.x && at.x < hi.x;
    let in_y = at.y > lo.y && at.y < hi.y;
    match (in_x, in_y) {
        (true, false) => Some(0),
        (false, true) => Some(1),
        _ => None,
    }
}

/// The directions (±u1, ±u2) bounding the angle between two lines that contains `w`.
fn angle_wedge(u1: DVec2, u2: DVec2, w: DVec2) -> (DVec2, DVec2) {
    for (s1, s2) in [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)] {
        let (v1, v2) = (u1 * s1, u2 * s2);
        let k = v1.perp_dot(v2).signum();
        if v1.perp_dot(w) * k >= 0.0 && w.perp_dot(v2) * k >= 0.0 {
            return (v1, v2);
        }
    }
    (u1, u2)
}

fn create_dimension(app: &mut App, at: DVec2) {
    let pending = std::mem::take(&mut app.sketch.as_mut().unwrap().dim_pending);
    let Some(c) = build_dimension(app, &pending, at) else {
        app.flash("Pick a line, circle or arc, or two items to dimension between", ToastKind::Info);
        return;
    };
    // the same dimension again edits the one already there
    let refs = |k: &Constraint| {
        let mut r = k.refs();
        r.sort();
        (std::mem::discriminant(k), r, matches!(k, Constraint::AxisDistance { axis: 1, .. }))
    };
    let existing = app.sk().0.constraints.iter().position(|k| k.dim().is_some() && refs(k) == refs(&c));
    if let Some(i) = existing {
        app.flash("That's already dimensioned; change its value here", ToastKind::Info);
        open_dim_edit(app, i);
        return;
    }
    let (dof, solved) = {
        let st = &app.sketch.as_ref().unwrap().status;
        (st.dof, st.error.is_none())
    };
    app.sk_checkpoint();
    app.sk_mut().constraints.push(c);
    let idx = app.sk().0.constraints.len() - 1;
    app.after_sketch_edit();
    let st = &app.sketch.as_ref().unwrap().status;
    if solved && st.error.is_none() && st.dof >= dof {
        // it adds nothing the other constraints don't already set: keep it as a reference dimension
        if let Some(d) = app.sk_mut().constraints[idx].dim_mut() {
            d.driven = true;
        }
        app.after_sketch_edit();
        app.flash("Other constraints already set this size, so it was added as a reference dimension (in parentheses)", ToastKind::Info);
        return;
    }
    open_dim_edit(app, idx);
}

/// Names the conflicting constraints for a message: "the 40 mm dimension", "the Horizontal constraint".
fn describe_conflicts(app: &App, idx: &[usize]) -> Option<String> {
    let (sk, _) = app.sk();
    let (vars, _) = app.doc.var_values();
    let env = Env { vars: &vars, units: app.doc.units };
    let name = |i: usize| -> Option<String> {
        let c = sk.constraints.get(i)?;
        Some(match dim_text(sk, &env, c) {
            Some(t) if c.is_angle() => format!("the {t} dimension"),
            Some(t) => format!("the {t} {} dimension", app.doc.units.label()),
            None => format!("the {} constraint", CONSTRAINTS.iter().find(|k| k.0 == c.icon()).map(|k| k.1).unwrap_or("other")),
        })
    };
    // dimensions first: they are what one changes or deletes
    let mut order: Vec<usize> = idx.iter().copied().filter(|i| sk.constraints.get(*i).map(|c| c.dim().is_some()).unwrap_or(false)).collect();
    let rest: Vec<usize> = idx.iter().copied().filter(|i| !order.contains(i)).collect();
    order.extend(rest);
    let names: Vec<String> = order.iter().take(2).filter_map(|i| name(*i)).collect();
    match names.len() {
        0 => None,
        1 => Some(names[0].clone()),
        _ => Some(format!("{} or {}", names[0], names[1])),
    }
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
        // halfway between the point and its foot on the line, so the label moves with both
        PointLineDistance { p, l, .. } => {
            let pp = sk.pv(*p);
            match sk.curve(*l).map(|c| &c.geom) {
                Some(Geom::Line { a, b }) => {
                    let (pa, d) = (sk.pv(*a), (sk.pv(*b) - sk.pv(*a)).normalize_or_zero());
                    Some((pp + pa + d * (pp - pa).dot(d)) / 2.0)
                }
                _ => Some(pp),
            }
        }
        Radius { c, .. } | Diameter { c, .. } | RadialDistance { a: c, .. } => center(*c),
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
            let col = if major { vp::grid_major() } else { vp::grid_minor() };
            let (a, b) = if axis == 0 {
                (DVec2::new(v, (cy - n) as f64 * g), DVec2::new(v, (cy + n) as f64 * g))
            } else {
                (DVec2::new((cx - n) as f64 * g, v), DVec2::new((cx + n) as f64 * g, v))
            };
            s.line(w(a), w(b), 1.0, rgba(col, 1.0));
        }
    }
    // sketch axes through the origin
    s.polyline(&[w(DVec2::new(-half * 4.0, 0.0)), w(DVec2::new(half * 4.0, 0.0))], 1.0, rgba(vp::axis_x(), 0.45), false);
    s.polyline(&[w(DVec2::new(0.0, -half * 4.0)), w(DVec2::new(0.0, half * 4.0))], 1.0, rgba(vp::axis_y(), 0.45), false);

    for r in &sess.regions {
        for t in &r.tris {
            s.tri(true, t.map(|p| w(p) + fr.normal().as_vec3() * 0.005), rgba(vp::sketch(), 0.10));
        }
    }
    let st = &sess.status;
    for c in &sk.curves {
        let sel = sess.sel.contains(&Sel::Curve(c.id));
        let hov = sess.hover == Some(Sel::Curve(c.id)) || sess.dim_pending.contains(&Sel::Curve(c.id));
        let col = if sel {
            vp::picked()
        } else if hov {
            vp::hover()
        } else if c.construction {
            vp::construction()
        } else if st.fixed_curves.contains(&c.id) {
            vp::geometry()
        } else {
            vp::under_defined()
        };
        let pts: Vec<Vec3> = sk.tessellate(c, (app.camera.wpp() * 3.0) as f64).iter().map(|p| w(*p)).collect();
        if sel {
            // a soft halo under selected curves
            s.polyline(&pts, 10.0, rgba(col, 0.25), true);
        }
        if c.construction {
            crate::viewport::dashed(s, &pts, if sel { 2.6 } else if hov { 2.0 } else { 1.3 }, rgba(col, 1.0), app.camera.wpp() * 6.0);
        } else {
            s.polyline(&pts, if sel { 3.6 } else if hov { 2.8 } else { 2.0 }, rgba(col, 1.0), true);
        }
    }
    let sel_ends: std::collections::HashSet<Id> = sk.curves.iter().filter(|c| sess.sel.contains(&Sel::Curve(c.id))).flat_map(|c| Sketch::curve_points(&c.geom)).collect();
    for p in &sk.points {
        let sel = sess.sel.contains(&Sel::Point(p.id)) || sel_ends.contains(&p.id);
        let hov = sess.hover == Some(Sel::Point(p.id)) || sess.dim_pending.contains(&Sel::Point(p.id));
        let col = if sel {
            vp::picked()
        } else if hov {
            vp::hover()
        } else if st.fixed_points.contains(&p.id) {
            vp::geometry()
        } else {
            vp::under_defined()
        };
        s.point(w(p.v()), if sel || hov { 8.0 } else { 5.5 }, rgba(col, 1.0));
    }
    // origin
    s.point(w(DVec2::ZERO), 6.0, rgba(vp::fixed(), 0.9));
    // a model point that a click would pick
    if let Some(m) = sess.model_hover {
        s.point(w(m), 9.0, rgba(vp::hover(), 1.0));
    }

    // tool preview
    if let Some(c) = sess.cursor {
        let prev = rgba(vp::preview(), 1.0);
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
            Tool::Ellipse | Tool::EllipticalArc if !sess.clicks.is_empty() => {
                let m = sess.clicks[0];
                if sess.clicks.len() == 1 {
                    poly(vec![m, c]);
                } else {
                    let u = sess.clicks[1] - m;
                    let minor_at = if sess.clicks.len() >= 3 { sess.clicks[2] } else { c };
                    let rmin = ((minor_at - m).perp_dot(u.normalize_or_zero())).abs();
                    let v = u.perp().normalize_or_zero() * rmin;
                    poly(
                        (0..=72)
                            .map(|i| {
                                let t = i as f64 / 72.0 * std::f64::consts::TAU;
                                m + u * t.cos() + v * t.sin()
                            })
                            .collect(),
                    );
                }
            }
            Tool::Spline if !sess.clicks.is_empty() => {
                let mut pts = sess.clicks.clone();
                pts.push(c);
                poly(crate::sketch::catmull_rom(&pts, false, 12));
            }
            Tool::TangentArc if sess.chain.is_some() => {
                let start = sess.chain.unwrap();
                if let Some((t, _)) = tangent_at(sk, start) {
                    let s0 = sk.pv(start);
                    let chord = c - s0;
                    let n = t.perp();
                    let den = 2.0 * chord.dot(n);
                    if den.abs() > 1e-9 {
                        let r = chord.length_squared() / den;
                        let ctr = s0 + n * r;
                        let (a0, mut a1) = ((s0 - ctr).to_angle(), (c - ctr).to_angle());
                        if r > 0.0 {
                            while a1 < a0 {
                                a1 += std::f64::consts::TAU;
                            }
                        } else {
                            while a1 > a0 {
                                a1 -= std::f64::consts::TAU;
                            }
                        }
                        poly((0..=48).map(|i| ctr + DVec2::from_angle(a0 + (a1 - a0) * i as f64 / 48.0) * r.abs()).collect());
                    } else {
                        poly(vec![s0, c]);
                    }
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
        if sess.tool != Tool::Select || sess.snap.is_some() {
            let col = match sess.snap {
                Some(Snap::Point(_)) | Some(Snap::Origin) | Some(Snap::Mid(_)) | Some(Snap::Model) => vp::snap(),
                Some(Snap::Curve(_)) => vp::hover(),
                _ => vp::preview(),
            };
            s.point(w(c), 6.0, rgba(col, 1.0));
        }
    }
}

/// Screen rectangles of dimension labels and constraint glyphs (for picking).
fn label_rects(app: &App, sk: &Sketch, fr: &Frame, rect: egui::Rect) -> Vec<(usize, egui::Rect)> {
    let (vars, _) = app.doc.var_values();
    let env = Env { vars: &vars, units: app.doc.units };
    let mut out = vec![];
    for (i, c) in sk.constraints.iter().enumerate() {
        if let Some(p) = label_pos(app, sk, fr, rect, c, i) {
            // roughly the drawn label's size, so the whole label can be grabbed
            let size = match dim_text(sk, &env, c) {
                Some(t) => egui::vec2((t.chars().count() as f32 * 7.0 + 10.0).max(28.0), 18.0),
                None => egui::vec2(16.0, 16.0),
            };
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
    let base =
        r.iter().find_map(|id| sk.curve(*id).map(|cv| sk.tessellate(cv, 1.0)).map(|pts| pts[pts.len() / 2])).or_else(|| r.iter().find_map(|id| sk.point(*id).map(|p| p.v())))?;
    let s = to_screen(app, fr, rect, base);
    // spread glyphs on the same spot
    let k = sk.constraints[..i].iter().filter(|o| o.dim().is_none() && o.refs().first() == r.first()).count();
    Some(s + egui::vec2(12.0 + 15.0 * k as f32, -12.0))
}

/// Dimension annotations (also drawn on the model when the sketch isn't being edited).
pub fn draw_dims(app: &App, sk: &Sketch, fr: &Frame, painter: &egui::Painter, rect: egui::Rect, sess: Option<&SketchSession>, ui: Option<&Ui>) {
    let glyphs = ui.is_some();
    let (vars, _) = app.doc.var_values();
    let env = Env { vars: &vars, units: app.doc.units };
    for (i, c) in sk.constraints.iter().enumerate() {
        let sel = sess.map(|s| s.sel.contains(&Sel::Constraint(i))).unwrap_or(false);
        let hov = sess.map(|s| s.hover == Some(Sel::Constraint(i))).unwrap_or(false);
        let bad = sess.map(|s| s.status.bad_dims.contains(&i) || s.conflicts.contains(&i)).unwrap_or(false);
        let col = if sel {
            vp::picked()
        } else if hov {
            vp::hover()
        } else if bad {
            vp::fixed()
        } else {
            vp::dim()
        };
        if c.dim().is_none() {
            if glyphs {
                if let Some(p) = label_pos(app, sk, fr, rect, c, i) {
                    let r = egui::Rect::from_center_size(p, egui::vec2(15.0, 15.0));
                    painter.rect_filled(r, 3.0, theme::label_bg());
                    let tint = if sel {
                        vp::picked()
                    } else if hov {
                        vp::hover()
                    } else if bad {
                        vp::fixed()
                    } else {
                        theme::muted()
                    };
                    if let Some(ui) = ui {
                        icons::image(c.icon(), 13.0).tint(tint).paint_at(ui, r.shrink(1.0));
                    }
                }
            }
            continue;
        }
        let editing = sess.and_then(|s| s.dim_edit.as_ref()).map(|e| e.0 == i).unwrap_or(false);
        draw_dim(app, sk, fr, painter, rect, &env, c, col, !editing);
    }
}

/// The text on a dimension's label: `Ø12`, `w = 40`, `Ø1 in = 25.4`; reference dimensions in parentheses.
fn dim_text(sk: &Sketch, env: &Env, c: &Constraint) -> Option<String> {
    let d = c.dim()?;
    let angle = c.is_angle();
    let prefix = match c {
        Constraint::Diameter { .. } => "Ø",
        Constraint::Radius { .. } => "R",
        _ => "",
    };
    let fmt = |v: f64| if angle { format!("{}°", expr::trim(v, 2)) } else { env.units.fmt(v) };
    if d.driven {
        return Some(match sk.measure(c) {
            Some(v) => format!("({prefix}{})", fmt(v)),
            None => "(?)".into(),
        });
    }
    Some(match expr::eval(&d.expr, if angle { Kind::Angle } else { Kind::Length }, env) {
        Err(_) => d.expr.clone(),
        Ok(v) if plain_value(&d.expr, angle, env) => format!("{prefix}{}", fmt(v)),
        Ok(v) => format!("{prefix}{} = {}", d.expr.trim(), fmt(v)),
    })
}

/// A bare number, maybe with the document's own unit (`12`, `12 mm`, `30°`): the label shows just the value.
fn plain_value(e: &str, angle: bool, env: &Env) -> bool {
    let t = e.trim();
    let units: &[&str] = if angle { &["deg", "°"] } else { &[env.units.label()] };
    let t = units.iter().find_map(|u| t.strip_suffix(u)).unwrap_or(t).trim();
    t.parse::<f64>().is_ok()
}

/// One dimension: extension and dimension lines, arrows and (unless it is being edited) its label.
#[allow(clippy::too_many_arguments)]
fn draw_dim(app: &App, sk: &Sketch, fr: &Frame, painter: &egui::Painter, rect: egui::Rect, env: &Env, c: &Constraint, col: Color32, label: bool) {
    let ts = |p: DVec2| to_screen(app, fr, rect, p);
    let stroke = Stroke::new(1.0, col.gamma_multiply(0.9));
    let (Some(d), Some(anchor)) = (c.dim(), dim_anchor(sk, c)) else { return };
    let lp = ts(anchor + DVec2::from(d.offset));
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
                // extend the line to the foot when it lies past the line's ends
                let t = (foot - pa).dot(dd);
                let len = (pb - pa).length();
                if t < 0.0 || t > len {
                    painter.line_segment([ts(foot), ts(if t < 0.0 { pa } else { pb })], Stroke::new(0.8, col.gamma_multiply(0.5)));
                }
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
        RadialDistance { a, b, .. } => {
            let r = |id: Id| sk.measure(&Radius { c: id, dim: d.clone() }).unwrap_or(0.0);
            let (ctr, ra, rb) = (anchor, r(*a), r(*b));
            // along the label's direction, from the inner circle out to the outer one
            let w = DVec2::from(d.offset);
            let dir = w.normalize_or(DVec2::X);
            let (p, q) = (ctr + dir * ra, ctr + dir * rb);
            arrow_line(painter, ts(p), ts(q), stroke);
            // a label off the arrow gets a leader from the nearer rim
            let t = w.length();
            if t > rb || t < ra {
                painter.line_segment([ts(if t > rb { q } else { p }), lp], stroke);
            }
        }
        Angle { a, b, .. } => {
            if let (Some(Geom::Line { a: p, b: q }), Some(Geom::Line { a: r, b: s })) = (sk.curve(*a).map(|c| &c.geom), sk.curve(*b).map(|c| &c.geom)) {
                let u1 = (sk.pv(*q) - sk.pv(*p)).normalize();
                let u2 = (sk.pv(*s) - sk.pv(*r)).normalize();
                let w = DVec2::from(d.offset);
                let rad = w.length().max(app.camera.wpp() as f64 * 30.0);
                // the arc spans the angle the label sits in
                let (v1, v2) = angle_wedge(u1, u2, w);
                let (t0, sweep) = (v1.to_angle(), v1.angle_to(v2));
                let pts: Vec<Pos2> = (0..=24).map(|i| ts(anchor + DVec2::from_angle(t0 + sweep * i as f64 / 24.0) * rad)).collect();
                painter.add(egui::Shape::line(pts, stroke));
                let faint = Stroke::new(0.8, col.gamma_multiply(0.5));
                for v in [v1, v2] {
                    painter.line_segment([ts(anchor), ts(anchor + v * rad * 1.08)], faint);
                }
            }
        }
        _ => {}
    }
    if label {
        let Some(text) = dim_text(sk, env, c) else { return };
        let font = egui::FontId::proportional(12.0);
        let g = painter.layout_no_wrap(text, font, col);
        let r = egui::Rect::from_center_size(lp, g.size() + egui::vec2(8.0, 2.0));
        painter.rect_filled(r, 3.0, theme::label_bg());
        painter.galley(r.min + egui::vec2(4.0, 1.0), g, col);
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
pub fn overlay(app: &mut App, ui: &Ui, painter: &egui::Painter, rect: egui::Rect) {
    if let Some(p) = painter.ctx().pointer_latest_pos() {
        LAST_POINTER.with(|c| c.set(p));
    }
    let Some(sess) = app.sketch.as_ref() else { return };
    let Some((sk, fr)) = sketch_of(&app.doc, sess.feature) else { return };
    let sk = sk.clone();
    draw_dims(app, &sk, &fr, painter, rect, Some(sess), Some(ui));

    // the dimension being placed follows the cursor until the click that puts it down
    if let (Tool::Dimension, Some(at)) = (sess.tool, sess.cursor) {
        let picking = matches!(sess.hover, Some(x @ (Sel::Point(_) | Sel::Curve(_))) if sess.dim_pending.len() < 2 && !sess.dim_pending.contains(&x));
        if !picking && !sess.dim_pending.is_empty() {
            if let Some(c) = build_dimension(app, &sess.dim_pending, at) {
                let (vars, _) = app.doc.var_values();
                let env = Env { vars: &vars, units: app.doc.units };
                draw_dim(app, &sk, &fr, painter, rect, &env, &c, vp::hover(), true);
            }
        }
    }

    if let (Some(Drag::Box { start }), Some(end)) = (&sess.drag, sess.box_end) {
        let r = egui::Rect::from_two_pos(*start, end);
        let crossing = end.x < start.x;
        painter.rect(r, 0.0, theme::accent().gamma_multiply(0.08), Stroke::new(1.0, theme::accent()), egui::StrokeKind::Inside);
        if crossing {
            painter.text(r.left_top() + egui::vec2(4.0, 4.0), egui::Align2::LEFT_TOP, "crossing", egui::FontId::proportional(10.0), theme::accent());
        }
    }

    // inference: dashed guides to the points the cursor lines up with, the line a parallel /
    // perpendicular refers to, and the constraint the next click will add next to the cursor
    if let (Some(c), Some(p), true) = (sess.cursor, painter.ctx().pointer_hover_pos(), sess.tool != Tool::Dimension && (sess.tool != Tool::Select || sess.snap.is_some())) {
        if rect.contains(p) {
            let cs = to_screen(app, &fr, rect, c);
            let guide = Stroke::new(1.0, vp::snap().gamma_multiply(0.8));
            for q in &sess.guides {
                painter.add(egui::Shape::dashed_line(&[to_screen(app, &fr, rect, *q), cs], guide, 4.0, 3.0));
            }
            if let Some(Geom::Line { a, b }) = sess.infer.and_then(|d| d.reference()).and_then(|l| sk.curve(l)).map(|x| &x.geom) {
                painter.line_segment([to_screen(app, &fr, rect, sk.pv(*a)), to_screen(app, &fr, rect, sk.pv(*b))], Stroke::new(3.0, vp::hover().gamma_multiply(0.7)));
            }
            let icon = match (sess.infer, sess.snap) {
                (Some(d), _) => Some(d.constraint(0).icon()),
                (None, Some(Snap::Point(_)) | Some(Snap::Curve(_)) | Some(Snap::Origin)) => Some("coincident"),
                (None, Some(Snap::Mid(_))) => Some("midpoint"),
                _ => None,
            };
            if let Some(icon) = icon {
                let r = egui::Rect::from_center_size(cs + egui::vec2(16.0, -18.0), egui::vec2(20.0, 20.0));
                painter.rect(r, 4.0, theme::label_bg(), Stroke::new(1.0, theme::accent()), egui::StrokeKind::Inside);
                icons::image(icon, 15.0).tint(theme::accent()).paint_at(ui, r.shrink(2.5));
            }
        }
    }

    // cursor label: size of what's being drawn
    if let (Some(c), Some(p)) = (sess.cursor, painter.ctx().pointer_hover_pos()) {
        if rect.contains(p) {
            let u = app.doc.units;
            let text = match sess.tool {
                Tool::Line => sess.chain.map(|a| {
                    let d = c - sk.pv(a);
                    format!("{}  {}°", u.fmt(d.length()), expr::trim(d.y.atan2(d.x).to_degrees(), 1))
                }),
                Tool::Rect if sess.clicks.len() == 1 => {
                    let d = c - sess.clicks[0];
                    Some(format!("{} × {}", u.fmt(d.x.abs()), u.fmt(d.y.abs())))
                }
                Tool::CenterRect if sess.clicks.len() == 1 => {
                    let d = c - sess.clicks[0];
                    Some(format!("{} × {}", u.fmt(2.0 * d.x.abs()), u.fmt(2.0 * d.y.abs())))
                }
                Tool::Circle | Tool::Polygon if sess.clicks.len() == 1 => Some(format!("Ø {}", u.fmt(2.0 * (c - sess.clicks[0]).length()))),
                Tool::Arc if sess.clicks.len() >= 1 => {
                    Some(format!("R {}", u.fmt((sess.clicks.last().copied().unwrap_or(c) - sess.clicks[0]).length().max((c - sess.clicks[0]).length()))))
                }
                Tool::Select => None,
                _ => None,
            };
            let typed = sess.typed.clone();
            if let Some(t) = typed {
                let hint = match sess.tool {
                    Tool::Rect | Tool::CenterRect => "width, height",
                    Tool::Circle => "diameter",
                    _ => "length",
                };
                crate::viewport::label_box(painter, p + egui::vec2(16.0, 20.0), &format!("{t}  ({hint}, Enter)"), theme::accent());
            } else if let Some(t) = text {
                crate::viewport::label_box(painter, p + egui::vec2(16.0, 20.0), &format!("{t}  · type a size"), theme::text());
            }
        }
    }

    // text tool input
    let text_edit = app.sketch.as_ref().and_then(|s| s.text_edit.clone());
    if let Some((at, mut text)) = text_edit {
        let pos = to_screen(app, &fr, rect, at);
        let mut done = None;
        egui::Area::new(egui::Id::new("text-edit")).order(egui::Order::Foreground).fixed_pos(pos + egui::vec2(0.0, 6.0)).show(painter.ctx(), |ui| {
            egui::Frame::new().fill(theme::bg()).stroke(Stroke::new(1.0, theme::accent())).corner_radius(4).inner_margin(4).show(ui, |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut text).hint_text("Text").desired_width(160.0));
                if !r.has_focus() && !r.lost_focus() {
                    r.request_focus();
                }
                if ui.input(|i| i.key_pressed(Key::Enter)) {
                    done = Some(true);
                } else if ui.input(|i| i.key_pressed(Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        match done {
            Some(true) => {
                app.sketch.as_mut().unwrap().text_edit = None;
                create_text(app, at, &text);
            }
            Some(false) => {
                let s = app.sketch.as_mut().unwrap();
                s.text_edit = None;
                s.text_editing = None;
            }
            None => app.sketch.as_mut().unwrap().text_edit = Some((at, text)),
        }
    }

    // sketch pattern popup
    let pat = app.sketch.as_ref().and_then(|s| s.pattern);
    if let Some((circular, mut count, mut a, mut b)) = pat {
        let mut apply = false;
        let mut close = false;
        let units = app.doc.units.label();
        egui::Area::new(egui::Id::new("sk-pattern")).order(egui::Order::Foreground).fixed_pos(rect.min + egui::vec2(10.0, 10.0)).show(painter.ctx(), |ui| {
            egui::Frame::new().fill(theme::panel()).stroke(Stroke::new(1.0, theme::border())).corner_radius(8).inner_margin(10).show(ui, |ui| {
                ui.label(RichText::new(if circular { "Circular pattern" } else { "Linear pattern" }).strong());
                egui::Grid::new("skpat").num_columns(2).show(ui, |ui| {
                    ui.label(RichText::new("Instances").color(theme::muted()));
                    ui.add(egui::DragValue::new(&mut count).range(2..=200));
                    ui.end_row();
                    if circular {
                        ui.label(RichText::new("Total angle (°)").color(theme::muted()));
                        ui.add(egui::DragValue::new(&mut a).range(1.0..=360.0));
                        ui.end_row();
                    } else {
                        ui.label(RichText::new(format!("Spacing X ({units})")).color(theme::muted()));
                        ui.add(egui::DragValue::new(&mut a).speed(0.5));
                        ui.end_row();
                        ui.label(RichText::new(format!("Spacing Y ({units})")).color(theme::muted()));
                        ui.add(egui::DragValue::new(&mut b).speed(0.5));
                        ui.end_row();
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        });
        app.sketch.as_mut().unwrap().pattern = if apply || close { None } else { Some((circular, count, a, b)) };
        if apply {
            apply_pattern(app, circular, count, a, b);
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
        let positive = matches!(c, Constraint::Length { .. } | Constraint::Radius { .. } | Constraint::Diameter { .. } | Constraint::RadialDistance { .. });
        let (vars, _) = app.doc.var_values();
        let units = app.doc.units;
        let mut done = None;
        egui::Area::new(egui::Id::new("dim-edit")).order(egui::Order::Foreground).pivot(egui::Align2::CENTER_CENTER).fixed_pos(pos).show(painter.ctx(), |ui| {
            egui::Frame::new().fill(theme::bg()).stroke(Stroke::new(1.0, theme::accent())).corner_radius(4).inner_margin(2).show(ui, |ui| {
              let err = match expr::eval(&text, if angle { Kind::Angle } else { Kind::Length }, &Env { vars: &vars, units }) {
                  Err(e) => Some(e),
                  // sizes can't be zero or negative (a negative radius would be stored as its size)
                  Ok(v) if positive && v <= 0.0 => Some("must be more than 0".to_string()),
                  Ok(_) => None,
              };
              ui.horizontal(|ui| {
                let ok = err.is_none();
                let out = egui::TextEdit::singleline(&mut text).desired_width(90.0).text_color(if ok { theme::text() } else { theme::error_soft() }).show(ui);
                let r = out.response;
                if !r.has_focus() && !r.lost_focus() {
                    // first frame: focus and select the current value so typing replaces it
                    r.request_focus();
                    let mut st = out.state;
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(text.chars().count()))));
                    st.store(ui.ctx(), r.id);
                }
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    done = Some(false);
                } else if ui.input(|i| i.key_pressed(Key::Enter)) && ok {
                    done = Some(true);
                } else if r.lost_focus() && !ui.input(|i| i.key_pressed(Key::Enter)) {
                    // clicking away keeps a valid value, like pressing Enter
                    done = Some(ok);
                }
                ui.label(RichText::new(if angle { "°" } else { units.label() }).color(theme::muted()).size(11.0));
              });
              // an invalid value keeps the box open and says why
              if let (Some(e), false) = (&err, text.trim().is_empty()) {
                  ui.label(RichText::new(e).color(theme::error_soft()).size(11.0));
              }
            });
        });
        match done {
            Some(true) => {
                app.sketch.as_mut().unwrap().dim_edit = None;
                let old = c.dim().map(|d| d.expr.clone()).unwrap_or_default();
                let new = text.trim().to_string();
                if new != old {
                    let was_ok = app.sketch.as_ref().unwrap().status.error.is_none();
                    // a value that gets put back is no edit: it keeps the redo steps
                    let redo = app.sketch.as_ref().unwrap().redo.clone();
                    app.sk_checkpoint();
                    if let Some(d) = app.sk_mut().constraints.get_mut(i).and_then(|c| c.dim_mut()) {
                        d.expr = new.clone();
                    }
                    // grow from the dimension's first point (it stays put), not about the middle
                    let hold: Vec<Id> = {
                        let (sk, _) = app.sk();
                        match sk.constraints.get(i) {
                            Some(Constraint::Length { l, .. }) => sk.curve(*l).map(|c| Sketch::curve_points(&c.geom)).and_then(|p| p.first().copied()).into_iter().collect(),
                            Some(Constraint::Distance { a, .. }) | Some(Constraint::AxisDistance { a, .. }) => vec![*a],
                            Some(Constraint::PointLineDistance { p, .. }) => vec![*p],
                            // the inner circle stays, the outer one grows
                            Some(Constraint::RadialDistance { a, .. }) => {
                                let mut h = vec![*a];
                                h.extend(sk.curve(*a).map(|c| Sketch::curve_points(&c.geom)).unwrap_or_default());
                                h
                            }
                            _ => vec![],
                        }
                    };
                    let fixed_already = app.sketch.as_ref().map(|s| hold.iter().all(|h| s.status.fixed_points.contains(h))).unwrap_or(false);
                    if !hold.is_empty() && !fixed_already {
                        app.solve_sketch(&hold);
                    }
                    app.after_sketch_edit();
                    if was_ok && app.sketch.as_ref().unwrap().status.error.is_some() {
                        // name (and highlight) what it conflicts with, then put the old value back
                        let conflicts = {
                            let (vars, _) = app.doc.var_values();
                            let env = Env { vars: &vars, units: app.doc.units };
                            app.sk().0.conflicts(&env, i)
                        };
                        if let Some(d) = app.sk_mut().constraints.get_mut(i).and_then(|c| c.dim_mut()) {
                            d.expr = old;
                        }
                        app.sketch.as_mut().unwrap().undo.pop();
                        app.sketch.as_mut().unwrap().redo = redo;
                        app.after_sketch_edit();
                        let msg = match describe_conflicts(app, &conflicts) {
                            Some(what) => format!("{new} can't be met: it conflicts with {what} (highlighted)"),
                            None => format!("{new} can't be met with the other constraints"),
                        };
                        app.sketch.as_mut().unwrap().conflicts = conflicts;
                        app.flash(msg, ToastKind::Error);
                    }
                }
            }
            Some(false) => app.sketch.as_mut().unwrap().dim_edit = None,
            None => app.sketch.as_mut().unwrap().dim_edit = Some((i, text)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_sizes_split_outside_names_and_brackets() {
        assert_eq!(split_size("40, 20"), Some(("40", "20")));
        assert_eq!(split_size("40x20"), Some(("40", "20")));
        assert_eq!(split_size("40 mm x 20 mm"), Some(("40 mm", "20 mm")));
        assert_eq!(split_size("40×20"), Some(("40", "20")));
        assert_eq!(split_size("40*20"), Some(("40", "20")));
        assert_eq!(split_size("25"), Some(("25", "25")));
        assert_eq!(split_size("max(10, 20), 5"), Some(("max(10, 20)", "5")));
        assert_eq!(split_size("box_w, 2*h"), Some(("box_w", "2*h")));
        assert_eq!(split_size("box_w"), Some(("box_w", "box_w")));
        assert_eq!(split_size("(w+2)x(h+2)"), Some(("(w+2)", "(h+2)")));
    }

    #[test]
    fn label_position_picks_the_measured_direction() {
        let (a, b) = (DVec2::new(0.0, 0.0), DVec2::new(10.0, 40.0));
        // above the pair: horizontal distance; beside it: vertical; off a corner: aligned
        assert_eq!(linear_axis(a, b, DVec2::new(5.0, 60.0)), Some(0));
        assert_eq!(linear_axis(a, b, DVec2::new(30.0, 20.0)), Some(1));
        assert_eq!(linear_axis(a, b, DVec2::new(30.0, 60.0)), None);
        // already axis-aligned pairs are always aligned
        assert_eq!(linear_axis(a, DVec2::new(10.0, 0.0), DVec2::new(5.0, 20.0)), None);
    }

    fn env_solve(sk: &mut Sketch) -> SolveStatus {
        let vars = crate::expr::Vars::default();
        sk.solve(&Env { vars: &vars, units: crate::expr::Units::Mm }, &[])
    }

    /// A triangle held in place, counter-clockwise: (points, lines).
    fn fixed_triangle() -> (Sketch, Vec<Id>, Vec<Id>) {
        let mut sk = Sketch::default();
        let p: Vec<Id> = [(0.0, 0.0), (40.0, 0.0), (20.0, 30.0)].iter().map(|&(x, y)| sk.add_point(DVec2::new(x, y))).collect();
        let l: Vec<Id> = (0..3).map(|i| sk.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 3] }, false)).collect();
        for &q in &p {
            let v = sk.pv(q);
            sk.constraints.push(Constraint::Fix { p: q, x: v.x, y: v.y });
        }
        (sk, p, l)
    }

    fn line_dist(sk: &Sketch, l: Id, p: DVec2) -> f64 {
        let Some(Geom::Line { a, b }) = sk.curve(l).map(|c| c.geom.clone()) else { panic!() };
        let (a, b) = (sk.pv(a), sk.pv(b));
        ((b - a).perp_dot(p - a) / (b - a).length()).abs()
    }

    fn set_offset_dim(sk: &mut Sketch, v: &str) {
        let dims: Vec<usize> = (0..sk.constraints.len()).filter(|i| sk.constraints[*i].dim().is_some()).collect();
        assert_eq!(dims.len(), 1, "one distance dimension for the chain");
        sk.constraints[dims[0]].dim_mut().unwrap().expr = v.into();
    }

    #[test]
    fn offset_chain_outward_gets_corner_arcs() {
        let (sk, _, l) = fixed_triangle();
        let out = offset_curves(&sk, &l, DVec2::new(20.0, -10.0), 5.0, "5").unwrap();
        let mut out = out;
        let new: Vec<&Curve> = out.curves[3..].iter().collect();
        assert_eq!(new.iter().filter(|c| matches!(c.geom, Geom::Line { .. })).count(), 3);
        assert_eq!(new.iter().filter(|c| matches!(c.geom, Geom::Arc { .. })).count(), 3);
        let st = env_solve(&mut out);
        assert!(st.ok && st.dof == 0, "{st:?}");
        // one closed outline around the triangle: triangle + ring = A + P d + π d²
        let per = 40.0 + 2.0 * (20f64 * 20.0 + 30.0 * 30.0).sqrt();
        let area: f64 = crate::regions::find_regions(&out).iter().map(|r| r.area).sum();
        // (arcs are measured on their tessellation)
        assert!((area - (600.0 + per * 5.0 + std::f64::consts::PI * 25.0)).abs() < 1.0, "{area}");
        // the single dimension drives the whole chain
        set_offset_dim(&mut out, "8");
        assert!(env_solve(&mut out).ok);
        for (i, c) in out.curves[3..6].iter().enumerate() {
            let Geom::Line { a, b } = c.geom else { panic!() };
            for p in [a, b] {
                assert!((line_dist(&out, l[i], out.pv(p)) - 8.0).abs() < 1e-6);
            }
        }
    }

    /// Offsetting a slot: the new lines and arcs meet tangentially, and stay so when the copy is dragged.
    #[test]
    fn offset_slot_keeps_its_tangents() {
        let mut sk = Sketch::default();
        let (c1, c2, n) = (DVec2::ZERO, DVec2::new(20.0, 0.0), DVec2::new(0.0, 4.0));
        let a = sk.add_point(c1);
        let b = sk.add_point(c2);
        let p: Vec<Id> = [c1 + n, c2 + n, c2 - n, c1 - n].iter().map(|v| sk.add_point(*v)).collect();
        let l1 = sk.add_curve(Geom::Line { a: p[0], b: p[1] }, false);
        let l2 = sk.add_curve(Geom::Line { a: p[2], b: p[3] }, false);
        let a1 = sk.add_curve(Geom::Arc { c: b, a: p[2], b: p[1] }, false);
        let a2 = sk.add_curve(Geom::Arc { c: a, a: p[0], b: p[3] }, false);
        for (l, ar) in [(l1, a1), (l1, a2), (l2, a1), (l2, a2)] {
            sk.constraints.push(Constraint::Tangent { a: l, b: ar, inner: false });
        }
        for q in [a, b] {
            let v = sk.pv(q);
            sk.constraints.push(Constraint::Fix { p: q, x: v.x, y: v.y });
        }
        sk.constraints.push(Constraint::Equal { a: a1, b: a2 });
        sk.constraints.push(Constraint::Radius { c: a1, dim: Dim::new("4", [0.0; 2]) });
        let before = sk.constraints.len();
        let mut out = offset_curves(&sk, &[l1, a1, l2, a2], DVec2::new(10.0, 20.0), 2.0, "2").unwrap();
        let tangents = out.constraints[before..].iter().filter(|k| matches!(k, Constraint::Tangent { .. })).count();
        assert_eq!(tangents, 4);
        let st = env_solve(&mut out);
        assert!(st.ok && st.dof == 0, "{st:?}");
        // drag an end of the copy's straight side along it: the copy must not leave the arcs
        let Some(Geom::Line { a: q, .. }) = out.curves.iter().rev().find(|c| matches!(c.geom, Geom::Line { .. })).map(|c| c.geom.clone()) else { panic!() };
        let v = out.pv(q);
        out.point_mut(q).unwrap().x = v.x + 3.0;
        let vars = crate::expr::Vars::default();
        assert!(out.solve(&Env { vars: &vars, units: crate::expr::Units::Mm }, &[q]).ok);
        assert!((out.pv(q) - v).length() < 1e-6, "{} {v}", out.pv(q));
    }

    #[test]
    fn offset_chain_inward_trims_corners() {
        let (sk, _, l) = fixed_triangle();
        let mut out = offset_curves(&sk, &l, DVec2::new(20.0, 10.0), 3.0, "3").unwrap();
        // three lines sharing three new corners
        assert_eq!(out.curves.len(), 6);
        assert_eq!(out.points.len(), 6);
        let st = env_solve(&mut out);
        assert!(st.ok && st.dof == 0, "{st:?}");
        set_offset_dim(&mut out, "4");
        assert!(env_solve(&mut out).ok);
        for (i, c) in out.curves[3..].iter().enumerate() {
            let Geom::Line { a, b } = c.geom else { panic!() };
            for p in [a, b] {
                assert!((line_dist(&out, l[i], out.pv(p)) - 4.0).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn trim_across_the_circle_seam_wraps() {
        // cuts at 0.2 and 0.8: a click at 0.9 or 0.1 removes the piece from 0.8 round to 0.2
        let splits = [0.0, 0.2, 0.8, 1.0];
        assert_eq!(trim_range(&splits, 0.9, true), (0.8, 0.2));
        assert_eq!(trim_range(&splits, 0.1, true), (0.8, 0.2));
        assert_eq!(trim_range(&splits, 0.5, true), (0.2, 0.8));
        // open curves keep their ends
        assert_eq!(trim_range(&splits, 0.1, false), (0.0, 0.2));
    }

    #[test]
    fn dimension_labels_read_cleanly() {
        let vars = crate::expr::Vars::from(std::collections::HashMap::from([("d".to_string(), 12.0)]));
        let env = Env { vars: &vars, units: crate::expr::Units::Mm };
        let mut sk = Sketch::default();
        let c = sk.add_point(DVec2::ZERO);
        let ci = sk.add_curve(Geom::Circle { c, r: 6.0 }, false);
        let dia = |e: &str| Constraint::Diameter { c: ci, dim: Dim::new(e, [0.0; 2]) };
        assert_eq!(dim_text(&sk, &env, &dia("12")).unwrap(), "Ø12");
        assert_eq!(dim_text(&sk, &env, &dia("12 mm")).unwrap(), "Ø12");
        assert_eq!(dim_text(&sk, &env, &dia("d")).unwrap(), "Ød = 12");
        let mut driven = dia("99");
        driven.dim_mut().unwrap().driven = true;
        assert_eq!(dim_text(&sk, &env, &driven).unwrap(), "(Ø12)");
    }

    #[test]
    fn angle_quadrant_follows_the_label() {
        let u1 = DVec2::X;
        let u2 = DVec2::from_angle(60f64.to_radians());
        let ang = |w: DVec2| {
            let (v1, v2) = angle_wedge(u1, u2, w);
            v1.angle_to(v2).to_degrees().abs().round()
        };
        assert_eq!(ang(DVec2::from_angle(30f64.to_radians())), 60.0);
        assert_eq!(ang(DVec2::from_angle(120f64.to_radians())), 120.0);
        assert_eq!(ang(DVec2::from_angle(210f64.to_radians())), 60.0);
        assert_eq!(ang(DVec2::from_angle(-60f64.to_radians())), 120.0);
    }
}
