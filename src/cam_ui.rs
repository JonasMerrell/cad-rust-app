//! Manufacture tab: setup, tool library, operations, toolpath preview and simulation.

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use egui::{Color32, Margin, Pos2, RichText, Sense, Stroke, Ui};
use glam::{DVec3, Vec2, Vec3};

use crate::app::{App, ToastKind};
use crate::cam::{self, Cam, OpKind, Operation, Origin, Side, Tool, ToolKind};
use crate::cam_paths::{self, Move, Part, PartInput, SketchPaths, Toolpath};
use crate::cam_sim::Sim;
use crate::cam_stock::StockMap;
use crate::doc::{BodyId, Document};
use crate::icons;
use crate::kernel::BodyMesh;
use crate::render::{SceneData, ViewportCallback};
use crate::theme::{self, vp};
use crate::workspace::Tab;

/// Result of a background job: input key, part, toolpaths, the stock the setup starts from and its carried stock box.
type Job = (u64, Arc<Part>, Vec<Toolpath>, Option<Arc<StockMap>>, Option<[[f64; 3]; 2]>);

#[derive(Default)]
pub struct CamUi {
    pub scene_dirty: bool,
    /// Last computed toolpaths and the input they were made from.
    pub key: u64,
    pub part: Option<Arc<Part>>,
    pub paths: Vec<Toolpath>,
    /// Stock left by the setups this one carries on from.
    pub stock_in: Option<Arc<StockMap>>,
    job: Option<(u64, Receiver<Job>)>,
    pub computing: bool,
    pub sel_op: Option<u64>,
    pub sel_tool: Option<u64>,
    pub library_open: bool,
    lib_kind: Option<ToolKind>,
    lib_search: String,
    pub sim: Option<Sim>,
    pub sim_on: bool,
    /// The view has been fitted to the stock once.
    pub fitted: bool,
    pub sim_target: usize,
    pub playing: bool,
    /// Moves per second while playing.
    pub speed: f32,
    sim_mesh_at: Option<Instant>,
    sim_mesh_done: usize,
    sim_mesh: Vec<([DVec3; 3], u8)>,
    undo: Vec<Cam>,
    redo: Vec<Cam>,
    last_ck: Option<Instant>,
    /// Picking edges for this deburr operation.
    pub pick_edges: Option<u64>,
    /// Show the part (see-through) over the simulated stock.
    pub show_part: bool,
    /// Name to save the operations under as a template.
    tpl_name: String,
}

fn cam_index(app: &App) -> Option<usize> {
    match app.tab {
        Tab::Cam(i) => Some(i),
        _ => None,
    }
}

pub fn cam(app: &App) -> Option<&Cam> {
    cam_index(app).and_then(|i| app.ws.cams.get(i))
}

pub fn cam_mut(app: &mut App) -> Option<&mut Cam> {
    let i = cam_index(app)?;
    app.ws.cams.get_mut(i)
}

pub fn undo(app: &mut App) {
    let Some(i) = cam_index(app) else { return };
    if let Some(c) = app.cam.undo.pop() {
        let cur = std::mem::replace(&mut app.ws.cams[i], c);
        app.cam.redo.push(cur);
        app.cam.scene_dirty = true;
    }
}

pub fn redo(app: &mut App) {
    let Some(i) = cam_index(app) else { return };
    if let Some(c) = app.cam.redo.pop() {
        let cur = std::mem::replace(&mut app.ws.cams[i], c);
        app.cam.undo.push(cur);
        app.cam.scene_dirty = true;
    }
}

pub fn can_undo(app: &App) -> bool {
    !app.cam.undo.is_empty()
}

pub fn can_redo(app: &App) -> bool {
    !app.cam.redo.is_empty()
}

/// Record an undo step with the state before an edit (edits closer together than a second share one step).
fn record(app: &mut App, before: Cam) {
    let now = Instant::now();
    if app.cam.last_ck.map(|t| now.duration_since(t).as_secs_f32() > 1.0).unwrap_or(true) {
        app.cam.undo.push(before);
        if app.cam.undo.len() > 200 {
            app.cam.undo.remove(0);
        }
        app.cam.redo.clear();
    }
    app.cam.last_ck = Some(now);
    app.cam.scene_dirty = true;
}

/// Edit the open Manufacture tab with an undo step.
fn edit(app: &mut App, f: impl FnOnce(&mut Cam)) {
    let Some(c) = cam(app).cloned() else { return };
    app.cam.last_ck = None;
    if let Some(m) = cam_mut(app) {
        f(m);
    }
    record(app, c);
}

/// Triangles and circular edges of the parts a setup machines, and the Part Studio's sketches (part coordinates).
pub fn part_input(bodies: &[BodyMesh], doc: Option<&Document>, sel: &[BodyId]) -> PartInput {
    let mut tris = vec![];
    let mut circles = vec![];
    for b in bodies {
        let hidden = doc.and_then(|d| d.parts.get(&crate::doc::body_key(b.id))).map(|p| p.hidden).unwrap_or(false);
        if if sel.is_empty() { hidden } else { !sel.contains(&b.id) } {
            continue;
        }
        let p = |i: u32| DVec3::new(b.positions[3 * i as usize] as f64, b.positions[3 * i as usize + 1] as f64, b.positions[3 * i as usize + 2] as f64);
        for t in b.indices.chunks(3) {
            tris.push([p(t[0]), p(t[1]), p(t[2])]);
        }
        for e in &b.edges {
            if let Some(c) = e.circle {
                circles.push(c);
            }
        }
    }
    PartInput { tris, circles, sketches: doc.map(sketch_input).unwrap_or_default() }
}

/// The curves of the active sketches, for tracing and carving (part coordinates).
pub fn sketch_input(doc: &Document) -> Vec<SketchPaths> {
    use crate::sketch::Geom;
    let n = doc.rollback.unwrap_or(doc.features.len()).min(doc.features.len());
    let mut out = vec![];
    for f in doc.features[..n].iter().filter(|f| !f.suppressed) {
        let crate::doc::Op::Sketch { sketch, frame, .. } = &f.op else { continue };
        let mut paths = vec![];
        for c in sketch.curves.iter().filter(|c| !c.construction) {
            let closed = match &c.geom {
                Geom::Circle { .. } | Geom::Ellipse { .. } => true,
                Geom::Spline { closed, .. } | Geom::Poly { closed, .. } => *closed,
                _ => false,
            };
            let pts: Vec<DVec3> = sketch.tessellate(c, 0.2).into_iter().map(|p| frame.to_world(p)).collect();
            if pts.len() >= 2 {
                paths.push((pts, closed));
            }
        }
        out.push(SketchPaths { id: f.id, name: f.name.clone(), paths });
    }
    out
}

fn input_key(cams: &[Cam], ci: usize, input: &PartInput) -> u64 {
    let tris = &input.tris;
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // this setup and the ones it carries on from (their stock boxes are derived, so left out)
    let mut chain = cam_paths::setup_chain(cams, ci).unwrap_or_default();
    chain.push(ci);
    for i in chain {
        let mut c = cams[i].clone();
        c.stock.carried = None;
        serde_json::to_string(&c).unwrap_or_default().hash(&mut h);
    }
    tris.len().hash(&mut h);
    let mut s = 0.0f64;
    for (k, t) in tris.iter().enumerate().step_by((tris.len() / 512).max(1)) {
        s += (t[0].x + t[1].y * 3.0 + t[2].z * 7.0) * (k as f64 + 1.0);
    }
    s.to_bits().hash(&mut h);
    for sk in &input.sketches {
        (sk.id, sk.paths.len()).hash(&mut h);
        for (pts, closed) in &sk.paths {
            closed.hash(&mut h);
            for p in pts {
                p.to_array().map(f64::to_bits).hash(&mut h);
            }
        }
    }
    h.finish()
}

