//! Assembly tab: insert parts, mate them, drag them around.

use std::sync::Arc;

use egui::{Color32, Margin, Pos2, RichText, Sense, Stroke, Ui};
use glam::{DMat3, DVec3, Vec2, Vec3};

use crate::app::{App, ToastKind};
use crate::assembly::{self, Assembly, Connector, Instance, Mate, MateKind, Pose, Relation, RelationKind};
use crate::drawing::Drawing;
use crate::icons;
use crate::render::{MeshVertex, SceneData, ViewportCallback};
use crate::theme::{self, vp};
use crate::workspace::Tab;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub inst: u64,
    /// Which part of the instance (for sub-assemblies).
    pub leaf: usize,
    pub face: Option<usize>,
    pub edge: Option<usize>,
    /// Hit point in the part's own coordinates.
    pub local: [f64; 3],
}

pub struct MateDialog {
    pub mate: u64,
    /// The assembly it edits (its index), so it never acts on another tab's assembly.
    pub asm: usize,
    pub before: Assembly,
    pub active: usize,
}

pub struct Drag {
    pub inst: u64,
    pub start: Pose,
    pub rotate: bool,
    pub moved: Vec2,
    /// Moving: the plane the grabbed point slides in (point, normal), world coordinates.
    pub plane: (DVec3, DVec3),
    /// The grabbed point in the instance's own coordinates.
    pub grab: DVec3,
}

pub type Snap = (Vec<Assembly>, Vec<Drawing>);

#[derive(Default)]
pub struct AsmUi {
    pub scene_dirty: bool,
    pub hover: Option<Hit>,
    pub sel: Vec<u64>,
    pub sel_mate: Option<u64>,
    pub dialog: Option<MateDialog>,
    pub insert_open: bool,
    pub drag: Option<Drag>,
    pub undo: Vec<Snap>,
    pub redo: Vec<Snap>,
    pub failed: Vec<u64>,
    /// Mates whose geometry could not be found after the parts changed.
    pub lost: Vec<u64>,
    /// Relation being made: (kind, first mate, second mate, ratio, reverse).
    pub relation: Option<(RelationKind, Option<u64>, Option<u64>, f64, bool)>,
    /// Last interference check: instance id pairs and overlap volume; None while running.
    pub clashes: Option<Vec<(u64, u64, f64)>>,
    pub clash_ids: Vec<u64>,
    pub checking: bool,
    /// Something moved or changed since the last interference check.
    pub clash_stale: bool,
    /// Fingerprint of the parts' geometry the mate connectors were last placed on.
    pub geom_sig: u64,
    /// Exploded view distance factor (0 = assembled); with explode steps, 0..1 runs through them.
    pub explode: f32,
    /// Measure tool picks: world point and face normal (None for edges).
    pub measure: Option<Vec<(DVec3, Option<DVec3>)>>,
    /// Standard parts and GrabCAD window.
    pub lib: crate::stdparts_ui::Lib,
    /// Playing back the mates a PCB simulation moved.
    pub motion: Option<Motion>,
    /// Viewport right-click menu: where it opened and the instance under the pointer.
    pub ctx: Option<(Pos2, Option<u64>)>,
}

/// Playback of a circuit simulation's motors and servos in an assembly.
pub struct Motion {
    pub asm: usize,
    /// The simulated time shown (s), and the time last solved for.
    pub t: f64,
    shown: Option<f64>,
    pub playing: bool,
    /// Simulated seconds per real second.
    pub rate: f64,
    /// The poses from before, to go back to.
    rest: Vec<(u64, Pose)>,
    last: Option<std::time::Instant>,
}

/// The simulation results' mate traces for assembly `ai`, with their times.
fn motion_data(app: &App, ai: usize) -> Option<(&[f64], Vec<&crate::circuit_mech::MateTrace>)> {
    let out = app.pcb.sim.out.as_ref()?;
    let tr: Vec<_> = out.mates.iter().filter(|m| m.assembly == ai).collect();
    (!tr.is_empty() && out.x.len() > 1).then_some((&out.x[..], tr))
}

/// Start playing the last simulation in assembly `ai` (from the start).
pub fn play_motion(app: &mut App, ai: usize) {
    let Some((x, _)) = motion_data(app, ai) else { return };
    let stop = *x.last().unwrap();
    let rest = match &app.asm.motion {
        Some(m) if m.asm == ai => m.rest.clone(),
        _ => app.ws.assemblies[ai].instances.iter().map(|i| (i.id, i.pose)).collect(),
    };
    // a run over a few seconds plays in real time; a short one slows down to about 4 s
    let rate = if stop > 4.0 { 1.0 } else { [1.0, 0.5, 0.25, 0.1, 0.05, 0.01].into_iter().find(|r| stop / r >= 2.0).unwrap_or(0.01) };
    app.asm.motion = Some(Motion { asm: ai, t: 0.0, shown: None, playing: true, rate, rest, last: None });
}

/// Advance the playback and move the parts to it.
fn motion_tick(app: &mut App, ctx: &egui::Context) {
    let Some(ai) = asm_index(app) else { return };
    if app.asm.motion.as_ref().map(|m| m.asm != ai).unwrap_or(true) {
        return;
    }
    let Some(end) = motion_data(app, ai).map(|(x, _)| *x.last().unwrap()) else { return };
    let now = std::time::Instant::now();
    let m = app.asm.motion.as_mut().unwrap();
    if m.playing {
        if let Some(l) = m.last {
            m.t += now.duration_since(l).as_secs_f64() * m.rate;
        }
        m.last = Some(now);
        if m.t >= end {
            m.t = end;
            m.playing = false;
            m.last = None;
        }
        ctx.request_repaint();
    } else {
        m.last = None;
    }
    if m.shown == Some(m.t) {
        return;
    }
    m.shown = Some(m.t);
    let t = m.t;
    let drives: Vec<assembly::Drive> = {
        let (x, tr) = motion_data(app, ai).unwrap();
        let k = x.partition_point(|v| *v <= t).saturating_sub(1).min(x.len() - 1);
        let f = if k + 1 < x.len() && x[k + 1] > x[k] { ((t - x[k]) / (x[k + 1] - x[k])).clamp(0.0, 1.0) } else { 0.0 };
        tr.iter().map(|m| m.drive(k, f)).collect()
    };
    let centers: Vec<(u64, DVec3)> = app.ws.assemblies[ai].instances.iter().map(|x| (x.id, centroid_local(app, x))).collect();
    let r = assembly::solve_driven(&mut app.ws.assemblies[ai], &[], &|x: &Instance| centers.iter().find(|c| c.0 == x.id).map(|c| c.1).unwrap_or(DVec3::ZERO), &drives);
    app.asm.failed = r.failed;
    app.asm.clash_stale = true;
    app.asm.scene_dirty = true;
}

fn motion_ui(app: &mut App, ui: &mut Ui, ai: usize) {
    let Some((end, names)) = motion_data(app, ai).map(|(x, tr)| (*x.last().unwrap(), tr.iter().map(|m| m.name.clone()).collect::<Vec<_>>())) else { return };
    let movers: Vec<String> = app.pcb.sim.out.as_ref().map(|o| o.mech.iter().filter(|p| p.role.map(|r| r.drives()).unwrap_or(false)).map(|p| p.reference.clone()).collect()).unwrap_or_default();
    let pcb_name = app.pcb.sim.ran_name().unwrap_or_default();
    ui.label(RichText::new(format!("{} from the {pcb_name} simulation moves {}.", movers.join(", "), names.join(", "))).size(12.0).color(theme::muted()));
    if app.asm.motion.as_ref().map(|m| m.asm != ai).unwrap_or(true) {
        if ui.button("Play").clicked() {
            checkpoint(app);
            play_motion(app, ai);
        }
        return;
    }
    let m = app.asm.motion.as_mut().unwrap();
    let mut reset = false;
    ui.horizontal(|ui| {
        let label = if m.playing { "Pause" } else { "Play" };
        if ui.button(label).clicked() {
            if !m.playing && m.t >= end {
                m.t = 0.0;
            }
            m.playing = !m.playing;
        }
        if ui.button("Back to start").on_hover_text("Put the parts back where they were before playing").clicked() {
            reset = true;
        }
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = ui.available_width() - 70.0;
        if ui.add(egui::Slider::new(&mut m.t, 0.0..=end).show_value(false)).changed() {
            m.playing = false;
        }
        ui.label(RichText::new(crate::circuit::si(m.t, "s")).size(11.5).monospace());
    });
    row_label(ui, "Speed", |ui| {
        egui::ComboBox::from_id_salt("motion_rate").width(90.0).selected_text(format!("{}×", m.rate)).show_ui(ui, |ui| {
            for r in [1.0, 0.5, 0.25, 0.1, 0.05, 0.01] {
                ui.selectable_value(&mut m.rate, r, format!("{r}×"));
            }
        });
    });
    if reset {
        let rest = m.rest.clone();
        app.asm.motion = None;
        if let Some(a) = app.ws.assemblies.get_mut(ai) {
            for (id, p) in rest {
                if let Some(i) = a.instances.iter_mut().find(|i| i.id == id) {
                    i.pose = p;
                }
            }
        }
        app.asm.failed = assembly::failed_mates(&app.ws.assemblies[ai]);
        app.asm.scene_dirty = true;
        app.asm.clash_stale = true;
    }
}

fn row_label(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(12.0).color(theme::muted()));
        add(ui);
    });
}

pub fn asm_index(app: &App) -> Option<usize> {
    match app.tab {
        Tab::Assembly(i) => Some(i),
        _ => None,
    }
}

pub fn asm(app: &App) -> Option<&Assembly> {
    asm_index(app).and_then(|i| app.ws.assemblies.get(i))
}

pub fn asm_mut(app: &mut App) -> Option<&mut Assembly> {
    let i = asm_index(app)?;
    app.ws.assemblies.get_mut(i)
}

/// Record an undo step for assembly/drawing edits.
pub fn checkpoint(app: &mut App) {
    app.asm.clash_stale = true;
    let snap = (app.ws.assemblies.clone(), app.ws.drawings.clone());
    app.asm.undo.push(snap);
    if app.asm.undo.len() > 200 {
        app.asm.undo.remove(0);
    }
    app.asm.redo.clear();
}

pub fn undo(app: &mut App) {
    if let Some(s) = app.asm.undo.pop() {
        let cur = (std::mem::replace(&mut app.ws.assemblies, s.0), std::mem::replace(&mut app.ws.drawings, s.1));
        app.asm.redo.push(cur);
        app.asm.dialog = None;
        app.asm.scene_dirty = true;
        app.dwg.dirty = true;
        prune_isolation(app);
        refresh_failed(app);
    }
}

pub fn redo(app: &mut App) {
    if let Some(s) = app.asm.redo.pop() {
        let cur = (std::mem::replace(&mut app.ws.assemblies, s.0), std::mem::replace(&mut app.ws.drawings, s.1));
        app.asm.undo.push(cur);
        app.asm.dialog = None;
        app.asm.scene_dirty = true;
        app.dwg.dirty = true;
        prune_isolation(app);
        refresh_failed(app);
    }
}

