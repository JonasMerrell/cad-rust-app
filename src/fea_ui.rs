//! Simulation tab: supports, loads and analysis settings in the sidebar, faces picked in the viewport,
//! and results drawn as colour maps on the deformed mesh.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Color32, Margin, Pos2, RichText, Sense, Stroke, Ui};
use glam::{DVec3, Vec2, Vec3};

use crate::app::App;
use crate::cam_ui::{row, section};
use crate::doc::BodyId;
use crate::fea::{self, Analysis, FacePick, Input, LoadKind, Outcome, ProfileKind, Solution, Study, SupportKind};
use crate::fea_mesh::Mesh;
use crate::icons;
use crate::kernel::BodyMesh;
use crate::render::{MeshVertex, SceneData, ViewportCallback};
use crate::theme::{self, vp};
use crate::workspace::Tab;

/// What the colours show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Field {
    #[default]
    Stress,
    Disp,
    DispX,
    DispY,
    DispZ,
    Safety,
}

impl Field {
    const ALL: [Field; 6] = [Field::Stress, Field::Disp, Field::DispX, Field::DispY, Field::DispZ, Field::Safety];
    fn label(self) -> &'static str {
        match self {
            Field::Stress => "Von Mises stress",
            Field::Disp => "Displacement",
            Field::DispX => "Displacement X",
            Field::DispY => "Displacement Y",
            Field::DispZ => "Displacement Z",
            Field::Safety => "Factor of safety",
        }
    }
}

type JobResult = (u64, Result<Outcome, String>);

pub struct FeaUi {
    pub scene_dirty: bool,
    /// The study and parts the shown result was solved from.
    pub key: u64,
    pub outcome: Option<Arc<Outcome>>,
    pub error: Option<String>,
    job: Option<(u64, Receiver<JobResult>, Arc<AtomicBool>)>,
    progress: Arc<Mutex<(String, f32)>>,
    pub computing: bool,
    /// A change waiting to settle before it is solved.
    pending: Option<(u64, Instant)>,
    mesh_cache: Option<(u64, Arc<Mesh>)>,
    mesh_rx: Option<(u64, Receiver<Arc<Mesh>>)>,
    /// The selected support or load (its faces are picked in the viewport).
    pub sel: Option<u64>,
    pub show_results: bool,
    pub show_mesh: bool,
    pub field: Field,
    /// Deformation as a multiple of the automatic exaggeration (0: undeformed).
    pub deform: f32,
    pub true_scale: bool,
    pub mode: usize,
    pub frame: usize,
    pub playing: bool,
    /// Animation phase (modal) or frame clock (transient).
    phase: f64,
    last_tick: Option<Instant>,
    stress_cache: Option<((usize, usize), Arc<Vec<[f32; 6]>>)>,
    /// Largest stress seen in the frames shown so far, so colours stay put while a time history plays.
    stress_hi: f32,
    undo: Vec<Study>,
    redo: Vec<Study>,
    last_ck: Option<Instant>,
    hover: Option<u32>,
    fitted: bool,
    /// Resolved faces of every support and load, for the current study and parts.
    picks_key: u64,
    picks: Vec<(u64, Vec<u32>)>,
}

impl Default for FeaUi {
    fn default() -> Self {
        FeaUi {
            scene_dirty: Default::default(),
            key: Default::default(),
            outcome: Default::default(),
            error: Default::default(),
            job: Default::default(),
            progress: Default::default(),
            computing: Default::default(),
            pending: Default::default(),
            mesh_cache: Default::default(),
            mesh_rx: Default::default(),
            sel: Default::default(),
            show_results: true,
            show_mesh: Default::default(),
            field: Default::default(),
            deform: 1.0,
            true_scale: Default::default(),
            mode: Default::default(),
            frame: Default::default(),
            playing: Default::default(),
            phase: Default::default(),
            last_tick: Default::default(),
            stress_cache: Default::default(),
            stress_hi: Default::default(),
            undo: Default::default(),
            redo: Default::default(),
            last_ck: Default::default(),
            hover: Default::default(),
            fitted: Default::default(),
            picks_key: Default::default(),
            picks: Default::default(),
        }
    }
}

fn index(app: &App) -> Option<usize> {
    match app.tab {
        Tab::Sim(i) => Some(i),
        _ => None,
    }
}

pub fn study(app: &App) -> Option<&Study> {
    index(app).and_then(|i| app.ws.sims.get(i))
}

pub fn study_mut(app: &mut App) -> Option<&mut Study> {
    let i = index(app)?;
    app.ws.sims.get_mut(i)
}

pub fn undo(app: &mut App) {
    let Some(i) = index(app) else { return };
    if let Some(c) = app.fea.undo.pop() {
        let cur = std::mem::replace(&mut app.ws.sims[i], c);
        app.fea.redo.push(cur);
        app.fea.scene_dirty = true;
    }
}

pub fn redo(app: &mut App) {
    let Some(i) = index(app) else { return };
    if let Some(c) = app.fea.redo.pop() {
        let cur = std::mem::replace(&mut app.ws.sims[i], c);
        app.fea.undo.push(cur);
        app.fea.scene_dirty = true;
    }
}

pub fn can_undo(app: &App) -> bool {
    !app.fea.undo.is_empty()
}

pub fn can_redo(app: &App) -> bool {
    !app.fea.redo.is_empty()
}

/// Record an undo step with the state before an edit (edits closer together than a second share one step).
fn record(app: &mut App, before: Study) {
    let now = Instant::now();
    if app.fea.last_ck.map(|t| now.duration_since(t).as_secs_f32() > 1.0).unwrap_or(true) {
        app.fea.undo.push(before);
        if app.fea.undo.len() > 200 {
            app.fea.undo.remove(0);
        }
        app.fea.redo.clear();
    }
    app.fea.last_ck = Some(now);
    app.fea.scene_dirty = true;
}

fn edit(app: &mut App, f: impl FnOnce(&mut Study)) {
    let Some(c) = study(app).cloned() else { return };
    app.fea.last_ck = None;
    if let Some(m) = study_mut(app) {
        f(m);
    }
    record(app, c);
}

fn study_input(app: &App, s: &Study) -> Input {
    fea::input(app.studio_bodies(s.studio), app.studio_doc(s.studio), &s.bodies)
}

fn hash_input(input: &Input, h: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    input.tris.len().hash(h);
    let mut s = 0.0f64;
    for (k, t) in input.tris.iter().enumerate().step_by((input.tris.len() / 1024).max(1)) {
        s += (t[0].x + t[1].y * 3.0 + t[2].z * 7.0) * (k as f64 + 1.0);
    }
    s.to_bits().hash(h);
    input.part_material.hash(h);
}

fn study_key(s: &Study, input: &Input) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut c = s.clone();
    c.name.clear();
    serde_json::to_string(&c).unwrap_or_default().hash(&mut h);
    hash_input(input, &mut h);
    h.finish()
}

fn mesh_key(input: &Input, size: f64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash_input(input, &mut h);
    size.to_bits().hash(&mut h);
    h.finish()
}