/// Keep the toolpaths up to date: start a background job when the setup or the parts change.
fn refresh(app: &mut App, ctx: &egui::Context) {
    if let Some((k, rx)) = &app.cam.job {
        if let Ok((key, part, paths, stock_in, carried)) = rx.try_recv() {
            if key == *k {
                app.cam.key = key;
                app.cam.part = Some(part);
                app.cam.paths = paths;
                app.cam.stock_in = stock_in;
                if let Some(c) = cam_mut(app) {
                    c.stock.carried = carried;
                }
                app.cam.sim = None;
                app.cam.sim_mesh.clear();
                app.cam.scene_dirty = true;
            }
            app.cam.job = None;
            app.cam.computing = false;
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
    let Some(ci) = cam_index(app) else { return };
    let Some(c) = cam(app).cloned() else { return };
    let input = part_input(app.studio_bodies(c.studio), app.studio_doc(c.studio), &c.bodies);
    let key = input_key(&app.ws.cams, ci, &input);
    if key == app.cam.key || app.cam.job.as_ref().map(|j| j.0 == key).unwrap_or(false) {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    app.cam.job = Some((key, rx));
    app.cam.computing = true;
    let ctx = ctx.clone();
    let mut cams = app.ws.cams.clone();
    std::thread::spawn(move || {
        let linked = cam_paths::link_setups(&mut cams, ci, &input);
        let c = &cams[ci];
        let part = Part::from_input(&input, &c.orient);
        let (stock_in, err) = match linked {
            Ok(s) => (s, None),
            Err(e) => (None, Some(e)),
        };
        let mut paths = cam_paths::generate_from(c, &part, stock_in.as_ref());
        if let Some(e) = err {
            for p in &mut paths {
                p.warnings.insert(0, e.clone());
            }
        }
        let _ = tx.send((key, Arc::new(part), paths, stock_in.map(Arc::new), c.stock.carried));
        ctx.request_repaint();
    });
}

// ---------------------------------------------------------------- toolbar

pub fn toolbar(app: &mut App, ui: &mut Ui) {
    App::group(ui, |ui| {
        for (k, icon) in [(OpKind::Face, "camFace"), (OpKind::Clear, "camClear"), (OpKind::Pocket, "camPocket"), (OpKind::Contour, "camContour"), (OpKind::Chamfer, "chamfer")] {
            if app.tool_button(ui, icon, k.label(), k.description(), matches!(k, OpKind::Clear | OpKind::Contour), false, true).clicked() {
                add_op(app, k);
            }
        }
    });
    App::group(ui, |ui| {
        for (k, icon) in [(OpKind::Drill, "camDrill"), (OpKind::Bore, "hole"), (OpKind::Thread, "camThread"), (OpKind::Tap, "camTap")] {
            if app.tool_button(ui, icon, k.label(), k.description(), k == OpKind::Drill, false, true).clicked() {
                add_op(app, k);
            }
        }
    });
    App::group(ui, |ui| {
        for (k, icon) in [(OpKind::Adaptive, "camAdaptive"), (OpKind::Parallel, "camParallel"), (OpKind::Waterline, "camWaterline"), (OpKind::Scallop, "camScallop"), (OpKind::Pencil, "camPencil"), (OpKind::Deburr, "camDeburr")] {
            if app.tool_button(ui, icon, k.label(), k.description(), matches!(k, OpKind::Adaptive | OpKind::Parallel), false, true).clicked() {
                add_op(app, k);
            }
        }
    });
    App::group(ui, |ui| {
        for (k, icon) in [(OpKind::Trace, "camTrace"), (OpKind::VCarve, "camVCarve")] {
            if app.tool_button(ui, icon, k.label(), k.description(), true, false, true).clicked() {
                add_op(app, k);
            }
        }
    });
    App::group(ui, |ui| {
        if app.tool_button(ui, "camTools", "Tool library", "Pick tools from the library", true, app.cam.library_open, true).clicked() {
            app.cam.library_open = !app.cam.library_open;
        }
        let on = app.cam.sim_on;
        if app.tool_button(ui, "camSim", "Simulate", "Play the toolpaths on the stock and check for crashes", true, on, !app.cam.paths.is_empty()).clicked() {
            app.cam.sim_on = !on;
            app.cam.scene_dirty = true;
            if app.cam.sim_on && app.cam.sim.is_none() {
                start_sim(app, true);
            }
        }
        if app.tool_button(ui, "camGcode", "G-code", "Export G-code for the machine", true, false, !app.cam.paths.is_empty()).clicked() {
            export_dialog(app);
        }
        if app.tool_button(ui, "camSheet", "Setup sheet", "Save a setup sheet for the operator: stock, work zero, fixtures, tools and operations", true, false, !app.cam.paths.is_empty()).clicked() {
            sheet_dialog(app);
        }
    });
}

fn add_op(app: &mut App, k: OpKind) {
    // a pencil pass wants a tool smaller than the ones before it
    let small = |c: &Cam| c.tools.iter().filter(|t| t.kind == ToolKind::Ball && t.diameter <= 3.0).map(|t| t.id).next();
    let missing = cam(app).map(|c| if k == OpKind::Pencil { small(c).is_none() } else { !c.tools.iter().any(|t| k.accepts(t.kind)) }).unwrap_or(true);
    if missing {
        // bring in a suitable tool from the library first
        let material = cam(app).map(|c| c.stock.material.clone()).unwrap_or_default();
        let max_rpm = cam(app).map(|c| c.max_rpm).unwrap_or(24000.0);
        let pick = cam::presets().into_iter().find(|t| {
            k.accepts(t.kind)
                && match k {
                    OpKind::Face => t.kind == ToolKind::Flat && t.diameter >= 10.0,
                    OpKind::Drill => t.kind == ToolKind::Drill,
                    OpKind::Thread | OpKind::Tap => true,
                    OpKind::Chamfer | OpKind::Deburr => t.kind == ToolKind::Chamfer,
                    OpKind::Parallel | OpKind::Waterline | OpKind::Scallop => t.kind == ToolKind::Ball && t.diameter >= 6.0,
                    OpKind::Pencil => t.kind == ToolKind::Ball && t.diameter <= 3.0,
                    OpKind::Trace | OpKind::VCarve => t.kind == ToolKind::Engrave && t.angle >= 60.0,
                    _ => t.kind == ToolKind::Flat && t.diameter >= 6.0,
                }
        });
        if let Some(mut t) = pick {
            cam::suggest(&mut t, &material, max_rpm);
            edit(app, |c| {
                c.add_tool(t);
            });
        }
    }
    let mut id = 0;
    let tool = if k == OpKind::Pencil { cam(app).and_then(small).unwrap_or(0) } else { 0 };
    // tracing and carving start on the latest sketch
    let sketch = cam(app).and_then(|c| app.studio_doc(c.studio)).and_then(|d| sketch_input(d).into_iter().rev().find(|s| !s.paths.is_empty())).map(|s| s.id);
    let engrave = cam(app).and_then(|c| c.tools.iter().find(|t| t.kind == ToolKind::Engrave && k.accepts(t.kind)).map(|t| t.id));
    let tool = if matches!(k, OpKind::Trace | OpKind::VCarve) { engrave.unwrap_or(tool) } else { tool };
    edit(app, |c| {
        id = c.add_op(k, tool);
        if let (Some(s), Some(o)) = (sketch.filter(|_| matches!(k, OpKind::Trace | OpKind::VCarve)), c.op_mut(id)) {
            o.sketches = vec![s];
        }
    });
    app.cam.sel_op = Some(id);
    app.cam.sel_tool = None;
}

pub fn shortcuts(app: &mut App, ctx: &egui::Context) {
    use egui::Key;
    let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    if pressed(Key::Space) && app.cam.sim_on {
        app.cam.playing = !app.cam.playing;
    }
    if pressed(Key::Delete) || pressed(Key::Backspace) {
        if let Some(id) = app.cam.sel_op.take() {
            edit(app, |c| c.ops.retain(|o| o.id != id));
        }
    }
    if pressed(Key::F) {
        fit(app);
    }
}

fn fit(app: &mut App) {
    let Some(part) = app.cam.part.clone() else { return };
    let Some(c) = cam(app) else { return };
    let (lo, hi) = cam_paths::stock_box(c, &part);
    let mut a = DVec3::splat(f64::INFINITY);
    let mut b = DVec3::splat(f64::NEG_INFINITY);
    for k in 0..8 {
        let p = part.to_part(DVec3::new([lo.x, hi.x][k & 1], [lo.y, hi.y][(k >> 1) & 1], [lo.z, hi.z][(k >> 2) & 1]));
        a = a.min(p);
        b = b.max(p);
    }
    app.camera.fit(a.as_vec3(), b.as_vec3());
    app.cam.scene_dirty = true;
}

// ---------------------------------------------------------------- sidebar

fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new().inner_margin(Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title.to_uppercase()).color(theme::muted()).size(11.0).strong());
        ui.add_space(4.0);
        add(ui);
    });
    let r = ui.max_rect();
    let y = ui.cursor().top();
    ui.painter().hline(r.x_range(), y, Stroke::new(1.0, theme::border()));
}

fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 18.0], egui::Label::new(RichText::new(label).color(theme::muted()).size(12.0)).truncate());
        add(ui);
    });
}

fn num(ui: &mut Ui, label: &str, v: &mut f64, unit: &str, range: std::ops::RangeInclusive<f64>, speed: f64) {
    row(ui, label, |ui| {
        ui.add(egui::DragValue::new(v).range(range).speed(speed).suffix(unit).max_decimals(3));
    });
}

/// A value that can be left to the app ("auto") or set.
fn opt(ui: &mut Ui, label: &str, v: &mut Option<f64>, auto: f64, unit: &str, tip: &str) {
    row(ui, label, |ui| {
        let mut on = v.is_some();
        if ui.checkbox(&mut on, "").on_hover_text("Set it yourself (unticked: worked out from the part)").changed() {
            *v = if on { Some((auto * 1000.0).round() / 1000.0) } else { None };
        }
        match v {
            Some(x) => {
                ui.add(egui::DragValue::new(x).speed(0.1).suffix(unit).max_decimals(3));
            }
            None => {
                ui.label(RichText::new(format!("auto ({}{unit})", cam::fmt_mm(auto))).color(theme::muted()).size(12.0)).on_hover_text(tip);
            }
        }
    });
}

pub fn sidebar(app: &mut App, ui: &mut Ui) {
    let Some(before) = cam(app).cloned() else {
        ui.label("No Manufacture tab");
        return;
    };
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        section(ui, "Setup", |ui| setup_ui(app, ui));
        section(ui, "Tools", |ui| tools_ui(app, ui));
        section(ui, "Operations", |ui| ops_ui(app, ui));
        if let Some(id) = app.cam.sel_op {
            if cam(app).and_then(|c| c.op(id)).is_some() {
                section(ui, "Operation", |ui| op_params(app, ui, id));
            }
        }
        if let Some(id) = app.cam.sel_tool {
            if cam(app).and_then(|c| c.tool(id)).is_some() {
                section(ui, "Tool", |ui| tool_params(app, ui, id));
            }
        }
        if app.cam.sim.is_some() {
            section(ui, "Simulation", |ui| sim_ui(app, ui));
        }
    });
    if cam(app).map(|c| *c != before).unwrap_or(false) {
        record(app, before);
    }
}