/// The pose an instance is drawn and picked at (moved outwards in an exploded view).
fn display_pose(app: &App, a: &Assembly, inst: &Instance) -> Pose {
    if app.asm.explode <= 0.0 {
        return inst.pose;
    }
    if !a.explode_steps.is_empty() {
        let total = app.asm.explode.clamp(0.0, 1.0) as f64 * a.explode_steps.len() as f64;
        let mut p = inst.pose;
        for (i, st) in a.explode_steps.iter().enumerate() {
            if st.insts.contains(&inst.id) {
                let f = (total - i as f64).clamp(0.0, 1.0);
                p.t = (DVec3::from(p.t) + DVec3::from(st.dir).normalize_or_zero() * st.distance * f).into();
            }
        }
        return p;
    }
    let cs: Vec<DVec3> = a.instances.iter().map(|x| x.pose.apply(centroid_local(app, x))).collect();
    let c = cs.iter().fold(DVec3::ZERO, |s, p| s + *p) / cs.len().max(1) as f64;
    let mine = inst.pose.apply(centroid_local(app, inst));
    let mut p = inst.pose;
    p.t = (DVec3::from(p.t) + (mine - c) * app.asm.explode as f64).into();
    p
}

/// A part drawn for an instance: its mesh, pose inside the instance and colour.
struct Part<'a> {
    local: Pose,
    mesh: &'a crate::kernel::BodyMesh,
    color: Color32,
}

/// The parts of an instance (one, or all of a sub-assembly's), indexed by `Hit::leaf`.
fn parts_of<'a>(app: &'a App, inst: &Instance) -> Vec<Option<Part<'a>>> {
    // numbered over every part (hidden ones inside sub-assemblies are None), so `Hit::leaf` and
    // the mate connectors' leaf numbers don't shift when something is hidden or shown
    assembly::leaves_shown(&app.ws.assemblies, inst)
        .into_iter()
        .map(|(l, shown)| {
            if !shown {
                return None;
            }
            let mesh = app.studio_bodies(l.studio).iter().find(|b| b.id == l.body)?;
            let color = app.studio_doc(l.studio).and_then(|d| d.parts.get(&crate::doc::body_key(l.body))).and_then(|p| p.color).map(|c| Color32::from_rgb(c[0], c[1], c[2])).unwrap_or(vp::body());
            Some(Part { local: l.pose, mesh, color })
        })
        .collect()
}

fn centroid_local(app: &App, inst: &Instance) -> DVec3 {
    let (mut sum, mut w) = (DVec3::ZERO, 0.0);
    for p in parts_of(app, inst).into_iter().flatten() {
        let v = p.mesh.volume.abs().max(1e-9);
        sum += p.local.apply(DVec3::from(p.mesh.centroid)) * v;
        w += v;
    }
    if w > 0.0 { sum / w } else { DVec3::ZERO }
}

/// World bounding box of an instance at its assembled pose.
fn world_bbox(app: &App, x: &Instance) -> Option<(DVec3, DVec3)> {
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for p in parts_of(app, x).into_iter().flatten() {
        let pose = x.pose.compose(&p.local);
        let m = p.mesh;
        for c in 0..8 {
            let w = pose.apply(DVec3::new(m.bbox[c & 1][0], m.bbox[(c >> 1) & 1][1], m.bbox[(c >> 2) & 1][2]));
            lo = lo.min(w);
            hi = hi.max(w);
        }
    }
    lo.x.is_finite().then_some((lo, hi))
}

/// Solve the mates of the open assembly, holding `hold` still.
pub fn solve(app: &mut App, hold: &[u64]) {
    let suspects: Vec<u64> = app.asm.dialog.as_ref().map(|d| d.mate).into_iter().collect();
    solve_suspect(app, hold, &suspects);
}

/// `solve`, where a conflict is first blamed on the mates in `suspects` (the one just made,
/// edited or unsuppressed), which are then left unmet rather than pulling every part askew.
pub fn solve_suspect(app: &mut App, hold: &[u64], suspects: &[u64]) {
    let Some(i) = asm_index(app) else { return };
    let centers: Vec<(u64, DVec3)> = app.ws.assemblies[i].instances.iter().map(|x| (x.id, centroid_local(app, x))).collect();
    let r = assembly::solve_suspect(&mut app.ws.assemblies[i], hold, &|x: &Instance| centers.iter().find(|c| c.0 == x.id).map(|c| c.1).unwrap_or(DVec3::ZERO), &[], suspects);
    // mates whose geometry went away in a Part Studio edit stay failed
    let lost: Vec<u64> = app.asm.failed.iter().copied().filter(|m| app.asm.lost.contains(m)).collect();
    app.asm.failed = r.failed;
    for m in lost {
        if !app.asm.failed.contains(&m) {
            app.asm.failed.push(m);
        }
    }
    app.asm.clash_stale = true;
    app.asm.scene_dirty = true;
}

/// Solve while an instance is dragged by a point of it (see `assembly::solve_drag`).
fn solve_drag(app: &mut App, inst: u64, grab: DVec3) {
    let Some(i) = asm_index(app) else { return };
    let centers: Vec<(u64, DVec3)> = app.ws.assemblies[i].instances.iter().map(|x| (x.id, centroid_local(app, x))).collect();
    let r = assembly::solve_drag(&mut app.ws.assemblies[i], &|x: &Instance| centers.iter().find(|c| c.0 == x.id).map(|c| c.1).unwrap_or(DVec3::ZERO), inst, grab);
    app.asm.failed = r.failed;
    for m in app.asm.lost.clone() {
        if !app.asm.failed.contains(&m) {
            app.asm.failed.push(m);
        }
    }
    app.asm.clash_stale = true;
    app.asm.scene_dirty = true;
}

/// Mark the mates that don't hold at the current poses (after an undo, a cancel or a load,
/// which put poses back without solving), plus those whose geometry is gone.
pub fn refresh_failed(app: &mut App) {
    let Some(a) = asm(app) else { return };
    let mut failed = assembly::failed_mates(a);
    for m in app.asm.lost.iter().copied().filter(|m| a.mates.iter().any(|x| x.id == *m && !x.suppressed)) {
        if !failed.contains(&m) {
            failed.push(m);
        }
    }
    app.asm.failed = failed;
}

/// After the parts change (a Part Studio rebuilt), move the mate connectors onto the new
/// geometry and solve again.
fn follow_parts(app: &mut App) {
    let Some(i) = asm_index(app) else { return };
    // a cheap fingerprint of the geometry the instances use
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let leaves: Vec<assembly::Leaf> = app.ws.assemblies[i].instances.iter().flat_map(|x| assembly::leaves_with(&app.ws.assemblies, x, true)).collect();
    for l in &leaves {
        if let Some(m) = app.studio_bodies(l.studio).iter().find(|b| b.id == l.body) {
            (l.studio, l.body, m.volume.to_bits(), m.area.to_bits(), m.faces.len(), m.edges.len()).hash(&mut h);
        } else {
            (l.studio, l.body).hash(&mut h);
        }
    }
    let sig = h.finish();
    if sig == app.asm.geom_sig {
        return;
    }
    // a Part Studio still building (just opened): its connectors would all look lost
    if leaves.iter().any(|l| !app.studio_bodies(l.studio).iter().any(|b| b.id == l.body)) {
        return;
    }
    let first = app.asm.geom_sig == 0;
    app.asm.geom_sig = sig;
    let meshes: std::collections::HashMap<(usize, crate::doc::BodyId), crate::kernel::BodyMesh> =
        leaves.iter().filter_map(|l| app.studio_bodies(l.studio).iter().find(|b| b.id == l.body).map(|m| ((l.studio, l.body), m.clone()))).collect();
    let before = serde_json::to_string(&app.ws.assemblies[i].mates).unwrap_or_default();
    let lost = assembly::refresh_connectors(&mut app.ws.assemblies, i, &|s, b| meshes.get(&(s, b)));
    let moved = serde_json::to_string(&app.ws.assemblies[i].mates).unwrap_or_default() != before;
    app.asm.lost = lost.clone();
    if moved || !lost.is_empty() {
        solve(app, &[]);
        for m in lost {
            if !app.asm.failed.contains(&m) {
                app.asm.failed.push(m);
            }
        }
        if !first {
            app.asm.clash_stale = true;
        }
    } else {
        refresh_failed(app);
    }
}

// ---------------------------------------------------------------- toolbar

pub fn toolbar(app: &mut App, ui: &mut Ui) {
    App::group(ui, |ui| {
        if app.tool_button(ui, "import", "Insert", "Insert parts from Part Studios", true, app.asm.insert_open, true).clicked() {
            app.asm.insert_open = !app.asm.insert_open;
        }
        if app.tool_button(ui, "library", "Library", "Standard parts (screws, nuts, bearings, motors…), models downloaded from GrabCAD and McMaster-Carr parts", true, app.asm.lib.open, true).clicked() {
            crate::stdparts_ui::toggle(app);
        }
    });
    App::group(ui, |ui| {
        for k in MateKind::ALL {
            let tip = format!("{} mate: pick a face, edge or circle on each of two parts", k.label());
            if app.tool_button(ui, k.icon(), k.label(), &tip, matches!(k, MateKind::Fastened | MateKind::Revolute), false, true).clicked() {
                start_mate(app, k);
            }
        }
    });
    App::group(ui, |ui| {
        for k in RelationKind::ALL {
            let tip = match k {
                RelationKind::Gear => "Gear relation between two revolute mates",
                RelationKind::Rack => "Rack and pinion: a revolute mate drives a slider",
                RelationKind::Screw => "Screw: a cylindrical mate's turning drives its travel",
            };
            let icon = match k {
                RelationKind::Gear => "relGear",
                RelationKind::Rack => "relRack",
                RelationKind::Screw => "relScrew",
            };
            if app.tool_button(ui, icon, k.label(), tip, false, app.asm.relation.map(|r| r.0 == k).unwrap_or(false), true).clicked() {
                app.asm.relation = Some((k, None, None, if k == RelationKind::Screw { 2.0 } else { 1.0 }, false));
            }
        }
    });
    App::group(ui, |ui| {
        if app.tool_button(ui, "clash", "Interference", "Check whether any parts overlap", false, app.asm.checking, !app.asm.checking).clicked() {
            check_interference(app);
        }
    });
    App::group(ui, |ui| {
        let has = !app.asm.sel.is_empty();
        if app.tool_button(ui, "fix", "Fix", "Fix or free the selected instances", false, false, has).clicked() {
            checkpoint(app);
            let sel = app.asm.sel.clone();
            if let Some(a) = asm_mut(app) {
                for x in &mut a.instances {
                    if sel.contains(&x.id) {
                        x.fixed = !x.fixed;
                    }
                }
            }
            app.asm.scene_dirty = true;
        }
        if app.tool_button(ui, "delete", "Delete", "Delete the selected instances (Del)", false, false, has || app.asm.sel_mate.is_some()).clicked() {
            delete_selection(app);
        }
    });
    App::group(ui, |ui| {
        let before = app.section;
        app.section_ui(ui);
        if before != app.section {
            app.asm.scene_dirty = true;
        }
    });
    App::group(ui, |ui| {
        let on = app.asm.explode > 0.0;
        let stepped = asm(app).map(|a| !a.explode_steps.is_empty()).unwrap_or(false);
        if app.tool_button(ui, "transform", "Explode", "Exploded view (add steps in the sidebar)", false, on, true).clicked() {
            app.asm.explode = if on { 0.0 } else if stepped { 1.0 } else { 0.8 };
            app.asm.scene_dirty = true;
        }
        let range = if stepped { 0.0..=1.0 } else { 0.05..=3.0 };
        if on && ui.add(egui::Slider::new(&mut app.asm.explode, range).show_value(false)).on_hover_text(if stepped { "Play through the explode steps" } else { "Explode distance" }).changed() {
            app.asm.scene_dirty = true;
        }
        let measuring = app.asm.measure.is_some();
        if app.tool_button(ui, "measure", "Measure", "Measure distance or angle between two picks", false, measuring, true).clicked() {
            app.asm.measure = if measuring { None } else { Some(vec![]) };
        }
        let tip = "Isolate: show only the selected instances (I); again, or Esc, shows the rest";
        if app.tool_button(ui, "isolate", "Isolate", tip, false, app.isolation.is_some(), app.isolation.is_some() || !app.asm.sel.is_empty()).clicked() {
            toggle_isolate(app);
        }
    });
}