/// Keep the results up to date: solve in the background once the study or the parts stop changing.
fn refresh(app: &mut App, ctx: &egui::Context) {
    if let Some((k, rx, _)) = &app.fea.job {
        if let Ok((key, res)) = rx.try_recv() {
            if key == *k {
                app.fea.key = key;
                match res {
                    Ok(o) => {
                        // a time history opens at its largest displacement
                        if let Solution::Transient(t) = &o.solution {
                            app.fea.frame = t.frames.iter().enumerate().max_by(|a, b| fea::max_mag(&a.1.1).total_cmp(&fea::max_mag(&b.1.1))).map(|x| x.0).unwrap_or(0);
                        }
                        app.fea.outcome = Some(Arc::new(o));
                        app.fea.stress_hi = 0.0;
                        if app.fea.sel.is_none() {
                            app.fea.show_results = true;
                        }
                        app.fea.error = None;
                    }
                    Err(e) => {
                        app.fea.outcome = None;
                        app.fea.error = Some(e);
                    }
                }
                app.fea.stress_cache = None;
                app.fea.frame = app.fea.frame.min(frames(app).saturating_sub(1));
                app.fea.mode = app.fea.mode.min(modes(app).saturating_sub(1));
                app.fea.scene_dirty = true;
            }
            app.fea.job = None;
            app.fea.computing = false;
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
    let Some(s) = study(app).cloned() else { return };
    let input = study_input(app, &s);
    let key = study_key(&s, &input);
    if key == app.fea.key || app.fea.job.as_ref().map(|j| j.0 == key).unwrap_or(false) {
        app.fea.pending = None;
        return;
    }
    // wait for edits to settle
    match app.fea.pending {
        Some((k, t)) if k == key => {
            if t.elapsed().as_secs_f32() < 0.4 {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
        }
        _ => {
            app.fea.pending = Some((key, Instant::now()));
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
    }
    app.fea.pending = None;
    if let Some((_, _, cancel)) = app.fea.job.take() {
        cancel.store(true, Ordering::Relaxed);
    }
    if input.tris.is_empty() {
        app.fea.key = key;
        app.fea.outcome = None;
        app.fea.error = Some("This Part Studio has no parts".into());
        app.fea.scene_dirty = true;
        return;
    }
    let size = s.mesh_size.unwrap_or_else(|| input.auto_size());
    let mk = mesh_key(&input, size);
    let cached = app.fea.mesh_cache.as_ref().filter(|m| m.0 == mk).map(|m| m.1.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    app.fea.job = Some((key, rx, cancel.clone()));
    app.fea.computing = true;
    let progress = app.fea.progress.clone();
    *progress.lock().unwrap() = ("Starting".into(), 0.0);
    let ctx = ctx.clone();
    let (mtx, mrx) = std::sync::mpsc::channel::<Arc<Mesh>>();
    std::thread::spawn(move || {
        let stop = || cancel.load(Ordering::Relaxed);
        let report = |what: &str, f: f32| {
            *progress.lock().unwrap() = (what.to_string(), f);
            ctx.request_repaint();
        };
        let mesh = match cached {
            Some(m) => Some(m),
            None => {
                report("Meshing", 0.0);
                let surf = crate::fea_mesh::Surface::new(input.tris.clone(), input.face.clone(), input.body.clone());
                match crate::fea_mesh::mesh(&surf, size, &stop) {
                    Ok(m) => {
                        let m = Arc::new(m);
                        let _ = mtx.send(m.clone());
                        Some(m)
                    }
                    Err(e) => {
                        let _ = tx.send((key, Err(e)));
                        ctx.request_repaint();
                        return;
                    }
                }
            }
        };
        let res = fea::run(&s, &input, mesh, &report, &stop);
        if !stop() {
            let _ = tx.send((key, res));
        }
        ctx.request_repaint();
    });
    // the new mesh, once made, is kept for the next solve
    app.fea.mesh_rx = Some((mk, mrx));
}

fn take_mesh(app: &mut App) {
    if let Some((mk, rx)) = &app.fea.mesh_rx {
        if let Ok(m) = rx.try_recv() {
            app.fea.mesh_cache = Some((*mk, m));
            app.fea.mesh_rx = None;
        }
    }
}

fn frames(app: &App) -> usize {
    match app.fea.outcome.as_deref().map(|o| &o.solution) {
        Some(Solution::Transient(t)) => t.frames.len(),
        _ => 0,
    }
}

fn modes(app: &App) -> usize {
    match app.fea.outcome.as_deref().map(|o| &o.solution) {
        Some(Solution::Modal { modes }) => modes.len(),
        _ => 0,
    }
}

// ---------------------------------------------------------------- toolbar

pub fn toolbar(app: &mut App, ui: &mut Ui) {
    App::group(ui, |ui| {
        for (k, tip) in [(SupportKind::Fixed, "Hold faces so they cannot move"), (SupportKind::Roller, "Let faces slide but not move off their surface (also for symmetry planes)")]
        {
            if app.tool_button(ui, k.icon(), k.label(), tip, k == SupportKind::Fixed, false, true).clicked() {
                add_support(app, k);
            }
        }
    });
    App::group(ui, |ui| {
        for (k, tip) in
            [(LoadKind::Force, "A force spread over faces"), (LoadKind::Pressure, "A pressure pushing on faces"), (LoadKind::Gravity, "The part's own weight, or any acceleration")]
        {
            if app.tool_button(ui, k.icon(), k.label(), tip, k == LoadKind::Force, false, true).clicked() {
                add_load(app, k);
            }
        }
    });
    App::group(ui, |ui| {
        let has = app.fea.outcome.is_some();
        if app.tool_button(ui, "feaResults", "Results", "Show the results on the deformed part", true, app.fea.show_results && has, has).clicked() {
            app.fea.show_results = !app.fea.show_results;
            if app.fea.show_results {
                app.fea.sel = None;
            }
            app.fea.scene_dirty = true;
        }
        if app.tool_button(ui, "feaMesh", "Mesh", "Show the element mesh", false, app.fea.show_mesh, true).clicked() {
            app.fea.show_mesh = !app.fea.show_mesh;
            app.fea.scene_dirty = true;
        }
        let can_play = has && app.fea.outcome.as_ref().map(|o| !matches!(o.solution, Solution::Static { .. })).unwrap_or(false);
        let icon = if app.fea.playing { "pause" } else { "camSim" };
        if app.tool_button(ui, icon, "Animate", "Play the mode shape or the time history (Space)", false, app.fea.playing, can_play).clicked() {
            toggle_play(app);
        }
    });
}

pub(crate) fn toggle_play(app: &mut App) {
    app.fea.playing = !app.fea.playing;
    app.fea.show_results = true;
    app.fea.last_tick = None;
    if app.fea.playing && app.fea.frame + 1 >= frames(app) {
        app.fea.frame = 0;
        app.fea.phase = 0.0;
    }
    app.fea.scene_dirty = true;
}

pub(crate) fn add_support(app: &mut App, k: SupportKind) {
    let mut id = 0;
    edit(app, |s| id = s.add_support(k));
    app.fea.sel = Some(id);
    app.fea.show_results = false;
}

pub(crate) fn add_load(app: &mut App, k: LoadKind) {
    let mut id = 0;
    edit(app, |s| id = s.add_load(k));
    app.fea.sel = Some(id);
    app.fea.show_results = false;
}

pub fn shortcuts(app: &mut App, ctx: &egui::Context) {
    use egui::Key;
    let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    if pressed(Key::Space) {
        toggle_play(app);
    }
    if pressed(Key::Escape) {
        app.fea.sel = None;
        app.fea.show_results = true;
        app.fea.scene_dirty = true;
    }
    if pressed(Key::Delete) || pressed(Key::Backspace) {
        if let Some(id) = app.fea.sel.take() {
            edit(app, |s| {
                s.supports.retain(|x| x.id != id);
                s.loads.retain(|x| x.id != id);
            });
        }
    }
    if pressed(Key::F) {
        fit(app);
    }
}

pub(crate) fn fit(app: &mut App) {
    let Some(s) = study(app) else { return };
    let input = study_input(app, s);
    if input.tris.is_empty() {
        return;
    }
    let (lo, hi) = input.bounds();
    app.camera.fit(lo.as_vec3(), hi.as_vec3());
    app.fea.scene_dirty = true;
}

// ---------------------------------------------------------------- sidebar

pub fn sidebar(app: &mut App, ui: &mut Ui) {
    let Some(before) = study(app).cloned() else {
        ui.label("No Simulation tab");
        return;
    };
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        section(ui, "Study", |ui| study_ui(app, ui));
        section(ui, "Supports", |ui| items_ui(app, ui, true));
        section(ui, "Loads", |ui| items_ui(app, ui, false));
        if let Some(id) = app.fea.sel {
            let s = study(app);
            if s.map(|s| s.supports.iter().any(|x| x.id == id)).unwrap_or(false) {
                section(ui, "Support", |ui| support_ui(app, ui, id));
            } else if s.map(|s| s.loads.iter().any(|x| x.id == id)).unwrap_or(false) {
                section(ui, "Load", |ui| load_ui(app, ui, id));
            }
        }
        match study(app).map(|s| s.analysis) {
            Some(Analysis::Modal) => section(ui, "Modes", |ui| modal_settings(app, ui)),
            Some(Analysis::Transient) => section(ui, "Time history", |ui| transient_settings(app, ui)),
            _ => {}
        }
        section(ui, "Mesh", |ui| mesh_ui(app, ui));
        section(ui, "Results", |ui| results_ui(app, ui));
    });
    if study(app).map(|c| *c != before).unwrap_or(false) {
        record(app, before);
    }
}

fn study_ui(app: &mut App, ui: &mut Ui) {
    let studios: Vec<String> = app.ws.studios.iter().map(|s| s.name.clone()).collect();
    let Some(si) = index(app) else { return };
    let studio = app.ws.sims[si].studio.min(studios.len().saturating_sub(1));
    let bodies: Vec<(BodyId, String)> = app.studio_bodies(studio).iter().filter(|b| !b.surface).map(|b| (b.id, b.name.clone())).collect();
    let part_material = study_input(app, &app.ws.sims[si]).part_material.filter(|m| fea::elastic(m).is_some());
    let mut need_build = None;
    let s = &mut app.ws.sims[si];
    row(ui, "Part Studio", |ui| {
        egui::ComboBox::from_id_salt("fea-studio").selected_text(studios.get(s.studio).cloned().unwrap_or_default()).show_ui(ui, |ui| {
            for (i, n) in studios.iter().enumerate() {
                if ui.selectable_value(&mut s.studio, i, n).changed() {
                    s.bodies.clear();
                    s.supports.iter_mut().for_each(|x| x.faces.clear());
                    s.loads.iter_mut().for_each(|x| x.faces.clear());
                    need_build = Some(i);
                }
            }
        });
    });
    if bodies.len() > 1 {
        row(ui, "Parts", |ui| {
            ui.vertical(|ui| {
                for (id, name) in &bodies {
                    let mut on = s.bodies.is_empty() || s.bodies.contains(id);
                    if ui.checkbox(&mut on, name).on_hover_text("Parts that touch are bonded together").changed() {
                        if s.bodies.is_empty() {
                            s.bodies = bodies.iter().map(|b| b.0).collect();
                        }
                        if on {
                            s.bodies.push(*id);
                        } else {
                            s.bodies.retain(|b| b != id);
                        }
                        if s.bodies.len() == bodies.len() {
                            s.bodies.clear();
                        }
                    }
                }
            });
        });
    }
    row(ui, "Analysis", |ui| {
        egui::ComboBox::from_id_salt("fea-analysis").width(160.0).selected_text(s.analysis.label()).show_ui(ui, |ui| {
            for a in Analysis::ALL {
                let tip = match a {
                    Analysis::Static => "Deflection and stress under steady loads",
                    Analysis::Modal => "Natural frequencies and how the part vibrates at each",
                    Analysis::Transient => "How the part moves over time when loads change suddenly, strike or shake it",
                };
                ui.selectable_value(&mut s.analysis, a, a.label()).on_hover_text(tip);
            }
        });
    });
    row(ui, "Material", |ui| {
        let from_part = format!("From part ({})", part_material.clone().unwrap_or_else(|| "Steel".into()));
        let text = if s.material.is_empty() { from_part.clone() } else { s.material.clone() };
        egui::ComboBox::from_id_salt("fea-material").width(160.0).selected_text(text).show_ui(ui, |ui| {
            ui.selectable_value(&mut s.material, String::new(), from_part).on_hover_text("The material set on the part in its Part Studio (steel when none is set)");
            for m in fea::MATERIALS {
                ui.selectable_value(&mut s.material, m.0.to_string(), m.0).on_hover_text(format!("E {} GPa · ν {} · {} g/cm³ · yield {} MPa", m.1 / 1000.0, m.2, m.3, m.4));
            }
        });
    });
    if let Some(i) = need_build {
        crate::cam_ui::ensure_built(app, i);
    }
    if bodies.is_empty() {
        ui.label(RichText::new("This Part Studio has no parts yet").color(theme::error_soft()));
    }
}

fn items_ui(app: &mut App, ui: &mut Ui, supports: bool) {
    let Some(s) = study(app) else { return };
    let rows: Vec<(u64, &'static str, String, bool, String)> = if supports {
        s.supports.iter().map(|x| (x.id, x.kind.icon(), x.name.clone(), x.suppressed, faces_text(x.faces.len()))).collect()
    } else {
        s.loads
            .iter()
            .map(|x| {
                let v = format!("{}{}", trim(x.value), x.kind.unit());
                let f = if x.kind == LoadKind::Gravity { v } else { format!("{v} · {}", faces_text(x.faces.len())) };
                (x.id, x.kind.icon(), x.name.clone(), x.suppressed, f)
            })
            .collect()
    };
    if rows.is_empty() {
        let text =
            if supports { "Add Fixed or Roller supports from the toolbar, then click the faces that are held." } else { "Add a Force, Pressure or Gravity load from the toolbar." };
        ui.label(RichText::new(text).color(theme::muted()).size(12.0));
    }
    let mut action: Option<(&str, u64)> = None;
    for (id, icon, name, sup, info) in rows {
        let sel = app.fea.sel == Some(id);
        ui.horizontal(|ui| {
            ui.add(icons::image(icon, 15.0).tint(if sup { theme::muted() } else { theme::text() }));
            let mut text = RichText::new(&name).size(12.5);
            if sup {
                text = text.color(theme::muted()).strikethrough();
            }
            let r = ui.add(egui::Button::selectable(sel, text).truncate());
            if r.clicked() {
                app.fea.sel = if sel { None } else { Some(id) };
                if app.fea.sel.is_some() {
                    app.fea.show_results = false;
                }
                app.fea.scene_dirty = true;
            }
            r.context_menu(|ui| {
                if ui.button(if sup { "Unsuppress" } else { "Suppress" }).clicked() {
                    action = Some(("suppress", id));
                    ui.close();
                }
                if ui.button(RichText::new("Delete").color(theme::error_soft())).clicked() {
                    action = Some(("delete", id));
                    ui.close();
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(info).color(theme::muted()).size(11.5));
            });
        });
    }
    if let Some((a, id)) = action {
        edit(app, |s| match a {
            "suppress" => {
                if let Some(x) = s.support_mut(id) {
                    x.suppressed = !x.suppressed;
                }
                if let Some(x) = s.load_mut(id) {
                    x.suppressed = !x.suppressed;
                }
            }
            _ => {
                s.supports.retain(|x| x.id != id);
                s.loads.retain(|x| x.id != id);
            }
        });
        if a == "delete" && app.fea.sel == Some(id) {
            app.fea.sel = None;
        }
    }
}

fn faces_text(n: usize) -> String {
    match n {
        0 => "no faces".into(),
        1 => "1 face".into(),
        n => format!("{n} faces"),
    }
}

fn trim(v: f64) -> String {
    crate::expr::trim(v, 4)
}

fn faces_row(app: &mut App, ui: &mut Ui, id: u64) {
    let n = study(app).and_then(|s| s.faces(id)).map(|f| f.len()).unwrap_or(0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(faces_text(n)).strong().size(12.5));
        if n > 0 && ui.small_button("Clear").clicked() {
            edit(app, |s| {
                if let Some(f) = s.faces_mut(id) {
                    f.clear();
                }
            });
        }
    });
    ui.label(RichText::new("Click faces in the viewport to add or remove them.").color(theme::muted()).size(12.0));
}

fn support_ui(app: &mut App, ui: &mut Ui, id: u64) {
    let Some(si) = index(app) else { return };
    let Some(sp) = app.ws.sims[si].support_mut(id) else { return };
    row(ui, "Name", |ui| {
        ui.text_edit_singleline(&mut sp.name);
    });
    row(ui, "Type", |ui| {
        for k in [SupportKind::Fixed, SupportKind::Roller] {
            ui.radio_value(&mut sp.kind, k, k.label());
        }
    });
    faces_row(app, ui, id);
}

const DIRS: [(&str, [f64; 3]); 6] =
    [("−Z (down)", [0.0, 0.0, -1.0]), ("+Z", [0.0, 0.0, 1.0]), ("−X", [-1.0, 0.0, 0.0]), ("+X", [1.0, 0.0, 0.0]), ("−Y", [0.0, -1.0, 0.0]), ("+Y", [0.0, 1.0, 0.0])];

fn load_ui(app: &mut App, ui: &mut Ui, id: u64) {
    let Some(si) = index(app) else { return };
    let Some(l) = app.ws.sims[si].load_mut(id) else { return };
    let kind = l.kind;
    row(ui, "Name", |ui| {
        ui.text_edit_singleline(&mut l.name);
    });
    let (label, speed) = match kind {
        LoadKind::Force => ("Force", 1.0),
        LoadKind::Pressure => ("Pressure", 0.01),
        LoadKind::Gravity => ("Acceleration", 0.05),
    };
    row(ui, label, |ui| {
        ui.add(egui::DragValue::new(&mut l.value).speed(speed).suffix(kind.unit()).max_decimals(4));
    });
    if kind == LoadKind::Pressure {
        ui.label(RichText::new("1 MPa = 10 bar = 145 psi").color(theme::muted()).size(11.5));
    }
    if kind != LoadKind::Pressure {
        row(ui, "Direction", |ui| {
            let text = if l.normal {
                "Into the faces".to_string()
            } else {
                DIRS.iter().find(|d| d.1 == l.direction).map(|d| d.0.to_string()).unwrap_or_else(|| format!("{:?}", l.direction.map(|x| (x * 1000.0).round() / 1000.0)))
            };
            egui::ComboBox::from_id_salt("fea-dir").width(140.0).selected_text(text).show_ui(ui, |ui| {
                if kind == LoadKind::Force && ui.selectable_label(l.normal, "Into the faces").clicked() {
                    l.normal = true;
                }
                for (n, d) in DIRS {
                    if ui.selectable_label(!l.normal && l.direction == d, n).clicked() {
                        l.normal = false;
                        l.direction = d;
                    }
                }
            });
        });
    }
    if kind != LoadKind::Gravity {
        faces_row(app, ui, id);
    } else {
        ui.label(RichText::new("Acts on the whole part: 1 g is its own weight.").color(theme::muted()).size(12.0));
    }
}

fn modal_settings(app: &mut App, ui: &mut Ui) {
    let Some(s) = study_mut(app) else { return };
    row(ui, "Modes", |ui| {
        ui.add(egui::DragValue::new(&mut s.modes).range(1..=50).speed(0.1));
    });
    if s.supports.iter().all(|x| x.suppressed || x.faces.is_empty()) {
        ui.label(RichText::new("No supports: the part vibrates freely, and the first six modes (about 0 Hz) are it moving as a whole.").color(theme::muted()).size(12.0));
    }
}

fn transient_settings(app: &mut App, ui: &mut Ui) {
    let auto = match app.fea.outcome.as_deref().map(|o| &o.solution) {
        Some(Solution::Transient(t)) => Some(t.duration),
        _ => None,
    };
    let Some(s) = study_mut(app) else { return };
    let t = &mut s.transient;
    row(ui, "Load over time", |ui| {
        egui::ComboBox::from_id_salt("fea-profile").width(150.0).selected_text(t.profile.label()).show_ui(ui, |ui| {
            for p in ProfileKind::ALL {
                ui.selectable_value(&mut t.profile, p, p.label());
            }
        });
    });
    match t.profile {
        ProfileKind::Ramp | ProfileKind::Pulse => {
            let mut ms = t.time * 1000.0;
            row(ui, if t.profile == ProfileKind::Ramp { "Rise time" } else { "Pulse length" }, |ui| {
                if ui.add(egui::DragValue::new(&mut ms).range(0.001..=100000.0).speed(0.01).suffix(" ms").max_decimals(4)).changed() {
                    t.time = ms / 1000.0;
                }
            });
        }
        ProfileKind::Sine => {
            row(ui, "Frequency", |ui| {
                ui.add(egui::DragValue::new(&mut t.frequency).range(0.01..=1e6).speed(1.0).suffix(" Hz").max_decimals(2));
            });
        }
        ProfileKind::Step => {}
    }
    row(ui, "Duration", |ui| {
        let mut on = t.duration.is_some();
        if ui.checkbox(&mut on, "").on_hover_text("Set it yourself (unticked: five periods of the lowest natural frequency)").changed() {
            t.duration = if on { Some(auto.unwrap_or(0.01)) } else { None };
        }
        match &mut t.duration {
            Some(d) => {
                let mut ms = *d * 1000.0;
                if ui.add(egui::DragValue::new(&mut ms).range(0.001..=1e6).speed(0.1).suffix(" ms").max_decimals(3)).changed() {
                    *d = ms / 1000.0;
                }
            }
            None => {
                ui.label(
                    RichText::new(match auto {
                        Some(a) => format!("auto ({})", fea::fmt_time(a)),
                        None => "auto".into(),
                    })
                    .color(theme::muted())
                    .size(12.0),
                );
            }
        }
    });
    row(ui, "Time steps", |ui| {
        ui.add(egui::DragValue::new(&mut t.steps).range(10..=20000).speed(5.0));
    });
    let mut pct = t.damping * 100.0;
    row(ui, "Damping", |ui| {
        if ui
            .add(egui::DragValue::new(&mut pct).range(0.0..=100.0).speed(0.1).suffix(" %").max_decimals(2))
            .on_hover_text("Fraction of critical damping (metals about 1–2 %, plastics 3–5 %)")
            .changed()
        {
            t.damping = pct / 100.0;
        }
    });
}

fn mesh_ui(app: &mut App, ui: &mut Ui) {
    let Some(s) = study(app).cloned() else { return };
    let auto = study_input(app, &s).auto_size();
    let mut size = s.mesh_size;
    row(ui, "Element size", |ui| {
        let mut on = size.is_some();
        if ui.checkbox(&mut on, "").on_hover_text("Set it yourself (unticked: worked out from the part)").changed() {
            size = if on { Some(auto) } else { None };
        }
        match &mut size {
            Some(x) => {
                ui.add(egui::DragValue::new(x).range(0.01..=10000.0).speed(0.05).suffix(" mm").max_decimals(3));
            }
            None => {
                ui.label(RichText::new(format!("auto ({} mm)", trim(auto))).color(theme::muted()).size(12.0));
            }
        }
    });
    if size != s.mesh_size {
        if let Some(m) = study_mut(app) {
            m.mesh_size = size;
        }
    }
    if let Some(o) = &app.fea.outcome {
        let m = &o.mesh;
        ui.label(RichText::new(format!("{} nodes · {} elements (10-node)", m.nodes.len(), m.tets.len())).color(theme::muted()).size(12.0));
        if m.min_dihedral < 8.0 {
            ui.label(RichText::new(format!("Some elements are flat (smallest angle {:.0}°); try another element size", m.min_dihedral)).color(theme::error_soft()).size(12.0));
        }
    }
    ui.label(RichText::new("Smaller elements are more accurate but slower. Thin walls want at least two elements across.").color(theme::muted()).size(11.5));
}

fn results_ui(app: &mut App, ui: &mut Ui) {
    if app.fea.computing {
        let (what, f) = app.fea.progress.lock().unwrap().clone();
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(format!("{what}…")).size(12.5));
        });
        if f > 0.0 {
            ui.add(egui::ProgressBar::new(f).desired_height(6.0));
        }
    }
    if let Some(e) = app.fea.error.as_ref().filter(|_| !app.fea.computing) {
        ui.label(RichText::new(e).color(theme::error_soft()).size(12.5));
        return;
    }
    let Some(o) = app.fea.outcome.clone() else {
        if !app.fea.computing {
            ui.label(RichText::new("Results appear here once the part is held and loaded.").color(theme::muted()).size(12.0));
        }
        return;
    };
    for w in &o.warnings {
        ui.horizontal_wrapped(|ui| {
            ui.add(icons::image("warning", 14.0).tint(theme::error_soft()));
            ui.label(RichText::new(w).color(theme::error_soft()).size(12.0));
        });
    }
    let modal = matches!(o.solution, Solution::Modal { .. });
    if !modal {
        row(ui, "Show", |ui| {
            egui::ComboBox::from_id_salt("fea-field").width(150.0).selected_text(app.fea.field.label()).show_ui(ui, |ui| {
                for f in Field::ALL {
                    if ui.selectable_value(&mut app.fea.field, f, f.label()).changed() {
                        app.fea.scene_dirty = true;
                    }
                }
            });
        });
    }
    row(ui, "Deformation", |ui| {
        if ui.add(egui::Slider::new(&mut app.fea.deform, 0.0..=3.0).show_value(false)).changed() {
            app.fea.scene_dirty = true;
        }
        if !modal && ui.checkbox(&mut app.fea.true_scale, "true scale").on_hover_text("Show the real deformation instead of exaggerating it").changed() {
            app.fea.scene_dirty = true;
        }
    });
    ui.add_space(4.0);
    let stat = |ui: &mut Ui, k: &str, v: String| {
        row(ui, k, |ui| {
            ui.label(RichText::new(v).strong().size(12.5));
        });
    };
    stat(ui, "Material", o.material.clone());
    stat(ui, "Mass", fmt_mass(o.mass));
    match &o.solution {
        Solution::Static { u, stress, reactions } => {
            let vm = stress.iter().map(crate::fea_solve::von_mises).fold(0.0f32, f32::max) as f64;
            stat(ui, "Max stress", format!("{} MPa", sig(vm)));
            stat(ui, "Max deflection", format!("{} mm", sig(fea::max_mag(u))));
            let sf = o.mat.yield_mpa / vm.max(1e-12);
            row(ui, "Factor of safety", |ui| {
                let col = if sf < 1.0 {
                    theme::error()
                } else if sf < 2.0 {
                    Color32::from_rgb(0xe0, 0x9a, 0x20)
                } else {
                    theme::ok_green()
                };
                ui.label(RichText::new(if sf > 1000.0 { "> 1000".into() } else { sig(sf) }).strong().size(12.5).color(col))
                    .on_hover_text(format!("Yield strength {} MPa over the largest von Mises stress", o.mat.yield_mpa));
            });
            if let Some(s) = study(app) {
                for (id, r) in reactions {
                    if let Some(sp) = s.supports.iter().find(|x| x.id == *id) {
                        stat(ui, &format!("{} reaction", sp.name), format!("{} N", sig(r.length())));
                    }
                }
            }
        }
        Solution::Modal { modes } => {
            ui.label(RichText::new("Click a mode to see its shape.").color(theme::muted()).size(12.0));
            egui::Grid::new("fea-modes").num_columns(3).spacing([12.0, 2.0]).show(ui, |ui| {
                ui.label(RichText::new("Mode").color(theme::muted()).size(11.5));
                ui.label(RichText::new("Frequency").color(theme::muted()).size(11.5));
                ui.label(RichText::new("Mass X / Y / Z").color(theme::muted()).size(11.5))
                    .on_hover_text("Effective mass along each axis, as a share of the whole: modes with a large share are the ones shaking excites");
                ui.end_row();
                for (i, m) in modes.iter().enumerate() {
                    if ui.add(egui::Button::selectable(app.fea.mode == i, RichText::new(format!("{}", i + 1)).size(12.5))).clicked() {
                        app.fea.mode = i;
                        app.fea.show_results = true;
                        app.fea.stress_cache = None;
                        app.fea.scene_dirty = true;
                    }
                    ui.label(RichText::new(format!("{} Hz", sig(m.freq))).size(12.5));
                    let p = m.participation.map(|x| format!("{:.0}", x * 100.0));
                    ui.label(RichText::new(format!("{} / {} / {} %", p[0], p[1], p[2])).color(theme::muted()).size(12.0));
                    ui.end_row();
                }
            });
        }
        Solution::Transient(t) => {
            let peak = t.max_disp.iter().copied().fold(0.0, f64::max);
            let at = t.max_disp.iter().position(|x| *x == peak).map(|i| t.times[i]).unwrap_or(0.0);
            stat(ui, "Peak deflection", format!("{} mm at {}", sig(peak), fea::fmt_time(at)));
            stat(ui, "First mode", format!("{} Hz", sig(t.f1)));
            stat(ui, "Time step", fea::fmt_time(t.dt));
            let fi = app.fea.frame.min(t.frames.len().saturating_sub(1));
            if let Some((tf, _)) = t.frames.get(fi) {
                stat(ui, "Showing", fea::fmt_time(*tf));
            }
            history_plot(app, ui, t);
        }
    }
}

