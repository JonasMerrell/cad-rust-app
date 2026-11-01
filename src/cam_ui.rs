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
    /// The setup (Manufacture tab index) `undo` and `redo` belong to; the other setups' histories are parked in `histories`.
    undo_of: Option<usize>,
    histories: std::collections::HashMap<usize, (Vec<Cam>, Vec<Cam>)>,
    last_ck: Option<Instant>,
    /// Picking edges for this deburr operation.
    pub pick_edges: Option<u64>,
    /// Show the part (see-through) over the simulated stock.
    pub show_part: bool,
    /// Name to save the operations under as a template.
    tpl_name: String,
    pub machine: crate::cam_machine_ui::MachineUi,
    /// The machine placed for the setup, kept while the setup and machine stay the same: (key, placed).
    placed: Option<(u64, Arc<crate::cam_machine::Placed>)>,
    /// The setup page shown under the work tree when no operation or tool is picked.
    pub page: Page,
    /// Folded work tree branches.
    folded: std::collections::HashSet<&'static str>,
    /// Height of the work tree above the properties (0: not dragged yet).
    tree_h: f32,
    /// Where the simulation card was last frame, so drags on it don't turn the view.
    sim_card: Option<egui::Rect>,
}

/// The parts of a setup, each a node of the work tree with its own settings page.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Page {
    #[default]
    Part,
    Stock,
    WorkZero,
    Fixtures,
    Machine,
}

impl Page {
    pub fn title(self) -> &'static str {
        match self {
            Page::Part => "Setup",
            Page::Stock => "Stock",
            Page::WorkZero => "Work zero",
            Page::Fixtures => "Fixtures",
            Page::Machine => "Machine",
        }
    }
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

/// Each setup keeps its own undo history: bring in the open one's, and drop the picks made in the setup
/// before (operation ids are only unique within a setup).
pub(crate) fn sync_setup(app: &mut App) {
    let Some(ci) = cam_index(app) else { return };
    if app.cam.undo_of == Some(ci) {
        return;
    }
    if let Some(prev) = app.cam.undo_of {
        let h = (std::mem::take(&mut app.cam.undo), std::mem::take(&mut app.cam.redo));
        app.cam.histories.insert(prev, h);
        app.cam.sel_op = None;
        app.cam.sel_tool = None;
        app.cam.pick_edges = None;
    }
    let (u, r) = app.cam.histories.remove(&ci).unwrap_or_default();
    app.cam.undo = u;
    app.cam.redo = r;
    app.cam.undo_of = Some(ci);
    app.cam.last_ck = None;
}

/// Forget the undo steps of every setup (tab operations renumber setups and Part Studios).
pub fn clear_history(app: &mut App) {
    app.cam.undo.clear();
    app.cam.redo.clear();
    app.cam.histories.clear();
    app.cam.last_ck = None;
}

/// (undo steps, redo steps) of the open setup.
fn history_len(app: &App) -> (usize, usize) {
    match cam_index(app) {
        Some(ci) if app.cam.undo_of != Some(ci) => app.cam.histories.get(&ci).map(|h| (h.0.len(), h.1.len())).unwrap_or((0, 0)),
        _ => (app.cam.undo.len(), app.cam.redo.len()),
    }
}

pub fn undo(app: &mut App) {
    sync_setup(app);
    let Some(i) = cam_index(app) else { return };
    if let Some(c) = app.cam.undo.pop() {
        let cur = std::mem::replace(&mut app.ws.cams[i], c);
        app.cam.redo.push(cur);
        app.cam.scene_dirty = true;
    }
}

pub fn redo(app: &mut App) {
    sync_setup(app);
    let Some(i) = cam_index(app) else { return };
    if let Some(c) = app.cam.redo.pop() {
        let cur = std::mem::replace(&mut app.ws.cams[i], c);
        app.cam.undo.push(cur);
        app.cam.scene_dirty = true;
    }
}

pub fn can_undo(app: &App) -> bool {
    history_len(app).0 > 0
}

pub fn can_redo(app: &App) -> bool {
    history_len(app).1 > 0
}

/// Record an undo step with the state before an edit (edits closer together than a second share one step).
fn record(app: &mut App, before: Cam) {
    sync_setup(app);
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
    sync_setup(app);
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
    // a tab that machines soft jaws works on those jaws (a broken link shows as a warning)
    let (input, jaws_err) = match crate::cam_jaws::input_for(&app.ws.cams, ci, input) {
        Ok(i) => (i, None),
        Err(e) => (PartInput::default(), Some(e)),
    };
    let key = input_key(&app.ws.cams, ci, &input) ^ jaws_err.is_some() as u64;
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
        let part = Part::from_input(&input, c);
        let (stock_in, err) = match linked {
            Ok(s) => (s, jaws_err),
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
    sync_setup(app);
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
            toggle_sim(app);
        }
        if app.tool_button(ui, "camGcode", "G-code", "Export G-code for the machine", true, false, !app.cam.paths.is_empty()).clicked() {
            export_dialog(app);
        }
        if app.tool_button(ui, "camGcode", "Send to rcnc", "Load the G-code, its tools and its setup sheet in rcnc, the CNC controller, running on this computer", true, false, !app.cam.paths.is_empty()).clicked() {
            send_to_rcnc(app);
        }
        if app.tool_button(ui, "camSheet", "Setup sheet", "Save a setup sheet for the operator: stock, work zero, fixtures, tools and operations", true, false, !app.cam.paths.is_empty()).clicked() {
            sheet_dialog(app);
        }
    });
}

pub(crate) fn toggle_sim(app: &mut App) {
    if app.cam.paths.is_empty() {
        return;
    }
    app.cam.sim_on = !app.cam.sim_on;
    app.cam.scene_dirty = true;
    if app.cam.sim_on && app.cam.sim.is_none() {
        start_sim(app, true);
    }
}