/// Fix or free instances (undoable).
fn set_fixed(app: &mut App, ids: &[u64], fixed: bool) {
    checkpoint(app);
    if let Some(a) = asm_mut(app) {
        for x in a.instances.iter_mut().filter(|x| ids.contains(&x.id)) {
            x.fixed = fixed;
        }
    }
    app.asm.scene_dirty = true;
}

fn start_mate(app: &mut App, kind: MateKind) {
    if app.asm.dialog.is_some() {
        mate_ok(app);
    }
    app.asm.insert_open = false;
    let Some(i) = asm_index(app) else { return };
    let before = app.ws.assemblies[i].clone();
    checkpoint(app);
    let a = &mut app.ws.assemblies[i];
    let id = a.new_id();
    let n = a.mates.iter().filter(|m| m.kind == kind).count() + 1;
    a.mates.push(Mate { id, name: format!("{} {n}", kind.label()), kind, a: None, b: None, flip: false, offset: 0.0, offset_xy: [0.0; 2], rotation: 0.0, limits: None, suppressed: false });
    app.asm.dialog = Some(MateDialog { mate: id, asm: i, before, active: 0 });
    app.asm.sel.clear();
}

fn mate_ok(app: &mut App) {
    if let Some(d) = app.asm.dialog.take().filter(|d| asm_index(app) == Some(d.asm)) {
        let incomplete = asm(app).and_then(|a| a.mates.iter().find(|m| m.id == d.mate)).map(|m| m.a.is_none() || m.b.is_none()).unwrap_or(false);
        if incomplete {
            if let Some(a) = asm_mut(app) {
                a.mates.retain(|m| m.id != d.mate);
            }
            app.flash("The mate needs a pick on each of two parts", ToastKind::Info);
        }
    }
    app.asm.scene_dirty = true;
}

fn mate_cancel(app: &mut App) {
    if let Some(d) = app.asm.dialog.take() {
        if asm_index(app) == Some(d.asm) && d.asm < app.ws.assemblies.len() {
            app.ws.assemblies[d.asm] = d.before;
            app.asm.undo.pop();
            refresh_failed(app);
        }
    }
    app.asm.scene_dirty = true;
}

/// Leaving this Assembly tab: finish the mate being made, and drop the selection and other
/// state that names this assembly's instances and mates (ids are per assembly, so they would
/// pick out different ones in the next assembly).
pub fn leave(app: &mut App) {
    if app.asm.dialog.is_some() {
        mate_ok(app);
    }
    app.asm.sel.clear();
    app.asm.sel_mate = None;
    app.asm.hover = None;
    app.asm.ctx = None;
    app.asm.drag = None;
    app.asm.measure = None;
    app.asm.relation = None;
    if app.asm.clashes.is_some() || app.asm.checking {
        app.asm.clashes = None;
        app.asm.clash_ids.clear();
        app.asm.checking = false;
    }
    app.asm.scene_dirty = true;
}

/// Drop isolated instances that are gone (deleted, or undone away); end the isolation when
/// none are left.
pub fn prune_isolation(app: &mut App) {
    let Some(keep) = isolated(app) else { return };
    let ids: Vec<u64> = asm(app).map(|a| keep.iter().copied().filter(|id| a.instance(*id).is_some()).collect()).unwrap_or_default();
    if ids.is_empty() {
        app.end_isolation();
    } else if let Some(i) = app.isolation.as_mut() {
        i.instances = ids;
    }
}

fn delete_selection(app: &mut App) {
    checkpoint(app);
    let sel = app.asm.sel.clone();
    let sel_mate = app.asm.sel_mate.take();
    if let Some(a) = asm_mut(app) {
        a.instances.retain(|x| !sel.contains(&x.id));
        a.mates.retain(|m| Some(m.id) != sel_mate && !m.a.map(|c| sel.contains(&c.inst)).unwrap_or(false) && !m.b.map(|c| sel.contains(&c.inst)).unwrap_or(false));
    }
    app.asm.sel.clear();
    prune_isolation(app);
    // the parts a deleted mate pulled askew settle again
    solve(app, &[]);
    app.asm.scene_dirty = true;
}

pub fn shortcuts(app: &mut App, ctx: &egui::Context) {
    let pressed = |k: egui::Key| ctx.input(|i| i.key_pressed(k));
    if crate::app::key_mods(ctx).0 {
        // Ctrl+A selects every instance; other Ctrl keys are not the letters' tools
        if pressed(egui::Key::A) && app.asm.dialog.is_none() {
            // the instances in sight, as Ctrl+A in a Part Studio
            app.asm.sel = asm(app).map(|a| a.instances.iter().filter(|x| shown(app, x)).map(|x| x.id).collect()).unwrap_or_default();
            app.asm.scene_dirty = true;
        }
        return;
    }
    if pressed(egui::Key::Escape) && app.asm.ctx.is_some() {
        // Esc closes the right-click menu and does nothing else
        app.asm.ctx = None;
        return;
    }
    if pressed(egui::Key::Escape) {
        if app.asm.dialog.is_some() {
            mate_cancel(app);
        } else if app.asm.measure.is_some() {
            app.asm.measure = None;
        } else if app.asm.sel.is_empty() && app.asm.sel_mate.is_none() && app.isolation.is_some() {
            // a second Esc (nothing left to clear) ends the isolation
            app.end_isolation();
        } else {
            app.asm.sel.clear();
            app.asm.sel_mate = None;
            app.asm.insert_open = false;
            app.asm.scene_dirty = true;
        }
    }
    if pressed(egui::Key::Enter) && app.asm.dialog.is_some() {
        mate_ok(app);
    }
    if pressed(egui::Key::Delete) && app.asm.dialog.is_none() {
        delete_selection(app);
    }
    for (k, v) in [
        (egui::Key::Num0, crate::camera::StdView::Iso),
        (egui::Key::Num1, crate::camera::StdView::Front),
        (egui::Key::Num2, crate::camera::StdView::Top),
        (egui::Key::Num3, crate::camera::StdView::Right),
    ] {
        if pressed(k) {
            app.camera.set_view(v);
            app.asm.scene_dirty = true;
        }
    }
    if pressed(egui::Key::F) {
        fit(app);
    }
    if pressed(egui::Key::I) && app.asm.dialog.is_none() {
        toggle_isolate(app);
    }
    if pressed(egui::Key::H) && app.asm.dialog.is_none() {
        if crate::app::key_mods(ctx).1 {
            show_all(app);
        } else {
            toggle_hide_selected(app);
        }
    }
}

/// In sight in the viewport: not hidden, or kept by the isolation.
pub fn shown(app: &App, x: &Instance) -> bool {
    match isolated(app) {
        // hiding wins: an isolated instance that gets hidden (Ctrl+Z, say) goes out of sight
        Some(keep) => keep.contains(&x.id) && !x.hidden,
        None => !x.hidden,
    }
}

/// The instances isolated in this assembly, if isolating.
pub fn isolated(app: &App) -> Option<&[u64]> {
    app.isolation.as_ref().filter(|i| i.tab == app.tab && matches!(app.tab, crate::workspace::Tab::Assembly(_))).map(|i| &i.instances[..])
}

/// Show only these instances until the isolation ends (Esc, I, or Exit isolate).
pub fn isolate(app: &mut App, ids: Vec<u64>) {
    if ids.is_empty() {
        app.flash("Select the instances to isolate", ToastKind::Info);
        return;
    }
    // hidden ones stay hidden (Show them first)
    let ids: Vec<u64> = ids.into_iter().filter(|id| asm(app).and_then(|a| a.instance(*id)).is_some_and(|x| !x.hidden)).collect();
    if ids.is_empty() {
        app.flash("The selected instances are hidden: show them first", ToastKind::Info);
        return;
    }
    app.asm.sel.retain(|x| ids.contains(x));
    if app.asm.hover.is_some_and(|h| !ids.contains(&h.inst)) {
        app.asm.hover = None;
    }
    app.isolation = Some(crate::app::Isolation { tab: app.tab, bodies: vec![], instances: ids });
    // an interference result covers what was in sight when it ran
    if app.asm.clashes.is_some() {
        app.asm.clash_stale = true;
    }
    app.asm.scene_dirty = true;
}

/// Isolate the selected instances, or end the isolation if one is on.
pub fn toggle_isolate(app: &mut App) {
    if app.isolation.is_some() {
        app.end_isolation();
    } else {
        let ids = app.asm.sel.clone();
        isolate(app, ids);
    }
}

/// Hide or show instances. Saved with the assembly and undoable; hiding one that is isolated
/// also takes it out of the isolation, so it goes out of sight either way.
pub fn set_hidden(app: &mut App, ids: &[u64], hidden: bool) {
    set_hidden_many(app, &[(ids, hidden)]);
}

/// Several hide/show changes as one undo step.
fn set_hidden_many(app: &mut App, changes: &[(&[u64], bool)]) {
    if app.asm.dialog.is_some() {
        // cancelling the mate would put the old visibility back and take the wrong undo step
        app.flash("Finish the mate first", ToastKind::Info);
        return;
    }
    let Some(a) = asm(app) else { return };
    if !changes.iter().any(|(ids, hidden)| a.instances.iter().any(|x| ids.contains(&x.id) && x.hidden != *hidden)) {
        return;
    }
    let stale = app.asm.clash_stale;
    checkpoint(app);
    // the interference result still holds for the parts it checked, unless parts come back
    // into sight that it didn't check
    let showing = changes.iter().any(|(ids, hidden)| !*hidden && !ids.is_empty());
    app.asm.clash_stale = stale || showing;
    for (ids, hidden) in changes {
        set_hidden_now(app, ids, *hidden);
    }
    app.asm.scene_dirty = true;
}

fn set_hidden_now(app: &mut App, ids: &[u64], hidden: bool) {
    if let Some(a) = asm_mut(app) {
        for x in a.instances.iter_mut().filter(|x| ids.contains(&x.id)) {
            x.hidden = hidden;
        }
    }
    if hidden {
        if app.asm.hover.is_some_and(|h| ids.contains(&h.inst)) {
            app.asm.hover = None;
        }
        if isolated(app).is_some() {
            let left = app.isolation.as_mut().map(|i| {
                i.instances.retain(|x| !ids.contains(x));
                i.instances.len()
            });
            if left == Some(0) {
                app.end_isolation();
            }
        }
    }
}

/// H: hide the selected instances, or show them when they are all hidden already.
pub fn toggle_hide_selected(app: &mut App) {
    let sel = app.asm.sel.clone();
    if sel.is_empty() {
        app.flash("Select the instances to hide", ToastKind::Info);
        return;
    }
    let all_hidden = asm(app).map(|a| a.instances.iter().filter(|x| sel.contains(&x.id)).all(|x| x.hidden)).unwrap_or(false);
    set_hidden(app, &sel, !all_hidden);
}

/// Hide every instance but these.
pub fn hide_others(app: &mut App, keep: &[u64]) {
    let others: Vec<u64> = asm(app).map(|a| a.instances.iter().map(|x| x.id).filter(|id| !keep.contains(id)).collect()).unwrap_or_default();
    // the kept ones come into sight if they were hidden
    set_hidden_many(app, &[(keep, false), (others.as_slice(), true)]);
}