/// Largest displacement over time, with the frame being shown.
fn history_plot(app: &mut App, ui: &mut Ui, t: &crate::fea_solve::TransientResult) {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 90.0), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, theme::panel_2());
    let tmax = t.times.last().copied().unwrap_or(1.0).max(1e-12);
    let dmax = t.max_disp.iter().copied().fold(0.0, f64::max).max(1e-12);
    let inner = rect.shrink2(egui::vec2(6.0, 8.0));
    let to = |tt: f64, d: f64| Pos2::new(inner.left() + (tt / tmax) as f32 * inner.width(), inner.bottom() - (d / dmax) as f32 * inner.height());
    let pts: Vec<Pos2> = t.times.iter().zip(&t.max_disp).map(|(a, b)| to(*a, *b)).collect();
    p.add(egui::Shape::line(pts, Stroke::new(1.5, theme::accent())));
    if let Some((tf, _)) = t.frames.get(app.fea.frame) {
        let x = to(*tf, 0.0).x;
        p.vline(x, inner.y_range(), Stroke::new(1.0, theme::text()));
    }
    p.text(rect.left_top() + egui::vec2(6.0, 2.0), egui::Align2::LEFT_TOP, format!("max displacement · peak {} mm", sig(dmax)), egui::FontId::proportional(10.5), theme::muted());
    if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.clicked() || resp.dragged()) {
        let tt = ((pos.x - inner.left()) / inner.width()).clamp(0.0, 1.0) as f64 * tmax;
        if let Some((i, _)) = t.frames.iter().enumerate().min_by(|a, b| (a.1.0 - tt).abs().total_cmp(&(b.1.0 - tt).abs())) {
            app.fea.frame = i;
            app.fea.playing = false;
            app.fea.show_results = true;
            app.fea.scene_dirty = true;
        }
    }
}