fn setup_ui(app: &mut App, ui: &mut Ui) {
    let studios: Vec<String> = app.ws.studios.iter().map(|s| s.name.clone()).collect();
    let Some(ci) = cam_index(app) else { return };
    let studio = app.ws.cams[ci].studio.min(studios.len().saturating_sub(1));
    let bodies: Vec<(BodyId, String)> = app.studio_bodies(studio).iter().map(|b| (b.id, b.name.clone())).collect();
    // other setups of the same parts this one can carry on from (not ones that already follow it)
    let others: Vec<String> = (0..app.ws.cams.len())
        .filter(|&i| i != ci && app.ws.cams[i].studio == studio && !cam_paths::setup_chain(&app.ws.cams, i).map(|ch| ch.contains(&ci)).unwrap_or(true))
        .map(|i| app.ws.cams[i].name.clone())
        .collect();
    let mut need_build = None;
    let mut new_setup = false;
    {
        let c = &mut app.ws.cams[ci];
        row(ui, "Part Studio", |ui| {
            egui::ComboBox::from_id_salt("cam-studio").selected_text(studios.get(c.studio).cloned().unwrap_or_default()).show_ui(ui, |ui| {
                for (i, s) in studios.iter().enumerate() {
                    if ui.selectable_value(&mut c.studio, i, s).changed() {
                        c.bodies.clear();
                        need_build = Some(i);
                    }
                }
            });
        });
        if bodies.len() > 1 {
            row(ui, "Parts", |ui| {
                ui.vertical(|ui| {
                    for (id, name) in &bodies {
                        let mut on = c.bodies.is_empty() || c.bodies.contains(id);
                        if ui.checkbox(&mut on, name).changed() {
                            if c.bodies.is_empty() {
                                c.bodies = bodies.iter().map(|b| b.0).collect();
                            }
                            if on {
                                c.bodies.push(*id);
                            } else {
                                c.bodies.retain(|b| b != id);
                            }
                            if c.bodies.len() == bodies.len() {
                                c.bodies.clear();
                            }
                        }
                    }
                });
            });
        }
        row(ui, "Up direction", |ui| {
            egui::ComboBox::from_id_salt("cam-orient").selected_text(c.orient.clone()).show_ui(ui, |ui| {
                for o in cam::ORIENTS {
                    ui.selectable_value(&mut c.orient, o.to_string(), o).on_hover_text(format!("The part's {o} faces the spindle"));
                }
            });
        });
        row(ui, "Work zero", |ui| {
            egui::ComboBox::from_id_salt("cam-origin").width(150.0).selected_text(c.origin.label()).show_ui(ui, |ui| {
                for o in Origin::ALL {
                    ui.selectable_value(&mut c.origin, o, o.label());
                }
            });
        });
        row(ui, "Starts from", |ui| {
            let label = if c.follows.is_empty() { "fresh stock".to_string() } else { format!("what {} leaves", c.follows) };
            egui::ComboBox::from_id_salt("cam-follows").width(150.0).selected_text(label).show_ui(ui, |ui| {
                ui.selectable_value(&mut c.follows, String::new(), "fresh stock");
                for n in &others {
                    ui.selectable_value(&mut c.follows, n.clone(), format!("what {n} leaves")).on_hover_text("Carry on from that setup's leftover stock: flip the part and machine the other side");
                }
            });
        });
        if ui.button("New setup from here").on_hover_text("Add a setup that carries on from this one's leftover stock, with the part flipped over").clicked() {
            new_setup = true;
        }
        let mut fixed = c.stock.size.is_some();
        if !c.follows.is_empty() {
            ui.label(RichText::new(format!("The stock is what {} leaves.", c.follows)).color(theme::muted()).size(12.0));
        } else {
        row(ui, "Stock", |ui| {
            if ui.radio(!fixed, "margins").clicked() {
                fixed = false;
            }
            if ui.radio(fixed, "fixed size").clicked() {
                fixed = true;
            }
        });
        if fixed != c.stock.size.is_some() {
            c.stock.size = if fixed {
                app.cam
                    .part
                    .as_ref()
                    .map(|p| {
                        let s = p.hi - p.lo;
                        [(s.x + 2.0 * c.stock.side).ceil(), (s.y + 2.0 * c.stock.side).ceil(), (s.z + c.stock.top + c.stock.bottom).ceil()]
                    })
                    .or(Some([100.0, 100.0, 20.0]))
            } else {
                None
            };
        }
        match &mut c.stock.size {
            Some(s) => {
                row(ui, "Size X/Y/Z", |ui| {
                    for v in s.iter_mut() {
                        ui.add(egui::DragValue::new(v).range(0.1..=10000.0).speed(0.5).max_decimals(2));
                    }
                });
                num(ui, "Below part", &mut c.stock.bottom, " mm", 0.0..=1000.0, 0.1);
            }
            None => {
                num(ui, "Sides", &mut c.stock.side, " mm", 0.0..=1000.0, 0.1);
                num(ui, "Top", &mut c.stock.top, " mm", 0.0..=1000.0, 0.1);
                num(ui, "Bottom", &mut c.stock.bottom, " mm", 0.0..=1000.0, 0.1);
            }
        }
        }
        row(ui, "Material", |ui| {
            egui::ComboBox::from_id_salt("cam-material").selected_text(c.stock.material.clone()).show_ui(ui, |ui| {
                for m in cam::MATERIALS {
                    ui.selectable_value(&mut c.stock.material, m.0.to_string(), m.0);
                }
            });
        });
        num(ui, "Clearance", &mut c.clearance, " mm", 1.0..=500.0, 0.5);
        num(ui, "Retract", &mut c.retract, " mm", 0.1..=100.0, 0.1);
        ui.add_space(4.0);
        ui.label(RichText::new("FIXTURES").color(theme::muted()).size(11.0).strong())
            .on_hover_text("Clamps, vise jaws and screws as boxes in work coordinates: toolpaths lift over them and the simulation flags any hit");
        let mut gone = None;
        for (k, f) in c.fixtures.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(140.0));
                if ui.small_button("✕").on_hover_text("Remove this fixture").clicked() {
                    gone = Some(k);
                }
            });
            row(ui, "From X/Y/Z", |ui| {
                for v in f.min.iter_mut() {
                    ui.add(egui::DragValue::new(v).speed(0.5).max_decimals(2));
                }
            });
            row(ui, "To X/Y/Z", |ui| {
                for v in f.max.iter_mut() {
                    ui.add(egui::DragValue::new(v).speed(0.5).max_decimals(2));
                }
            });
        }
        if let Some(k) = gone {
            c.fixtures.remove(k);
        }
        let mut add = None;
        egui::ComboBox::from_id_salt("cam-fixture-add").selected_text("Add a fixture").width(150.0).show_ui(ui, |ui| {
            for (n, size) in cam::FIXTURES {
                if ui.selectable_label(false, n).clicked() {
                    add = Some((n, size));
                }
            }
        });
        if let Some((n, size)) = add {
            c.fixtures.push(match &app.cam.part {
                Some(p) => cam::place_fixture(n, size, cam_paths::stock_box(c, p), cam_paths::work_origin(c, p), c.fixtures.len()),
                None => cam::Fixture { name: n.into(), min: [0.0; 3], max: size },
            });
        }
        ui.add_space(4.0);
        ui.label(RichText::new("MACHINE").color(theme::muted()).size(11.0).strong());
        row(ui, "Machine", |ui| {
            let label = if c.machine.name.is_empty() { "custom".to_string() } else { c.machine.name.clone() };
            egui::ComboBox::from_id_salt("cam-machine").width(150.0).selected_text(label).show_ui(ui, |ui| {
                for m in cam::MACHINES {
                    if ui.selectable_label(c.machine.name == m.0, m.0).clicked() {
                        cam::set_machine(c, m.0);
                    }
                }
            });
        });
        let before = (c.machine.clone(), c.max_rpm, c.rapid_rate);
        row(ui, "Controller", |ui| {
            egui::ComboBox::from_id_salt("cam-post").selected_text(cam::POSTS.iter().find(|p| p.0 == c.post).map(|p| p.1).unwrap_or("GRBL")).show_ui(ui, |ui| {
                for (k, l) in cam::POSTS {
                    ui.selectable_value(&mut c.post, k.to_string(), l);
                }
            });
        });
        row(ui, "Travel X/Y/Z", |ui| {
            for v in c.machine.travel.iter_mut() {
                ui.add(egui::DragValue::new(v).range(0.0..=10000.0).speed(1.0).max_decimals(1)).on_hover_text("0 = not checked");
            }
        });
        num(ui, "Max spindle", &mut c.max_rpm, " rpm", 1000.0..=100000.0, 100.0);
        num(ui, "Max feed", &mut c.machine.max_feed, " mm/min", 0.0..=100000.0, 50.0);
        num(ui, "Rapid X/Y", &mut c.rapid_rate, " mm/min", 100.0..=100000.0, 50.0);
        num(ui, "Rapid Z", &mut c.machine.rapid_z, " mm/min", 0.0..=100000.0, 50.0);
        ui.checkbox(&mut c.machine.tool_changer, "Automatic tool changer").on_hover_text("Without one the program stops (M0) for you to change tools");
        ui.checkbox(&mut c.arcs, "Arcs in the G-code (G2/G3)").on_hover_text("Write arcs where the toolpath runs along circles: shorter files and smoother motion than many short lines");
        row(ui, "Tool probing", |ui| {
            let label = |m: &str| match m {
                "plate" => "touch plate",
                "setter" => "tool setter",
                _ => "off",
            };
            egui::ComboBox::from_id_salt("cam-probe").selected_text(label(&c.probe.mode)).show_ui(ui, |ui| {
                ui.selectable_value(&mut c.probe.mode, "off".to_string(), "off");
                ui.selectable_value(&mut c.probe.mode, "plate".to_string(), "touch plate")
                    .on_hover_text("After each tool is loaded, probe down onto a touch plate on the work and set the work Z zero from it");
                ui.selectable_value(&mut c.probe.mode, "setter".to_string(), "tool setter").on_hover_text(
                    "After each tool is loaded, measure it on a fixed tool setter and set its length offset against the first tool (set the work Z zero with the first tool). Needs LinuxCNC, Mach3 or Fanuc",
                );
            });
        });
        match c.probe.mode.as_str() {
            "plate" => {
                row(ui, "Plate at X/Y/Z", |ui| {
                    for v in c.probe.plate_at.iter_mut() {
                        ui.add(egui::DragValue::new(v).speed(0.5).max_decimals(2)).on_hover_text("Where the plate lies, in work coordinates; Z is the surface under it (0 = stock top with the default work zero)");
                    }
                });
                num(ui, "Plate thickness", &mut c.probe.plate_thickness, " mm", 0.0..=100.0, 0.05);
            }
            "setter" => {
                if c.post == "grbl" {
                    ui.label(RichText::new("GRBL can't keep a setter measurement: choose another controller or a touch plate.").color(theme::muted()).size(12.0));
                }
                row(ui, "Setter X/Y/Z", |ui| {
                    for v in c.probe.setter.iter_mut() {
                        ui.add(egui::DragValue::new(v).speed(0.5).max_decimals(2)).on_hover_text("Machine coordinates (G53) above the setter; Z is where probing starts");
                    }
                });
            }
            _ => {}
        }
        if c.probe.mode != "off" {
            num(ui, "Probe travel", &mut c.probe.travel, " mm", 1.0..=1000.0, 1.0);
            num(ui, "Probe feed", &mut c.probe.fast, " mm/min", 1.0..=5000.0, 5.0);
            num(ui, "Second touch", &mut c.probe.slow, " mm/min", 1.0..=1000.0, 1.0);
        }
        ui.collapsing("Custom G-code", |ui| {
            ui.label(RichText::new("Placeholders: {tool} {name} {desc} {diameter} {rpm} {program} {setup}").color(theme::muted()).size(11.5));
            for (label, text, hint) in [
                ("Start", &mut c.post_text.header, "after the units and work offset"),
                ("Tool change", &mut c.post_text.tool_change, "replaces the standard tool change, e.g. M0 / T{tool} M6"),
                ("End", &mut c.post_text.footer, "before M30, e.g. park the spindle"),
            ] {
                ui.label(label);
                ui.add(egui::TextEdit::multiline(text).code_editor().desired_rows(2).desired_width(f32::INFINITY).hint_text(hint));
            }
        });
        // hand edits make it a custom machine
        if (c.machine.clone(), c.max_rpm, c.rapid_rate) != before {
            c.machine.name.clear();
        }
    }
    if new_setup {
        let c = app.ws.cams[ci].clone();
        let flip = match c.orient.as_str() {
            "top" => "bottom",
            "bottom" => "top",
            "front" => "back",
            "back" => "front",
            "left" => "right",
            _ => "left",
        };
        let n = app.ws.cams.len() + 1;
        let mut name = format!("{} side 2", c.name);
        while app.ws.cams.iter().any(|x| x.name == name) {
            name = format!("Manufacture {n}");
        }
        let next = Cam { name, follows: c.name.clone(), orient: flip.into(), ops: vec![], stock: cam::Stock { carried: None, ..c.stock.clone() }, ..c };
        app.ws.cams.push(next);
        let t = Tab::Cam(app.ws.cams.len() - 1);
        app.switch_tab(t);
        return;
    }
    if let Some(i) = need_build {
        ensure_built(app, i);
    }
    if app.studio_bodies(studio).is_empty() {
        ui.label(RichText::new("This Part Studio has no parts yet").color(theme::error_soft()));
    }
}

/// Make sure a Part Studio's parts have been built.
pub fn ensure_built(app: &mut App, studio: usize) {
    if studio != app.cur_studio && !app.studio_results.contains_key(&studio) && studio < app.ws.studios.len() {
        let d = app.ws.studios[studio].doc.clone();
        let docs = app.studio_docs();
        app.kernel.set_studios(docs);
        app.kernel.rebuild(studio, &d);
    }
}

fn kind_icon(k: OpKind) -> &'static str {
    match k {
        OpKind::Face => "camFace",
        OpKind::Clear => "camClear",
        OpKind::Pocket => "camPocket",
        OpKind::Contour => "camContour",
        OpKind::Chamfer => "chamfer",
        OpKind::Drill => "camDrill",
        OpKind::Bore => "hole",
        OpKind::Thread => "camThread",
        OpKind::Tap => "camTap",
        OpKind::Adaptive => "camAdaptive",
        OpKind::Parallel => "camParallel",
        OpKind::Waterline => "camWaterline",
        OpKind::Scallop => "camScallop",
        OpKind::Deburr => "camDeburr",
        OpKind::Pencil => "camPencil",
        OpKind::Trace => "camTrace",
        OpKind::VCarve => "camVCarve",
    }
}

fn tools_ui(app: &mut App, ui: &mut Ui) {
    let Some(c) = cam(app) else { return };
    let tools: Vec<(u64, u32, String, ToolKind)> = c.tools.iter().map(|t| (t.id, t.number, t.name.clone(), t.kind)).collect();
    if tools.is_empty() {
        ui.label(RichText::new("No tools yet: open the tool library, or add an operation and a suitable tool comes with it.").color(theme::muted()).size(12.0));
    }
    let mut delete = None;
    for (id, n, name, _) in &tools {
        let sel = app.cam.sel_tool == Some(*id);
        ui.horizontal(|ui| {
            let r = ui.add(egui::Button::selectable(sel, RichText::new(format!("T{n}  {name}")).size(12.5)).truncate());
            if r.clicked() {
                app.cam.sel_tool = if sel { None } else { Some(*id) };
                app.cam.sel_op = None;
            }
            r.context_menu(|ui| {
                if ui.button("Save to my library").clicked() {
                    if let Some(t) = cam(app).and_then(|c| c.tool(*id)).cloned() {
                        let mut lib = cam::load_library();
                        lib.retain(|x| x.name != t.name);
                        lib.push(t);
                        cam::save_library(&lib);
                        app.flash("Saved to your tool library", ToastKind::Info);
                    }
                    ui.close();
                }
                if ui.button(RichText::new("Delete").color(theme::error_soft())).clicked() {
                    delete = Some(*id);
                    ui.close();
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::image(icons::image("delete", 14.0).tint(theme::muted())).frame(false)).on_hover_text("Delete tool").clicked() {
                    delete = Some(*id);
                }
            });
        });
    }
    if let Some(id) = delete {
        edit(app, |c| c.tools.retain(|t| t.id != id));
        if app.cam.sel_tool == Some(id) {
            app.cam.sel_tool = None;
        }
    }
    ui.horizontal(|ui| {
        if ui.button("Library…").clicked() {
            app.cam.library_open = true;
        }
        ui.menu_button("New tool", |ui| {
            for k in ToolKind::ALL {
                if ui.button(k.label()).clicked() {
                    let (material, max_rpm) = cam(app).map(|c| (c.stock.material.clone(), c.max_rpm)).unwrap_or_default();
                    let mut t = cam::presets().into_iter().find(|t| t.kind == k).unwrap_or(Tool { kind: k, ..Default::default() });
                    cam::suggest(&mut t, &material, max_rpm);
                    let mut id = 0;
                    edit(app, |c| id = c.add_tool(t));
                    app.cam.sel_tool = Some(id);
                    app.cam.sel_op = None;
                    ui.close();
                }
            }
        });
    });
}