pub(crate) fn add_op(app: &mut App, k: OpKind) {
    // a pencil pass wants a tool smaller than the ones before it
    let small = |c: &Cam| c.tools.iter().filter(|t| t.kind == ToolKind::Ball && t.diameter <= 3.0).map(|t| t.id).next();
    let missing = cam(app).map(|c| if k == OpKind::Pencil { small(c).is_none() } else { !c.tools.iter().any(|t| k.accepts(t.kind)) }).unwrap_or(true);
    if missing {
        // bring in a suitable tool from the library first
        let material = cam(app).map(|c| c.stock.material.clone()).unwrap_or_default();
        let (max_rpm, min_rpm) = cam(app).map(|c| (c.max_rpm, c.min_rpm)).unwrap_or((24000.0, 0.0));
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
            cam::suggest(&mut t, &material, max_rpm, min_rpm);
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
    sync_setup(app);
    let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
    // no Ctrl combinations here yet; Ctrl+letter is not the letter's tool
    if ctx.egui_wants_keyboard_input() || crate::app::key_mods(ctx).0 {
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

pub(crate) fn fit(app: &mut App) {
    let Some(part) = app.cam.part.clone() else { return };
    let Some(c) = cam(app) else { return };
    let (lo, hi) = cam_paths::stock_box(c, &part);
    let mut a = DVec3::splat(f64::INFINITY);
    let mut b = DVec3::splat(f64::NEG_INFINITY);
    for k in 0..8 {
        let p = DVec3::new([lo.x, hi.x][k & 1], [lo.y, hi.y][(k >> 1) & 1], [lo.z, hi.z][(k >> 2) & 1]);
        a = a.min(p);
        b = b.max(p);
    }
    app.camera.fit(a.as_vec3(), b.as_vec3());
    app.cam.scene_dirty = true;
}

// ---------------------------------------------------------------- sidebar

pub(crate) fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
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

pub(crate) fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
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

/// A machine limit where 0 means "not set" (nothing is checked against it).
fn limit_value<'a>(v: &'a mut f64, unit: &'a str) -> egui::DragValue<'a> {
    egui::DragValue::new(v)
        .range(0.0..=100000.0)
        .speed(1.0)
        .max_decimals(1)
        .custom_formatter(move |x, _| if x == 0.0 { "not set".into() } else { format!("{}{unit}", cam::fmt_mm((x * 10.0).round() / 10.0)) })
        .custom_parser(|t| {
            let t = t.trim();
            if t.is_empty() || t.starts_with("not") { Some(0.0) } else { t.trim_end_matches(|c: char| c.is_alphabetic() || c == '/' || c == ' ').parse().ok() }
        })
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
    sync_setup(app);
    let ci = cam_index(app);
    let Some(before) = cam(app).cloned() else {
        ui.label("No Manufacture tab");
        return;
    };
    // the work tree on top, the picked node's settings below, split by a bar to drag
    let full = ui.available_height();
    if app.cam.tree_h <= 0.0 {
        app.cam.tree_h = (full * 0.42).max(160.0);
    }
    let h = app.cam.tree_h.clamp(90.0, (full - 140.0).max(90.0));
    let w = ui.available_width();
    ui.allocate_ui(egui::vec2(w, h), |ui| {
        ui.set_min_height(h);
        egui::ScrollArea::vertical().id_salt("cam-tree").auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(Margin { left: 6, right: 6, top: 8, bottom: 6 }).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                work_tree(app, ui);
            });
        });
    });
    let (bar, resp) = ui.allocate_exact_size(egui::vec2(w, 7.0), Sense::drag());
    let col = if resp.hovered() || resp.dragged() { theme::accent() } else { theme::border() };
    ui.painter().hline(bar.x_range(), bar.center().y, Stroke::new(1.0, col));
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    if resp.dragged() {
        app.cam.tree_h = (h + resp.drag_delta().y).clamp(90.0, (full - 140.0).max(90.0));
    }
    egui::ScrollArea::vertical().id_salt("cam-props").auto_shrink([false, false]).show(ui, |ui| {
        let op = app.cam.sel_op.filter(|&id| cam(app).and_then(|c| c.op(id)).is_some());
        let tool = app.cam.sel_tool.filter(|&id| cam(app).and_then(|c| c.tool(id)).is_some());
        if let Some(id) = op {
            section(ui, "Operation", |ui| op_params(app, ui, id));
        } else if let Some(id) = tool {
            section(ui, "Tool", |ui| tool_params(app, ui, id));
        } else {
            let page = app.cam.page;
            section(ui, page.title(), |ui| setup_ui(app, ui, page));
        }
    });
    // a pick of another node ends picking edges for a deburr
    if app.cam.pick_edges.is_some() && app.cam.pick_edges != app.cam.sel_op {
        app.cam.pick_edges = None;
    }
    // (when the tree opened another setup, `before` is the one left, not an edit of this one)
    if cam_index(app) == ci && cam(app).map(|c| *c != before).unwrap_or(false) {
        record(app, before);
    }
}

/// One row of the work tree: fold arrow, icon, name and a muted note on the right.
/// `trail` is room kept at the right for buttons (shown when hovered or picked, or always with `keep`);
/// returns the row, whether its arrow was clicked and where the buttons go.
#[allow(clippy::too_many_arguments)]
fn tree_row(ui: &mut Ui, depth: u8, fold: Option<bool>, icon: &str, name: &str, note: &str, sel: bool, dim: bool, trail: f32, keep: bool) -> (egui::Response, bool, Option<egui::Rect>) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), Sense::click());
    let over = ui.rect_contains_pointer(rect);
    let p = ui.painter_at(rect);
    if sel {
        p.rect_filled(rect, 6.0, theme::sel_fill());
    } else if over {
        p.rect_filled(rect, 6.0, theme::panel_2());
    }
    let x0 = rect.left() + 4.0 + depth as f32 * 16.0;
    let arrow = egui::Rect::from_min_size(egui::pos2(x0, rect.center().y - 6.0), egui::vec2(12.0, 12.0));
    if let Some(open) = fold {
        icons::image(if open { "chevronDown" } else { "chevronRight" }, 12.0).tint(theme::muted()).paint_at(ui, arrow);
    }
    let ic = egui::Rect::from_min_size(egui::pos2(x0 + 15.0, rect.center().y - 8.0), egui::vec2(16.0, 16.0));
    let fg = if dim { theme::muted() } else { theme::text() };
    icons::image(icon, 16.0).tint(if dim { theme::muted() } else { theme::lerp(theme::text(), theme::muted(), 0.35) }).paint_at(ui, ic);
    let room = if trail > 0.0 && (keep || over || sel) { trail } else { 0.0 };
    let right = rect.right() - 6.0 - room;
    // the name comes first; the note gets what is left, shortened to fit
    let tp = egui::pos2(ic.right() + 6.0, rect.center().y);
    let clip = egui::Rect::from_min_max(rect.min, egui::pos2(right.max(tp.x), rect.bottom()));
    let g = p.layout_no_wrap(name.to_string(), egui::FontId::proportional(12.5), fg);
    let end = tp.x + g.size().x;
    let pos = egui::pos2(tp.x, tp.y - g.size().y / 2.0);
    p.with_clip_rect(clip).galley(pos, g, fg);
    if dim && fold.is_none() {
        p.with_clip_rect(clip).hline(egui::Rangef::new(tp.x, end.min(clip.right())), rect.center().y, Stroke::new(1.0, theme::muted()));
    }
    if !note.is_empty() && room == 0.0 {
        let font = egui::FontId::proportional(11.5);
        let space = right - end - 10.0;
        let mut text = note.to_string();
        let mut g = p.layout_no_wrap(text.clone(), font.clone(), theme::muted());
        while g.size().x > space && !text.is_empty() {
            text.pop();
            g = p.layout_no_wrap(format!("{}…", text.trim_end()), font.clone(), theme::muted());
        }
        if !text.is_empty() {
            p.galley(egui::pos2(right - g.size().x, rect.center().y - g.size().y / 2.0), g, theme::muted());
        }
    }
    let arrow_hit = fold.is_some() && resp.clicked() && resp.interact_pointer_pos().map(|q| q.x < arrow.right() + 3.0).unwrap_or(false);
    let slot = (room > 0.0).then(|| egui::Rect::from_min_max(egui::pos2(rect.right() - 4.0 - room, rect.top()), egui::pos2(rect.right() - 4.0, rect.bottom())));
    (resp, arrow_hit, slot)
}

/// Widgets in a tree row's button slot, right to left.
fn in_slot<R>(ui: &mut Ui, slot: egui::Rect, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope_builder(egui::UiBuilder::new().max_rect(slot).layout(egui::Layout::right_to_left(egui::Align::Center)), add).inner
}

fn small_icon(ui: &mut Ui, icon: &str, tip: &str, enabled: bool) -> bool {
    ui.add_enabled(enabled, egui::Button::image(icons::image(icon, 13.0).tint(theme::muted())).frame(false)).on_hover_text(tip).clicked()
}

/// The setups, and under the open one its part, stock, work zero, fixtures, machine, tools and operations.
fn work_tree(app: &mut App, ui: &mut Ui) {
    let Some(ci) = cam_index(app) else { return };
    let names: Vec<(String, usize)> = app.ws.cams.iter().map(|c| (c.name.clone(), c.ops.iter().filter(|o| !o.suppressed).count())).collect();
    let mut go = None;
    for (i, (name, n)) in names.iter().enumerate() {
        if i != ci {
            let note = format!("{n} op{}", if *n == 1 { "" } else { "s" });
            let (r, _, _) = tree_row(ui, 0, Some(false), "camSetup", name, &note, false, false, 0.0, false);
            if r.on_hover_text("Open this setup").clicked() {
                go = Some(i);
            }
            continue;
        }
        let open = !app.cam.folded.contains("setup");
        let picked = app.cam.sel_op.is_none() && app.cam.sel_tool.is_none();
        // the setup row stands for its part: which Part Studio, which way up
        let c = &app.ws.cams[ci];
        let studio = app.ws.studios.get(c.studio).map(|s| s.name.clone()).unwrap_or_default();
        let note = if c.jaws_of.is_empty() { format!("{studio} · {} up", c.orient) } else { format!("jaws of {}", c.jaws_of) };
        let (r, arrow, _) = tree_row(ui, 0, Some(open), "camSetup", name, &note, picked && app.cam.page == Page::Part, false, 0.0, false);
        if arrow {
            fold(app, "setup");
        } else if r.clicked() {
            pick_page(app, Page::Part);
        }
        if open {
            setup_nodes(app, ui);
            tools_ui(app, ui);
            ops_ui(app, ui);
        }
    }
    if let Some(i) = go {
        app.switch_tab(Tab::Cam(i));
        sync_setup(app);
    }
}