/// Shift+H: show every hidden instance.
pub fn show_all(app: &mut App) {
    if app.asm.dialog.is_some() {
        app.flash("Finish the mate first", ToastKind::Info);
        return;
    }
    // everything in sight again, as Show all parts in a Part Studio
    app.end_isolation();
    let ids: Vec<u64> = asm(app).map(|a| a.instances.iter().filter(|x| x.hidden).map(|x| x.id).collect()).unwrap_or_default();
    set_hidden(app, &ids, false);
}

pub fn any_hidden(app: &App) -> bool {
    asm(app).is_some_and(|a| a.instances.iter().any(|x| x.hidden))
}

pub fn fit(app: &mut App) {
    let Some(a) = asm(app) else { return };
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    // just the instances in sight (hidden ones, or ones an isolation leaves out, don't count)
    for x in a.instances.iter().filter(|x| shown(app, x)) {
        if let Some((l, h)) = world_bbox(app, x) {
            lo = lo.min(l);
            hi = hi.max(h);
        }
    }
    if lo.x.is_finite() {
        app.camera.fit(lo.as_vec3(), hi.as_vec3());
    }
    app.asm.scene_dirty = true;
}

// ---------------------------------------------------------------- sidebar

pub fn sidebar(app: &mut App, ui: &mut Ui) {
    motion_tick(app, &ui.ctx().clone());
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if app.asm.insert_open {
            section(ui, "Insert", |ui| insert_panel(app, ui));
        }
        section(ui, "Instances", |ui| instances(app, ui));
        section(ui, "Mates", |ui| mates(app, ui));
        if app.asm.checking || app.asm.clashes.is_some() {
            section(ui, "Interference", |ui| clashes_ui(app, ui));
        }
        if app.asm.relation.is_some() || asm(app).map(|a| !a.relations.is_empty()).unwrap_or(false) {
            section(ui, "Relations", |ui| relations(app, ui));
        }
        if let Some(ai) = asm_index(app).filter(|ai| motion_data(app, *ai).is_some()) {
            section(ui, "Motion", |ui| motion_ui(app, ui, ai));
        }
        if app.asm.explode > 0.0 {
            section(ui, "Exploded view", |ui| explode_ui(app, ui));
        }
        section(ui, "Bill of materials", |ui| bom_ui(app, ui));
    });
}

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

fn insert_panel(app: &mut App, ui: &mut Ui) {
    let mut add: Option<(usize, crate::doc::BodyId, String)> = None;
    for (si, s) in app.ws.studios.iter().enumerate() {
        ui.label(RichText::new(&s.name).color(theme::muted()).size(12.0));
        let bodies = app.studio_bodies(si);
        if bodies.is_empty() {
            ui.label(RichText::new("  (no parts yet)").color(theme::muted()).italics().size(12.0));
        }
        for b in bodies {
            // unnamed parts are called after their Part Studio when there are several studios
            let name = app.studio_doc(si).and_then(|d| d.parts.get(&crate::doc::body_key(b.id))).and_then(|p| p.name.clone()).unwrap_or_else(|| {
                if app.ws.studios.len() > 1 {
                    if bodies.len() == 1 { s.name.clone() } else { format!("{} {}", s.name, b.name) }
                } else {
                    b.name.clone()
                }
            });
            if ui
                .add(egui::Button::image_and_text(icons::image("extrude", 14.0).tint(theme::text()), RichText::new(&name).size(12.5)))
                .on_hover_text("Insert an instance")
                .clicked()
            {
                add = Some((si, b.id, name));
            }
        }
    }
    // other assemblies, as rigid sub-assemblies
    let cur = asm_index(app);
    let subs: Vec<(usize, String)> = app
        .ws
        .assemblies
        .iter()
        .enumerate()
        .filter(|(k, a)| cur.map(|c| !assembly::contains(&app.ws.assemblies, *k, c)).unwrap_or(true) && !a.instances.is_empty())
        .map(|(k, a)| (k, a.name.clone()))
        .collect();
    let mut add_sub = None;
    if !subs.is_empty() {
        ui.label(RichText::new("Assemblies").color(theme::muted()).size(12.0));
        for (k, name) in subs {
            if ui
                .add(egui::Button::image_and_text(icons::image("assembly", 14.0).tint(theme::text()), RichText::new(&name).size(12.5)))
                .on_hover_text("Insert as a sub-assembly")
                .clicked()
            {
                add_sub = Some((k, name));
            }
        }
    }
    if let Some((studio, body, name)) = add {
        insert(app, studio, body, None, name);
    }
    if let Some((k, name)) = add_sub {
        insert(app, 0, (0, 0), Some(k), name);
    }
}

pub fn insert(app: &mut App, studio: usize, body: crate::doc::BodyId, sub: Option<usize>, name: String) {
    checkpoint(app);
    // place new instances beside what is already there
    let mut max_x = f64::NEG_INFINITY;
    if let Some(a) = asm(app) {
        for x in &a.instances {
            if let Some((_, hi)) = world_bbox(app, x) {
                max_x = max_x.max(hi.x);
            }
        }
    }
    let probe = Instance { id: 0, name: String::new(), studio, body, pose: Pose::default(), fixed: false, hidden: false, sub };
    let min_x = world_bbox(app, &probe).map(|b| b.0.x).unwrap_or(0.0);
    let t = if max_x.is_finite() { [max_x + 10.0 - min_x, 0.0, 0.0] } else { [0.0; 3] };
    let Some(a) = asm_mut(app) else { return };
    let id = a.new_id();
    let n = a.instances.iter().filter(|x| x.sub == sub && (sub.is_some() || (x.studio == studio && x.body == body))).count() + 1;
    let fixed = a.instances.is_empty();
    a.instances.push(Instance { id, name: format!("{name} <{n}>"), studio, body, pose: Pose { t, ..Default::default() }, fixed, hidden: false, sub });
    app.asm.scene_dirty = true;
    if fixed {
        fit(app);
    }
}

fn instances(app: &mut App, ui: &mut Ui) {
    let Some(a) = asm(app) else { return };
    if a.instances.is_empty() {
        ui.label(RichText::new("Use Insert to add parts").color(theme::muted()).size(12.0));
        return;
    }
    let rows: Vec<(u64, String, bool, bool, bool)> = a.instances.iter().map(|x| (x.id, x.name.clone(), x.fixed, x.hidden, !shown(app, x))).collect();
    let any_hidden = rows.iter().any(|r| r.3);
    let mut hide: Option<(Vec<u64>, bool)> = None;
    let mut others: Option<Vec<u64>> = None;
    let mut show_every = false;
    let mut iso: Option<Option<Vec<u64>>> = None;
    let mut fix: Option<(Vec<u64>, bool)> = None;
    let fixed_ids: Vec<u64> = rows.iter().filter(|r| r.2).map(|r| r.0).collect();
    for (id, name, fixed, hidden, out_of_sight) in rows {
        let picked = app.asm.sel.contains(&id);
        let label = if fixed { format!("📌 {name}") } else { name };
        // room on the right for the eye, so long names don't run under it
        let w = (ui.available_width() - 22.0).max(40.0);
        let r = ui
            .scope(|ui| {
                ui.set_max_width(w);
                ui.add(egui::Button::selectable(picked, RichText::new(label).size(12.5).color(if out_of_sight { theme::muted() } else { theme::text() })).truncate().min_size(egui::vec2(w, 20.0)))
            })
            .inner;
        // the selected instances when this row is one of them, else just this one
        let these = if picked { app.asm.sel.clone() } else { vec![id] };
        if crate::app::eye_toggle(ui, &r, r.rect.right() + 11.0, hidden, ("asm-eye", id)) {
            hide = Some((these.clone(), !hidden));
        } else if r.clicked() {
            if !ui.input(|i| i.modifiers.shift || i.modifiers.command) {
                app.asm.sel.clear();
            }
            app.asm.sel.push(id);
            app.asm.sel_mate = None;
            app.asm.scene_dirty = true;
        }
        r.context_menu(|ui| {
            let n = these.len();
            let what = if n > 1 { format!(" {n} instances") } else { String::new() };
            if ui.button(format!("{}{what}", if hidden { "Show" } else { "Hide" })).on_hover_text("H").clicked() {
                hide = Some((these.clone(), !hidden));
                ui.close();
            }
            if ui.button("Hide others").on_hover_text("Hide every instance but this").clicked() {
                others = Some(these.clone());
                ui.close();
            }
            if any_hidden && ui.button("Show all").on_hover_text("Shift+H").clicked() {
                show_every = true;
                ui.close();
            }
            ui.separator();
            let label = if n > 1 { format!("Isolate {n} instances") } else { "Isolate".to_string() };
            if ui.button(label).on_hover_text("Show only this, until Esc, I or Exit isolate").clicked() {
                iso = Some(Some(these.clone()));
                ui.close();
            }
            if app.isolation.is_some() && ui.button("Exit isolate").clicked() {
                iso = Some(None);
                ui.close();
            }
            ui.separator();
            let all_fixed = these.iter().all(|i| fixed_ids.contains(i));
            if ui.button(format!("{}{what}", if all_fixed { "Unfix" } else { "Fix" })).on_hover_text("Fixed instances stay where they are when mates are solved").clicked() {
                fix = Some((these.clone(), !all_fixed));
                ui.close();
            }
        });
    }
    if let Some((ids, on)) = fix {
        set_fixed(app, &ids, on);
    }
    match iso {
        Some(Some(ids)) => isolate(app, ids),
        Some(None) => app.end_isolation(),
        None => {}
    }
    if let Some((ids, h)) = hide {
        set_hidden(app, &ids, h);
    }
    if let Some(keep) = others {
        hide_others(app, &keep);
    }
    if show_every {
        show_all(app);
    }
}

fn mates(app: &mut App, ui: &mut Ui) {
    let Some(a) = asm(app) else { return };
    if a.mates.is_empty() {
        ui.label(RichText::new("Pick a mate type in the toolbar").color(theme::muted()).size(12.0));
        return;
    }
    // a mate still being picked says so
    let rows: Vec<(u64, String, MateKind, bool)> =
        a.mates.iter().map(|m| (m.id, if m.a.is_none() || m.b.is_none() { format!("{} · picking…", m.name) } else { m.name.clone() }, m.kind, m.suppressed)).collect();
    let mut action = None;
    for (id, name, kind, suppressed) in rows {
        let failed = app.asm.failed.contains(&id);
        let picked = match &app.asm.dialog {
            Some(d) => d.mate == id,
            None => app.asm.sel_mate == Some(id),
        };
        let col = if failed {
            theme::error_soft()
        } else if suppressed {
            theme::muted()
        } else {
            theme::text()
        };
        let r = ui.add(
            egui::Button::image_and_text(icons::image(kind.icon(), 14.0).tint(col), RichText::new(&name).size(12.5).color(col))
                .selected(picked)
                .min_size(egui::vec2(ui.available_width(), 20.0)),
        );
        let r = if failed { r.on_hover_text("This mate can't be satisfied") } else { r };
        if r.clicked() {
            app.asm.sel_mate = Some(id);
            app.asm.sel.clear();
            app.asm.scene_dirty = true;
        }
        if r.double_clicked() {
            action = Some(("edit", id));
        }
        r.context_menu(|ui| {
            if ui.button("Edit").clicked() {
                action = Some(("edit", id));
                ui.close();
            }
            if ui.button(if suppressed { "Unsuppress" } else { "Suppress" }).clicked() {
                action = Some(("suppress", id));
                ui.close();
            }
            if ui.button(RichText::new("Delete").color(theme::error_soft())).clicked() {
                action = Some(("delete", id));
                ui.close();
            }
        });
    }
    match action {
        Some(("edit", id)) => {
            if let Some(i) = asm_index(app) {
                let before = app.ws.assemblies[i].clone();
                checkpoint(app);
                app.asm.dialog = Some(MateDialog { mate: id, asm: i, before, active: 0 });
            }
        }
        Some(("suppress", id)) => {
            checkpoint(app);
            if let Some(a) = asm_mut(app) {
                if let Some(m) = a.mates.iter_mut().find(|m| m.id == id) {
                    m.suppressed = !m.suppressed;
                }
            }
            solve_suspect(app, &[], &[id]);
        }
        Some(("delete", id)) => {
            checkpoint(app);
            if let Some(a) = asm_mut(app) {
                a.mates.retain(|m| m.id != id);
            }
            solve(app, &[]);
            app.asm.scene_dirty = true;
        }
        _ => {}
    }
}