fn fmt_time(s: f64) -> String {
    let s = s.round() as i64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn ops_ui(app: &mut App, ui: &mut Ui) {
    let Some(c) = cam(app) else { return };
    let ops: Vec<(u64, OpKind, String, bool, String)> =
        c.ops.iter().map(|o| (o.id, o.kind, o.name.clone(), o.suppressed, c.tool(o.tool).map(|t| format!("T{}", t.number)).unwrap_or("no tool".into()))).collect();
    if ops.is_empty() {
        ui.label(RichText::new("Add operations from the toolbar: face, clear, pocket, contour, chamfer, drill, bore, thread, tap.").color(theme::muted()).size(12.0));
    }
    let mut action: Option<(&str, u64)> = None;
    let n = ops.len();
    for (i, (id, kind, name, sup, tool)) in ops.iter().enumerate() {
        let tp = app.cam.paths.iter().find(|p| p.op == *id);
        let sel = app.cam.sel_op == Some(*id);
        ui.horizontal(|ui| {
            ui.add(icons::image(kind_icon(*kind), 15.0).tint(if *sup { theme::muted() } else { theme::text() }));
            let mut text = RichText::new(format!("{name} · {tool}")).size(12.5);
            if *sup {
                text = text.color(theme::muted()).strikethrough();
            }
            let r = ui.add(egui::Button::selectable(sel, text).truncate());
            if r.clicked() {
                app.cam.sel_op = if sel { None } else { Some(*id) };
                app.cam.sel_tool = None;
                app.cam.scene_dirty = true;
            }
            r.context_menu(|ui| {
                if ui.button(if *sup { "Unsuppress" } else { "Suppress" }).clicked() {
                    action = Some(("suppress", *id));
                    ui.close();
                }
                if ui.button("Duplicate").clicked() {
                    action = Some(("duplicate", *id));
                    ui.close();
                }
                if ui.button(RichText::new("Delete").color(theme::error_soft())).clicked() {
                    action = Some(("delete", *id));
                    ui.close();
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(i + 1 < n, egui::Button::image(icons::image("down", 12.0).tint(theme::muted())).frame(false)).on_hover_text("Move down").clicked() {
                    action = Some(("down", *id));
                }
                if ui.add_enabled(i > 0, egui::Button::image(icons::image("up", 12.0).tint(theme::muted())).frame(false)).on_hover_text("Move up").clicked() {
                    action = Some(("up", *id));
                }
                if let Some(tp) = tp {
                    if !tp.warnings.is_empty() {
                        ui.add(icons::image("warning", 14.0).tint(theme::error_soft())).on_hover_text(tp.warnings.join("\n"));
                    }
                    ui.label(RichText::new(fmt_time(tp.time)).color(theme::muted()).size(11.5));
                }
            });
        });
    }
    if let Some((a, id)) = action {
        edit(app, |c| {
            let Some(i) = c.ops.iter().position(|o| o.id == id) else { return };
            match a {
                "suppress" => c.ops[i].suppressed = !c.ops[i].suppressed,
                "delete" => {
                    c.ops.remove(i);
                }
                "duplicate" => {
                    let mut o = c.ops[i].clone();
                    o.id = c.new_id();
                    o.name = format!("{} copy", o.name);
                    c.ops.insert(i + 1, o);
                }
                "up" if i > 0 => c.ops.swap(i, i - 1),
                "down" if i + 1 < c.ops.len() => c.ops.swap(i, i + 1),
                _ => {}
            }
        });
    }
    templates_ui(app, ui);
    if !app.cam.paths.is_empty() || app.cam.computing {
        let total: f64 = app.cam.paths.iter().map(|p| p.time).sum();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Cycle time {}", fmt_time(total))).strong().size(12.5));
            if app.cam.computing {
                ui.spinner();
                ui.label(RichText::new("updating").color(theme::muted()).size(12.0));
            }
        });
    }
}

/// Save the setup's operations as a template, or add a saved template's operations.
fn templates_ui(app: &mut App, ui: &mut Ui) {
    let Some(has_ops) = cam(app).map(|c| c.ops.iter().any(|o| !o.suppressed)) else { return };
    let mut save = false;
    let mut add = None;
    let mut gone = None;
    ui.add_space(2.0);
    ui.menu_button(RichText::new("Templates").size(12.5), |ui| {
        ui.set_min_width(240.0);
        ui.label(RichText::new("SAVE THESE OPERATIONS").color(theme::muted()).size(11.0).strong());
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut app.cam.tpl_name).hint_text("Template name").desired_width(150.0));
            if ui.add_enabled(has_ops && !app.cam.tpl_name.trim().is_empty(), egui::Button::new("Save")).on_hover_text("The operations that aren't suppressed and their tools; heights and picked holes, points, edges and sketches are left out").clicked() {
                save = true;
                ui.close();
            }
        });
        ui.separator();
        let all = crate::cam_template::list();
        if all.is_empty() {
            ui.label(RichText::new("No templates saved yet").color(theme::muted()).size(12.0));
        }
        for t in &all {
            ui.horizontal(|ui| {
                if ui.button(&t.name).on_hover_text(t.ops.iter().map(|o| o.name.as_str()).collect::<Vec<_>>().join(", ")).clicked() {
                    add = Some(t.clone());
                    ui.close();
                }
                ui.label(RichText::new(format!("{} ops", t.ops.len())).color(theme::muted()).size(11.5));
                if ui.small_button("✕").on_hover_text("Delete this template").clicked() {
                    gone = Some(t.name.clone());
                }
            });
        }
    });
    if save {
        let Some(c) = cam(app) else { return };
        let t = crate::cam_template::from_cam(c, &app.cam.tpl_name, &[]);
        match crate::cam_template::save(&t) {
            Ok(_) => app.flash_info(format!("Saved template \"{}\"", t.name), Some(std::time::Duration::from_secs(2))),
            Err(e) => app.flash(format!("Could not save the template: {e}"), ToastKind::Error),
        }
    }
    if let Some(name) = gone {
        if let Err(e) = crate::cam_template::delete(&name) {
            app.flash(format!("Could not delete the template: {e}"), ToastKind::Error);
        }
    }
    if let Some(t) = add {
        let sketch = cam(app).and_then(|c| app.studio_doc(c.studio)).and_then(|d| sketch_input(d).into_iter().rev().find(|s| !s.paths.is_empty())).map(|s| s.id);
        let mut ids = vec![];
        edit(app, |c| ids = crate::cam_template::apply(c, &t, sketch));
        app.cam.sel_op = ids.first().copied();
        app.cam.sel_tool = None;
        app.flash_info(format!("Added {} operation{} from \"{}\"", ids.len(), if ids.len() == 1 { "" } else { "s" }, t.name), Some(std::time::Duration::from_secs(2)));
    }
}

fn tool_combo(ui: &mut Ui, c: &Cam, kind: OpKind, tool: &mut u64) {
    let cur = c.tool(*tool).map(|t| format!("T{} {}", t.number, t.name)).unwrap_or("choose a tool".into());
    egui::ComboBox::from_id_salt("cam-op-tool").width(170.0).selected_text(cur).show_ui(ui, |ui| {
        for t in c.tools.iter().filter(|t| kind.accepts(t.kind)) {
            ui.selectable_value(tool, t.id, format!("T{} {}", t.number, t.name));
        }
    });
}

fn stepover_row(ui: &mut Ui, label: &str, v: &mut Option<f64>, auto: f64) {
    row(ui, label, |ui| {
        let mut on = v.is_some();
        if ui.checkbox(&mut on, "").changed() {
            *v = if on { Some(auto) } else { None };
        }
        match v {
            Some(v) => {
                let mut pct = *v * 100.0;
                if ui.add(egui::DragValue::new(&mut pct).range(1.0..=100.0).suffix(" % of Ø")).changed() {
                    *v = pct / 100.0;
                }
            }
            None => {
                ui.label(RichText::new(format!("auto ({:.0} % of Ø)", auto * 100.0)).color(theme::muted()).size(12.0));
            }
        }
    });
}