/// Three significant figures.
fn sig(v: f64) -> String {
    if v.abs() < 1e-12 || !v.is_finite() {
        return "0".into();
    }
    let d = (3 - v.abs().log10().floor() as i32 - 1).max(0) as usize;
    format!("{:.*}", d.min(6), v)
}

fn fmt_mass(t: f64) -> String {
    let g = t * 1e6;
    if g >= 1000.0 { format!("{} kg", sig(g / 1000.0)) } else { format!("{} g", sig(g)) }
}

// ---------------------------------------------------------------- results on the mesh

/// Displacement of every node in the state being shown (mm; mode shapes scaled to 1 at most).
fn current_disp(app: &App, o: &Outcome) -> Option<Vec<DVec3>> {
    let u: Vec<f64> = match &o.solution {
        Solution::Static { u, .. } => u.clone(),
        Solution::Modal { modes } => {
            let m = modes.get(app.fea.mode)?;
            let peak = fea::max_mag(&m.shape).max(1e-300);
            let k = if app.fea.playing { app.fea.phase.sin() } else { 1.0 } / peak;
            m.shape.iter().map(|x| x * k).collect()
        }
        Solution::Transient(t) => t.frames.get(app.fea.frame.min(t.frames.len().saturating_sub(1)))?.1.clone(),
    };
    Some(u.chunks(3).map(|c| DVec3::new(c[0], c[1], c[2])).collect())
}