/// A bill of materials line.
pub struct BomRow {
    pub item: usize,
    pub name: String,
    pub qty: usize,
    pub material: String,
    /// Mass each, g.
    pub mass: Option<f64>,
    /// Raw stock each, like "Flat bar 10 × 40 × 85 mm" ("" for parts not cut from stock).
    pub stock: String,
}

pub fn bom(app: &dyn crate::drawing_ui::DocCtx, a: &Assembly) -> Vec<BomRow> {
    let mut rows: Vec<((usize, crate::doc::BodyId), BomRow)> = vec![];
    // hidden parts are only out of sight: they are still in the assembly, at every level
    for leaf in a.instances.iter().flat_map(|x| assembly::leaves_with(&app.ws().assemblies, x, true).into_iter().map(move |l| (x, l))) {
        let (x, l) = leaf;
        let key = (l.studio, l.body);
        if let Some(r) = rows.iter_mut().find(|r| r.0 == key) {
            r.1.qty += 1;
            continue;
        }
        let doc = app.studio_doc(l.studio);
        let props = doc.and_then(|d| d.parts.get(&crate::doc::body_key(l.body))).cloned().unwrap_or_default();
        let mesh = app.bodies(l.studio).iter().find(|b| b.id == l.body);
        // parts from different Part Studios all start out as "Part 1": say which studio
        let studio_name = app.ws().studios.get(l.studio).map(|s| s.name.clone()).filter(|_| app.ws().studios.len() > 1);
        let name = props.name.clone().or_else(|| mesh.map(|m| match &studio_name {
            Some(s) => format!("{s} / {}", m.name),
            None => m.name.clone(),
        })).unwrap_or_else(|| x.name.clone());
        let density = props.material.as_ref().and_then(|m| crate::doc::MATERIALS.iter().find(|q| q.0 == m)).map(|q| q.1);
        let mass = match (density, mesh) {
            (Some(d), Some(m)) => Some(m.volume / 1000.0 * d),
            _ => None,
        };
        // bought parts (screws, bearings) are not cut from stock
        let bought = app.ws().studios.get(l.studio).is_some_and(|s| s.name == crate::stdparts::STUDIO);
        let stock = if bought { String::new() } else { crate::stock_size::bom_text(mesh, &props, doc.map(|d| d.units).unwrap_or_default()) };
        rows.push((key, BomRow { item: 0, name, qty: 1, material: props.material.unwrap_or_default(), mass, stock }));
    }
    rows.into_iter().enumerate().map(|(i, r)| BomRow { item: i + 1, ..r.1 }).collect()
}

const DIRS: [(&str, [f64; 3]); 6] = [("+X", [1.0, 0.0, 0.0]), ("−X", [-1.0, 0.0, 0.0]), ("+Y", [0.0, 1.0, 0.0]), ("−Y", [0.0, -1.0, 0.0]), ("+Z", [0.0, 0.0, 1.0]), ("−Z", [0.0, 0.0, -1.0])];

fn explode_ui(app: &mut App, ui: &mut Ui) {
    let Some(a) = asm(app) else { return };
    let steps = a.explode_steps.clone();
    let names = |ids: &[u64]| ids.iter().filter_map(|i| a.instance(*i).map(|x| x.name.clone())).collect::<Vec<_>>().join(", ");
    let labels: Vec<String> = steps.iter().map(|s| names(&s.insts)).collect();
    if steps.is_empty() {
        ui.label(RichText::new("Spreading parts out from the centre. Select parts and add steps to explode them in order.").color(theme::muted()).size(12.0));
    }
    let mut edited = steps.clone();
    let mut delete = None;
    for (i, st) in edited.iter_mut().enumerate() {
        ui.label(RichText::new(format!("Step {} · {}", i + 1, labels[i])).size(12.5));
        ui.horizontal(|ui| {
            let cur = DIRS.iter().find(|d| d.1 == st.dir).map(|d| d.0).unwrap_or("custom");
            egui::ComboBox::from_id_salt(("xdir", i)).width(54.0).selected_text(cur).show_ui(ui, |ui| {
                for (n, d) in DIRS {
                    if ui.selectable_label(st.dir == d, n).clicked() {
                        st.dir = d;
                    }
                }
            });
            ui.add(egui::DragValue::new(&mut st.distance).speed(1.0).suffix(" mm"));
            if ui.add(egui::Button::image(icons::image("close", 12.0).tint(theme::muted())).frame(false)).on_hover_text("Delete step").clicked() {
                delete = Some(i);
            }
        });
    }
    let sel = app.asm.sel.clone();
    let add = ui.add_enabled(!sel.is_empty(), egui::Button::new("Add step from selection")).on_disabled_hover_text("Select parts to move in this step").clicked();
    if edited != steps || delete.is_some() || add {
        checkpoint(app);
        let n = edited.len();
        if let Some(a) = asm_mut(app) {
            a.explode_steps = edited;
            if let Some(i) = delete {
                a.explode_steps.remove(i);
            }
            if add {
                a.explode_steps.push(assembly::ExplodeStep { insts: sel, dir: [0.0, 0.0, 1.0], distance: 50.0 });
            }
        }
        if add || (n == 0) {
            app.asm.explode = 1.0;
        }
        app.asm.scene_dirty = true;
    }
}

/// Distance / angle text and the two world points of the measure picks.
fn measure_result(picks: &[(DVec3, Option<DVec3>)]) -> Option<(DVec3, DVec3, String)> {
    let [(a, na), (b, nb)] = picks else { return None };
    let f = |v: f64| crate::expr::trim(v, 3);
    if let (Some(na), Some(nb)) = (na, nb) {
        if na.cross(*nb).length() < 1e-6 {
            let d = (*b - *a).dot(*na).abs();
            return Some((*b - *na * (*b - *a).dot(*na), *b, format!("distance {} mm", f(d))));
        }
        return Some((*a, *b, format!("angle {}°", crate::expr::trim(na.angle_between(*nb).to_degrees(), 2))));
    }
    let dd = *b - *a;
    Some((*a, *b, format!("distance {} mm (dx {}, dy {}, dz {})", f(dd.length()), f(dd.x.abs()), f(dd.y.abs()), f(dd.z.abs()))))
}

fn bom_ui(app: &mut App, ui: &mut Ui) {
    let Some(a) = asm(app) else { return };
    let rows = bom(app, a);
    if rows.is_empty() {
        ui.label(RichText::new("Empty").color(theme::muted()).size(12.0));
        return;
    }
    let any_stock = rows.iter().any(|r| !r.stock.is_empty());
    egui::Grid::new("bom").num_columns(if any_stock { 5 } else { 4 }).striped(true).spacing([8.0, 3.0]).show(ui, |ui| {
        for h in ["#", "Part", "Qty", "Material", "Stock"].into_iter().take(if any_stock { 5 } else { 4 }) {
            ui.label(RichText::new(h).color(theme::muted()).size(11.5));
        }
        ui.end_row();
        for r in &rows {
            ui.label(RichText::new(r.item.to_string()).size(12.0));
            ui.label(RichText::new(&r.name).size(12.0));
            ui.label(RichText::new(r.qty.to_string()).size(12.0));
            ui.label(RichText::new(if r.material.is_empty() { "—" } else { &r.material }).size(12.0));
            if any_stock {
                ui.label(RichText::new(if r.stock.is_empty() { "—" } else { &r.stock }).size(12.0));
            }
            ui.end_row();
        }
    });
    let total: f64 = rows.iter().filter_map(|r| r.mass.map(|m| m * r.qty as f64)).sum();
    if total > 0.0 {
        ui.label(RichText::new(format!("Total mass {} g", crate::expr::trim(total, 1))).color(theme::muted()).size(12.0));
    }
    if ui.button("Export CSV…").clicked() {
        let mut csv = String::from("Item,Part,Quantity,Material,Mass each (g),Stock\n");
        for r in &rows {
            let q = |t: &str| t.replace('"', "'");
            csv.push_str(&format!("{},\"{}\",{},{},{},\"{}\"\n", r.item, q(&r.name), r.qty, r.material, r.mass.map(|v| format!("{v:.2}")).unwrap_or_default(), q(&r.stock)));
        }
        if let Some(p) = app.save_file_dialog("CSV", &["csv"], "bom.csv") {
            match std::fs::write(&p, csv) {
                Ok(()) => app.flash_info(format!("Exported {}", p.display()), Some(std::time::Duration::from_secs(3))),
                Err(e) => app.flash(format!("Could not export: {e}"), ToastKind::Error),
            }
        }
    }
}

// ---------------------------------------------------------------- viewport

fn world_tri(inst: &Instance, m: &crate::kernel::BodyMesh, t: &[u32]) -> [DVec3; 3] {
    let v = |i: u32| inst.pose.apply(DVec3::new(m.positions[3 * i as usize] as f64, m.positions[3 * i as usize + 1] as f64, m.positions[3 * i as usize + 2] as f64));
    [v(t[0]), v(t[1]), v(t[2])]
}

fn pick(app: &App, s: Vec2) -> Option<Hit> {
    let a = asm(app)?;
    let cam = &app.camera;
    // edges first: the nearest one on screen that no part hides
    let mut near: Vec<(f32, DVec3, Hit)> = vec![];
    for x in a.instances.iter().filter(|x| shown(app, x)) {
      for (leaf, part) in parts_of(app, x).into_iter().enumerate() {
        let Some(part) = part else { continue };
        let m = part.mesh;
        let pose = display_pose(app, a, x).compose(&part.local);
        for (ei, e) in m.edges.iter().enumerate() {
            let world: Vec<DVec3> = e.pts.iter().map(|p| pose.apply(DVec3::from(p.map(|v| v as f64)))).collect();
            let pts: Vec<Vec2> = world.iter().map(|p| cam.project(p.as_vec3())).collect();
            for (k, w) in pts.windows(2).enumerate() {
                let d = w[1] - w[0];
                let t = if d.length_squared() < 1e-9 { 0.0 } else { ((s - w[0]).dot(d) / d.length_squared()).clamp(0.0, 1.0) };
                let dist = (w[0] + d * t - s).length();
                if dist < 6.0 {
                    // the view is orthographic, so the screen parameter carries over to the 3D point
                    let at = world[k] + (world[k + 1] - world[k]) * t as f64;
                    near.push((dist, at, Hit { inst: x.id, leaf, face: None, edge: Some(ei), local: e.mid }));
                }
            }
        }
      }
    }
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((_, _, h)) = near.into_iter().find(|(_, p, _)| !occluded(app, a, *p)) {
        return Some(h);
    }
    let (o, d) = cam.ray(s);
    let (o, d) = (o.as_dvec3(), d.as_dvec3());
    let mut best: Option<(f64, Hit)> = None;
    for x in a.instances.iter().filter(|x| shown(app, x)) {
      for (leaf, part) in parts_of(app, x).into_iter().enumerate() {
        let Some(part) = part else { continue };
        let m = part.mesh;
        // ray into the part's coordinates
        let pose = display_pose(app, a, x).compose(&part.local);
        let lo = pose.inverse_apply(o);
        let ld = pose.inverse_dir(d);
        for (ti, t) in m.indices.chunks(3).enumerate() {
            let v = |i: u32| DVec3::new(m.positions[3 * i as usize] as f64, m.positions[3 * i as usize + 1] as f64, m.positions[3 * i as usize + 2] as f64);
            if let Some(tt) = ray_tri(lo, ld, v(t[0]), v(t[1]), v(t[2])) {
                if best.as_ref().map(|b| tt < b.0).unwrap_or(true) {
                    best = Some((tt, Hit { inst: x.id, leaf, face: Some(m.tri_face[ti] as usize), edge: None, local: (lo + ld * tt).into() }));
                }
            }
        }
      }
    }
    best.map(|b| b.1)
}