fn op_params(app: &mut App, ui: &mut Ui, id: u64) {
    let warnings: Vec<String> = app.cam.paths.iter().find(|p| p.op == id).map(|p| p.warnings.clone()).unwrap_or_default();
    let stats = app.cam.paths.iter().find(|p| p.op == id).map(|p| (p.time, p.cut_length));
    let origin = match (&app.cam.part, cam(app)) {
        (Some(p), Some(c)) => cam_paths::work_origin(c, p),
        _ => DVec3::ZERO,
    };
    let (stock_top, part_top, part_bottom) = match (&app.cam.part, cam(app)) {
        (Some(p), Some(c)) => (cam_paths::stock_box(c, p).1.z - origin.z, p.hi.z - origin.z, p.lo.z - origin.z),
        _ => (0.0, 0.0, 0.0),
    };
    let Some(ci) = cam_index(app) else { return };
    let snapshot = app.ws.cams[ci].clone();
    let Some(oi) = snapshot.ops.iter().position(|o| o.id == id) else { return };
    let sketches: Vec<(u64, String)> = app.studio_doc(snapshot.studio).map(sketch_input).unwrap_or_default().into_iter().map(|s| (s.id, s.name)).collect();
    let mut o: Operation = snapshot.ops[oi].clone();
    let tool = snapshot.tool(o.tool).cloned();
    ui.label(RichText::new(o.kind.description()).color(theme::muted()).size(12.0));
    ui.add_space(4.0);
    row(ui, "Name", |ui| {
        ui.text_edit_singleline(&mut o.name);
    });
    row(ui, "Tool", |ui| tool_combo(ui, &snapshot, o.kind, &mut o.tool));
    if snapshot.tools.iter().all(|t| !o.kind.accepts(t.kind)) {
        ui.label(RichText::new("No suitable tool yet: add one from the library.").color(theme::error_soft()).size(12.0));
    }
    let (sd, so) = tool.as_ref().map(|t| (t.stepdown, t.stepover)).unwrap_or((1.0, 0.4));
    match o.kind {
        OpKind::Face | OpKind::Clear | OpKind::Pocket | OpKind::Contour => {
            opt(ui, "Top", &mut o.top, stock_top, " mm", "the top of the stock");
            let auto_bottom = match o.kind {
                OpKind::Face => part_top,
                _ => part_bottom,
            };
            opt(ui, "Bottom", &mut o.bottom, auto_bottom, " mm", "found from the part");
            if tool.as_ref().map(|t| t.kind != ToolKind::Slot).unwrap_or(true) {
                opt(ui, "Stepdown", &mut o.stepdown, sd, " mm", "the tool's stepdown");
            }
            if o.kind != OpKind::Contour {
                stepover_row(ui, "Stepover", &mut o.stepover, so);
            }
            if o.kind != OpKind::Face {
                num(ui, "Leave on walls", &mut o.leave, " mm", -5.0..=50.0, 0.05);
            }
            num(ui, "Leave on floors", &mut o.leave_floor, " mm", 0.0..=50.0, 0.05);
            if o.kind == OpKind::Contour {
                row(ui, "Walls", |ui| {
                    for (s, l) in [(Side::Outside, "outside"), (Side::Inside, "inside"), (Side::Both, "both")] {
                        ui.radio_value(&mut o.side, s, l);
                    }
                });
                if tool.as_ref().map(|t| t.kind == ToolKind::Slot).unwrap_or(false) {
                    num(ui, "Slot depth", &mut o.slot_depth, " mm", 0.0..=100.0, 0.1);
                } else {
                    row(ui, "Tabs", |ui| {
                        let mut n = o.tabs as i32;
                        if ui.add(egui::DragValue::new(&mut n).range(0..=32)).changed() {
                            o.tabs = n as u32;
                        }
                    });
                    if o.tabs > 0 {
                        num(ui, "Tab width", &mut o.tab_width, " mm", 0.5..=100.0, 0.1);
                        num(ui, "Tab height", &mut o.tab_height, " mm", 0.1..=100.0, 0.1);
                    }
                    num(ui, "Breakthrough", &mut o.breakthrough, " mm", 0.0..=10.0, 0.05);
                    row(ui, "Lead in/out", |ui| {
                        let mut on = o.lead > 0.0;
                        let tip = "Arc onto the wall and off it again instead of ramping along it (leaves no mark where the cut starts)";
                        if ui.checkbox(&mut on, "").on_hover_text(tip).changed() {
                            o.lead = if on { tool.as_ref().map(|t| (t.diameter * 0.5 * 10.0).round() / 10.0).unwrap_or(3.0) } else { 0.0 };
                        }
                        if on {
                            ui.add(egui::DragValue::new(&mut o.lead).range(0.1..=100.0).speed(0.05).prefix("R ").suffix(" mm"));
                        } else {
                            ui.label(RichText::new("ramp along the wall").color(theme::muted()).size(12.0)).on_hover_text(tip);
                        }
                    });
                }
            }
            if o.kind == OpKind::Pocket {
                row(ui, "Only at", |ui| {
                    let mut on = o.at.is_some();
                    if ui.checkbox(&mut on, "").on_hover_text("Only the pocket around this point (work coordinates)").changed() {
                        o.at = if on { Some([0.0, 0.0]) } else { None };
                    }
                    if let Some(p) = &mut o.at {
                        ui.add(egui::DragValue::new(&mut p[0]).prefix("X ").speed(0.5));
                        ui.add(egui::DragValue::new(&mut p[1]).prefix("Y ").speed(0.5));
                    } else {
                        ui.label(RichText::new("all pockets").color(theme::muted()).size(12.0));
                    }
                });
            }
            if o.kind.uses_stock() {
                rest_row(ui, &mut o.rest);
            }
            if o.kind != OpKind::Face {
                num(ui, "Ramp angle", &mut o.ramp_angle, "°", 0.5..=90.0, 0.1);
                row(ui, "Direction", |ui| {
                    ui.radio_value(&mut o.climb, true, "climb");
                    ui.radio_value(&mut o.climb, false, "conventional");
                });
            }
        }
        OpKind::Adaptive => {
            opt(ui, "Top", &mut o.top, stock_top, " mm", "the top of the stock");
            opt(ui, "Bottom", &mut o.bottom, part_bottom, " mm", "the bottom of the part");
            opt(ui, "Stepdown", &mut o.stepdown, sd, " mm", "the tool's stepdown");
            stepover_row(ui, "Optimal load", &mut o.stepover, so);
            row(ui, "Loop radius", |ui| {
                let mut on = o.trochoid > 0.0;
                let auto = tool.as_ref().map(|t| t.diameter * 0.4).unwrap_or(2.0);
                if ui.checkbox(&mut on, "").on_hover_text("Radius of the cycloidal loops").changed() {
                    o.trochoid = if on { (auto * 10.0).round() / 10.0 } else { 0.0 };
                }
                if on {
                    ui.add(egui::DragValue::new(&mut o.trochoid).range(0.05..=100.0).speed(0.05).suffix(" mm"));
                } else {
                    ui.label(RichText::new(format!("auto ({} mm)", cam::fmt_mm(auto))).color(theme::muted()).size(12.0));
                }
            });
            num(ui, "Leave on walls", &mut o.leave, " mm", -5.0..=50.0, 0.05);
            num(ui, "Leave on floors", &mut o.leave_floor, " mm", 0.0..=50.0, 0.05);
            num(ui, "Helix angle", &mut o.ramp_angle, "°", 0.5..=45.0, 0.1);
            rest_row(ui, &mut o.rest);
            row(ui, "Loops", |ui| {
                ui.radio_value(&mut o.climb, true, "climb");
                ui.radio_value(&mut o.climb, false, "conventional");
            });
        }
        OpKind::Parallel | OpKind::Waterline | OpKind::Scallop => {
            if o.kind == OpKind::Waterline {
                opt(ui, "Top", &mut o.top, part_top, " mm", "the top of the part");
                opt(ui, "Stepdown", &mut o.stepdown, sd, " mm", "the tool's stepdown");
            } else {
                stepover_row(ui, "Stepover", &mut o.stepover, so);
            }
            opt(ui, "Lowest", &mut o.bottom, part_bottom, " mm", "the bottom of the part");
            if o.kind == OpKind::Parallel {
                num(ui, "Angle", &mut o.angle, "°", -180.0..=180.0, 1.0);
            }
            row(ui, "Slopes", |ui| {
                let tip = "Only where the surface slopes between these angles from horizontal: e.g. 0–45° for parallel and 45–90° for waterline";
                ui.add(egui::DragValue::new(&mut o.slope_min).range(0.0..=90.0).speed(0.5).suffix("°")).on_hover_text(tip);
                ui.label("to");
                ui.add(egui::DragValue::new(&mut o.slope_max).range(0.0..=90.0).speed(0.5).suffix("°")).on_hover_text(tip);
            });
            num(ui, "Leave", &mut o.leave, " mm", -1.0..=10.0, 0.02);
            rest_row(ui, &mut o.rest);
            if o.kind != OpKind::Parallel {
                row(ui, "Direction", |ui| {
                    ui.radio_value(&mut o.climb, true, "climb");
                    ui.radio_value(&mut o.climb, false, "conventional");
                });
            }
        }
        OpKind::Pencil => {
            row(ui, "Bigger tool", |ui| {
                let mut on = o.ref_diameter > 0.0;
                let tip = "The tool whose leftovers to clean up: a ball end mill of this diameter, or else the smallest bigger tool used before";
                if ui.checkbox(&mut on, "").on_hover_text(tip).changed() {
                    o.ref_diameter = if on { tool.as_ref().map(|t| (t.diameter * 2.0).max(1.0)).unwrap_or(6.0) } else { 0.0 };
                }
                if on {
                    ui.add(egui::DragValue::new(&mut o.ref_diameter).range(0.1..=100.0).speed(0.05).prefix("Ø").suffix(" ball"));
                } else {
                    ui.label(RichText::new("the one used before").color(theme::muted()).size(12.0)).on_hover_text(tip);
                }
            });
            stepover_row(ui, "Stepover", &mut o.stepover, so);
            opt(ui, "Lowest", &mut o.bottom, part_bottom, " mm", "the bottom of the part");
            row(ui, "Slopes", |ui| {
                ui.add(egui::DragValue::new(&mut o.slope_min).range(0.0..=90.0).speed(0.5).suffix("°"));
                ui.label("to");
                ui.add(egui::DragValue::new(&mut o.slope_max).range(0.0..=90.0).speed(0.5).suffix("°"));
            });
            num(ui, "Leave", &mut o.leave, " mm", -1.0..=10.0, 0.02);
            rest_row(ui, &mut o.rest);
            row(ui, "Direction", |ui| {
                ui.radio_value(&mut o.climb, true, "climb");
                ui.radio_value(&mut o.climb, false, "conventional");
            });
        }
        OpKind::Trace | OpKind::VCarve => {
            row(ui, "Sketches", |ui| {
                ui.vertical(|ui| {
                    if sketches.is_empty() {
                        ui.label(RichText::new("the Part Studio has no sketches").color(theme::muted()).size(12.0));
                    }
                    for (sid, name) in &sketches {
                        let mut on = o.sketches.contains(sid);
                        if ui.checkbox(&mut on, name).changed() {
                            if on {
                                o.sketches.push(*sid);
                            } else {
                                o.sketches.retain(|x| x != sid);
                            }
                        }
                    }
                });
            });
            if o.kind == OpKind::Trace {
                num(ui, "Depth", &mut o.depth, " mm", 0.0..=200.0, 0.05);
                opt(ui, "Stepdown", &mut o.stepdown, sd, " mm", "the tool's stepdown");
                row(ui, "Closed shapes", |ui| {
                    for (s, l) in [(Side::Both, "on the line"), (Side::Outside, "outside"), (Side::Inside, "inside")] {
                        ui.radio_value(&mut o.side, s, l);
                    }
                });
                if o.side != Side::Both {
                    num(ui, "Leave", &mut o.leave, " mm", -5.0..=50.0, 0.05);
                }
            } else {
                row(ui, "Deepest", |ui| {
                    let mut on = o.depth > 0.0;
                    let tip = "Stop at this depth: wider parts get a flat (ridged) floor";
                    if ui.checkbox(&mut on, "").on_hover_text(tip).changed() {
                        o.depth = if on { 1.0 } else { 0.0 };
                    }
                    if on {
                        ui.add(egui::DragValue::new(&mut o.depth).range(0.01..=100.0).speed(0.05).suffix(" mm"));
                    } else {
                        ui.label(RichText::new("as deep as the V-bit goes").color(theme::muted()).size(12.0)).on_hover_text(tip);
                    }
                });
                stepover_row(ui, "Stepover", &mut o.stepover, so);
            }
            row(ui, "Direction", |ui| {
                ui.radio_value(&mut o.climb, true, "climb");
                ui.radio_value(&mut o.climb, false, "conventional");
            });
        }
        OpKind::Deburr => {
            num(ui, "Chamfer", &mut o.chamfer, " mm", 0.0..=20.0, 0.05);
            num(ui, "Sharper than", &mut o.edge_angle, "°", 5.0..=170.0, 1.0);
            row(ui, "Edges", |ui| {
                if o.edges.is_empty() {
                    ui.label(RichText::new("every sharp edge").color(theme::muted()).size(12.0));
                } else {
                    ui.label(format!("{} picked", o.edges.len()));
                    if ui.small_button("All edges").clicked() {
                        o.edges.clear();
                    }
                }
            });
            let picking = app.cam.pick_edges == Some(id);
            if ui.selectable_label(picking, if picking { "Click edges in the view… (done)" } else { "Pick edges" }).on_hover_text("Click near edges in the view to chamfer only those").clicked() {
                app.cam.pick_edges = if picking { None } else { Some(id) };
            }
            row(ui, "Direction", |ui| {
                ui.radio_value(&mut o.climb, true, "climb");
                ui.radio_value(&mut o.climb, false, "conventional");
            });
        }
        OpKind::Chamfer => {
            num(ui, "Chamfer", &mut o.chamfer, " mm", 0.0..=50.0, 0.05);
            opt(ui, "Edge height", &mut o.top, part_top, " mm", "the top of the part");
            row(ui, "Edges", |ui| {
                for (s, l) in [(Side::Outside, "outside"), (Side::Inside, "inside"), (Side::Both, "both")] {
                    ui.radio_value(&mut o.side, s, l);
                }
            });
            row(ui, "Direction", |ui| {
                ui.radio_value(&mut o.climb, true, "climb");
                ui.radio_value(&mut o.climb, false, "conventional");
            });
        }
        OpKind::Drill | OpKind::Bore | OpKind::Thread | OpKind::Tap => {
            row(ui, "Hole Ø from", |ui| {
                let mut on = o.hole_min.is_some() || o.hole_max.is_some();
                if ui.checkbox(&mut on, "").on_hover_text("Only holes in this diameter range (unticked: the sizes this tool suits)").changed() {
                    let d = tool.as_ref().map(|t| t.diameter).unwrap_or(5.0);
                    (o.hole_min, o.hole_max) = if on { (Some(d - 0.1), Some(d + 0.5)) } else { (None, None) };
                }
                match (&mut o.hole_min, &mut o.hole_max) {
                    (Some(a), Some(b)) => {
                        ui.add(egui::DragValue::new(a).speed(0.05).max_decimals(2));
                        ui.label("to");
                        ui.add(egui::DragValue::new(b).speed(0.05).max_decimals(2));
                    }
                    _ => {
                        ui.label(RichText::new("suited to the tool").color(theme::muted()).size(12.0));
                    }
                }
            });
            opt(ui, "Bottom", &mut o.bottom, part_bottom, " mm", "each hole's own bottom");
            match o.kind {
                OpKind::Drill => {
                    num(ui, "Peck", &mut o.peck, " mm", 0.0..=100.0, 0.1);
                    num(ui, "Dwell", &mut o.dwell, " s", 0.0..=10.0, 0.05);
                    if tool.as_ref().map(|t| matches!(t.kind, ToolKind::SpotDrill | ToolKind::CenterDrill)).unwrap_or(false) {
                        num(ui, "Chamfer", &mut o.chamfer, " mm", 0.0..=10.0, 0.05);
                    }
                }
                OpKind::Bore => {
                    opt(ui, "Pitch", &mut o.stepdown, sd, " mm", "the tool's stepdown (limited by the ramp angle)");
                    num(ui, "Leave on walls", &mut o.leave, " mm", -1.0..=10.0, 0.02);
                    num(ui, "Ramp angle", &mut o.ramp_angle, "°", 0.5..=45.0, 0.1);
                }
                OpKind::Thread => {
                    opt(ui, "Thread Ø", &mut o.thread_diameter, 6.0, " mm", "from the hole size (ISO metric coarse)");
                    opt(ui, "Pitch", &mut o.pitch, tool.as_ref().map(|t| t.pitch).unwrap_or(1.0), " mm", "the tool's pitch");
                    row(ui, "Hand", |ui| {
                        ui.radio_value(&mut o.left_hand, false, "right");
                        ui.radio_value(&mut o.left_hand, true, "left");
                    });
                }
                _ => {}
            }
            num(ui, "Breakthrough", &mut o.breakthrough, " mm", 0.0..=10.0, 0.05);
            ui.checkbox(&mut o.coolant, "Coolant");
        }
    }
    if let Some((t, l)) = stats {
        ui.label(RichText::new(format!("{} · {:.0} mm of cutting", fmt_time(t), l)).color(theme::muted()).size(12.0));
    }
    for w in &warnings {
        ui.label(RichText::new(format!("⚠ {w}")).color(theme::error_soft()).size(12.0));
    }
    if o != snapshot.ops[oi] {
        app.ws.cams[ci].ops[oi] = o;
        app.cam.scene_dirty = true;
    }
}