fn fold(app: &mut App, key: &'static str) {
    if !app.cam.folded.remove(key) {
        app.cam.folded.insert(key);
    }
}

fn pick_page(app: &mut App, page: Page) {
    app.cam.page = page;
    app.cam.sel_op = None;
    app.cam.sel_tool = None;
    app.cam.scene_dirty = true;
}

/// The setup's own nodes, each noting what it is set to.
fn setup_nodes(app: &mut App, ui: &mut Ui) {
    let Some(c) = cam(app) else { return };
    let stock = if !c.follows.is_empty() {
        format!("from {}", c.follows)
    } else {
        let size = app.cam.part.as_ref().map(|p| {
            let (lo, hi) = cam_paths::stock_box(c, p);
            let e = hi - lo;
            format!("{}×{}×{} · ", cam::fmt_mm((e.x * 10.0).round() / 10.0), cam::fmt_mm((e.y * 10.0).round() / 10.0), cam::fmt_mm((e.z * 10.0).round() / 10.0))
        });
        format!("{}{}", size.unwrap_or_default(), c.stock.material)
    };
    let mut held = vec![];
    if !c.fixtures.is_empty() {
        held.push(format!("{} fixture{}", c.fixtures.len(), if c.fixtures.len() == 1 { "" } else { "s" }));
    }
    if c.soft_jaws.is_some() {
        held.push("soft jaws".into());
    }
    let held = if held.is_empty() { "none".to_string() } else { held.join(", ") };
    let post = cam::POSTS.iter().find(|p| p.0 == c.post).map(|p| p.1).unwrap_or("GRBL");
    let machine = if c.machine.name.is_empty() { format!("custom · {post}") } else { format!("{} · {post}", c.machine.name) };
    let rows = [
        (Page::Stock, "cube", "Stock", stock),
        (Page::WorkZero, "origin", "Work zero", c.origin.label().to_string()),
        (Page::Fixtures, "fix", "Fixtures", held),
        (Page::Machine, "cam", "Machine", machine),
    ];
    let picked = app.cam.sel_op.is_none() && app.cam.sel_tool.is_none();
    for (page, icon, name, note) in rows {
        let sel = picked && app.cam.page == page;
        let (r, _, _) = tree_row(ui, 1, None, icon, name, &note, sel, false, 0.0, false);
        if r.on_hover_text(note).clicked() {
            pick_page(app, page);
        }
    }
}