/// Whether a shown part lies in front of world point `p` (an edge behind another part).
fn occluded(app: &App, a: &Assembly, p: DVec3) -> bool {
    let cam = &app.camera;
    let (o, d) = cam.ray(cam.project(p.as_vec3()));
    let (o, d) = (o.as_dvec3(), d.as_dvec3().normalize());
    let depth = (p - o).dot(d);
    // the faces that end at the edge meet the ray at its own depth: a few pixels of slack
    let slack = cam.wpp() as f64 * 3.0;
    for x in a.instances.iter().filter(|x| shown(app, x)) {
        for part in parts_of(app, x).into_iter().flatten() {
            let m = part.mesh;
            let pose = display_pose(app, a, x).compose(&part.local);
            let (lo, ld) = (pose.inverse_apply(o), pose.inverse_dir(d));
            for t in m.indices.chunks(3) {
                let v = |i: u32| DVec3::new(m.positions[3 * i as usize] as f64, m.positions[3 * i as usize + 1] as f64, m.positions[3 * i as usize + 2] as f64);
                if let Some(tt) = ray_tri(lo, ld, v(t[0]), v(t[1]), v(t[2])) {
                    if tt < depth - slack {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn ray_tri(o: DVec3, d: DVec3, a: DVec3, b: DVec3, c: DVec3) -> Option<f64> {
    let e1 = b - a;
    let e2 = c - a;
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let tv = o - a;
    let u = tv.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = tv.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some(t)
}

fn connector_for(app: &App, h: &Hit) -> Option<Connector> {
    let a = asm(app)?;
    let x = a.instance(h.inst)?;
    let part = parts_of(app, x).into_iter().nth(h.leaf)??;
    let c = assembly::infer_connector(h.inst, h.leaf, part.mesh, h.face, h.edge, h.local.into())?;
    // from the part's coordinates into the instance's
    let l = part.local;
    Some(Connector { origin: l.apply(c.origin.into()).into(), z: l.dir(c.z.into()).into(), x: l.dir(c.x.into()).into(), ..c })
}

/// World point and (for faces) normal of a hit, at its displayed pose.
fn hit_world(app: &App, h: &Hit) -> Option<(DVec3, Option<DVec3>)> {
    let a = asm(app)?;
    let x = a.instance(h.inst)?;
    let part = parts_of(app, x).into_iter().nth(h.leaf)??;
    let pose = display_pose(app, a, x).compose(&part.local);
    let n = h.face.and_then(|f| part.mesh.faces.get(f)).filter(|f| f.planar).map(|f| pose.dir(DVec3::from(f.normal)).normalize());
    Some((pose.apply(h.local.into()), n))
}

fn build_scene(app: &App) -> SceneData {
    let mut s = SceneData::default();
    let Some(a) = asm(app) else { return s };
    let dialog_conns: Vec<Connector> =
        app.asm.dialog.as_ref().and_then(|d| a.mates.iter().find(|m| m.id == d.mate)).map(|m| m.a.iter().chain(m.b.iter()).copied().collect()).unwrap_or_default();
    let mate_insts: Vec<u64> =
        app.asm.sel_mate.filter(|_| app.asm.dialog.is_none()).and_then(|id| a.mates.iter().find(|m| m.id == id)).map(|m| m.a.iter().chain(m.b.iter()).map(|c| c.inst).collect()).unwrap_or_default();
    for x in a.instances.iter().filter(|x| shown(app, x)) {
      for (leaf, part) in parts_of(app, x).into_iter().enumerate() {
        let Some(part) = part else { continue };
        let m = part.mesh;
        let sel = app.asm.sel.contains(&x.id) || mate_insts.contains(&x.id);
        let base = part.color;
        let hov_face = app.asm.hover.filter(|h| h.inst == x.id && h.leaf == leaf).and_then(|h| h.face);
        let pose = display_pose(app, a, x).compose(&part.local);
        let rot = pose.m().as_mat3();
        for (ti, t) in m.indices.chunks(3).enumerate() {
            let fi = m.tri_face[ti] as usize;
            let col = if hov_face == Some(fi) {
                vp::hover()
            } else if sel {
                vp::picked()
            } else {
                base
            };
            for &i in t {
                let i = i as usize;
                let p = pose.apply(DVec3::new(m.positions[3 * i] as f64, m.positions[3 * i + 1] as f64, m.positions[3 * i + 2] as f64));
                let n = rot * Vec3::new(m.normals[3 * i], m.normals[3 * i + 1], m.normals[3 * i + 2]);
                s.opaque.push(MeshVertex { pos: p.as_vec3().into(), normal: n.into(), color: crate::viewport::rgba(col, 1.0), finish: if app.realistic { 2.0 } else { 0.0 } });
            }
        }
        let hov_edge = app.asm.hover.filter(|h| h.inst == x.id && h.leaf == leaf).and_then(|h| h.edge);
        for (ei, e) in m.edges.iter().enumerate() {
            let pts: Vec<Vec3> = e.pts.iter().map(|p| pose.apply(DVec3::from(p.map(|v| v as f64))).as_vec3()).collect();
            let hov = hov_edge == Some(ei);
            s.polyline(&pts, if hov { 3.0 } else { 1.4 }, crate::viewport::rgba(if hov { vp::hover() } else { vp::edge() }, 1.0), hov);
        }
        let _ = world_tri;
      }
    }
    // mate connectors being picked
    let size = (app.camera.wpp() * 28.0) as f64;
    for c in &dialog_conns {
        if let Some((o, z, xx)) = a.world(c) {
            let y = z.cross(xx);
            for (d, col) in [(xx, vp::axis_x()), (y, vp::axis_y()), (z, vp::axis_z())] {
                s.lines_top.push(crate::render::LineInst {
                    a: o.as_vec3().into(),
                    b: (o + d * size).as_vec3().into(),
                    width: 2.5,
                    bias: 0.0,
                    color: crate::viewport::rgba(col, 1.0),
                });
            }
            s.point(o.as_vec3(), 8.0, crate::viewport::rgba(vp::snap(), 1.0));
        }
    }
    s
}

pub fn viewport(app: &mut App, ui: &mut Ui) {
    crate::stdparts_ui::window(app, &ui.ctx().clone());
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    if app.viewport_rect != rect {
        app.viewport_rect = rect;
        app.asm.scene_dirty = true;
    }
    app.camera.size = Vec2::new(rect.width().max(1.0), rect.height().max(1.0));
    crate::viewport::paint_background(ui.painter(), rect, app.realistic);
    let before = (app.camera.target, app.camera.back, app.camera.up, app.camera.half_height);
    let free = app.asm.hover.is_none() && app.asm.dialog.is_none() && app.asm.measure.is_none();
    crate::viewport::navigate(app, ui, &resp, free);
    if before != (app.camera.target, app.camera.back, app.camera.up, app.camera.half_height) {
        app.asm.scene_dirty = true;
    }
    let local = |p: Pos2| Vec2::new(p.x - rect.min.x, p.y - rect.min.y);

    follow_parts(app);
    if asm(app).map(|a| a.instances.is_empty()).unwrap_or(true) && !app.asm.insert_open {
        app.asm.insert_open = true;
    }

    // hover
    if app.nav.is_none() && app.asm.drag.is_none() {
        let h = resp.hover_pos().and_then(|p| pick(app, local(p)));
        if h != app.asm.hover {
            app.asm.hover = h;
            app.asm.scene_dirty = true;
        }
    }

    // clicks: mate picks or selection
    if resp.clicked_by(egui::PointerButton::Primary) {
        let h = app.asm.hover;
        if app.asm.measure.is_some() {
            if let Some(p) = h.and_then(|h| hit_world(app, &h)) {
                let m = app.asm.measure.as_mut().unwrap();
                if m.len() >= 2 {
                    m.clear();
                }
                m.push(p);
            }
        } else if let Some(d) = &app.asm.dialog {
            if let Some(h) = h {
                // both sides of a mate must be on different parts
                let other = asm(app).and_then(|a| a.mates.iter().find(|m| m.id == d.mate)).and_then(|m| if d.active == 0 { m.b } else { m.a });
                if other.map(|o| o.inst == h.inst).unwrap_or(false) {
                    app.flash("Pick the second side on a different part", ToastKind::Info);
                } else if let Some(c) = connector_for(app, &h) {
                    let (mid, active) = (d.mate, d.active);
                    let mut solve_now = false;
                    if let Some(a) = asm_mut(app) {
                        if let Some(m) = a.mates.iter_mut().find(|m| m.id == mid) {
                            if active == 0 {
                                m.a = Some(c);
                            } else {
                                m.b = Some(c);
                            }
                            solve_now = m.a.is_some() && m.b.is_some() && m.a.unwrap().inst != m.b.unwrap().inst;
                        }
                    }
                    if let Some(d) = &mut app.asm.dialog {
                        d.active = if active == 0 { 1 } else { 0 };
                    }
                    if solve_now {
                        // the instance with fewer mates (or inserted later) moves to meet the other
                        let hold = asm(app).map(|a| assembly::mate_anchor(a, mid)).unwrap_or_default();
                        let before: Vec<Pose> = asm(app).map(|a| a.instances.iter().map(|x| x.pose).collect()).unwrap_or_default();
                        if let Some(a) = asm_mut(app) {
                            assembly::place_new_mate(a, mid, &hold);
                        }
                        solve(app, &hold);
                        if app.asm.failed.contains(&mid) {
                            // it can't be met with the other mates: leave the parts where they were,
                            // not wherever the placing put them along the travel the others leave free
                            if let Some(a) = asm_mut(app) {
                                for (x, p) in a.instances.iter_mut().zip(&before) {
                                    x.pose = *p;
                                }
                            }
                            solve(app, &hold);
                        }
                    }
                    app.asm.scene_dirty = true;
                }
            }
        } else {
            let add = ui.input(|i| i.modifiers.shift || i.modifiers.command);
            if !add {
                app.asm.sel.clear();
            }
            app.asm.sel_mate = None;
            if let Some(h) = h {
                if let Some(i) = app.asm.sel.iter().position(|x| *x == h.inst) {
                    app.asm.sel.remove(i);
                } else {
                    app.asm.sel.push(h.inst);
                }
            }
            app.asm.scene_dirty = true;
        }
    }

    if resp.secondary_clicked() && app.asm.dialog.is_none() && app.asm.measure.is_none() {
        if let Some(p) = resp.interact_pointer_pos().or(resp.hover_pos()) {
            app.asm.ctx = Some((p, app.asm.hover.map(|h| h.inst)));
        }
    }

    // dragging instances (left drag; Shift rotates)
    if app.asm.dialog.is_none() {
        if resp.drag_started_by(egui::PointerButton::Primary) {
            if let Some(h) = app.asm.hover {
                let fixed = asm(app).and_then(|a| a.instance(h.inst)).map(|x| x.fixed).unwrap_or(true);
                if !fixed {
                    checkpoint(app);
                    let start = asm(app).and_then(|a| a.instance(h.inst)).map(|x| x.pose).unwrap_or_default();
                    let rotate = ui.input(|i| i.modifiers.shift);
                    // slide in the plane of the grabbed face, else in the world plane facing the camera most
                    let (p, n) = hit_world(app, &h).unwrap_or((DVec3::ZERO, None));
                    let back = app.camera.back.as_dvec3().normalize();
                    let world = [DVec3::Z, DVec3::Y, DVec3::X].into_iter().max_by(|a, b| a.dot(back).abs().partial_cmp(&b.dot(back).abs()).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(DVec3::Z);
                    let n = n.filter(|n| n.dot(back).abs() > 0.2).unwrap_or(world);
                    app.asm.drag = Some(Drag { inst: h.inst, start, rotate, moved: Vec2::ZERO, plane: (p, n), grab: start.inverse_apply(p) });
                }
            }
        }
        if let Some(mut d) = app.asm.drag.take() {
            let down = ui.input(|i| i.pointer.primary_down());
            let delta = ui.input(|i| i.pointer.delta());
            d.moved += Vec2::new(delta.x, delta.y);
            let cam = app.camera.clone();
            let c_local = asm(app).and_then(|a| a.instance(d.inst)).map(|x| centroid_local(app, x)).unwrap_or(DVec3::ZERO);
            // each frame moves the part on from where the mates last left it (not from where the
            // drag started), so a part on a revolute swings round after the pointer step by step
            // and one at a limit stays there however far the drag goes
            let cur = asm(app).and_then(|a| a.instance(d.inst)).map(|x| x.pose).unwrap_or(d.start);
            let mut target = None;
            let pose = if d.rotate {
                let c = cur.apply(c_local);
                let w = (cam.up.as_dvec3() * delta.x as f64 + cam.right().as_dvec3() * delta.y as f64) * 0.01;
                cur.perturbed(w, DVec3::ZERO, c)
            } else {
                // the grabbed point follows the pointer across its plane
                let (p0, n) = d.plane;
                let hit = |s: Vec2| {
                    let (o, dir) = cam.ray(s);
                    let (o, dir) = (o.as_dvec3(), dir.as_dvec3());
                    let den = dir.dot(n);
                    (den.abs() > 1e-9).then(|| o + dir * ((p0 - o).dot(n) / den))
                };
                let now = ui.input(|i| i.pointer.latest_pos()).map(local);
                let t = match now.and_then(hit).filter(|t| (*t - p0).length() < 1e6) {
                    Some(t) => t,
                    None => {
                        let wpp = cam.wpp() as f64;
                        d.start.apply(d.grab) + cam.right().as_dvec3() * (d.moved.x as f64 * wpp) - cam.up.as_dvec3() * (d.moved.y as f64 * wpp)
                    }
                };
                target = Some(t);
                cur.perturbed(DVec3::ZERO, t - cur.apply(d.grab), DVec3::ZERO)
            };
            let id = d.inst;
            if let Some(a) = asm_mut(app) {
                if let Some(x) = a.instances.iter_mut().find(|x| x.id == id) {
                    x.pose = pose;
                }
            }
            if d.rotate {
                solve(app, &[id]);
            } else {
                solve_drag(app, id, d.grab);
            }
            if let (Some(t), Some(now)) = (target, asm(app).and_then(|a| a.instance(id)).map(|x| x.pose)) {
                // still closing in on the pointer: keep going without waiting for the mouse
                let (before, after) = ((t - cur.apply(d.grab)).length(), (t - now.apply(d.grab)).length());
                if down && after > 0.05 && before - after > 1e-3 {
                    ui.ctx().request_repaint();
                }
            }
            if down {
                app.asm.drag = Some(d);
            }
        }
    }

    if app.asm.scene_dirty {
        app.asm.scene_dirty = false;
        let mut s = build_scene(app);
        s.version = app.scene.version.wrapping_add(1);
        app.scene = Arc::new(s);
    }
    let ppp = ui.ctx().pixels_per_point();
    let cb = ViewportCallback {
        scene: app.scene.clone(),
        view_proj: app.camera.view_proj(),
        eye_dir: app.camera.back,
        clip: app.section.map(|s| s.clip()).unwrap_or([0.0; 4]),
        size_px: [rect.width() * ppp, rect.height() * ppp],
        pixels_per_point: ppp,
        realistic: app.realistic,
    };
    ui.painter().add(eframe::egui_wgpu::Callback::new_paint_callback(rect, cb));
    let painter = ui.painter_at(rect);
    crate::viewport::view_cube(app, ui, rect);
    if let Some(n) = isolated(app).map(|k| k.len()) {
        if crate::viewport::isolation_banner(ui, rect, &format!("Isolated: {n} instance{}", if n == 1 { "" } else { "s" })) {
            app.end_isolation();
        }
    }
    if let (Some(h), Some(p), None) = (app.asm.hover, resp.hover_pos(), app.asm.ctx) {
        if let Some(x) = asm(app).and_then(|a| a.instance(h.inst)) {
            let what = if h.edge.is_some() { "edge" } else { "face" };
            crate::viewport::label_box(&painter, p + egui::vec2(14.0, 18.0), &format!("{} · {what}", x.name), theme::text());
        }
    }
    if let Some(m) = &app.asm.measure {
        let to_screen = |p: DVec3| {
            let v = app.camera.project(p.as_vec3());
            rect.min + egui::vec2(v.x, v.y)
        };
        for (p, _) in m {
            painter.circle_filled(to_screen(*p), 4.0, vp::dim());
        }
        let text = match measure_result(m) {
            Some((a, b, label)) => {
                let (sa, sb) = (to_screen(a), to_screen(b));
                painter.line_segment([sa, sb], Stroke::new(1.4, vp::dim()));
                crate::viewport::label_box(&painter, sa + (sb - sa) / 2.0 + egui::vec2(0.0, -14.0), &label, vp::dim());
                format!("Measure: {label} · click again to start over")
            }
            None => "Measure: click a face or edge, then another".into(),
        };
        crate::viewport::label_box(&painter, rect.left_top() + egui::vec2(rect.width() / 2.0 - 120.0, 16.0), &text, theme::text());
    }
    mate_dialog(app, ui, rect);
    viewport_menu(app, ui);
}

/// Right-click in the viewport: hide, show and isolate.
fn viewport_menu(app: &mut App, ui: &mut Ui) {
    let Some((pos, target)) = app.asm.ctx else { return };
    if app.nav.as_ref().is_some_and(|n| n.moved > 3.0) {
        app.asm.ctx = None;
        return;
    }
    let target = target.filter(|id| asm(app).is_some_and(|a| a.instance(*id).is_some()));
    let mut close = false;
    let area = egui::Area::new(egui::Id::new("asm-ctx-menu")).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
        egui::Frame::menu(ui.style()).fill(theme::panel_2()).show(ui, |ui| {
            ui.set_min_width(200.0);
            if let Some(id) = target {
                let name = asm(app).and_then(|a| a.instance(id)).map(|x| x.name.clone()).unwrap_or_default();
                ui.label(RichText::new(name).color(theme::muted()).size(11.0));
                // the selected instances when the clicked one is among them, else just that one
                let these = if app.asm.sel.contains(&id) { app.asm.sel.clone() } else { vec![id] };
                let n = these.len();
                if ui.button(if n > 1 { format!("Hide {n} instances") } else { "Hide".to_string() }).on_hover_text("H").clicked() {
                    set_hidden(app, &these, true);
                    close = true;
                }
                if ui.button("Hide others").clicked() {
                    hide_others(app, &these);
                    close = true;
                }
                if ui.button(if n > 1 { format!("Isolate {n} instances") } else { "Isolate".to_string() }).on_hover_text("I").clicked() {
                    isolate(app, these.clone());
                    close = true;
                }
                let all_fixed = asm(app).is_some_and(|a| these.iter().all(|id| a.instance(*id).is_some_and(|x| x.fixed)));
                if ui.button(if all_fixed { "Unfix" } else { "Fix" }).on_hover_text("Fixed instances stay where they are when mates are solved").clicked() {
                    set_fixed(app, &these, !all_fixed);
                    close = true;
                }
                ui.separator();
            }
            if any_hidden(app) && ui.button("Show all").on_hover_text("Shift+H").clicked() {
                show_all(app);
                close = true;
            }
            if app.isolation.is_some() && ui.button("Exit isolate").clicked() {
                app.end_isolation();
                close = true;
            }
            if ui.button("Zoom to fit").on_hover_text("F").clicked() {
                fit(app);
                close = true;
            }
            if !app.asm.sel.is_empty() && ui.button("Clear selection").clicked() {
                app.asm.sel.clear();
                app.asm.scene_dirty = true;
                close = true;
            }
        });
    });
    let clicked_elsewhere = ui.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer() && !ui.input(|i| i.pointer.secondary_pressed());
    if close || clicked_elsewhere || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.asm.ctx = None;
    }
}

fn check_interference(app: &mut App) {
    let Some(a) = asm(app) else { return };
    let mut items: Vec<(usize, crate::doc::BodyId, [f64; 12])> = vec![];
    let mut ids = vec![];
    for x in a.instances.iter().filter(|x| shown(app, x)) {
        for l in assembly::leaves(&app.ws.assemblies, x) {
            let p = x.pose.compose(&l.pose);
            let m = p.m();
            let t = p.t;
            items.push((l.studio, l.body, [m.x_axis.x, m.y_axis.x, m.z_axis.x, t[0], m.x_axis.y, m.y_axis.y, m.z_axis.y, t[1], m.x_axis.z, m.y_axis.z, m.z_axis.z, t[2]]));
            ids.push(x.id);
        }
    }
    app.asm.clash_ids = ids;
    app.asm.clash_stale = false;
    let docs = app.studio_docs();
    app.kernel.set_studios(docs);
    app.kernel.interference(items);
    app.asm.checking = true;
    app.asm.clashes = None;
}

pub fn interference_result(app: &mut App, pairs: Vec<(usize, usize, f64)>) {
    let ids = app.asm.clash_ids.clone();
    // clashes inside one sub-assembly belong to that assembly, not this one
    app.asm.clashes = Some(pairs.into_iter().filter_map(|(i, j, v)| Some((*ids.get(i)?, *ids.get(j)?, v))).filter(|(p, q, _)| p != q).collect());
    app.asm.checking = false;
    app.asm.scene_dirty = true;
}

fn clashes_ui(app: &mut App, ui: &mut Ui) {
    if app.asm.checking {
        ui.label(RichText::new("Checking…").color(theme::muted()).size(12.0));
        return;
    }
    let Some(list) = app.asm.clashes.clone() else { return };
    let Some(a) = asm(app) else { return };
    let stale = app.asm.clash_stale;
    if stale {
        ui.label(RichText::new("Out of date: parts changed since this check. Run Interference again.").color(theme::muted()).italics().size(12.0));
    }
    if list.is_empty() {
        ui.label(RichText::new("No parts overlap").color(if stale { theme::muted() } else { theme::ok_green() }).size(12.5));
    }
    let name = |id: u64| a.instance(id).map(|x| x.name.clone()).unwrap_or_default();
    let rows: Vec<(u64, u64, String)> = list.iter().map(|(p, q, v)| (*p, *q, format!("{} overlaps {} · {} mm³", name(*p), name(*q), crate::expr::trim(*v, 2)))).collect();
    for (p, q, text) in rows {
        if ui.add(egui::Button::new(RichText::new(text).size(12.0).color(if stale { theme::muted() } else { theme::error_soft() })).min_size(egui::vec2(ui.available_width(), 20.0))).clicked() {
            app.asm.sel = vec![p, q];
            app.asm.scene_dirty = true;
        }
    }
}

fn relations(app: &mut App, ui: &mut Ui) {
    // the one being made
    if let Some((kind, mut ma, mut mb, mut ratio, mut reverse)) = app.asm.relation {
        let Some(a) = asm(app) else { return };
        let mates: Vec<(u64, String, MateKind)> = a.mates.iter().map(|m| (m.id, m.name.clone(), m.kind)).collect();
        let want_a: &[MateKind] = match kind {
            RelationKind::Gear | RelationKind::Rack => &[MateKind::Revolute, MateKind::Cylindrical],
            RelationKind::Screw => &[MateKind::Cylindrical],
        };
        let want_b: &[MateKind] = match kind {
            RelationKind::Gear => &[MateKind::Revolute, MateKind::Cylindrical],
            _ => &[MateKind::Slider, MateKind::Cylindrical],
        };
        ui.label(RichText::new(kind.label()).strong());
        let pick = |ui: &mut Ui, id: &str, label: &str, cur: &mut Option<u64>, want: &[MateKind], other: Option<u64>| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).color(theme::muted()).size(12.0));
                let name = cur.and_then(|c| mates.iter().find(|m| m.0 == c)).map(|m| m.1.clone()).unwrap_or_else(|| "Choose…".into());
                egui::ComboBox::from_id_salt(id).selected_text(name).show_ui(ui, |ui| {
                    for (mid, n, k) in &mates {
                        // a mate can't drive itself
                        if want.contains(k) && other != Some(*mid) && ui.selectable_label(*cur == Some(*mid), n).clicked() {
                            *cur = Some(*mid);
                        }
                    }
                });
            });
        };
        let other_b = if kind == RelationKind::Screw { None } else { mb };
        pick(ui, "rel-a", "Driving mate", &mut ma, want_a, other_b);
        if kind != RelationKind::Screw {
            pick(ui, "rel-b", "Driven mate", &mut mb, want_b, ma);
        }
        ui.horizontal(|ui| {
            let label = match kind {
                RelationKind::Gear => "Ratio",
                RelationKind::Rack => "Pitch radius (mm)",
                RelationKind::Screw => "Pitch (mm/turn)",
            };
            ui.label(RichText::new(label).color(theme::muted()).size(12.0));
            ui.add(egui::DragValue::new(&mut ratio).speed(0.05));
        });
        ui.checkbox(&mut reverse, "Reverse direction");
        let ready = ma.is_some() && (kind == RelationKind::Screw || mb.is_some());
        let mut done = false;
        ui.horizontal(|ui| {
            if ui.add_enabled(ready, egui::Button::new("OK")).clicked() {
                checkpoint(app);
                if let Some(asm) = asm_mut(app) {
                    let id = asm.new_id();
                    let n = asm.relations.iter().filter(|r| r.kind == kind).count() + 1;
                    let mut r = Relation { id, name: format!("{} {n}", kind.label()), kind, a: ma.unwrap(), b: mb, ratio, reverse, phase: 0.0 };
                    r.phase = assembly::relation_phase(asm, &r);
                    asm.relations.push(r);
                }
                done = true;
            }
            if ui.button("Cancel").clicked() {
                done = true;
            }
        });
        app.asm.relation = if done { None } else { Some((kind, ma, mb, ratio, reverse)) };
        ui.separator();
    }
    let Some(a) = asm(app) else { return };
    let rows: Vec<(u64, String)> = a.relations.iter().map(|r| (r.id, r.name.clone())).collect();
    let mut delete = None;
    for (id, name) in rows {
        let r = ui.add(egui::Button::image_and_text(icons::image("relGear", 14.0).tint(theme::text()), RichText::new(&name).size(12.5)).min_size(egui::vec2(ui.available_width(), 20.0)));
        r.context_menu(|ui| {
            if ui.button("Delete").clicked() {
                delete = Some(id);
                ui.close();
            }
        });
    }
    if let Some(id) = delete {
        checkpoint(app);
        if let Some(a) = asm_mut(app) {
            a.relations.retain(|r| r.id != id);
        }
    }
}