fn rest_row(ui: &mut Ui, rest: &mut bool) {
    row(ui, "Rest", |ui| {
        ui.checkbox(rest, "only what's left").on_hover_text("Machine only the material the operations before it (and earlier setups) left; skips cutting air");
    });
}

fn tool_params(app: &mut App, ui: &mut Ui, id: u64) {
    let Some(ci) = cam_index(app) else { return };
    let material = app.ws.cams[ci].stock.material.clone();
    let max_rpm = app.ws.cams[ci].max_rpm;
    let Some(t) = app.ws.cams[ci].tools.iter_mut().find(|t| t.id == id) else { return };
    ui.label(RichText::new(t.describe()).color(theme::muted()).size(12.0));
    tool_preview(ui, t);
    row(ui, "Name", |ui| {
        ui.text_edit_singleline(&mut t.name);
    });
    row(ui, "Number", |ui| {
        ui.add(egui::DragValue::new(&mut t.number).range(1..=999).prefix("T"));
    });
    row(ui, "Type", |ui| {
        egui::ComboBox::from_id_salt("cam-tool-kind").selected_text(t.kind.label()).show_ui(ui, |ui| {
            for k in ToolKind::ALL {
                ui.selectable_value(&mut t.kind, k, k.label());
            }
        });
    });
    num(ui, "Diameter", &mut t.diameter, " mm", 0.01..=200.0, 0.01);
    match t.kind {
        ToolKind::Bull => num(ui, "Corner radius", &mut t.corner_radius, " mm", 0.0..=50.0, 0.01),
        ToolKind::TaperedBall => {
            num(ui, "Tip radius", &mut t.corner_radius, " mm", 0.01..=20.0, 0.01);
            num(ui, "Taper/side", &mut t.angle, "°", 0.1..=45.0, 0.1);
        }
        ToolKind::Chamfer => {
            num(ui, "Angle", &mut t.angle, "°", 10.0..=170.0, 1.0);
            num(ui, "Tip Ø", &mut t.tip_diameter, " mm", 0.0..=50.0, 0.05);
        }
        ToolKind::Engrave | ToolKind::Drill | ToolKind::SpotDrill | ToolKind::CenterDrill => num(ui, "Point angle", &mut t.angle, "°", 10.0..=180.0, 1.0),
        ToolKind::Slot => {
            num(ui, "Thickness", &mut t.cutting_width, " mm", 0.1..=50.0, 0.05);
            num(ui, "Neck Ø", &mut t.neck_diameter, " mm", 0.1..=100.0, 0.05);
        }
        ToolKind::ThreadMill => {
            num(ui, "Pitch", &mut t.pitch, " mm", 0.1..=10.0, 0.05);
            num(ui, "Teeth length", &mut t.cutting_width, " mm", 0.1..=100.0, 0.1);
            num(ui, "Neck Ø", &mut t.neck_diameter, " mm", 0.1..=100.0, 0.05);
        }
        ToolKind::Tap => num(ui, "Pitch", &mut t.pitch, " mm", 0.1..=10.0, 0.05),
        _ => {}
    }
    num(ui, "Flute length", &mut t.flute_length, " mm", 0.1..=500.0, 0.1);
    num(ui, "Stickout", &mut t.stickout, " mm", 1.0..=500.0, 0.5);
    num(ui, "Shank Ø", &mut t.shank_diameter, " mm", 0.1..=100.0, 0.05);
    row(ui, "Holder", |ui| {
        let label = if t.holder.len() >= 2 { if t.holder_name.is_empty() { "custom".to_string() } else { t.holder_name.clone() } } else { "plain cylinder".to_string() };
        egui::ComboBox::from_id_salt("cam-holder").width(170.0).selected_text(label).show_ui(ui, |ui| {
            if ui.selectable_label(t.holder.is_empty(), "plain cylinder").clicked() {
                t.holder.clear();
                t.holder_name.clear();
            }
            for (n, _) in cam::holder_presets() {
                if ui.selectable_label(t.holder_name == n, n).clicked() {
                    cam::set_holder(t, n);
                }
            }
        });
    });
    if t.holder.len() < 2 {
        num(ui, "Holder Ø", &mut t.holder_diameter, " mm", 1.0..=300.0, 0.5);
    }
    row(ui, "Flutes", |ui| {
        ui.add(egui::DragValue::new(&mut t.flutes).range(1..=20));
    });
    row(ui, "Material", |ui| {
        for m in ["carbide", "hss", "cobalt"] {
            ui.radio_value(&mut t.material, m.to_string(), m);
        }
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("CUTTING DATA").color(theme::muted()).size(11.0).strong());
        if ui.small_button("Suggest").on_hover_text(format!("Feeds and speeds for {material}")).clicked() {
            cam::suggest(t, &material, max_rpm);
        }
    });
    num(ui, "Spindle", &mut t.rpm, " rpm", 10.0..=100000.0, 50.0);
    num(ui, "Feed", &mut t.feed, " mm/min", 1.0..=50000.0, 5.0);
    num(ui, "Plunge feed", &mut t.plunge_feed, " mm/min", 1.0..=50000.0, 5.0);
    if !t.kind.is_hole_tool() {
        num(ui, "Stepdown", &mut t.stepdown, " mm", 0.01..=100.0, 0.05);
        row(ui, "Stepover", |ui| {
            let mut pct = t.stepover * 100.0;
            if ui.add(egui::DragValue::new(&mut pct).range(1.0..=100.0).suffix(" % of Ø")).changed() {
                t.stepover = pct / 100.0;
            }
        });
    }
    ui.checkbox(&mut t.coolant, "Coolant");
}

/// Side view of a tool's cutting profile.
fn tool_preview(ui: &mut Ui, t: &Tool) {
    let prof = t.profile();
    let h = 70.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::hover());
    let top = prof.iter().map(|p| p.1).fold(0.0f64, f64::max).max(t.radius() * 2.0).max(1e-3);
    let wmax = prof.iter().map(|p| p.0).fold(0.0f64, f64::max).max(1e-3);
    let s = ((h as f64 - 8.0) / top).min((rect.width() as f64 * 0.4) / wmax) as f32;
    let base = Pos2::new(rect.center().x, rect.bottom() - 4.0);
    let mut pts: Vec<Pos2> = prof.iter().map(|(r, z)| base + egui::vec2(*r as f32 * s, -*z as f32 * s)).collect();
    let mut left: Vec<Pos2> = prof.iter().rev().map(|(r, z)| base + egui::vec2(-*r as f32 * s, -*z as f32 * s)).collect();
    pts.append(&mut left);
    ui.painter().add(egui::Shape::closed_line(pts, Stroke::new(1.4, theme::accent())));
}

fn library_window(app: &mut App, ctx: &egui::Context) {
    if !app.cam.library_open {
        return;
    }
    let mut open = true;
    let mut add: Vec<Tool> = vec![];
    let mut imported = None;
    egui::Window::new("Tool library").open(&mut open).default_size([420.0, 480.0]).collapsible(false).show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut app.cam.lib_search).hint_text("Search, e.g. 6 flat or M5").desired_width(170.0));
            egui::ComboBox::from_id_salt("lib-kind").selected_text(app.cam.lib_kind.map(|k| k.label()).unwrap_or("All types")).show_ui(ui, |ui| {
                ui.selectable_value(&mut app.cam.lib_kind, None, "All types");
                for k in ToolKind::ALL {
                    ui.selectable_value(&mut app.cam.lib_kind, Some(k), k.label());
                }
            });
        });
        ui.horizontal(|ui| {
            if ui.button("Import library…").on_hover_text("Add the tools of a Fusion 360 tool library (.json or .tools) or another tools.json to My tools").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("Tool library", &["json", "tools"]).pick_file() {
                    imported = Some(crate::cam_tool_import::import_file(&p));
                }
            }
        });
        ui.separator();
        let mine = cam::load_library();
        let mut all: Vec<(bool, Tool)> = mine.into_iter().map(|t| (true, t)).collect();
        all.extend(cam::presets().into_iter().map(|t| (false, t)));
        let q = app.cam.lib_search.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut last_kind = None;
            for (own, t) in all.iter().filter(|(_, t)| app.cam.lib_kind.map(|k| k == t.kind).unwrap_or(true)) {
                let hay = format!("{} {} {} ø{}", t.name.to_lowercase(), t.kind.label().to_lowercase(), t.describe().to_lowercase(), cam::fmt_mm(t.diameter));
                if !words.iter().all(|w| hay.contains(w)) {
                    continue;
                }
                if last_kind != Some((*own, t.kind)) {
                    ui.add_space(4.0);
                    ui.label(RichText::new(if *own { format!("My tools · {}", t.kind.label()) } else { t.kind.label().to_string() }).color(theme::muted()).size(11.5).strong());
                    last_kind = Some((*own, t.kind));
                }
                ui.horizontal(|ui| {
                    if ui.small_button("Add").clicked() {
                        add.push(t.clone());
                    }
                    ui.label(RichText::new(&t.name).size(12.5));
                    ui.label(RichText::new(format!("{} rpm · {} mm/min", t.rpm, t.feed)).color(theme::muted()).size(11.5));
                });
            }
        });
    });
    if !open {
        app.cam.library_open = false;
    }
    match imported {
        Some(Ok(r)) => {
            let new = crate::cam_tool_import::add_to_library(&r.tools);
            let mut msg = format!("Imported {} tool{} ({new} new) into My tools", r.tools.len(), if r.tools.len() == 1 { "" } else { "s" });
            if !r.skipped.is_empty() {
                msg += &format!("; skipped {}", r.skipped.join(", "));
            }
            app.flash_info(&msg, Some(std::time::Duration::from_secs(5)));
        }
        Some(Err(e)) => app.flash_info(&format!("Couldn't import the library: {e}"), Some(std::time::Duration::from_secs(5))),
        None => {}
    }
    if !add.is_empty() {
        let (material, max_rpm) = cam(app).map(|c| (c.stock.material.clone(), c.max_rpm)).unwrap_or_default();
        let mut last = 0;
        edit(app, |c| {
            for mut t in add {
                // the user's own tools keep their feeds and speeds when they have them
                if t.rpm <= 0.0 || cam::presets().iter().any(|p| p.name == t.name) {
                    cam::suggest(&mut t, &material, max_rpm);
                }
                last = c.add_tool(t);
            }
        });
        app.cam.sel_tool = Some(last);
        app.cam.sel_op = None;
        app.flash_info("Tool added", Some(std::time::Duration::from_secs(1)));
    }
}

// ---------------------------------------------------------------- G-code

pub fn gcode_for(c: &Cam, part: &Part, paths: &[Toolpath]) -> String {
    let ctx = cam_paths::Ctx::new(c, part);
    crate::cam_post::gcode(c, paths, ctx.origin, ctx.safe())
}