fn current_stress(app: &mut App, o: &Outcome) -> Option<Arc<Vec<[f32; 6]>>> {
    match &o.solution {
        Solution::Static { stress, .. } => Some(Arc::new(stress.clone())),
        Solution::Transient(t) => {
            let k = (1, app.fea.frame);
            if let Some((ck, s)) = &app.fea.stress_cache {
                if *ck == k {
                    return Some(s.clone());
                }
            }
            let u = &t.frames.get(app.fea.frame)?.1;
            let s = Arc::new(crate::fea_solve::nodal_stress(&o.mesh, &o.mat, u));
            app.fea.stress_cache = Some((k, s.clone()));
            Some(s)
        }
        Solution::Modal { .. } => None,
    }
}

/// Values at the nodes for the colour map, the range to colour over, its unit, and whether low values are bad.
fn field_values(app: &mut App, o: &Outcome, disp: &[DVec3]) -> (Vec<f32>, f32, f32, &'static str, bool) {
    let modal = matches!(o.solution, Solution::Modal { .. });
    let field = if modal { Field::Disp } else { app.fea.field };
    let vals: Vec<f32> = match field {
        Field::Disp => disp.iter().map(|d| d.length() as f32).collect(),
        Field::DispX => disp.iter().map(|d| d.x as f32).collect(),
        Field::DispY => disp.iter().map(|d| d.y as f32).collect(),
        Field::DispZ => disp.iter().map(|d| d.z as f32).collect(),
        Field::Stress | Field::Safety => {
            let s = current_stress(app, o).unwrap_or_default();
            let vm: Vec<f32> = s.iter().map(crate::fea_solve::von_mises).collect();
            if field == Field::Safety { vm.iter().map(|v| (o.mat.yield_mpa as f32 / v.max(1e-9)).min(10.0)).collect() } else { vm }
        }
    };
    let unit = match field {
        Field::Stress => " MPa",
        Field::Safety => "",
        _ if modal => " (relative)",
        _ => " mm",
    };
    // the range over the whole history for transient results, so colours compare between frames
    let (mut lo, mut hi) = vals.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), v| (a.min(*v), b.max(*v)));
    if let (Solution::Transient(t), Field::Disp) = (&o.solution, field) {
        lo = 0.0;
        hi = t.max_disp.iter().copied().fold(0.0, f64::max) as f32;
    }
    if let (Solution::Transient(_), Field::Stress) = (&o.solution, field) {
        app.fea.stress_hi = app.fea.stress_hi.max(hi);
        hi = app.fea.stress_hi;
    }
    if field == Field::Safety {
        lo = lo.min(10.0);
        hi = 10.0f32.min(hi.max(lo + 1e-6));
    }
    if matches!(field, Field::Stress | Field::Disp) {
        lo = lo.min(0.0).max(0.0);
    }
    if modal {
        (lo, hi) = (0.0, 1.0);
    }
    if !(hi > lo) {
        hi = lo + 1e-9;
    }
    (vals, lo, hi, unit, field == Field::Safety)
}

