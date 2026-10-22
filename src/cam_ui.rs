//! Manufacture tab: setup, tool library, operations, toolpath preview and simulation.

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use egui::{Color32, Margin, Pos2, RichText, Sense, Stroke, Ui};
use glam::{DVec3, Vec2, Vec3};

use crate::app::{App, ToastKind};
use crate::cam::{self, Cam, OpKind, Operation, Origin, Side, Tool, ToolKind};
use crate::cam_paths::{self, Circle, Move, Part, Toolpath};
use crate::cam_sim::Sim;
use crate::doc::{BodyId, Document};
use crate::icons;
use crate::kernel::BodyMesh;
use crate::render::{SceneData, ViewportCallback};
use crate::theme::{self, vp};
use crate::workspace::Tab;

type Job = (u64, Arc<Part>, Vec<Toolpath>);

#[derive(Default)]
pub struct CamUi {
    pub scene_dirty: bool,
    /// Last computed toolpaths and the input they were made from.
    pub key: u64,
    pub part: Option<Arc<Part>>,
    pub paths: Vec<Toolpath>,
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

/// Triangles and circular edges of the parts a setup machines (part coordinates).
pub fn part_input(bodies: &[BodyMesh], doc: Option<&Document>, sel: &[BodyId]) -> (Vec<[DVec3; 3]>, Vec<Circle>) {
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
    (tris, circles)
}

fn input_key(c: &Cam, tris: &[[DVec3; 3]]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(c).unwrap_or_default().hash(&mut h);
    tris.len().hash(&mut h);
    let mut s = 0.0f64;
    for (k, t) in tris.iter().enumerate().step_by((tris.len() / 512).max(1)) {
        s += (t[0].x + t[1].y * 3.0 + t[2].z * 7.0) * (k as f64 + 1.0);
    }
    s.to_bits().hash(&mut h);
    h.finish()
}

/// Keep the toolpaths up to date: start a background job when the setup or the parts change.
fn refresh(app: &mut App, ctx: &egui::Context) {
    if let Some((k, rx)) = &app.cam.job {
        if let Ok((key, part, paths)) = rx.try_recv() {
            if key == *k {
                app.cam.key = key;
                app.cam.part = Some(part);
                app.cam.paths = paths;
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
    let Some(c) = cam(app).cloned() else { return };
    let (tris, circles) = part_input(app.studio_bodies(c.studio), app.studio_doc(c.studio), &c.bodies);
    let key = input_key(&c, &tris);
    if key == app.cam.key || app.cam.job.as_ref().map(|j| j.0 == key).unwrap_or(false) {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    app.cam.job = Some((key, rx));
    app.cam.computing = true;
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let part = Part::new(&tris, &circles, &c.orient);
        let paths = cam_paths::generate(&c, &part);
        let _ = tx.send((key, Arc::new(part), paths));
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
        for (k, icon) in [(OpKind::Adaptive, "camAdaptive"), (OpKind::Parallel, "camParallel"), (OpKind::Waterline, "camWaterline"), (OpKind::Scallop, "camScallop"), (OpKind::Deburr, "camDeburr")] {
            if app.tool_button(ui, icon, k.label(), k.description(), matches!(k, OpKind::Adaptive | OpKind::Parallel), false, true).clicked() {
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
    });
}

fn add_op(app: &mut App, k: OpKind) {
    if cam(app).map(|c| !c.tools.iter().any(|t| k.accepts(t.kind))).unwrap_or(true) {
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
    edit(app, |c| id = c.add_op(k, 0));
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
    let mut need_build = None;
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
        let mut fixed = c.stock.size.is_some();
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
        row(ui, "Material", |ui| {
            egui::ComboBox::from_id_salt("cam-material").selected_text(c.stock.material.clone()).show_ui(ui, |ui| {
                for m in cam::MATERIALS {
                    ui.selectable_value(&mut c.stock.material, m.0.to_string(), m.0);
                }
            });
        });
        num(ui, "Clearance", &mut c.clearance, " mm", 1.0..=500.0, 0.5);
        num(ui, "Retract", &mut c.retract, " mm", 0.1..=100.0, 0.1);
        row(ui, "Machine", |ui| {
            egui::ComboBox::from_id_salt("cam-post").selected_text(cam::POSTS.iter().find(|p| p.0 == c.post).map(|p| p.1).unwrap_or("GRBL")).show_ui(ui, |ui| {
                for (k, l) in cam::POSTS {
                    ui.selectable_value(&mut c.post, k.to_string(), l);
                }
            });
        });
        num(ui, "Max spindle", &mut c.max_rpm, " rpm", 1000.0..=100000.0, 100.0);
        num(ui, "Rapid rate", &mut c.rapid_rate, " mm/min", 100.0..=100000.0, 50.0);
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
            if o.kind != OpKind::Parallel {
                row(ui, "Direction", |ui| {
                    ui.radio_value(&mut o.climb, true, "climb");
                    ui.radio_value(&mut o.climb, false, "conventional");
                });
            }
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
    num(ui, "Holder Ø", &mut t.holder_diameter, " mm", 1.0..=300.0, 0.5);
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
    if !add.is_empty() {
        let (material, max_rpm) = cam(app).map(|c| (c.stock.material.clone(), c.max_rpm)).unwrap_or_default();
        let mut last = 0;
        edit(app, |c| {
            for mut t in add {
                cam::suggest(&mut t, &material, max_rpm);
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
    let safe = ctx.stock.1.z.max(part.hi.z) + c.clearance;
    crate::cam_post::gcode(c, paths, ctx.origin, safe)
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

// ---------------------------------------------------------------- simulation

fn start_sim(app: &mut App, run: bool) {
    let (Some(c), Some(part)) = (cam(app).cloned(), app.cam.part.clone()) else { return };
    let stock = cam_paths::stock_box(&c, &part);
    let sim = Sim::new(&c, &part, stock, &app.cam.paths, 500);
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
    // parts
    let doc = app.studio_doc(c.studio);
    for m in app.studio_bodies(c.studio) {
        let hidden = doc.and_then(|d| d.parts.get(&crate::doc::body_key(m.id))).map(|p| p.hidden).unwrap_or(false);
        if if c.bodies.is_empty() { hidden } else { !c.bodies.contains(&m.id) } {
            continue;
        }
        let col = doc.and_then(|d| d.parts.get(&crate::doc::body_key(m.id))).and_then(|p| p.color).map(|c| Color32::from_rgb(c[0], c[1], c[2])).unwrap_or(vp::body());
        for t in m.indices.chunks(3) {
            let p = |i: u32| Vec3::new(m.positions[3 * i as usize], m.positions[3 * i as usize + 1], m.positions[3 * i as usize + 2]);
            s.tri(sim_on, [p(t[0]), p(t[1]), p(t[2])], rgba(col, if sim_on { 0.18 } else { 1.0 }));
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
    let size = (app.camera.wpp() * 40.0) as f64;
    for (d, col) in [(DVec3::X, vp::axis_x()), (DVec3::Y, vp::axis_y()), (DVec3::Z, vp::axis_z())] {
        s.lines_top.push(crate::render::LineInst { a: to(o).into(), b: to(o + d * size).into(), width: 2.5, bias: 0.0, color: rgba(col, 1.0) });
    }
    s.point(to(o), 7.0, rgba(vp::snap(), 1.0));
    // simulated stock
    if sim_on {
        let stock_c = Color32::from_rgb(0xd2, 0xb4, 0x82);
        let cut_c = Color32::from_rgb(0x9f, 0xb4, 0xc8);
        let bad_c = Color32::from_rgb(0xe0, 0x40, 0x40);
        for (t, cl) in &app.cam.sim_mesh {
            let col = match cl {
                0 => stock_c,
                1 => cut_c,
                _ => bad_c,
            };
            s.tri(false, [to(t[0]), to(t[1]), to(t[2])], rgba(col, 1.0));
        }
    }
    // toolpaths
    let sel = app.cam.sel_op;
    let limit = if sim_on { app.cam.sim.as_ref().map(|x| x.done).unwrap_or(0) } else { usize::MAX };
    let mut count = 0usize;
    for tp in &app.cam.paths {
        let on = sel.map(|id| id == tp.op).unwrap_or(true);
        let feed_col = if sel == Some(tp.op) {
            rgba(theme::accent(), 1.0)
        } else if on {
            rgba(Color32::from_rgb(0x3a, 0x7b, 0xd5), 0.9)
        } else {
            rgba(theme::muted(), 0.5)
        };
        let rapid_col = rgba(Color32::from_rgb(0xe8, 0x9a, 0x1e), if on { 0.9 } else { 0.35 });
        let mut prev: Option<DVec3> = None;
        for m in &tp.moves {
            if count >= limit {
                break;
            }
            count += 1;
            let p0 = prev.unwrap_or_else(|| {
                let q = m.end(DVec3::ZERO);
                DVec3::new(q.x, q.y, hi.z.max(part.hi.z) + c.clearance)
            });
            let p1 = m.end(p0);
            if sim_on && !matches!(m, Move::Rapid(_)) {
                prev = Some(p1);
                continue;
            }
            match m {
                Move::Rapid(_) => s.line(to(p0), to(p1), if on { 1.0 } else { 0.7 }, rapid_col),
                Move::Feed(..) => s.line(to(p0), to(p1), if sel == Some(tp.op) { 1.8 } else { 1.2 }, feed_col),
                Move::Tap { z, .. } => s.line(to(p0), to(DVec3::new(p0.x, p0.y, *z)), 1.8, feed_col),
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
            let tp_tool = sim.current_cutter().and_then(|ci| {
                let ids: Vec<u64> = app.cam.paths.iter().filter(|p| c.tool(p.tool).is_some()).map(|p| p.tool).collect();
                ids.get(ci).and_then(|id| c.tool(*id)).cloned()
            });
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