fn export_dialog(app: &mut App) {
    let (Some(c), Some(part)) = (cam(app).cloned(), app.cam.part.clone()) else { return };
    if app.cam.computing {
        app.flash("Toolpaths are still being made", ToastKind::Info);
        return;
    }
    let ext = if c.post == "fanuc" {
        "nc"
    } else if c.post == "linuxcnc" {
        "ngc"
    } else {
        "gcode"
    };
    let name: String = c.name.chars().map(|ch| if ch.is_ascii_alphanumeric() || "-_".contains(ch) { ch } else { '_' }).collect();
    let Some(p) = rfd::FileDialog::new().add_filter("G-code", &[ext, "nc", "gcode", "ngc", "tap"]).set_file_name(format!("{name}.{ext}")).save_file() else { return };
    let text = gcode_for(&c, &part, &app.cam.paths);
    match std::fs::write(&p, text) {
        Ok(()) => {
            let crashes = app.cam.sim.as_ref().map(|s| s.events.len()).unwrap_or(0);
            if crashes > 0 {
                app.flash(format!("Saved {} · the simulation found {crashes} problem(s)", p.display()), ToastKind::Error);
            } else {
                app.flash_info(format!("Saved {}", p.display()), Some(std::time::Duration::from_secs(3)));
            }
        }
        Err(e) => app.flash(format!("Could not save: {e}"), ToastKind::Error),
    }
}

fn sheet_dialog(app: &mut App) {
    let (Some(c), Some(part)) = (cam(app).cloned(), app.cam.part.clone()) else { return };
    if app.cam.computing {
        app.flash("Toolpaths are still being made", ToastKind::Info);
        return;
    }
    let name: String = c.name.chars().map(|ch| if ch.is_ascii_alphanumeric() || "-_".contains(ch) { ch } else { '_' }).collect();
    let Some(p) = rfd::FileDialog::new().add_filter("Web page", &["html", "htm"]).set_file_name(format!("{name} setup sheet.html")).save_file() else { return };
    let png = crate::cam_sheet::picture(&c, &part, 640, 480);
    match std::fs::write(&p, crate::cam_sheet::html(&c, &part, &app.cam.paths, png.as_deref())) {
        Ok(()) => app.flash_info(format!("Saved {}", p.display()), Some(std::time::Duration::from_secs(3))),
        Err(e) => app.flash(format!("Could not save: {e}"), ToastKind::Error),
    }
}

// ---------------------------------------------------------------- simulation

fn start_sim(app: &mut App, run: bool) {
    let (Some(c), Some(part)) = (cam(app).cloned(), app.cam.part.clone()) else { return };
    let stock = cam_paths::stock_box(&c, &part);
    let mut sim = Sim::new(&c, &part, stock, &app.cam.paths, 500);
    if let Some(m) = &app.cam.stock_in {
        sim.start_from(m);
    }
    app.cam.sim_target = if run { sim.total() } else { 0 };
    app.cam.sim = Some(sim);
    app.cam.sim_mesh_done = usize::MAX;
    app.cam.playing = false;
    app.cam.scene_dirty = true;
}

fn sim_ui(app: &mut App, ui: &mut Ui) {
    let Some(sim) = &app.cam.sim else { return };
    let total = sim.total();
    let done = sim.done;
    let events = sim.events.clone();
    let removed = sim.removed;
    ui.horizontal(|ui| {
        let icon = if app.cam.playing { "pause" } else { "camSim" };
        if ui.add(egui::Button::image(icons::image(icon, 16.0).tint(theme::text()))).on_hover_text("Play / pause (Space)").clicked() {
            app.cam.playing = !app.cam.playing;
            if app.cam.playing && done >= total {
                app.cam.sim_target = 0;
            }
            app.cam.sim_on = true;
        }
        if ui.small_button("⏮").on_hover_text("Back to the start").clicked() {
            app.cam.sim_target = 0;
            app.cam.playing = false;
            app.cam.sim_on = true;
        }
        if ui.small_button("⏭").on_hover_text("Run to the end").clicked() {
            app.cam.sim_target = total;
            app.cam.playing = false;
            app.cam.sim_on = true;
        }
        let mut t = app.cam.sim_target as f64;
        if ui.add(egui::Slider::new(&mut t, 0.0..=total.max(1) as f64).show_value(false)).changed() {
            app.cam.sim_target = t as usize;
            app.cam.playing = false;
            app.cam.sim_on = true;
        }
    });
    if app.cam.speed <= 0.0 {
        app.cam.speed = 200.0;
    }
    row(ui, "Speed", |ui| {
        ui.add(egui::Slider::new(&mut app.cam.speed, 1.0..=2000.0).logarithmic(true).suffix(" moves/s"));
    });
    ui.label(RichText::new(format!("Move {done} of {total} · {:.1} cm³ removed", removed / 1000.0)).color(theme::muted()).size(12.0));
    if ui.checkbox(&mut app.cam.show_part, "Show the part").on_hover_text("Show the finished part, see-through, over the stock").changed() {
        app.cam.scene_dirty = true;
    }
    // colour key: each operation colours what it cut
    let swatch = |ui: &mut Ui, rgb: [u8; 3], text: &str| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
            ui.label(RichText::new(text).size(12.0));
        });
    };
    let names: Vec<String> = app.cam.paths.iter().map(|p| cam(app).and_then(|c| c.op(p.op)).map(|o| o.name.clone()).unwrap_or_default()).collect();
    swatch(ui, crate::cam_sim::class_rgb(crate::cam_sim::CLASS_STOCK), "Stock");
    if app.cam.stock_in.is_some() {
        swatch(ui, crate::cam_sim::class_rgb(crate::cam_sim::CLASS_EARLIER), "Cut in the earlier setup");
    }
    for (k, n) in names.iter().enumerate() {
        swatch(ui, crate::cam_sim::PATH_COLOURS[k % crate::cam_sim::PATH_COLOURS.len()], n);
    }
    swatch(ui, crate::cam_sim::class_rgb(crate::cam_sim::CLASS_GOUGE), "Gouge");
    if events.is_empty() {
        let txt = if done >= total { "No crashes or gouges found" } else { "No problems so far" };
        ui.label(RichText::new(format!("✓ {txt}")).color(theme::ok_green()).size(12.5));
    } else {
        ui.label(RichText::new(format!("⚠ {} problem(s)", events.len())).color(theme::error()).strong());
        let names: Vec<(u64, String)> = cam(app).map(|c| c.ops.iter().map(|o| (o.id, o.name.clone())).collect()).unwrap_or_default();
        egui::ScrollArea::vertical().max_height(160.0).id_salt("sim-events").show(ui, |ui| {
            for e in &events {
                let op = names.iter().find(|n| n.0 == e.op).map(|n| n.1.as_str()).unwrap_or("?");
                let r =
                    ui.add(egui::Button::new(RichText::new(format!("{op}: {} ({} mm)", e.kind.label(), cam::fmt_mm((e.depth * 100.0).round() / 100.0))).size(12.0)).frame(false));
                if r.on_hover_text("Show where").clicked() {
                    app.cam.sim_target = e.step + 1;
                    app.cam.playing = false;
                    app.cam.sel_op = Some(e.op);
                    app.cam.sim_on = true;
                }
            }
        });
    }
}

/// Advance the simulation towards its target.
fn step_sim(app: &mut App, ctx: &egui::Context) {
    if !app.cam.sim_on {
        return;
    }
    if app.cam.sim.is_none() && !app.cam.paths.is_empty() && !app.cam.computing {
        start_sim(app, false);
        app.cam.sim_target = app.cam.sim.as_ref().map(|s| s.total()).unwrap_or(0);
    }
    let dt = ctx.input(|i| i.stable_dt).min(0.1);
    let speed = if app.cam.speed <= 0.0 { 200.0 } else { app.cam.speed };
    let Some(sim) = &mut app.cam.sim else { return };
    if app.cam.playing {
        let adv = (speed * dt).ceil().max(1.0) as usize;
        app.cam.sim_target = (sim.done + adv).min(sim.total());
        if sim.done >= sim.total() {
            app.cam.playing = false;
        }
    }
    let target = app.cam.sim_target.min(sim.total());
    if target < sim.done {
        start_sim(app, false);
        app.cam.sim_target = target;
    }
    let Some(sim) = &mut app.cam.sim else { return };
    if sim.done < target {
        sim.run_to(target, 60_000);
        ctx.request_repaint();
    }
    if app.cam.playing {
        ctx.request_repaint();
    }
    // rebuild the stock mesh now and then
    let due = app.cam.sim_mesh_at.map(|t| t.elapsed().as_millis() > 120).unwrap_or(true);
    if app.cam.sim_mesh_done != sim.done && (due || sim.done >= target) {
        let (tris, cls) = sim.mesh(260);
        app.cam.sim_mesh = tris.into_iter().zip(cls).collect();
        app.cam.sim_mesh_done = sim.done;
        app.cam.sim_mesh_at = Some(Instant::now());
        app.cam.scene_dirty = true;
    } else if app.cam.sim_mesh_done != sim.done {
        ctx.request_repaint_after(std::time::Duration::from_millis(60));
    }
}

// ---------------------------------------------------------------- viewport

fn rgba(c: Color32, a: f32) -> [f32; 4] {
    crate::viewport::rgba(c, a)
}