/// Blue → cyan → green → yellow → red.
pub fn colormap(t: f32) -> Color32 {
    let stops: [(f32, [f32; 3]); 5] = [(0.0, [0.10, 0.25, 0.95]), (0.25, [0.05, 0.75, 0.95]), (0.5, [0.15, 0.85, 0.25]), (0.75, [0.98, 0.85, 0.10]), (1.0, [0.92, 0.12, 0.10])];
    let t = t.clamp(0.0, 1.0);
    for w in stops.windows(2) {
        if t <= w[1].0 {
            let f = (t - w[0].0) / (w[1].0 - w[0].0);
            let c: [f32; 3] = std::array::from_fn(|k| w[0].1[k] + (w[1].1[k] - w[0].1[k]) * f);
            return Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8);
        }
    }
    Color32::from_rgb(235, 30, 25)
}

fn rgba(c: Color32, a: f32) -> [f32; 4] {
    crate::viewport::rgba(c, a)
}

/// Deformation scale for drawing: exaggerated to a twentieth of the part size unless true scale is on.
fn deform_scale(app: &App, o: &Outcome) -> f64 {
    let modal = matches!(o.solution, Solution::Modal { .. });
    if app.fea.true_scale && !modal {
        return app.fea.deform.min(1.0) as f64;
    }
    let (lo, hi) = o.mesh.nodes.iter().fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)), |(a, b), p| (a.min(*p), b.max(*p)));
    let size = (hi - lo).length();
    let peak = o.max_disp().max(1e-300);
    app.fea.deform as f64 * size * if modal { 0.08 } else { 0.05 } / peak
}

/// Sub-triangles of a 6-node face (corners 0-2, mid-edges 3: 01, 4: 12, 5: 20).
const SUB: [[usize; 3]; 4] = [[0, 3, 5], [3, 1, 4], [5, 4, 2], [3, 4, 5]];

/// Global face numbers of every triangle of the parts the study uses, in `studio_bodies` order.
fn body_faces(bodies: &[BodyMesh], doc: Option<&crate::doc::Document>, s: &Study) -> Vec<Option<(usize, u32)>> {
    // same numbering as fea::input
    let mut base = 0u32;
    let mut out = vec![];
    for (bi, b) in bodies.iter().enumerate() {
        let hidden = doc.and_then(|d| d.parts.get(&crate::doc::body_key(b.id))).map(|p| p.hidden).unwrap_or(false);
        if if s.bodies.is_empty() { hidden || b.surface } else { !s.bodies.contains(&b.id) } {
            out.push(None);
            continue;
        }
        out.push(Some((bi, base)));
        base += b.faces.len().max(1) as u32;
    }
    out
}

fn update_picks(app: &mut App) {
    let Some(s) = study(app).cloned() else { return };
    let input = study_input(app, &s);
    let key = study_key(&s, &input);
    if key == app.fea.picks_key {
        return;
    }
    app.fea.picks_key = key;
    let ids = s.supports.iter().map(|x| (x.id, &x.faces)).chain(s.loads.iter().map(|x| (x.id, &x.faces)));
    app.fea.picks = ids.map(|(id, f)| (id, f.iter().filter_map(|p| input.resolve(p)).collect())).collect();
}