fn setup_ui(app: &mut App, ui: &mut Ui, page: Page) {
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
    let mut make_jaws = false;
    let mut print_jaws = false;
    let mut goto: Option<String> = None;
    let mut save_machine = None;
    let importing = crate::cam_machine_ui::is_importing(app);
    let mut machine_act = None;
    // the stock settings of the part being machined (one part: its own; several: the defaults)
    let (stock_pref, imperial) = {
        let doc = app.studio_doc(studio);
        let one = match &app.ws.cams[ci].bodies[..] {
            [b] => Some(*b),
            [] if bodies.len() == 1 => Some(bodies[0].0),
            _ => None,
        };
        let pref = one.and_then(|b| doc?.parts.get(&crate::doc::body_key(b))?.stock.clone()).unwrap_or_default();
        let imperial = crate::stock_size::imperial_for(&pref, doc.map(|d| d.units).unwrap_or_default());
        (pref, imperial)
    };
    {
        let c = &mut app.ws.cams[ci];
        let jaw_tab = !c.jaws_of.is_empty();
        if page == Page::Part {
        if jaw_tab {
            ui.label(RichText::new(format!("Machines the soft jaws of {}.", c.jaws_of)).strong());
            ui.label(
                RichText::new("Clamp both jaw blanks in the vise on a spacer as wide as the jaws' gap, tops level. The work zero is on top of the blanks, in the middle of the gap.")
                    .color(theme::muted())
                    .size(12.0),
            );
            if ui.button(format!("Go to {}", c.jaws_of)).clicked() {
                goto = Some(c.jaws_of.clone());
            }
        }
        if !jaw_tab {
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
        row(ui, "Tilt (3+2)", |ui| {
            for (k, axis) in ["X", "Y"].iter().enumerate() {
                ui.add(egui::DragValue::new(&mut c.tilt[k]).speed(0.1).range(-90.0..=90.0).suffix("°").prefix(format!("{axis} ")))
                    .on_hover_text(format!("Tilt the part about the setup's {axis} axis (angle block, tilting vise, compound-angle bore)"));
            }
        });
        }
        }
        if page == Page::WorkZero {
        row(ui, "Work zero", |ui| {
            egui::ComboBox::from_id_salt("cam-origin").width(150.0).selected_text(c.origin.label()).show_ui(ui, |ui| {
                for o in Origin::ALL {
                    ui.selectable_value(&mut c.origin, o, o.label());
                }
            });
        });
        }
        if page == Page::Part {
        if !jaw_tab {
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
        }
        }
        if page == Page::Stock {
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
        let mut standard = false;
        match &mut c.stock.size {
            Some(s) => {
                row(ui, "Size X/Y/Z", |ui| {
                    for v in s.iter_mut() {
                        ui.add(egui::DragValue::new(v).range(0.1..=10000.0).speed(0.5).max_decimals(2));
                    }
                });
                num(ui, "Below part", &mut c.stock.bottom, " mm", 0.0..=1000.0, 0.1);
                row(ui, "", |ui| {
                    standard = ui
                        .add_enabled(app.cam.part.is_some(), egui::Button::new("Standard stock size"))
                        .on_hover_text("The smallest standard plate or bar the part fits in, with the part's stock allowances, faced evenly top and bottom")
                        .clicked();
                });
            }
            None => {
                num(ui, "Sides", &mut c.stock.side, " mm", 0.0..=1000.0, 0.1);
                num(ui, "Top", &mut c.stock.top, " mm", 0.0..=1000.0, 0.1);
                num(ui, "Bottom", &mut c.stock.bottom, " mm", 0.0..=1000.0, 0.1);
            }
        }
        if let (true, Some(p)) = (standard, app.cam.part.as_ref()) {
            let e = p.hi - p.lo;
            let (size, below) = crate::stock_size::for_setup([e.x, e.y, e.z], &stock_pref, imperial);
            c.stock.size = Some(size);
            c.stock.bottom = below;
        }
        }
        row(ui, "Material", |ui| {
            egui::ComboBox::from_id_salt("cam-material").selected_text(c.stock.material.clone()).show_ui(ui, |ui| {
                for m in cam::MATERIALS {
                    ui.selectable_value(&mut c.stock.material, m.0.to_string(), m.0);
                }
            });
        });
        }
        if page == Page::WorkZero {
        num(ui, "Clearance", &mut c.clearance, " mm", 1.0..=500.0, 0.5);
        num(ui, "Retract", &mut c.retract, " mm", 0.1..=100.0, 0.1);
        }
        if page == Page::Fixtures {
        ui.label(RichText::new("Clamps, vise jaws and screws as boxes in work coordinates: toolpaths lift over them and the simulation flags any hit.").color(theme::muted()).size(12.0));
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
        if !jaw_tab {
            ui.add_space(4.0);
            ui.label(RichText::new("SOFT JAWS").color(theme::muted()).size(11.0).strong())
                .on_hover_text("Vise jaws with a pocket machined to the part's shape, for holding it by a side that is already machined (usually after flipping it)");
            let mut on = c.soft_jaws.is_some();
            if ui.checkbox(&mut on, "Hold the part in soft jaws").changed() {
                c.soft_jaws = on.then(|| {
                    let mut sj = cam::SoftJaws::default();
                    // a third of the part's height, at most 8 mm
                    if let Some(p) = &app.cam.part {
                        sj.grip = ((p.hi.z - p.lo.z) / 3.0).clamp(1.0, 8.0).round();
                    }
                    sj
                });
            }
            if let Some(sj) = &mut c.soft_jaws {
                row(ui, "Blank X/Y/Z", |ui| {
                    ui.add(egui::DragValue::new(&mut sj.width).range(5.0..=1000.0).speed(0.5).max_decimals(2)).on_hover_text("Along the vise");
                    ui.add(egui::DragValue::new(&mut sj.thickness).range(3.0..=200.0).speed(0.5).max_decimals(2)).on_hover_text("Front to back, each jaw");
                    ui.add(egui::DragValue::new(&mut sj.height).range(3.0..=200.0).speed(0.5).max_decimals(2)).on_hover_text("Height");
                });
                row(ui, "Mounting", |ui| {
                    let label = sj.mount.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| "plain blanks".into());
                    egui::ComboBox::from_id_salt("cam-jaw-mount").selected_text(label).show_ui(ui, |ui| {
                        if ui.selectable_label(sj.mount.is_none(), "plain blanks").clicked() {
                            sj.mount = None;
                        }
                        for m in cam::JAW_MOUNTS {
                            let on = sj.mount.as_ref().is_some_and(|x| x.name == m.0);
                            if ui.selectable_label(on, m.0).clicked() && !on {
                                if let Some((mount, w, t)) = cam::jaw_mount(m.1) {
                                    sj.mount = Some(mount);
                                    sj.width = w;
                                    sj.thickness = t;
                                }
                            }
                        }
                    });
                });
                if let Some(m) = &mut sj.mount {
                    row(ui, "Holes at X", |ui| {
                        // the text as typed while editing (a lone "-" doesn't parse yet)
                        let id = egui::Id::new("cam-jaw-holes");
                        let mut text = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| m.holes.iter().map(|x| cam::fmt_mm(*x)).collect::<Vec<_>>().join(", "));
                        let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(140.0)).on_hover_text("Hole centres along the jaw from its middle, mm, comma separated");
                        if r.changed() {
                            m.holes = text.split(',').filter_map(|v| v.trim().parse().ok()).collect();
                        }
                        let focused = r.has_focus();
                        ui.data_mut(|d| if focused { d.insert_temp(id, text); } else { d.remove::<String>(id); });
                    });
                    num(ui, "Holes from grip face", &mut m.inset, " mm", 0.0..=200.0, 0.1);
                    row(ui, "Drill / c'bore Ø", |ui| {
                        ui.add(egui::DragValue::new(&mut m.drill).range(0.0..=50.0).speed(0.05).max_decimals(2).suffix(" mm")).on_hover_text("Through hole");
                        ui.add(egui::DragValue::new(&mut m.cbore).range(0.0..=60.0).speed(0.05).max_decimals(2).suffix(" mm")).on_hover_text("Counterbore for the screw head");
                    });
                    num(ui, "Under the head", &mut m.seat, " mm", 0.0..=200.0, 0.1);
                }
                num(ui, "Grip depth", &mut sj.grip, " mm", 0.5..=200.0, 0.1);
                if sj.printed && sj.one_piece {
                    num(ui, "Slit", &mut sj.gap, " mm", 0.0..=100.0, 0.1);
                    num(ui, "Slit stops above base", &mut sj.web, " mm", 0.0..=200.0, 0.1);
                    num(ui, "Slit end rounds", &mut sj.flare, " mm", 0.0..=50.0, 0.1);
                } else {
                    num(ui, "Gap", &mut sj.gap, " mm", 0.0..=100.0, 0.1);
                }
                if ui.checkbox(&mut sj.printed, "3D printed").on_hover_text("Print the jaws instead of machining them: no reliefs for a cutter, and they go to a Part Studio to export as STEP").changed() {
                    sj.clearance = if sj.printed { 0.15 } else { 0.05 };
                }
                if sj.printed {
                    if ui
                        .checkbox(&mut sj.one_piece, "One block")
                        .on_hover_text("Print one block instead of two jaws: it goes in between the vise jaws, sits on the vise bed and is squeezed round the part, with a slit down the middle so it closes on it")
                        .changed()
                    {
                        sj.gap = if sj.one_piece { 1.0 } else { 4.0 };
                    }
                }
                num(ui, "Clearance", &mut sj.clearance, " mm", 0.0..=2.0, 0.01);
                if !sj.printed {
                    num(ui, "Cutter Ø", &mut sj.cutter, " mm", 0.5..=40.0, 0.1);
                }
                if !sj.printed {
                    row(ui, "Jaw material", |ui| {
                        egui::ComboBox::from_id_salt("cam-jaw-material").selected_text(sj.material.clone()).show_ui(ui, |ui| {
                            for m in cam::MATERIALS {
                                ui.selectable_value(&mut sj.material, m.0.to_string(), m.0);
                            }
                        });
                    });
                }
                ui.label(
                    RichText::new(if sj.printed {
                        "The vise closes along Y. The pocket is the part's outline below the jaw tops plus the clearance; the gap is left between the jaws so they can squeeze the part."
                    } else {
                        "The vise closes along Y. The pocket is the part's outline below the jaw tops plus the clearance, with reliefs in corners the cutter can't reach; the gap is the spacer between the jaws while it is cut."
                    })
                        .color(theme::muted())
                        .size(12.0),
                );
            }
            if c.soft_jaws.is_some() {
                if let Some(j) = app.cam.part.as_ref().and_then(|p| crate::cam_jaws::jaws(c, p)) {
                    for w in &j.warnings {
                        ui.label(RichText::new(w.trim_start_matches("soft jaws: ")).color(theme::error_soft()).size(12.0));
                    }
                }
                if c.soft_jaws.as_ref().map(|j| j.printed).unwrap_or(false) {
                    if ui.button("Make printable jaws").on_hover_text("Put both jaws in a Part Studio, standing on their bases with the pocket up, to export as STEP for the slicer").clicked() {
                        print_jaws = true;
                    }
                } else if ui.button("Machine the jaws").on_hover_text("Open the Manufacture tab that cuts the pocket in the jaw blanks (made the first time)").clicked() {
                    make_jaws = true;
                }
            }
        }
        }
        if page == Page::Machine {
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
        machine_act = crate::cam_machine_ui::setup_rows(ui, c, importing);
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
                ui.add(limit_value(v, "")).on_hover_text("Travel the toolpaths are checked against; not set: no check");
            }
        });
        num(ui, "Max spindle", &mut c.max_rpm, " rpm", 1000.0..=100000.0, 100.0);
        num(ui, "Min spindle", &mut c.min_rpm, " rpm", 0.0..=100000.0, 100.0);
        row(ui, "Max feed", |ui| {
            ui.add(limit_value(&mut c.machine.max_feed, " mm/min")).on_hover_text("Feeds above this are capped; not set: no cap");
        });
        num(ui, "Rapid X/Y", &mut c.rapid_rate, " mm/min", 100.0..=100000.0, 50.0);
        row(ui, "Rapid Z", |ui| {
            ui.add(limit_value(&mut c.machine.rapid_z, " mm/min")).on_hover_text("Z rapid rate for cycle times; not set: same as Rapid X/Y");
        });
        num(ui, "Spin-up wait", &mut c.machine.spin_up, " s", 0.0..=60.0, 0.5);
        coolant_combo(ui, "cam-setup-coolant", &mut c.coolant, "auto (flood on metal)");
        ui.checkbox(&mut c.machine.tool_changer, "Automatic tool changer").on_hover_text("Without one the program stops (M0) with a message for you to change tools");
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
        if ui.button("Save as my machine").on_hover_text("New setups start from these machine settings (controller, spindle, machine, coolant, probing, custom G-code) when the document has no setup to copy them from").clicked() {
            save_machine = Some(cam::MachineSettings::of(c));
        }
        }
    }
    match machine_act {
        Some(crate::cam_machine_ui::Action::Import) => crate::cam_machine_ui::start_import(app, &ui.ctx().clone(), ci),
        Some(crate::cam_machine_ui::Action::Edit) => {
            let n = app.ws.cams[ci].machine.model.clone();
            crate::cam_machine_ui::edit(app, &n);
        }
        None => {}
    }
    if let Some(m) = save_machine {
        match cam::save_machine(&m) {
            Ok(p) => app.flash_info(format!("Saved the machine to {}", p.display()), Some(std::time::Duration::from_secs(3))),
            Err(e) => app.flash(format!("Could not save the machine: {e}"), ToastKind::Error),
        }
    }
    if print_jaws {
        let c = app.ws.cams[ci].clone();
        let Some(j) = app.cam.part.as_ref().and_then(|p| crate::cam_jaws::jaws(&c, p)) else { return };
        let doc = crate::cam_jaws::print_doc(&j);
        let name = format!("{} jaws (print)", c.name);
        let i = match app.ws.studios.iter().position(|s| s.name == name) {
            Some(i) => {
                app.ws.studios[i].doc = doc.clone();
                app.studio_results.remove(&i);
                i
            }
            None => {
                app.ws.studios.push(crate::workspace::Studio { name, doc: doc.clone() });
                app.ws.studios.len() - 1
            }
        };
        if i == app.cur_studio {
            app.doc = doc;
            app.dirty_model = true;
        }
        app.switch_tab(Tab::Studio(i));
        return;
    }
    if make_jaws || goto.is_some() {
        let c = app.ws.cams[ci].clone();
        let found = match &goto {
            Some(n) => app.ws.cams.iter().position(|x| &x.name == n),
            None => app.ws.cams.iter().position(|x| x.jaws_of == c.name),
        };
        let i = match found {
            Some(i) => i,
            None if make_jaws => {
                let mut j = crate::cam_jaws::jaw_setup(&c);
                let base = j.name.clone();
                let mut n = 2;
                while app.ws.cams.iter().any(|x| x.name == j.name) {
                    j.name = format!("{base} {n}");
                    n += 1;
                }
                app.ws.cams.push(j);
                app.ws.cams.len() - 1
            }
            None => return,
        };
        app.switch_tab(Tab::Cam(i));
        return;
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
        let name = side_name(&c.name, &app.ws.cams);
        let next = Cam { name, follows: c.name.clone(), orient: flip.into(), tilt: [0.0; 2], ops: vec![], stock: cam::Stock { carried: None, ..c.stock.clone() }, soft_jaws: None, jaws_of: String::new(), ..c };
        app.ws.cams.push(next);
        let t = Tab::Cam(app.ws.cams.len() - 1);
        app.switch_tab(t);
        return;
    }
    if let Some(i) = need_build {
        ensure_built(app, i);
    }
    if page == Page::Part && app.studio_bodies(studio).is_empty() {
        ui.label(RichText::new("This Part Studio has no parts yet").color(theme::error_soft()));
    }
}