fn build_scene(app: &App) -> SceneData {
    let mut s = SceneData::default();
    let Some(c) = cam(app) else { return s };
    let sim_on = app.cam.sim_on && app.cam.sim.is_some();
    // parts (hidden while simulating unless asked for, so the stock reads clearly)
    let doc = app.studio_doc(c.studio);
    let bodies: &[BodyMesh] = if sim_on && !app.cam.show_part { &[] } else { app.studio_bodies(c.studio) };
    for m in bodies {
        let hidden = doc.and_then(|d| d.parts.get(&crate::doc::body_key(m.id))).map(|p| p.hidden).unwrap_or(false);
        if if c.bodies.is_empty() { hidden } else { !c.bodies.contains(&m.id) } {
            continue;
        }
        let col = doc.and_then(|d| d.parts.get(&crate::doc::body_key(m.id))).and_then(|p| p.color).map(|c| Color32::from_rgb(c[0], c[1], c[2])).unwrap_or(vp::body());
        for t in m.indices.chunks(3) {
            let p = |i: u32| Vec3::new(m.positions[3 * i as usize], m.positions[3 * i as usize + 1], m.positions[3 * i as usize + 2]);
            s.tri(sim_on, [p(t[0]), p(t[1]), p(t[2])], rgba(col, if sim_on { 0.25 } else { 1.0 }));
        }
        if !sim_on {
            for e in &m.edges {
                let pts: Vec<Vec3> = e.pts.iter().map(|p| Vec3::from(*p)).collect();
                s.polyline(&pts, 1.2, rgba(vp::edge(), 1.0), false);
            }
        }
    }
    let Some(part) = &app.cam.part else { return s };
    let to = |p: DVec3| part.to_part(p).as_vec3();
    // stock box
    let (lo, hi) = cam_paths::stock_box(c, part);
    let corner = |k: usize| to(DVec3::new([lo.x, hi.x][k & 1], [lo.y, hi.y][(k >> 1) & 1], [lo.z, hi.z][(k >> 2) & 1]));
    let stock_col = rgba(Color32::from_rgb(0xc8, 0x8a, 0x3c), 0.9);
    for (a, b) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
        s.line(corner(a), corner(b), 1.2, stock_col);
    }
    // work zero
    let o = cam_paths::work_origin(c, part);
    // fixtures
    let fix_col = rgba(Color32::from_rgb(0xe0, 0x5a, 0x2a), 0.95);
    for (_, lo, hi) in cam_paths::fixture_boxes(c, o) {
        let corner = |k: usize| to(DVec3::new([lo.x, hi.x][k & 1], [lo.y, hi.y][(k >> 1) & 1], [lo.z, hi.z][(k >> 2) & 1]));
        for (a, b) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
            s.line(corner(a), corner(b), 1.6, fix_col);
        }
        for f in [[0, 1, 3], [0, 3, 2], [4, 5, 7], [4, 7, 6], [0, 1, 5], [0, 5, 4], [2, 3, 7], [2, 7, 6], [0, 2, 6], [0, 6, 4], [1, 3, 7], [1, 7, 5]] {
            s.tri(true, [corner(f[0]), corner(f[1]), corner(f[2])], rgba(Color32::from_rgb(0xe0, 0x5a, 0x2a), 0.25));
        }
    }
    let size = (app.camera.wpp() * 40.0) as f64;
    for (d, col) in [(DVec3::X, vp::axis_x()), (DVec3::Y, vp::axis_y()), (DVec3::Z, vp::axis_z())] {
        s.lines_top.push(crate::render::LineInst { a: to(o).into(), b: to(o + d * size).into(), width: 2.5, bias: 0.0, color: rgba(col, 1.0) });
    }
    s.point(to(o), 7.0, rgba(vp::snap(), 1.0));
    // simulated stock
    if sim_on {
        for (t, cl) in &app.cam.sim_mesh {
            let [r, g, b] = crate::cam_sim::class_rgb(*cl);
            s.tri(false, [to(t[0]), to(t[1]), to(t[2])], rgba(Color32::from_rgb(r, g, b), 1.0));
        }
    }
    // toolpaths: cutting blue, plunges and ramps green, rapids yellow (while simulating, the current
    // operation's moves so far, in its colour)
    let sel = app.cam.sel_op;
    let done = if sim_on { app.cam.sim.as_ref().map(|x| x.done).unwrap_or(0) } else { usize::MAX };
    let mut count = 0usize;
    for (k, tp) in app.cam.paths.iter().enumerate() {
        let start = count;
        count += tp.moves.len();
        if sim_on && !(start <= done && done < count.max(start + 1)) {
            continue;
        }
        let on = sel.map(|id| id == tp.op).unwrap_or(true);
        let [r, g, b] = crate::cam_sim::PATH_COLOURS[k % crate::cam_sim::PATH_COLOURS.len()];
        let feed_col = if sim_on {
            rgba(Color32::from_rgb(r, g, b), 1.0)
        } else if sel == Some(tp.op) {
            rgba(theme::accent(), 1.0)
        } else if on {
            rgba(Color32::from_rgb(0x2f, 0x6f, 0xe0), 0.95)
        } else {
            rgba(theme::muted(), 0.45)
        };
        let plunge_col = rgba(Color32::from_rgb(0x1f, 0xb3, 0x4a), if on { 0.95 } else { 0.4 });
        let rapid_col = rgba(Color32::from_rgb(0xf5, 0xc5, 0x18), if on { 0.95 } else { 0.35 });
        let plunge = c.tool(tp.tool).map(|t| t.plunge_feed).unwrap_or(-1.0);
        let mut prev: Option<DVec3> = None;
        for (i, m) in tp.moves.iter().enumerate() {
            if start + i >= done {
                break;
            }
            let p0 = prev.unwrap_or_else(|| {
                let q = m.end(DVec3::ZERO);
                DVec3::new(q.x, q.y, hi.z.max(part.hi.z) + c.clearance)
            });
            let p1 = m.end(p0);
            let w = if sel == Some(tp.op) || sim_on { 1.8 } else { 1.2 };
            match m {
                Move::Rapid(_) => s.line(to(p0), to(p1), if on { 1.0 } else { 0.7 }, rapid_col),
                Move::Feed(_, f) if (*f - plunge).abs() < 1e-6 && p1.z < p0.z - 1e-6 => s.line(to(p0), to(p1), w, plunge_col),
                Move::Feed(..) => s.line(to(p0), to(p1), w, feed_col),
                Move::Tap { z, .. } => s.line(to(p0), to(DVec3::new(p0.x, p0.y, *z)), 1.8, plunge_col),
                Move::Dwell(_) => {}
            }
            prev = Some(p1);
        }
    }
    // sharp edges while picking them for a deburr, and the picks of the selected deburr
    if let Some(op) = sel.and_then(|id| c.op(id)).filter(|o| o.kind == OpKind::Deburr) {
        let origin = cam_paths::work_origin(c, part);
        if app.cam.pick_edges == Some(op.id) {
            for (pts, _, _) in crate::cam_3d::sharp_edges(part, op.edge_angle) {
                let v: Vec<Vec3> = pts.iter().map(|p| to(*p)).collect();
                s.polyline(&v, 2.0, rgba(Color32::from_rgb(0xd0, 0x60, 0xd0), 0.9), false);
            }
        }
        for e in &op.edges {
            s.point(to(DVec3::from(*e) + origin), 9.0, rgba(theme::accent(), 1.0));
        }
    }
    // the tool where the simulation is
    if sim_on {
        if let Some(sim) = &app.cam.sim {
            let tp_tool = sim.current_cutter().and_then(|ci| app.cam.paths.get(ci)).and_then(|p| c.tool(p.tool)).cloned();
            if let Some(t) = tp_tool {
                let p = sim.tool_pos();
                let bad = sim.events.iter().any(|e| e.step + 1 >= sim.done && e.step < sim.done + 1);
                let col = rgba(if bad { Color32::from_rgb(0xe0, 0x40, 0x40) } else { Color32::from_rgb(0x30, 0x30, 0x38) }, 1.0);
                let mut prof = t.profile();
                let top = t.stickout.max(prof.last().map(|q| q.1).unwrap_or(0.0));
                prof.push(((t.shank_diameter / 2.0).max(0.1), prof.last().map(|q| q.1).unwrap_or(0.0)));
                prof.push(((t.shank_diameter / 2.0).max(0.1), top));
                for k in 0..8 {
                    let a = k as f64 / 8.0 * std::f64::consts::TAU;
                    let d = DVec3::new(a.cos(), a.sin(), 0.0);
                    let pts: Vec<Vec3> = prof.iter().map(|(r, z)| to(p + d * *r + DVec3::Z * *z)).collect();
                    s.polyline(&pts, 1.4, col, true);
                }
                for (r, z) in prof.iter().skip(1) {
                    let pts: Vec<Vec3> = (0..=24).map(|k| k as f64 / 24.0 * std::f64::consts::TAU).map(|a| to(p + DVec3::new(a.cos() * r, a.sin() * r, *z))).collect();
                    s.polyline(&pts, 1.4, col, true);
                }
            }
            for e in &sim.events {
                s.point(to(DVec3::from(e.at)), 9.0, rgba(Color32::from_rgb(0xe0, 0x40, 0x40), 1.0));
            }
        }
    }
    s
}

pub fn viewport(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    if let Some(c) = cam(app) {
        let s = c.studio;
        ensure_built(app, s);
    }
    refresh(app, &ctx);
    step_sim(app, &ctx);
    library_window(app, &ctx);
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    if app.viewport_rect != rect {
        app.viewport_rect = rect;
        app.cam.scene_dirty = true;
    }
    app.camera.size = Vec2::new(rect.width().max(1.0), rect.height().max(1.0));
    crate::viewport::paint_background(ui.painter(), rect, app.realistic);
    let before = (app.camera.target, app.camera.back, app.camera.up, app.camera.half_height);
    crate::viewport::navigate(app, ui, &resp, true);
    if before != (app.camera.target, app.camera.back, app.camera.up, app.camera.half_height) {
        app.cam.scene_dirty = true;
    }
    if let (Some(id), true) = (app.cam.pick_edges, resp.clicked()) {
        if let (Some(pos), Some(part), Some(c)) = (resp.interact_pointer_pos(), app.cam.part.clone(), cam(app)) {
            let local = Vec2::new(pos.x - rect.left(), pos.y - rect.top());
            let origin = cam_paths::work_origin(c, &part);
            let angle = c.op(id).map(|o| o.edge_angle).unwrap_or(30.0);
            let scr = |p: DVec3| app.camera.project(part.to_part(p).as_vec3());
            let existing: Vec<[f64; 3]> = c.op(id).map(|o| o.edges.clone()).unwrap_or_default();
            // clicking a pick again removes it
            if let Some(k) = existing.iter().position(|e| scr(DVec3::from(*e) + origin).distance(local) < 8.0) {
                edit(app, |c| {
                    if let Some(o) = c.ops.iter_mut().find(|o| o.id == id) {
                        o.edges.remove(k);
                    }
                });
            } else {
                let mut best: Option<(f32, DVec3)> = None;
                for (pts, _, _) in crate::cam_3d::sharp_edges(&part, angle) {
                    for w in pts.windows(2) {
                        let (a, b) = (scr(w[0]), scr(w[1]));
                        let ab = b - a;
                        let t = if ab.length_squared() > 1e-6 { ((local - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
                        let d = (a + ab * t).distance(local);
                        if d < 12.0 && best.map(|b| d < b.0).unwrap_or(true) {
                            best = Some((d, w[0].lerp(w[1], t as f64)));
                        }
                    }
                }
                if let Some((_, p)) = best {
                    let q = ((p - origin) * 1000.0).round() / 1000.0;
                    edit(app, |c| {
                        if let Some(o) = c.ops.iter_mut().find(|o| o.id == id) {
                            o.edges.push(q.to_array());
                        }
                    });
                }
            }
            app.cam.scene_dirty = true;
        }
    }
    if !app.cam.fitted && app.cam.part.is_some() {
        app.cam.fitted = true;
        fit(app);
    }
    if app.cam.scene_dirty {
        app.cam.scene_dirty = false;
        let mut s = build_scene(app);
        s.version = app.scene.version.wrapping_add(1);
        app.scene = Arc::new(s);
    }
    let ppp = ui.ctx().pixels_per_point();
    let cb = ViewportCallback {
        scene: app.scene.clone(),
        view_proj: app.camera.view_proj(),
        eye_dir: app.camera.back,
        clip: [0.0; 4],
        size_px: [rect.width() * ppp, rect.height() * ppp],
        pixels_per_point: ppp,
        realistic: app.realistic,
    };
    ui.painter().add(eframe::egui_wgpu::Callback::new_paint_callback(rect, cb));
    let painter = ui.painter_at(rect);
    crate::viewport::view_cube(app, ui, rect);
    // summary in the corner
    let total: f64 = app.cam.paths.iter().map(|p| p.time).sum();
    let mut lines = vec![];
    if app.cam.computing {
        lines.push("Making toolpaths…".to_string());
    } else if !app.cam.paths.is_empty() {
        lines.push(format!("Cycle time {}", fmt_time(total)));
    }
    if let Some(sim) = &app.cam.sim {
        if app.cam.sim_on {
            lines.push(if sim.events.is_empty() { "Simulation: no problems".to_string() } else { format!("Simulation: {} problem(s)", sim.events.len()) });
        }
    }
    if !lines.is_empty() {
        let text = lines.join(" · ");
        let w = painter.layout_no_wrap(text.clone(), egui::FontId::proportional(12.0), theme::text()).size().x;
        crate::viewport::label_box(&painter, rect.left_top() + egui::vec2(16.0 + w / 2.0 + 6.0, 20.0), &text, theme::text());
    }
    if app.cam.sim_on {
        // playback strip along the bottom
        let strip = egui::Rect::from_min_size(Pos2::new(rect.left() + 16.0, rect.bottom() - 88.0), egui::vec2((rect.width() - 32.0).min(560.0), 40.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(strip));
        egui::Frame::new().fill(theme::window()).stroke(Stroke::new(1.0, theme::border())).corner_radius(6).inner_margin(Margin::symmetric(8, 4)).show(&mut child, |ui| {
            ui.horizontal(|ui| {
                let total = app.cam.sim.as_ref().map(|s| s.total()).unwrap_or(0);
                let icon = if app.cam.playing { "pause" } else { "camSim" };
                if ui.add(egui::Button::image(icons::image(icon, 16.0).tint(theme::text()))).clicked() {
                    app.cam.playing = !app.cam.playing;
                    if app.cam.playing && app.cam.sim.as_ref().map(|s| s.done >= s.total()).unwrap_or(false) {
                        app.cam.sim_target = 0;
                    }
                }
                let mut t = app.cam.sim_target as f64;
                ui.spacing_mut().slider_width = (strip.width() - 120.0).max(60.0);
                if ui.add(egui::Slider::new(&mut t, 0.0..=total.max(1) as f64).show_value(false)).changed() {
                    app.cam.sim_target = t as usize;
                    app.cam.playing = false;
                }
                if ui.small_button("Close").clicked() {
                    app.cam.sim_on = false;
                    app.cam.playing = false;
                    app.cam.scene_dirty = true;
                }
            });
        });
    }
    let _ = resp;
}