fn build_scene(app: &mut App) -> SceneData {
    let mut sd = SceneData::default();
    let Some(s) = study(app).cloned() else { return sd };
    let results = app.fea.show_results && app.fea.sel.is_none();
    let outcome = app.fea.outcome.clone().filter(|_| results);
    // displacements, exaggeration and colour values of the result being shown
    let shown = outcome.as_ref().map(|o| {
        let disp = current_disp(app, o).unwrap_or_else(|| vec![DVec3::ZERO; o.mesh.nodes.len()]);
        let k = deform_scale(app, o);
        let f = field_values(app, o, &disp);
        (disp, k, f)
    });
    let doc = app.studio_doc(s.studio);
    let bodies = app.studio_bodies(s.studio);
    let globals = body_faces(bodies, doc, &s);
    let face_of_item = |id: u64| app.fea.picks.iter().find(|p| p.0 == id).map(|p| p.1.clone()).unwrap_or_default();
    let support_col = Color32::from_rgb(0x2a, 0xa1, 0x98);
    let roller_col = Color32::from_rgb(0x4a, 0x7c, 0xd8);
    let load_col = Color32::from_rgb(0xa8, 0x4c, 0xd8);
    let sel_col = vp::picked();
    // face colours in setup view
    let mut tint: std::collections::HashMap<u32, Color32> = std::collections::HashMap::new();
    for sp in s.supports.iter().filter(|x| !x.suppressed) {
        for f in face_of_item(sp.id) {
            tint.insert(f, if sp.kind == SupportKind::Fixed { support_col } else { roller_col });
        }
    }
    for l in s.loads.iter().filter(|x| !x.suppressed) {
        for f in face_of_item(l.id) {
            tint.insert(f, load_col);
        }
    }
    if let Some(id) = app.fea.sel {
        for f in face_of_item(id) {
            tint.insert(f, sel_col);
        }
    }
    if let Some(h) = app.fea.hover.filter(|_| app.fea.sel.is_some()) {
        tint.entry(h).and_modify(|c| *c = theme::lerp(*c, vp::hover(), 0.5)).or_insert(vp::hover());
    }
    let (lo, hi) = {
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for (b, g) in bodies.iter().zip(&globals) {
            if g.is_some() {
                lo = lo.min(Vec3::from(b.bbox[0].map(|x| x as f32)));
                hi = hi.max(Vec3::from(b.bbox[1].map(|x| x as f32)));
            }
        }
        (lo, hi)
    };
    let size = (hi - lo).length().max(1e-3);
    match (&outcome, shown) {
        (Some(o), Some((disp, k, (vals, vlo, vhi, _, rev)))) => {
            let at = |n: u32| (o.mesh.nodes[n as usize] + disp[n as usize] * k).as_vec3();
            let col = |n: u32| {
                let t = (vals[n as usize] - vlo) / (vhi - vlo);
                rgba(colormap(if rev { 1.0 - t } else { t }), 1.0)
            };
            for (f, _) in &o.mesh.faces {
                for st in SUB {
                    let ids = st.map(|i| f[i]);
                    let p = ids.map(at);
                    let n = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
                    for (v, id) in p.iter().zip(ids) {
                        sd.opaque.push(MeshVertex { pos: (*v).into(), normal: n.into(), color: col(id), finish: 0.0 });
                    }
                }
            }
            if app.fea.show_mesh {
                for (f, _) in &o.mesh.faces {
                    let ring = [f[0], f[3], f[1], f[4], f[2], f[5], f[0]].map(at);
                    sd.polyline(&ring, 0.8, rgba(Color32::from_rgb(0x20, 0x24, 0x2c), 0.55), false);
                }
            }
            // the undeformed part's edges, faintly
            if k * o.max_disp() > size as f64 * 1e-3 {
                for (b, g) in bodies.iter().zip(&globals) {
                    if g.is_some() {
                        for e in &b.edges {
                            let pts: Vec<Vec3> = e.pts.iter().map(|p| Vec3::from(*p)).collect();
                            sd.polyline(&pts, 1.0, rgba(theme::muted(), 0.45), false);
                        }
                    }
                }
            }
        }
        _ => {
            for (b, g) in bodies.iter().zip(&globals) {
                let Some((_, base)) = g else { continue };
                let pc = doc.and_then(|d| d.parts.get(&crate::doc::body_key(b.id))).and_then(|p| p.color).map(|c| Color32::from_rgb(c[0], c[1], c[2])).unwrap_or(vp::body());
                for (ti, t) in b.indices.chunks(3).enumerate() {
                    let gf = base + b.tri_face.get(ti).copied().unwrap_or(0);
                    let c = tint.get(&gf).copied().unwrap_or(pc);
                    let p = |i: u32| Vec3::new(b.positions[3 * i as usize], b.positions[3 * i as usize + 1], b.positions[3 * i as usize + 2]);
                    sd.tri(false, [p(t[0]), p(t[1]), p(t[2])], rgba(c, 1.0));
                }
                for e in &b.edges {
                    let pts: Vec<Vec3> = e.pts.iter().map(|p| Vec3::from(*p)).collect();
                    sd.polyline(&pts, 1.2, rgba(vp::edge(), 1.0), false);
                }
            }
            if app.fea.show_mesh {
                if let Some(m) = app.fea.outcome.as_ref().map(|o| o.mesh.clone()).or_else(|| app.fea.mesh_cache.as_ref().map(|m| m.1.clone())) {
                    for (f, _) in &m.faces {
                        let ring = [f[0], f[3], f[1], f[4], f[2], f[5], f[0]].map(|n| m.nodes[n as usize].as_vec3());
                        sd.polyline(&ring, 0.8, rgba(Color32::from_rgb(0x20, 0x24, 0x2c), 0.5), false);
                    }
                }
            }
            // load arrows
            let len = size * 0.07;
            for l in s.loads.iter().filter(|x| !x.suppressed) {
                let c = rgba(if app.fea.sel == Some(l.id) { sel_col } else { load_col }, 1.0);
                let dir = Vec3::from(l.direction.map(|x| x as f32)).normalize_or_zero();
                if l.kind == LoadKind::Gravity {
                    let p = (lo + hi) * 0.5;
                    arrow(&mut sd, p - dir * len * 1.5, p, c, len);
                    continue;
                }
                let into = l.normal || l.kind == LoadKind::Pressure;
                let faces = face_of_item(l.id);
                for (b, g) in bodies.iter().zip(&globals) {
                    let Some((_, base)) = g else { continue };
                    let tris: Vec<usize> = (0..b.indices.len() / 3).filter(|ti| faces.contains(&(base + b.tri_face.get(*ti).copied().unwrap_or(0)))).collect();
                    let step = (tris.len() / 9).max(1);
                    for &ti in tris.iter().step_by(step) {
                        let t = &b.indices[3 * ti..3 * ti + 3];
                        let p = |i: u32| Vec3::new(b.positions[3 * i as usize], b.positions[3 * i as usize + 1], b.positions[3 * i as usize + 2]);
                        let (a, bb, cc) = (p(t[0]), p(t[1]), p(t[2]));
                        let n = (bb - a).cross(cc - a).normalize_or_zero();
                        let at = (a + bb + cc) / 3.0;
                        let d = if into { -n } else { dir };
                        arrow(&mut sd, at - d * len, at, c, len);
                    }
                }
            }
        }
    }
    sd
}

fn arrow(sd: &mut SceneData, from: Vec3, to: Vec3, c: [f32; 4], len: f32) {
    let d = (to - from).normalize_or_zero();
    let side = if d.z.abs() < 0.9 { d.cross(Vec3::Z) } else { d.cross(Vec3::X) }.normalize_or_zero();
    let up = d.cross(side);
    sd.lines_top.push(crate::render::LineInst { a: from.into(), b: to.into(), width: 2.2, bias: 0.0, color: c });
    for s in [side, -side, up, -up] {
        sd.lines_top.push(crate::render::LineInst { a: to.into(), b: (to - d * len * 0.28 + s * len * 0.12).into(), width: 2.2, bias: 0.0, color: c });
    }
}

/// The face under the cursor: a pick for it and its global face number.
fn hit(app: &App, s: &Study, local: Vec2) -> Option<(FacePick, u32)> {
    let (o, d) = app.camera.ray(local);
    let (o, d) = (o.as_dvec3(), d.as_dvec3());
    let bodies = app.studio_bodies(s.studio);
    let globals = body_faces(bodies, app.studio_doc(s.studio), s);
    let mut best: Option<(f64, FacePick, u32)> = None;
    for (b, g) in bodies.iter().zip(&globals) {
        let Some((_, base)) = g else { continue };
        for (ti, t) in b.indices.chunks(3).enumerate() {
            let v = |i: u32| DVec3::new(b.positions[3 * i as usize] as f64, b.positions[3 * i as usize + 1] as f64, b.positions[3 * i as usize + 2] as f64);
            let (a, bb, c) = (v(t[0]), v(t[1]), v(t[2]));
            if let Some(tt) = ray_tri(o, d, a, bb, c) {
                if best.as_ref().map(|x| tt < x.0).unwrap_or(true) {
                    let n = (bb - a).cross(c - a).normalize_or_zero();
                    let fi = b.tri_face.get(ti).copied().unwrap_or(0);
                    let p = o + d * tt;
                    let r = |x: f64| (x * 1e4).round() / 1e4;
                    best = Some((tt, FacePick { point: [r(p.x), r(p.y), r(p.z)], normal: [r(n.x), r(n.y), r(n.z)] }, base + fi));
                }
            }
        }
    }
    best.map(|b| (b.1, b.2))
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
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some(t)
}

fn step_anim(app: &mut App, ctx: &egui::Context) {
    if !app.fea.playing {
        app.fea.last_tick = None;
        return;
    }
    let Some(o) = app.fea.outcome.clone() else {
        app.fea.playing = false;
        return;
    };
    let now = Instant::now();
    let dt = app.fea.last_tick.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
    app.fea.last_tick = Some(now);
    match &o.solution {
        Solution::Modal { .. } => {
            app.fea.phase = (app.fea.phase + dt * std::f64::consts::TAU * 0.8) % std::f64::consts::TAU;
        }
        Solution::Transient(t) => {
            // the whole history in about four seconds
            app.fea.phase += dt * t.frames.len() as f64 / 4.0;
            let adv = app.fea.phase.floor() as usize;
            app.fea.phase -= adv as f64;
            app.fea.frame += adv;
            if app.fea.frame + 1 >= t.frames.len() {
                app.fea.frame = t.frames.len().saturating_sub(1);
                app.fea.playing = false;
            }
        }
        Solution::Static { .. } => app.fea.playing = false,
    }
    app.fea.scene_dirty = true;
    ctx.request_repaint();
}