/// A name for a setup that carries on from `base`: "<base> side 2", or side 3, 4… when that is taken.
fn side_name(base: &str, cams: &[Cam]) -> String {
    (2..).map(|k| format!("{base} side {k}")).find(|n| !cams.iter().any(|c| &c.name == n)).unwrap_or_default()
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

pub(crate) fn kind_icon(k: OpKind) -> &'static str {
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
    let open = !app.cam.folded.contains("tools");
    let (r, _, slot) = tree_row(ui, 1, Some(open), "camTools", "Tools", &tools.len().to_string(), false, false, 44.0, true);
    if r.clicked() {
        fold(app, "tools");
    }
    if let Some(slot) = slot {
        in_slot(ui, slot, |ui| {
            ui.menu_image_button(icons::image("plus", 13.0).tint(theme::muted()), |ui| {
                for k in ToolKind::ALL {
                    if ui.button(k.label()).clicked() {
                        let (material, max_rpm, min_rpm) = cam(app).map(|c| (c.stock.material.clone(), c.max_rpm, c.min_rpm)).unwrap_or_default();
                        let mut t = cam::presets().into_iter().find(|t| t.kind == k).unwrap_or(Tool { kind: k, ..Default::default() });
                        cam::suggest(&mut t, &material, max_rpm, min_rpm);
                        let mut id = 0;
                        edit(app, |c| id = c.add_tool(t));
                        app.cam.sel_tool = Some(id);
                        app.cam.sel_op = None;
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text("New tool");
            if small_icon(ui, "library", "Tool library", true) {
                app.cam.library_open = true;
            }
        });
    }
    if !open {
        return;
    }
    if tools.is_empty() {
        hint(ui, "No tools yet: open the tool library, or add an operation and a suitable tool comes with it.");
    }
    let mut delete = None;
    for (id, n, name, _) in &tools {
        let sel = app.cam.sel_tool == Some(*id);
        let (r, _, slot) = tree_row(ui, 2, None, "camTools", &format!("T{n}  {name}"), "", sel, false, 20.0, false);
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
        if let Some(slot) = slot {
            if in_slot(ui, slot, |ui| small_icon(ui, "delete", "Delete tool", true)) {
                delete = Some(*id);
            }
        }
    }
    if let Some(id) = delete {
        edit(app, |c| c.tools.retain(|t| t.id != id));
        if app.cam.sel_tool == Some(id) {
            app.cam.sel_tool = None;
        }
    }
}

/// A muted note in the tree, under a branch.
fn hint(ui: &mut Ui, text: &str) {
    egui::Frame::new().inner_margin(Margin { left: 40, right: 6, top: 2, bottom: 4 }).show(ui, |ui| {
        ui.label(RichText::new(text).color(theme::muted()).size(12.0));
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
    let open = !app.cam.folded.contains("ops");
    let total = cam(app).map(|c| cam_paths::cycle_time(c, &app.cam.paths)).unwrap_or_default();
    let note = if app.cam.computing { "updating…".to_string() } else if app.cam.paths.is_empty() { ops.len().to_string() } else { fmt_time(total) };
    let (r, _, slot) = tree_row(ui, 1, Some(open), "features", "Operations", &note, false, false, 24.0, false);
    if r.on_hover_text(if app.cam.paths.is_empty() { String::new() } else { format!("Cycle time {}", fmt_time(total)) }).clicked() {
        fold(app, "ops");
    }
    if let Some(slot) = slot {
        in_slot(ui, slot, |ui| templates_ui(app, ui));
    }
    if !open {
        return;
    }
    if ops.is_empty() {
        hint(ui, "Add operations from the toolbar: face, clear, pocket, contour, chamfer, drill, bore, thread, tap.");
    }
    let mut action: Option<(&str, u64)> = None;
    let n = ops.len();
    for (i, (id, kind, name, sup, tool)) in ops.iter().enumerate() {
        let tp = app.cam.paths.iter().find(|p| p.op == *id);
        let sel = app.cam.sel_op == Some(*id);
        let warn = tp.map(|t| t.warnings.join("\n")).filter(|w| !w.is_empty());
        let note = tp.map(|t| fmt_time(t.time)).unwrap_or_default();
        let note = if warn.is_some() { format!("⚠ {note}") } else { note };
        let (r, _, slot) = tree_row(ui, 2, None, kind_icon(*kind), &format!("{name} · {tool}"), &note, sel, *sup, 40.0, false);
        let r = match &warn {
            Some(w) => r.on_hover_text(w),
            None => r,
        };
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
        if let Some(slot) = slot {
            in_slot(ui, slot, |ui| {
                if small_icon(ui, "down", "Move down", i + 1 < n) {
                    action = Some(("down", *id));
                }
                if small_icon(ui, "up", "Move up", i > 0) {
                    action = Some(("up", *id));
                }
            });
        }
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
}

/// Save the setup's operations as a template, or add a saved template's operations.
fn templates_ui(app: &mut App, ui: &mut Ui) {
    let Some(has_ops) = cam(app).map(|c| c.ops.iter().any(|o| !o.suppressed)) else { return };
    let mut save = false;
    let mut add = None;
    let mut gone = None;
    // a click in the name box must not close it: only a click outside does
    let keep_open = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    let button = egui::containers::menu::MenuButton::from_button(egui::Button::image(icons::image("file", 13.0).tint(theme::muted())).frame(false));
    let (r, _) = button.config(keep_open).ui(ui, |ui| {
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
    r.on_hover_text("Templates: save these operations, or add saved ones");
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
    egui::ComboBox::from_id_salt("cam-op-tool").width((ui.available_width() - 24.0).clamp(80.0, 170.0)).selected_text(cur).show_ui(ui, |ui| {
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
            let thin = tool.as_ref().map(|t| cam::chip_thinning(cam_paths::stepover(&o, t) / t.diameter.max(1e-9))).unwrap_or(1.0);
            row(ui, "Chip thinning", |ui| {
                ui.checkbox(&mut o.chip_thinning, format!("feed ×{thin:.2} on the loops")).on_hover_text(
                    "The loops take a thin cut, so the chips come out thinner than the feed per tooth: raise the feed to keep the chip load. \
                     Kept on the safe side: at most ×1.5, and not on the helix entries",
                );
            });
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
            opt(ui, "Top", &mut o.top, stock_top, " mm", "the top of the stock");
            opt(ui, "Bottom", &mut o.bottom, part_bottom, " mm", "every edge the tool reaches");
            num(ui, "Sharper than", &mut o.edge_angle, "°", 5.0..=170.0, 1.0);
            row(ui, "Edges", |ui| {
                if o.edges.is_empty() && o.edge_picks.is_empty() {
                    ui.label(RichText::new("every sharp edge").color(theme::muted()).size(12.0));
                } else {
                    ui.label(format!("{} picked", o.edges.len() + o.edge_picks.len()));
                    if ui.small_button("All edges").clicked() {
                        o.edges.clear();
                        o.edge_picks.clear();
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
    let (max_rpm, min_rpm) = (app.ws.cams[ci].max_rpm, app.ws.cams[ci].min_rpm);
    // the other tools' numbers, which this one must not share
    let others: Vec<(u32, String)> = app.ws.cams[ci].tools.iter().filter(|t| t.id != id).map(|t| (t.number, t.describe())).collect();
    let Some(t) = app.ws.cams[ci].tools.iter_mut().find(|t| t.id == id) else { return };
    ui.label(RichText::new(t.describe()).color(theme::muted()).size(12.0));
    tool_preview(ui, t);
    row(ui, "Name", |ui| {
        ui.text_edit_singleline(&mut t.name);
    });
    row(ui, "Number", |ui| {
        ui.add(egui::DragValue::new(&mut t.number).range(1..=999).prefix("T"));
    });
    if let Some(other) = others.iter().find(|o| o.0 == t.number) {
        ui.label(RichText::new(format!("T{} is taken by {}: give each tool its own number", t.number, other.1)).color(theme::error_soft()).size(12.0));
    }
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
        ToolKind::Tap => {
            num(ui, "Pitch", &mut t.pitch, " mm", 0.1..=10.0, 0.05);
            ui.checkbox(&mut t.tapping_head, "Self-reversing tapping head").on_hover_text(
                "The tap sits in a tapping head that reverses it when the spindle pulls back: the G-code just feeds in and out with the spindle running forward (no G84, no spindle reversal)",
            );
            if t.tapping_head {
                num(ui, "Reverse ratio", &mut t.reverse_ratio, "×", 0.5..=4.0, 0.05);
            }
        }
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
            cam::suggest(t, &material, max_rpm, min_rpm);
        }
    });
    num(ui, "Spindle", &mut t.rpm, " rpm", 10.0..=100000.0, 50.0);
    if max_rpm > 0.0 && t.rpm > max_rpm {
        ui.label(RichText::new(format!("Runs at the spindle's {} rpm, feeds scaled to {}%", max_rpm.round(), (max_rpm / t.rpm * 100.0).round())).color(theme::error_soft()).size(12.0));
    } else if min_rpm > 0.0 && t.rpm < min_rpm {
        ui.label(RichText::new(format!("Below the spindle's {} rpm minimum: little torque", min_rpm.round())).color(theme::error_soft()).size(12.0));
    }
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
    coolant_combo(ui, "cam-tool-coolant", &mut t.coolant, "the setup's");
}

fn coolant_combo(ui: &mut Ui, id: &str, v: &mut cam::Coolant, auto: &str) {
    row(ui, "Coolant", |ui| {
        let label = |c: cam::Coolant| if c == cam::Coolant::Auto { auto.to_string() } else { c.label().to_string() };
        egui::ComboBox::from_id_salt(id).selected_text(label(*v)).show_ui(ui, |ui| {
            for c in cam::Coolant::ALL {
                ui.selectable_value(v, c, label(c));
            }
        });
    });
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
    egui::Window::new("Tool library").open(&mut open).default_size([420.0, 480.0]).default_pos(ctx.content_rect().center() - egui::vec2(210.0, 240.0)).constrain(true).collapsible(false).show(ctx, |ui| {
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
                if let Some(p) = app.open_file_dialog("Tool library", &["json", "tools"]) {
                    imported = Some(crate::cam_tool_import::import_file(&p));
                }
            }
        });
        ui.separator();
        let setup = cam(app).cloned();
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
                    // the speed and feed it gets in this setup: suggested for its stock and spindle, capped at the top speed
                    let (rpm, feed, low) = match &setup {
                        Some(c) => {
                            let mut a = t.clone();
                            if a.rpm <= 0.0 || !*own {
                                cam::suggest(&mut a, &c.stock.material, c.max_rpm, c.min_rpm);
                            }
                            let rpm = c.spindle_rpm(&a);
                            (rpm, a.feed * rpm / a.rpm.max(1.0), c.min_rpm > 0.0 && rpm < c.min_rpm)
                        }
                        None => (t.rpm, t.feed, false),
                    };
                    let r = ui.label(RichText::new(format!("{} rpm · {} mm/min", rpm.round(), feed.round())).color(if low { theme::error_soft() } else { theme::muted() }).size(11.5));
                    if low {
                        r.on_hover_text("Below the spindle's minimum speed");
                    }
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
        let (material, max_rpm, min_rpm) = cam(app).map(|c| (c.stock.material.clone(), c.max_rpm, c.min_rpm)).unwrap_or_default();
        let mut last = 0;
        edit(app, |c| {
            for mut t in add {
                // the user's own tools keep their feeds and speeds when they have them
                if t.rpm <= 0.0 || cam::presets().iter().any(|p| p.name == t.name) {
                    cam::suggest(&mut t, &material, max_rpm, min_rpm);
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
    crate::cam_post::gcode_noted(c, paths, ctx.origin, ctx.safe(), Some(cam_paths::work_zero_note(c, part)))
}

pub(crate) fn export_dialog(app: &mut App) {
    let (Some(c), Some(part)) = (cam(app).cloned(), app.cam.part.clone()) else { return };
    if app.cam.computing {
        app.flash("Toolpaths are still being made", ToastKind::Info);
        return;
    }
    let ext = match c.post.as_str() {
        "fanuc" => "nc",
        "linuxcnc" => "ngc",
        "mach3" => "tap",
        "rcnc" => "nc",
        _ => "gcode",
    };
    let name: String = c.name.chars().map(|ch| if ch.is_ascii_alphanumeric() || "-_".contains(ch) { ch } else { '_' }).collect();
    let Some(p) = app.save_file_dialog("G-code", &[ext, "nc", "gcode", "ngc", "tap"], format!("{name}.{ext}")) else { return };
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

pub(crate) fn send_to_rcnc(app: &mut App) {
    let (Some(c), Some(part)) = (cam(app).cloned(), app.cam.part.clone()) else { return };
    if app.cam.computing {
        app.flash("Toolpaths are still being made", ToastKind::Info);
        return;
    }
    // the Mach3 post probes and dwells on top of what rcnc's M6 and M3 already do, and posts G84
    if c.post != "rcnc" {
        app.flash("Set the controller to rcnc in the machine settings first", ToastKind::Error);
        return;
    }
    let name: String = c.name.chars().map(|ch| if ch.is_ascii_alphanumeric() || "-_".contains(ch) { ch } else { '_' }).collect();
    let text = gcode_for(&c, &part, &app.cam.paths);
    let mut body = crate::rcnc::payload(&c, &app.cam.paths, &format!("{name}.nc"), text);
    // rcnc shows the setup sheet beside the program
    let sim = crate::cam_sheet::new_sim(&c, &part, &app.cam.paths, app.cam.stock_in.as_deref());
    let vis = crate::cam_sheet::visuals(&c, &part, &app.cam.paths, Some(sim));
    body["setup_sheet"] = crate::cam_sheet::html(&c, &part, &app.cam.paths, &vis).into();
    match crate::rcnc::send(&body) {
        Ok(msg) => {
            // what the sheet's own simulation found, not whatever the Simulate tab last ran
            let crashes = vis.events.len();
            if crashes > 0 {
                app.flash(format!("{msg} · the simulation found {crashes} problem(s)"), ToastKind::Error);
            } else {
                app.flash_info(msg, Some(std::time::Duration::from_secs(3)));
            }
        }
        Err(e) => app.flash(e, ToastKind::Error),
    }
}

pub(crate) fn sheet_dialog(app: &mut App) {
    let (Some(c), Some(part)) = (cam(app).cloned(), app.cam.part.clone()) else { return };
    if app.cam.computing {
        app.flash("Toolpaths are still being made", ToastKind::Info);
        return;
    }
    let name: String = c.name.chars().map(|ch| if ch.is_ascii_alphanumeric() || "-_".contains(ch) { ch } else { '_' }).collect();
    let Some(p) = app.save_file_dialog("Web page", &["html", "htm"], format!("{name} setup sheet.html")) else { return };
    let sim = crate::cam_sheet::new_sim(&c, &part, &app.cam.paths, app.cam.stock_in.as_deref());
    match std::fs::write(&p, crate::cam_sheet::sheet(&c, &part, &app.cam.paths, Some(sim))) {
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
    if !c.machine.model.is_empty() {
        match crate::cam_machine::load(&c.machine.model) {
            Ok(l) => {
                // placing a detailed machine takes a moment: keep it while nothing it depends on changes
                let key = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    (Arc::as_ptr(&l) as usize).hash(&mut h);
                    for v in [stock.0, stock.1, DVec3::from(c.on_table)] {
                        v.to_array().map(f64::to_bits).hash(&mut h);
                    }
                    serde_json::to_string(&(&c.fixtures, &c.soft_jaws)).unwrap_or_default().hash(&mut h);
                    h.finish()
                };
                let placed = match &app.cam.placed {
                    Some((k, p)) if *k == key => p.clone(),
                    _ => {
                        let p = Arc::new(Sim::place_machine(&c, &part, stock, l));
                        app.cam.placed = Some((key, p.clone()));
                        p
                    }
                };
                sim.set_machine(&c, &part, placed);
            }
            Err(e) => app.flash(e, ToastKind::Error),
        }
    }
    app.cam.sim_target = if run { sim.total() } else { 0 };
    app.cam.sim = Some(sim);
    app.cam.sim_mesh_done = usize::MAX;
    app.cam.playing = false;
    app.cam.scene_dirty = true;
}

/// The operation and the kind of move the simulation is at: "Contour 1 (T1) · feed to X12 Y-3 Z-4 · move 812 of 2400".
fn sim_step_text(app: &App) -> Option<String> {
    let sim = app.cam.sim.as_ref()?;
    let k = sim.done.min(sim.total().checked_sub(1)?);
    let (op, m) = sim.move_at(k)?;
    let c = cam(app)?;
    let op = c.op(op)?;
    let tool = c.tool(op.tool).map(|t| format!(" (T{})", t.number)).unwrap_or_default();
    let o = app.cam.part.as_ref().map(|p| cam_paths::work_origin(c, p)).unwrap_or_default();
    let at = |p: [f64; 3]| format!("X{} Y{} Z{}", cam::fmt_mm(p[0] - o.x), cam::fmt_mm(p[1] - o.y), cam::fmt_mm(p[2] - o.z));
    let what = match m {
        Move::Rapid(p) => format!("rapid to {}", at(p)),
        Move::Feed(p, f) => format!("feed to {} at {}", at(p), f.round()),
        Move::Tap { z, .. } => format!("tap to Z{}", cam::fmt_mm(z - o.z)),
        Move::Dwell(s) => format!("dwell {} s", cam::fmt_mm(s)),
    };
    Some(format!("{}{tool} · {what} · move {} of {}", op.name, k + 1, sim.total()))
}

fn sim_ui(app: &mut App, ui: &mut Ui) {
    let Some(sim) = &app.cam.sim else { return };
    let total = sim.total();
    let done = sim.done;
    let events = sim.events.clone();
    let removed = sim.removed;
    if app.cam.speed <= 0.0 {
        app.cam.speed = 200.0;
    }
    row(ui, "Speed", |ui| {
        ui.add(egui::Slider::new(&mut app.cam.speed, 1.0..=2000.0).logarithmic(true).suffix("/s").max_decimals(0)).on_hover_text("Moves per second");
    });
    ui.label(RichText::new(format!("Move {done} of {total} · {:.1} cm³ removed", removed / 1000.0)).color(theme::muted()).size(12.0));
    if let Some(now) = sim_step_text(app) {
        ui.label(RichText::new(now).size(12.0));
    }
    if ui.checkbox(&mut app.cam.show_part, "Show the part").on_hover_text("Show the finished part, see-through, over the stock").changed() {
        app.cam.scene_dirty = true;
    }
    crate::cam_machine_ui::sim_rows(app, ui);
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
                    ui.add(egui::Button::new(RichText::new(format!("{op}: {} ({} mm)", if e.what.is_empty() { e.kind.label() } else { &e.what }, cam::fmt_mm((e.depth * 100.0).round() / 100.0))).size(12.0)).frame(false));
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
    // (a tab that machines soft jaws shows the jaws instead)
    let jaw_tab = !c.jaws_of.is_empty();
    let bodies: &[BodyMesh] = if (sim_on && !app.cam.show_part) || jaw_tab { &[] } else { app.studio_bodies(c.studio) };
    // everything is drawn as it sits on the machine, spindle up: the parts turned by the setup's rotation
    let rot = app.cam.part.as_ref().map(|p| p.rot.as_mat3()).unwrap_or(glam::Mat3::IDENTITY);
    for m in bodies {
        let hidden = doc.and_then(|d| d.parts.get(&crate::doc::body_key(m.id))).map(|p| p.hidden).unwrap_or(false);
        if if c.bodies.is_empty() { hidden } else { !c.bodies.contains(&m.id) } {
            continue;
        }
        let col = doc.and_then(|d| d.parts.get(&crate::doc::body_key(m.id))).and_then(|p| p.color).map(|c| Color32::from_rgb(c[0], c[1], c[2])).unwrap_or(vp::body());
        for t in m.indices.chunks(3) {
            let p = |i: u32| rot * Vec3::new(m.positions[3 * i as usize], m.positions[3 * i as usize + 1], m.positions[3 * i as usize + 2]);
            s.tri(sim_on, [p(t[0]), p(t[1]), p(t[2])], rgba(col, if sim_on { 0.25 } else { 1.0 }));
        }
        if !sim_on {
            for e in &m.edges {
                let pts: Vec<Vec3> = e.pts.iter().map(|p| rot * Vec3::from(*p)).collect();
                s.polyline(&pts, 1.2, rgba(vp::edge(), 1.0), false);
            }
        }
    }
    let Some(part) = &app.cam.part else { return s };
    let to = |p: DVec3| p.as_vec3();
    if jaw_tab && (!sim_on || app.cam.show_part) {
        let col = rgba(Color32::from_rgb(0x9f, 0xb8, 0xd8), if sim_on { 0.25 } else { 1.0 });
        for t in &part.tris {
            s.tri(sim_on, [to(t[0]), to(t[1]), to(t[2])], col);
        }
    }
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
    for (_, lo, hi) in cam_paths::fixture_boxes(c, part, o).into_iter().filter(|f| !f.0.starts_with("Soft jaw")) {
        let corner = |k: usize| to(DVec3::new([lo.x, hi.x][k & 1], [lo.y, hi.y][(k >> 1) & 1], [lo.z, hi.z][(k >> 2) & 1]));
        for (a, b) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
            s.line(corner(a), corner(b), 1.6, fix_col);
        }
        for f in [[0, 1, 3], [0, 3, 2], [4, 5, 7], [4, 7, 6], [0, 1, 5], [0, 5, 4], [2, 3, 7], [2, 7, 6], [0, 2, 6], [0, 6, 4], [1, 3, 7], [1, 7, 5]] {
            s.tri(true, [corner(f[0]), corner(f[1]), corner(f[2])], rgba(Color32::from_rgb(0xe0, 0x5a, 0x2a), 0.25));
        }
    }
    // soft jaws, shaded with their edges
    if let Some(j) = crate::cam_jaws::jaws(c, part) {
        let col = rgba(Color32::from_rgb(0x6f, 0x9f, 0xd8), 0.55);
        for t in crate::cam_jaws::jaw_tris(&j) {
            s.tri(true, [to(t[0]), to(t[1]), to(t[2])], col);
        }
        let edge = rgba(Color32::from_rgb(0x3a, 0x6c, 0xb0), 0.95);
        for (a, b) in j.rects {
            let corner = |k: usize| to(DVec3::new([a.x, b.x][k & 1], [a.y, b.y][(k >> 1) & 1], [j.bottom, j.top][(k >> 2) & 1]));
            for (p, q) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
                s.line(corner(p), corner(q), 1.2, edge);
            }
        }
        for sh in &j.pocket {
            for cn in sh {
                for i in 0..cn.len() {
                    let (p, q) = (cn[i], cn[(i + 1) % cn.len()]);
                    s.line(to(DVec3::new(p[0], p[1], j.top)), to(DVec3::new(q[0], q[1], j.top)), 1.2, edge);
                }
            }
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
        for e in crate::cam_3d::deburr_picks(op, part, origin) {
            s.point(to(e), 9.0, rgba(theme::accent(), 1.0));
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
        crate::cam_machine_ui::draw(app, &mut s);
    }
    s
}

pub fn viewport(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    sync_setup(app);
    if let Some(c) = cam(app) {
        let s = c.studio;
        ensure_built(app, s);
    }
    refresh(app, &ctx);
    step_sim(app, &ctx);
    library_window(app, &ctx);
    crate::cam_machine_ui::window(app, &ctx);
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
    // only while the deburr picking edges is the one shown
    let picking = app.cam.pick_edges.filter(|id| app.cam.sel_op == Some(*id) && cam(app).and_then(|c| c.op(*id)).is_some_and(|o| o.kind == OpKind::Deburr));
    if app.cam.pick_edges.is_some() && picking.is_none() {
        app.cam.pick_edges = None;
    }
    if let (Some(id), true) = (picking, resp.clicked()) {
        if let (Some(pos), Some(part), Some(c)) = (resp.interact_pointer_pos(), app.cam.part.clone(), cam(app)) {
            let local = Vec2::new(pos.x - rect.left(), pos.y - rect.top());
            let origin = cam_paths::work_origin(c, &part);
            let angle = c.op(id).map(|o| o.edge_angle).unwrap_or(30.0);
            let scr = |p: DVec3| app.camera.project(p.as_vec3());
            // the picks given in work coordinates come first, then those picked here (part coordinates)
            let (n_work, existing) = c.op(id).map(|o| (o.edges.len(), crate::cam_3d::deburr_picks(o, &part, origin))).unwrap_or_default();
            // clicking a pick again removes it
            if let Some(k) = existing.iter().position(|e| scr(*e).distance(local) < 8.0) {
                edit(app, |c| {
                    if let Some(o) = c.ops.iter_mut().find(|o| o.id == id) {
                        if k < n_work {
                            o.edges.remove(k);
                        } else {
                            o.edge_picks.remove(k - n_work);
                        }
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
                    // kept on the part, so changes to the stock or the work zero don't move it off its edge
                    let q = (part.to_part(p) * 1000.0).round() / 1000.0;
                    edit(app, |c| {
                        if let Some(o) = c.ops.iter_mut().find(|o| o.id == id) {
                            o.edge_picks.push(q.to_array());
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
    let total = cam(app).map(|c| cam_paths::cycle_time(c, &app.cam.paths)).unwrap_or_default();
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
                if ui.add(egui::Button::image(icons::image("rollback", 14.0).tint(theme::text())).frame(false)).on_hover_text("Back to the start").clicked() {
                    app.cam.sim_target = 0;
                    app.cam.playing = false;
                }
                let mut t = app.cam.sim_target as f64;
                ui.spacing_mut().slider_width = (strip.width() - 190.0).max(60.0);
                let r = ui.add(egui::Slider::new(&mut t, 0.0..=total.max(1) as f64).show_value(false));
                if r.changed() {
                    app.cam.sim_target = t as usize;
                    app.cam.playing = false;
                }
                if let Some(now) = sim_step_text(app) {
                    r.on_hover_text(now);
                }
                if ui.small_button("⏭").on_hover_text("Run to the end").clicked() {
                    app.cam.sim_target = total;
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
    sim_card(app, ui, rect);
    let _ = resp;
}

/// The simulation's results on a card at the viewport's right: speed, what was cut, the colour key and any problems.
fn sim_card(app: &mut App, ui: &mut Ui, rect: egui::Rect) {
    if !app.cam.sim_on || app.cam.sim.is_none() {
        app.cam.sim_card = None;
        return;
    }
    // under the view cube, clear of the playback strip
    let w = 270.0_f32.min(rect.width() * 0.45);
    let top = rect.top() + 180.0;
    let room = egui::Rect::from_min_max(Pos2::new(rect.right() - 12.0 - w, top), Pos2::new(rect.right() - 12.0, (rect.bottom() - 100.0).max(top + 80.0)));
    if let Some(r) = app.cam.sim_card {
        // drags on the card's background stay off the view
        ui.interact(r, egui::Id::new("cam-sim-card"), Sense::click_and_drag());
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(room));
    let r = egui::Frame::new().fill(theme::window()).stroke(Stroke::new(1.0, theme::border())).corner_radius(10).inner_margin(Margin::same(10)).show(&mut child, |ui| {
        ui.set_width(w - 22.0);
        ui.set_max_width(w - 22.0);
        ui.spacing_mut().slider_width = (w - 200.0).max(60.0);
        ui.label(RichText::new("SIMULATION").color(theme::muted()).size(11.0).strong());
        ui.add_space(4.0);
        egui::ScrollArea::vertical().id_salt("cam-sim-card").max_height(room.height() - 44.0).show(ui, |ui| sim_ui(app, ui));
    });
    app.cam.sim_card = Some(r.response.rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_names_skip_taken_ones() {
        let cams = |names: &[&str]| names.iter().map(|n| Cam { name: n.to_string(), ..Default::default() }).collect::<Vec<_>>();
        assert_eq!(side_name("Manufacture 1", &cams(&["Manufacture 1"])), "Manufacture 1 side 2");
        // taken names, among them the old fallback "Manufacture <count + 1>", no longer loop forever
        let c = cams(&["Manufacture 1", "Manufacture 1 side 2", "Manufacture 4"]);
        assert_eq!(side_name("Manufacture 1", &c), "Manufacture 1 side 3");
    }
}