fn mate_dialog(app: &mut App, ui: &mut Ui, rect: egui::Rect) {
    let Some(d) = &app.asm.dialog else { return };
    if asm_index(app) != Some(d.asm) {
        app.asm.dialog = None;
        return;
    }
    let (mid, active) = (d.mate, d.active);
    let Some(a) = asm(app) else { return };
    let Some(m) = a.mates.iter().find(|m| m.id == mid).cloned() else {
        app.asm.dialog = None;
        return;
    };
    let name_of = |c: &Option<Connector>| c.and_then(|c| a.instance(c.inst)).map(|x| x.name.clone());
    let (na, nb) = (name_of(&m.a), name_of(&m.b));
    let mut edited = m.clone();
    let mut ok = false;
    let mut cancel = false;
    let mut set_active = None;
    let units = app.studio_doc(0).map(|d| d.units).unwrap_or_default();
    egui::Area::new(egui::Id::new("mate-dialog")).order(egui::Order::Middle).fixed_pos(rect.min + egui::vec2(10.0, 10.0)).show(ui.ctx(), |ui| {
        egui::Frame::new().fill(theme::window()).stroke(Stroke::new(1.0, theme::border())).corner_radius(8).inner_margin(Margin { left: 10, right: 10, top: 8, bottom: 10 }).show(
            ui,
            |ui| {
                ui.set_width(300.0);
                ui.horizontal(|ui| {
                    ui.add(icons::image(m.kind.icon(), 18.0).tint(theme::text()));
                    ui.label(RichText::new(&m.name).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::image(icons::image("close", 16.0).tint(theme::text()))).on_hover_text("Cancel (Esc)").clicked() {
                            cancel = true;
                        }
                        if ui.add(egui::Button::image(icons::image("check", 16.0).tint(theme::ok_green()))).on_hover_text("OK (Enter)").clicked() {
                            ok = true;
                        }
                    });
                });
                egui::Grid::new("mate-grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label(RichText::new("Type").color(theme::muted()));
                    egui::ComboBox::from_id_salt("mate-kind").selected_text(edited.kind.label()).show_ui(ui, |ui| {
                        for k in MateKind::ALL {
                            ui.selectable_value(&mut edited.kind, k, k.label());
                        }
                    });
                    ui.end_row();
                    for (i, (label, n)) in [("First", &na), ("Second", &nb)].into_iter().enumerate() {
                        ui.label(RichText::new(label).color(theme::muted()));
                        let fr = egui::Frame::new()
                            .fill(theme::bg())
                            .stroke(Stroke::new(1.0, if active == i { theme::accent() } else { theme::border() }))
                            .corner_radius(5)
                            .inner_margin(Margin::symmetric(6, 3));
                        let r = fr
                            .show(ui, |ui| {
                                ui.set_width(180.0);
                                match n {
                                    Some(n) => ui.label(RichText::new(n).size(12.0)),
                                    None => ui.label(RichText::new("Pick a face, edge or circle").italics().color(theme::muted()).size(12.0)),
                                };
                            })
                            .response;
                        if r.interact(Sense::click()).clicked() {
                            set_active = Some(i);
                        }
                        ui.end_row();
                    }
                    ui.label(RichText::new(format!("Offset ({})", units.label())).color(theme::muted()));
                    let mut off = edited.offset / units.scale();
                    if ui.add(egui::DragValue::new(&mut off).speed(0.5)).changed() {
                        edited.offset = off * units.scale();
                    }
                    ui.end_row();
                    ui.label(RichText::new("Offset X / Y").color(theme::muted()));
                    ui.horizontal(|ui| {
                        for k in 0..2 {
                            let mut v = edited.offset_xy[k] / units.scale();
                            if ui.add(egui::DragValue::new(&mut v).speed(0.5)).changed() {
                                edited.offset_xy[k] = v * units.scale();
                            }
                        }
                    });
                    ui.end_row();
                    ui.label(RichText::new("Rotation (°)").color(theme::muted()));
                    ui.add(egui::DragValue::new(&mut edited.rotation).speed(1.0));
                    ui.end_row();
                    if matches!(edited.kind, MateKind::Revolute | MateKind::Slider | MateKind::Cylindrical) {
                        let unit = if edited.kind == MateKind::Revolute { "°" } else { units.label() };
                        let mut on = edited.limits.is_some();
                        ui.label(RichText::new("Limits").color(theme::muted()));
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut on, "").changed() {
                                edited.limits = if on { Some(if edited.kind == MateKind::Revolute { [-90.0, 90.0] } else { [0.0, 50.0] }) } else { None };
                            }
                            if let Some(l) = &mut edited.limits {
                                let sc = if edited.kind == MateKind::Revolute { 1.0 } else { units.scale() };
                                for k in 0..2 {
                                    let mut v = l[k] / sc;
                                    if ui.add(egui::DragValue::new(&mut v).speed(1.0).suffix(format!(" {unit}"))).changed() {
                                        l[k] = v * sc;
                                    }
                                }
                            }
                        });
                        ui.end_row();
                    }
                });
                ui.checkbox(&mut edited.flip, "Flip primary axis");
                ui.add_space(4.0);
                ui.label(RichText::new("Click a face, edge or circular edge on each part · Enter to accept").color(theme::muted()).size(12.0));
            },
        );
    });
    if let Some(i) = set_active {
        if let Some(d) = &mut app.asm.dialog {
            d.active = i;
        }
    }
    let changed = edited.kind != m.kind
        || edited.flip != m.flip
        || edited.offset != m.offset
        || edited.offset_xy != m.offset_xy
        || edited.rotation != m.rotation
        || edited.limits != m.limits;
    if changed {
        if let Some(a) = asm_mut(app) {
            if let Some(mm) = a.mates.iter_mut().find(|x| x.id == mid) {
                *mm = edited.clone();
            }
            if let Some(k) = a.mates.iter().position(|x| x.id == mid).filter(|_| edited.kind != m.kind) {
                // "Revolute 2" becomes the next free "Fastened n", not a second "Fastened 1"
                let auto = |kind: MateKind, name: &str| name.strip_prefix(kind.label()).and_then(|r| r.trim().parse::<u32>().ok()).is_some();
                if auto(m.kind, &m.name) {
                    let used = |name: &str| a.mates.iter().any(|x| x.id != mid && x.name == name);
                    let mut n = 1;
                    while used(&format!("{} {n}", edited.kind.label())) {
                        n += 1;
                    }
                    a.mates[k].name = format!("{} {n}", edited.kind.label());
                } else {
                    a.mates[k].name = m.name.replace(m.kind.label(), edited.kind.label());
                }
            }
        }
        solve(app, &[]);
    }
    if ok {
        mate_ok(app);
    } else if cancel {
        mate_cancel(app);
    }
}

#[allow(dead_code)]
fn rotation_between(a: DVec3, b: DVec3) -> DMat3 {
    let axis = a.cross(b);
    if axis.length() < 1e-12 {
        return DMat3::IDENTITY;
    }
    DMat3::from_axis_angle(axis.normalize(), a.angle_between(b))
}