pub fn viewport(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    if let Some(s) = study(app) {
        let st = s.studio;
        crate::cam_ui::ensure_built(app, st);
    }
    take_mesh(app);
    refresh(app, &ctx);
    take_mesh(app);
    step_anim(app, &ctx);
    update_picks(app);
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    if app.viewport_rect != rect {
        app.viewport_rect = rect;
        app.fea.scene_dirty = true;
    }
    app.camera.size = Vec2::new(rect.width().max(1.0), rect.height().max(1.0));
    crate::viewport::paint_background(ui.painter(), rect, app.realistic);
    let before = (app.camera.target, app.camera.back, app.camera.up, app.camera.half_height);
    crate::viewport::navigate(app, ui, &resp, true);
    if before != (app.camera.target, app.camera.back, app.camera.up, app.camera.half_height) {
        app.fea.scene_dirty = true;
    }
    let local = resp.hover_pos().map(|p| Vec2::new(p.x - rect.left(), p.y - rect.top()));
    // picking faces for the selected support or load
    let picking =
        app.fea.sel.filter(|id| study(app).map(|s| s.supports.iter().any(|x| x.id == *id) || s.loads.iter().any(|x| x.id == *id && x.kind != LoadKind::Gravity)).unwrap_or(false));
    if let (Some(id), Some(s)) = (picking, study(app).cloned()) {
        let h = local.and_then(|l| hit(app, &s, l));
        let hf = h.as_ref().map(|x| x.1);
        if hf != app.fea.hover {
            app.fea.hover = hf;
            app.fea.scene_dirty = true;
        }
        if resp.clicked() {
            if let Some((pick, gf)) = h {
                let input = study_input(app, &s);
                edit(app, |st| {
                    if let Some(f) = st.faces_mut(id) {
                        if let Some(k) = f.iter().position(|p| input.resolve(p) == Some(gf)) {
                            f.remove(k);
                        } else {
                            f.push(pick);
                        }
                    }
                });
            } else {
                app.fea.sel = None;
                app.fea.show_results = true;
                app.fea.scene_dirty = true;
            }
        }
    } else if app.fea.hover.take().is_some() {
        app.fea.scene_dirty = true;
    }
    if !app.fea.fitted && study(app).map(|s| !app.studio_bodies(s.studio).is_empty()).unwrap_or(false) {
        app.fea.fitted = true;
        fit(app);
    }
    if app.fea.scene_dirty {
        app.fea.scene_dirty = false;
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
    overlay(app, ui, &painter, rect, local);
}

fn overlay(app: &mut App, ui: &mut Ui, painter: &egui::Painter, rect: egui::Rect, local: Option<Vec2>) {
    // status in the corner
    let mut line = String::new();
    if app.fea.computing {
        let (what, f) = app.fea.progress.lock().unwrap().clone();
        line = if f > 0.0 { format!("{what}… {:.0}%", f * 100.0) } else { format!("{what}…") };
    } else if let Some(e) = &app.fea.error {
        line = e.clone();
    } else if let Some(o) = &app.fea.outcome {
        line = match &o.solution {
            Solution::Static { u, stress, .. } => {
                let vm = stress.iter().map(crate::fea_solve::von_mises).fold(0.0f32, f32::max) as f64;
                format!("Max stress {} MPa · max displacement {} mm · factor of safety {}", sig(vm), sig(fea::max_mag(u)), sig((o.mat.yield_mpa / vm.max(1e-12)).min(1e4)))
            }
            Solution::Modal { modes } => modes.get(app.fea.mode).map(|m| format!("Mode {} · {} Hz", app.fea.mode + 1, sig(m.freq))).unwrap_or_default(),
            Solution::Transient(t) => t.frames.get(app.fea.frame).map(|f| format!("t = {}", fea::fmt_time(f.0))).unwrap_or_default(),
        };
    }
    if app.fea.sel.is_some() && !app.fea.computing {
        line = "Click faces to add or remove them · Esc when done".into();
    }
    if !line.is_empty() {
        let col = if app.fea.error.is_some() && !app.fea.computing && app.fea.sel.is_none() { theme::error_soft() } else { theme::text() };
        let w = painter.layout_no_wrap(line.clone(), egui::FontId::proportional(12.0), col).size().x;
        crate::viewport::label_box(painter, rect.left_top() + egui::vec2(16.0 + w / 2.0 + 6.0, 20.0), &line, col);
    }
    let results = app.fea.show_results && app.fea.sel.is_none();
    let Some(o) = app.fea.outcome.clone().filter(|_| results) else { return };
    let Some(disp) = current_disp(app, &o) else { return };
    let (vals, lo, hi, unit, rev) = field_values(app, &o, &disp);
    // legend
    let modal = matches!(o.solution, Solution::Modal { .. });
    let title = if modal {
        "Displacement (relative)".to_string()
    } else {
        format!("{}{}", app.fea.field.label(), if unit.trim().is_empty() { String::new() } else { format!(" ({})", unit.trim()) })
    };
    let bar = egui::Rect::from_min_size(rect.left_top() + egui::vec2(18.0, 58.0), egui::vec2(16.0, 180.0));
    let bg = bar.expand2(egui::vec2(8.0, 22.0)).with_max_x(bar.right() + 82.0).translate(egui::vec2(0.0, 8.0));
    painter.rect(bg, 6.0, theme::label_bg(), Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
    painter.text(bg.left_top() + egui::vec2(8.0, 4.0), egui::Align2::LEFT_TOP, title, egui::FontId::proportional(11.0), theme::text());
    let bar = bar.translate(egui::vec2(0.0, 14.0));
    let n = 24;
    for i in 0..n {
        let t0 = i as f32 / n as f32;
        let r =
            egui::Rect::from_min_max(Pos2::new(bar.left(), bar.bottom() - bar.height() * (t0 + 1.0 / n as f32)), Pos2::new(bar.right(), bar.bottom() - bar.height() * t0 + 0.5));
        let t = t0 + 0.5 / n as f32;
        painter.rect_filled(r, 0.0, colormap(if rev { 1.0 - t } else { t }));
    }
    for i in 0..=4 {
        let t = i as f32 / 4.0;
        let v = lo + (hi - lo) * t;
        let y = bar.bottom() - bar.height() * t;
        let text = if rev && i == 4 && hi >= 10.0 { "≥ 10".to_string() } else { sig(v as f64) };
        painter.text(Pos2::new(bar.right() + 6.0, y), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(11.0), theme::text());
    }
    // probe: the value at the surface node nearest the cursor
    if let Some(l) = local {
        let k = deform_scale(app, &o);
        let cam = &app.camera;
        let mut best: Option<(f32, u32)> = None;
        let eye = cam.back.as_dvec3();
        for (f, _) in &o.mesh.faces {
            let p = |i: usize| o.mesh.nodes[f[i] as usize] + disp[f[i] as usize] * k;
            if (p(1) - p(0)).cross(p(2) - p(0)).dot(eye) <= 0.0 {
                continue;
            }
            for &nd in f {
                let s = cam.project((o.mesh.nodes[nd as usize] + disp[nd as usize] * k).as_vec3());
                let d = s.distance(l);
                if d < 10.0 && best.map(|b| d < b.0).unwrap_or(true) {
                    best = Some((d, nd));
                }
            }
        }
        if let Some((_, nd)) = best {
            let s = cam.project((o.mesh.nodes[nd as usize] + disp[nd as usize] * k).as_vec3());
            let at = rect.left_top() + egui::vec2(s.x, s.y);
            painter.circle_filled(at, 3.5, theme::text());
            let text = format!("{}{}", sig(vals[nd as usize] as f64), unit);
            let w = painter.layout_no_wrap(text.clone(), egui::FontId::proportional(12.0), theme::text()).size().x;
            crate::viewport::label_box(painter, at + egui::vec2(w / 2.0 + 14.0, -14.0), &text, theme::text());
        }
    }
    // playback strip for transient results
    if let Solution::Transient(t) = &o.solution {
        let strip = egui::Rect::from_min_size(Pos2::new(rect.left() + 16.0, rect.bottom() - 88.0), egui::vec2((rect.width() - 32.0).min(560.0), 40.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(strip));
        egui::Frame::new().fill(theme::window()).stroke(Stroke::new(1.0, theme::border())).corner_radius(6).inner_margin(Margin::symmetric(8, 4)).show(&mut child, |ui| {
            ui.horizontal(|ui| {
                let icon = if app.fea.playing { "pause" } else { "camSim" };
                if ui.add(egui::Button::image(icons::image(icon, 16.0).tint(theme::text()))).clicked() {
                    toggle_play(app);
                }
                let mut f = app.fea.frame as f64;
                ui.spacing_mut().slider_width = (strip.width() - 150.0).max(60.0);
                if ui.add(egui::Slider::new(&mut f, 0.0..=(t.frames.len().max(1) - 1) as f64).show_value(false)).changed() {
                    app.fea.frame = f as usize;
                    app.fea.playing = false;
                    app.fea.scene_dirty = true;
                }
                let tt = t.frames.get(app.fea.frame).map(|x| x.0).unwrap_or(0.0);
                ui.label(RichText::new(fea::fmt_time(tt)).size(12.0));
            });
        });
    }
}
