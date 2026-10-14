//! Assembly tab: insert parts, mate them, drag them around.

use std::sync::Arc;

use egui::{Color32, Margin, Pos2, RichText, Sense, Stroke, Ui};
use glam::{DMat3, DVec3, Vec2, Vec3};

use crate::app::{App, ToastKind};
use crate::assembly::{self, Assembly, Connector, Instance, Mate, MateKind, Pose};
use crate::drawing::Drawing;
use crate::icons;
use crate::render::{MeshVertex, SceneData, ViewportCallback};
use crate::theme::{self, vp, BORDER, MUTED, TEXT};
use crate::workspace::Tab;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub inst: u64,
    pub face: Option<usize>,
    pub edge: Option<usize>,
    /// Hit point in the instance's own coordinates.
    pub local: [f64; 3],
}

pub struct MateDialog {
    pub mate: u64,
    pub before: Assembly,
    pub active: usize,
}

pub struct Drag {
    pub inst: u64,
    pub start: Pose,
    pub rotate: bool,
    pub moved: Vec2,
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
    /// Exploded view distance factor (0 = assembled).
    pub explode: f32,
}

fn asm_index(app: &App) -> Option<usize> {
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
    }
}

pub fn redo(app: &mut App) {
    if let Some(s) = app.asm.redo.pop() {
        let cur = (std::mem::replace(&mut app.ws.assemblies, s.0), std::mem::replace(&mut app.ws.drawings, s.1));
        app.asm.undo.push(cur);
        app.asm.scene_dirty = true;
        app.dwg.dirty = true;
    }
}

/// The pose an instance is drawn and picked at (moved outwards in an exploded view).
fn display_pose(app: &App, a: &Assembly, inst: &Instance) -> Pose {
    if app.asm.explode <= 0.0 {
        return inst.pose;
    }
    let cs: Vec<DVec3> = a.instances.iter().map(|x| x.pose.apply(centroid_local(app, x))).collect();
    let c = cs.iter().fold(DVec3::ZERO, |s, p| s + *p) / cs.len().max(1) as f64;
    let mine = inst.pose.apply(centroid_local(app, inst));
    let mut p = inst.pose;
    p.t = (DVec3::from(p.t) + (mine - c) * app.asm.explode as f64).into();
    p
}

fn mesh_of<'a>(app: &'a App, inst: &Instance) -> Option<&'a crate::kernel::BodyMesh> {
    app.studio_bodies(inst.studio).iter().find(|b| b.id == inst.body)
}

fn part_color(app: &App, inst: &Instance) -> Color32 {
    app.studio_doc(inst.studio)
        .and_then(|d| d.parts.get(&crate::doc::body_key(inst.body)))
        .and_then(|p| p.color)
        .map(|c| Color32::from_rgb(c[0], c[1], c[2]))
        .unwrap_or(vp::BODY)
}

fn centroid_local(app: &App, inst: &Instance) -> DVec3 {
    mesh_of(app, inst).map(|m| DVec3::from(m.centroid)).unwrap_or(DVec3::ZERO)
}

/// Solve the mates of the open assembly, holding `hold` still.
pub fn solve(app: &mut App, hold: &[u64]) {
    let Some(i) = asm_index(app) else { return };
    let centers: Vec<(u64, DVec3)> = app.ws.assemblies[i].instances.iter().map(|x| (x.id, centroid_local(app, x))).collect();
    let r = assembly::solve(&mut app.ws.assemblies[i], hold, &|x: &Instance| centers.iter().find(|c| c.0 == x.id).map(|c| c.1).unwrap_or(DVec3::ZERO));
    app.asm.failed = r.failed;
    app.asm.scene_dirty = true;
}

// ---------------------------------------------------------------- toolbar

pub fn toolbar(app: &mut App, ui: &mut Ui) {
    App::group(ui, |ui| {
        if app.tool_button(ui, "import", "Insert", "Insert parts from Part Studios", true, app.asm.insert_open, true).clicked() {
            app.asm.insert_open = !app.asm.insert_open;
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
        if app.tool_button(ui, "transform", "Explode", "Exploded view", false, on, true).clicked() {
            app.asm.explode = if on { 0.0 } else { 0.8 };
            app.asm.scene_dirty = true;
        }
        if on && ui.add(egui::Slider::new(&mut app.asm.explode, 0.05..=3.0).show_value(false)).on_hover_text("Explode distance").changed() {
            app.asm.scene_dirty = true;
        }
    });
}

fn start_mate(app: &mut App, kind: MateKind) {
    if app.asm.dialog.is_some() {
        mate_ok(app);
    }
    let Some(i) = asm_index(app) else { return };
    let before = app.ws.assemblies[i].clone();
    checkpoint(app);
    let a = &mut app.ws.assemblies[i];
    let id = a.new_id();
    let n = a.mates.iter().filter(|m| m.kind == kind).count() + 1;
    a.mates.push(Mate { id, name: format!("{} {n}", kind.label()), kind, a: None, b: None, flip: false, offset: 0.0, suppressed: false });
    app.asm.dialog = Some(MateDialog { mate: id, before, active: 0 });
    app.asm.sel.clear();
}

fn mate_ok(app: &mut App) {
    if let Some(d) = app.asm.dialog.take() {
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
        if let Some(i) = asm_index(app) {
            app.ws.assemblies[i] = d.before;
            app.asm.undo.pop();
        }
    }
    app.asm.scene_dirty = true;
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
    app.asm.scene_dirty = true;
}

pub fn shortcuts(app: &mut App, ctx: &egui::Context) {
    let pressed = |k: egui::Key| ctx.input(|i| i.key_pressed(k));
    if pressed(egui::Key::Escape) {
        if app.asm.dialog.is_some() {
            mate_cancel(app);
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
}

fn fit(app: &mut App) {
    let Some(a) = asm(app) else { return };
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for x in &a.instances {
        if let Some(m) = mesh_of(app, x) {
            for c in 0..8 {
                let p = DVec3::new(m.bbox[c & 1][0], m.bbox[(c >> 1) & 1][1], m.bbox[(c >> 2) & 1][2]);
                let w = x.pose.apply(p);
                lo = lo.min(w);
                hi = hi.max(w);
            }
        }
    }
    if lo.x.is_finite() {
        app.camera.fit(lo.as_vec3(), hi.as_vec3());
    }
    app.asm.scene_dirty = true;
}

// ---------------------------------------------------------------- sidebar

pub fn sidebar(app: &mut App, ui: &mut Ui) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if app.asm.insert_open {
            section(ui, "Insert", |ui| insert_panel(app, ui));
        }
        section(ui, "Instances", |ui| instances(app, ui));
        section(ui, "Mates", |ui| mates(app, ui));
        section(ui, "Bill of materials", |ui| bom_ui(app, ui));
    });
}

fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new().inner_margin(Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title.to_uppercase()).color(MUTED).size(11.0).strong());
        ui.add_space(4.0);
        add(ui);
    });
    let r = ui.max_rect();
    let y = ui.cursor().top();
    ui.painter().hline(r.x_range(), y, Stroke::new(1.0, BORDER));
}

fn insert_panel(app: &mut App, ui: &mut Ui) {
    let mut add: Option<(usize, crate::doc::BodyId, String)> = None;
    for (si, s) in app.ws.studios.iter().enumerate() {
        ui.label(RichText::new(&s.name).color(MUTED).size(12.0));
        let bodies = app.studio_bodies(si);
        if bodies.is_empty() {
            ui.label(RichText::new("  (no parts yet)").color(MUTED).italics().size(12.0));
        }
        for b in bodies {
            let name = app
                .studio_doc(si)
                .and_then(|d| d.parts.get(&crate::doc::body_key(b.id)))
                .and_then(|p| p.name.clone())
                .unwrap_or_else(|| b.name.clone());
            if ui.add(egui::Button::image_and_text(icons::image("extrude", 14.0).tint(TEXT), RichText::new(&name).size(12.5))).on_hover_text("Insert an instance").clicked() {
                add = Some((si, b.id, name));
            }
        }
    }
    if let Some((studio, body, name)) = add {
        insert(app, studio, body, name);
    }
}

fn insert(app: &mut App, studio: usize, body: crate::doc::BodyId, name: String) {
    checkpoint(app);
    // place new instances beside what is already there
    let mut max_x = f64::NEG_INFINITY;
    if let Some(a) = asm(app) {
        for x in &a.instances {
            if let Some(m) = mesh_of(app, x) {
                for c in 0..8 {
                    let p = DVec3::new(m.bbox[c & 1][0], m.bbox[(c >> 1) & 1][1], m.bbox[(c >> 2) & 1][2]);
                    max_x = max_x.max(x.pose.apply(p).x);
                }
            }
        }
    }
    let min_x = app.studio_bodies(studio).iter().find(|b| b.id == body).map(|m| m.bbox[0][0]).unwrap_or(0.0);
    let t = if max_x.is_finite() { [max_x + 10.0 - min_x, 0.0, 0.0] } else { [0.0; 3] };
    let Some(a) = asm_mut(app) else { return };
    let id = a.new_id();
    let n = a.instances.iter().filter(|x| x.studio == studio && x.body == body).count() + 1;
    let fixed = a.instances.is_empty();
    a.instances.push(Instance { id, name: format!("{name} <{n}>"), studio, body, pose: Pose { t, ..Default::default() }, fixed, hidden: false });
    app.asm.scene_dirty = true;
    if fixed {
        fit(app);
    }
}

fn instances(app: &mut App, ui: &mut Ui) {
    let Some(a) = asm(app) else { return };
    if a.instances.is_empty() {
        ui.label(RichText::new("Use Insert to add parts").color(MUTED).size(12.0));
        return;
    }
    let rows: Vec<(u64, String, bool, bool)> = a.instances.iter().map(|x| (x.id, x.name.clone(), x.fixed, x.hidden)).collect();
    let mut toggle_hide = None;
    for (id, name, fixed, hidden) in rows {
        let picked = app.asm.sel.contains(&id);
        let label = if fixed { format!("📌 {name}") } else { name };
        let r = ui.add(egui::Button::selectable(picked, RichText::new(label).size(12.5).color(if hidden { MUTED } else { TEXT })).min_size(egui::vec2(ui.available_width(), 20.0)));
        if r.clicked() {
            if !ui.input(|i| i.modifiers.shift || i.modifiers.command) {
                app.asm.sel.clear();
            }
            app.asm.sel.push(id);
            app.asm.sel_mate = None;
            app.asm.scene_dirty = true;
        }
        r.context_menu(|ui| {
            if ui.button(if hidden { "Show" } else { "Hide" }).clicked() {
                toggle_hide = Some(id);
                ui.close();
            }
        });
    }
    if let Some(id) = toggle_hide {
        if let Some(a) = asm_mut(app) {
            if let Some(x) = a.instances.iter_mut().find(|x| x.id == id) {
                x.hidden = !x.hidden;
            }
        }
        app.asm.scene_dirty = true;
    }
}

fn mates(app: &mut App, ui: &mut Ui) {
    let Some(a) = asm(app) else { return };
    if a.mates.is_empty() {
        ui.label(RichText::new("Pick a mate type in the toolbar").color(MUTED).size(12.0));
        return;
    }
    let rows: Vec<(u64, String, MateKind, bool)> = a.mates.iter().map(|m| (m.id, m.name.clone(), m.kind, m.suppressed)).collect();
    let mut action = None;
    for (id, name, kind, suppressed) in rows {
        let failed = app.asm.failed.contains(&id);
        let picked = app.asm.sel_mate == Some(id) || app.asm.dialog.as_ref().map(|d| d.mate == id).unwrap_or(false);
        let col = if failed { theme::ERROR_SOFT } else if suppressed { MUTED } else { TEXT };
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
            if ui.button(RichText::new("Delete").color(theme::ERROR_SOFT)).clicked() {
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
                app.asm.dialog = Some(MateDialog { mate: id, before, active: 0 });
            }
        }
        Some(("suppress", id)) => {
            checkpoint(app);
            if let Some(a) = asm_mut(app) {
                if let Some(m) = a.mates.iter_mut().find(|m| m.id == id) {
                    m.suppressed = !m.suppressed;
                }
            }
            solve(app, &[]);
        }
        Some(("delete", id)) => {
            checkpoint(app);
            if let Some(a) = asm_mut(app) {
                a.mates.retain(|m| m.id != id);
            }
            app.asm.scene_dirty = true;
        }
        _ => {}
    }
}

/// (item, part name, quantity, material, mass each in g)
pub fn bom(app: &App, a: &Assembly) -> Vec<(usize, String, usize, String, Option<f64>)> {
    let mut rows: Vec<((usize, crate::doc::BodyId), String, usize, String, Option<f64>)> = vec![];
    for x in &a.instances {
        let key = (x.studio, x.body);
        if let Some(r) = rows.iter_mut().find(|r| r.0 == key) {
            r.2 += 1;
            continue;
        }
        let props = app.studio_doc(x.studio).and_then(|d| d.parts.get(&crate::doc::body_key(x.body))).cloned().unwrap_or_default();
        let mesh = mesh_of(app, x);
        let name = props.name.clone().or_else(|| mesh.map(|m| m.name.clone())).unwrap_or_else(|| x.name.clone());
        let density = props.material.as_ref().and_then(|m| crate::doc::MATERIALS.iter().find(|q| q.0 == m)).map(|q| q.1);
        let mass = match (density, mesh) {
            (Some(d), Some(m)) => Some(m.volume / 1000.0 * d),
            _ => None,
        };
        rows.push((key, name, 1, props.material.unwrap_or_default(), mass));
    }
    rows.into_iter().enumerate().map(|(i, r)| (i + 1, r.1, r.2, r.3, r.4)).collect()
}

fn bom_ui(app: &mut App, ui: &mut Ui) {
    let Some(a) = asm(app) else { return };
    let rows = bom(app, a);
    if rows.is_empty() {
        ui.label(RichText::new("Empty").color(MUTED).size(12.0));
        return;
    }
    egui::Grid::new("bom").num_columns(4).striped(true).spacing([8.0, 3.0]).show(ui, |ui| {
        for h in ["#", "Part", "Qty", "Material"] {
            ui.label(RichText::new(h).color(MUTED).size(11.5));
        }
        ui.end_row();
        for (i, n, q, m, _) in &rows {
            ui.label(RichText::new(i.to_string()).size(12.0));
            ui.label(RichText::new(n).size(12.0));
            ui.label(RichText::new(q.to_string()).size(12.0));
            ui.label(RichText::new(if m.is_empty() { "—" } else { m }).size(12.0));
            ui.end_row();
        }
    });
    let total: f64 = rows.iter().filter_map(|r| r.4.map(|m| m * r.2 as f64)).sum();
    if total > 0.0 {
        ui.label(RichText::new(format!("Total mass {} g", crate::expr::trim(total, 1))).color(MUTED).size(12.0));
    }
    if ui.button("Export CSV…").clicked() {
        let mut csv = String::from("Item,Part,Quantity,Material,Mass each (g)\n");
        for (i, n, q, m, mass) in &rows {
            csv.push_str(&format!("{i},\"{}\",{q},{m},{}\n", n.replace('"', "'"), mass.map(|v| format!("{v:.2}")).unwrap_or_default()));
        }
        if let Some(p) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name("bom.csv").save_file() {
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
    // edges first
    let mut best: Option<(f32, Hit)> = None;
    for x in a.instances.iter().filter(|x| !x.hidden) {
        let Some(m) = mesh_of(app, x) else { continue };
        let pose = display_pose(app, a, x);
        for (ei, e) in m.edges.iter().enumerate() {
            let pts: Vec<Vec2> = e.pts.iter().map(|p| cam.project(pose.apply(DVec3::from(p.map(|v| v as f64))).as_vec3())).collect();
            for w in pts.windows(2) {
                let d = w[1] - w[0];
                let t = if d.length_squared() < 1e-9 { 0.0 } else { ((s - w[0]).dot(d) / d.length_squared()).clamp(0.0, 1.0) };
                let dist = (w[0] + d * t - s).length();
                if dist < 6.0 && best.as_ref().map(|b| dist < b.0).unwrap_or(true) {
                    best = Some((dist, Hit { inst: x.id, face: None, edge: Some(ei), local: e.mid }));
                }
            }
        }
    }
    if let Some((_, h)) = best {
        return Some(h);
    }
    let (o, d) = cam.ray(s);
    let (o, d) = (o.as_dvec3(), d.as_dvec3());
    let mut best: Option<(f64, Hit)> = None;
    for x in a.instances.iter().filter(|x| !x.hidden) {
        let Some(m) = mesh_of(app, x) else { continue };
        // ray into the instance's coordinates
        let pose = display_pose(app, a, x);
        let lo = pose.inverse_apply(o);
        let ld = pose.inverse_dir(d);
        for (ti, t) in m.indices.chunks(3).enumerate() {
            let v = |i: u32| DVec3::new(m.positions[3 * i as usize] as f64, m.positions[3 * i as usize + 1] as f64, m.positions[3 * i as usize + 2] as f64);
            if let Some(tt) = ray_tri(lo, ld, v(t[0]), v(t[1]), v(t[2])) {
                if best.as_ref().map(|b| tt < b.0).unwrap_or(true) {
                    best = Some((tt, Hit { inst: x.id, face: Some(m.tri_face[ti] as usize), edge: None, local: (lo + ld * tt).into() }));
                }
            }
        }
    }
    best.map(|b| b.1)
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
    let m = mesh_of(app, x)?;
    assembly::infer_connector(h.inst, m, h.face, h.edge, h.local.into())
}

fn build_scene(app: &App) -> SceneData {
    let mut s = SceneData::default();
    let Some(a) = asm(app) else { return s };
    let dialog_conns: Vec<Connector> = app
        .asm
        .dialog
        .as_ref()
        .and_then(|d| a.mates.iter().find(|m| m.id == d.mate))
        .map(|m| m.a.iter().chain(m.b.iter()).copied().collect())
        .unwrap_or_default();
    let mate_insts: Vec<u64> = app.asm.sel_mate.and_then(|id| a.mates.iter().find(|m| m.id == id)).map(|m| m.a.iter().chain(m.b.iter()).map(|c| c.inst).collect()).unwrap_or_default();
    for x in a.instances.iter().filter(|x| !x.hidden) {
        let Some(m) = mesh_of(app, x) else { continue };
        let sel = app.asm.sel.contains(&x.id) || mate_insts.contains(&x.id);
        let base = part_color(app, x);
        let hov_face = app.asm.hover.filter(|h| h.inst == x.id).and_then(|h| h.face);
        let pose = display_pose(app, a, x);
        let rot = pose.m().as_mat3();
        for (ti, t) in m.indices.chunks(3).enumerate() {
            let fi = m.tri_face[ti] as usize;
            let col = if hov_face == Some(fi) {
                vp::HOVER
            } else if sel {
                vp::PICKED
            } else {
                base
            };
            for &i in t {
                let i = i as usize;
                let p = pose.apply(DVec3::new(m.positions[3 * i] as f64, m.positions[3 * i + 1] as f64, m.positions[3 * i + 2] as f64));
                let n = rot * Vec3::new(m.normals[3 * i], m.normals[3 * i + 1], m.normals[3 * i + 2]);
                s.opaque.push(MeshVertex { pos: p.as_vec3().into(), normal: n.into(), color: crate::viewport::rgba(col, 1.0) });
            }
        }
        let hov_edge = app.asm.hover.filter(|h| h.inst == x.id).and_then(|h| h.edge);
        for (ei, e) in m.edges.iter().enumerate() {
            let pts: Vec<Vec3> = e.pts.iter().map(|p| pose.apply(DVec3::from(p.map(|v| v as f64))).as_vec3()).collect();
            let hov = hov_edge == Some(ei);
            s.polyline(&pts, if hov { 3.0 } else { 1.4 }, crate::viewport::rgba(if hov { vp::HOVER } else { vp::EDGE }, 1.0), hov);
        }
        let _ = world_tri;
    }
    // mate connectors being picked
    let size = (app.camera.wpp() * 28.0) as f64;
    for c in &dialog_conns {
        if let Some((o, z, xx)) = a.world(c) {
            let y = z.cross(xx);
            for (d, col) in [(xx, vp::AXIS_X), (y, vp::AXIS_Y), (z, vp::AXIS_Z)] {
                s.lines_top.push(crate::render::LineInst { a: o.as_vec3().into(), b: (o + d * size).as_vec3().into(), width: 2.5, bias: 0.0, color: crate::viewport::rgba(col, 1.0) });
            }
            s.point(o.as_vec3(), 8.0, crate::viewport::rgba(vp::SNAP, 1.0));
        }
    }
    s
}

pub fn viewport(app: &mut App, ui: &mut Ui) {
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    if app.viewport_rect != rect {
        app.viewport_rect = rect;
        app.asm.scene_dirty = true;
    }
    app.camera.size = Vec2::new(rect.width().max(1.0), rect.height().max(1.0));
    ui.painter().rect_filled(rect, 0.0, vp::BACKGROUND);
    let before = (app.camera.target, app.camera.back, app.camera.up, app.camera.half_height);
    crate::viewport::navigate(app, ui, &resp);
    if before != (app.camera.target, app.camera.back, app.camera.up, app.camera.half_height) {
        app.asm.scene_dirty = true;
    }
    let local = |p: Pos2| Vec2::new(p.x - rect.min.x, p.y - rect.min.y);

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
        if let Some(d) = &app.asm.dialog {
            if let Some(h) = h {
                if let Some(c) = connector_for(app, &h) {
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
                        solve(app, &[]);
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

    // dragging instances (left drag; Shift rotates)
    if app.asm.dialog.is_none() {
        if resp.drag_started_by(egui::PointerButton::Primary) {
            if let Some(h) = app.asm.hover {
                let fixed = asm(app).and_then(|a| a.instance(h.inst)).map(|x| x.fixed).unwrap_or(true);
                if !fixed {
                    checkpoint(app);
                    let start = asm(app).and_then(|a| a.instance(h.inst)).map(|x| x.pose).unwrap_or_default();
                    let rotate = ui.input(|i| i.modifiers.shift);
                    app.asm.drag = Some(Drag { inst: h.inst, start, rotate, moved: Vec2::ZERO });
                }
            }
        }
        if let Some(mut d) = app.asm.drag.take() {
            let down = ui.input(|i| i.pointer.primary_down());
            let delta = ui.input(|i| i.pointer.delta());
            d.moved += Vec2::new(delta.x, delta.y);
            let cam = app.camera.clone();
            let c_local = asm(app).and_then(|a| a.instance(d.inst)).map(|x| centroid_local(app, x)).unwrap_or(DVec3::ZERO);
            let pose = if d.rotate {
                let c = d.start.apply(c_local);
                let w = (cam.up.as_dvec3() * d.moved.x as f64 + cam.right().as_dvec3() * d.moved.y as f64) * 0.01;
                d.start.perturbed(w, DVec3::ZERO, c)
            } else {
                let wpp = cam.wpp() as f64;
                let v = cam.right().as_dvec3() * (d.moved.x as f64 * wpp) - cam.up.as_dvec3() * (d.moved.y as f64 * wpp);
                d.start.perturbed(DVec3::ZERO, v, DVec3::ZERO)
            };
            let id = d.inst;
            if let Some(a) = asm_mut(app) {
                if let Some(x) = a.instances.iter_mut().find(|x| x.id == id) {
                    x.pose = pose;
                }
            }
            solve(app, &[id]);
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
        clip: match app.section {
            Some(sec) => {
                let n = [Vec3::Z, -Vec3::Y, Vec3::X][sec.plane as usize];
                let (n, w) = if sec.flip { (-n, -(sec.offset as f32)) } else { (n, sec.offset as f32) };
                [n.x, n.y, n.z, w]
            }
            None => [0.0; 4],
        },
        size_px: [rect.width() * ppp, rect.height() * ppp],
        pixels_per_point: ppp,
    };
    ui.painter().add(eframe::egui_wgpu::Callback::new_paint_callback(rect, cb));
    let painter = ui.painter_at(rect);
    crate::viewport::axis_triad(app, &painter, rect);
    if let (Some(h), Some(p)) = (app.asm.hover, resp.hover_pos()) {
        if let Some(x) = asm(app).and_then(|a| a.instance(h.inst)) {
            let what = if h.edge.is_some() { "edge" } else { "face" };
            crate::viewport::label_box(&painter, p + egui::vec2(14.0, 18.0), &format!("{} · {what}", x.name), TEXT);
        }
    }
    mate_dialog(app, ui, rect);
}

fn mate_dialog(app: &mut App, ui: &mut Ui, rect: egui::Rect) {
    let Some(d) = &app.asm.dialog else { return };
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
        egui::Frame::new()
            .fill(Color32::from_rgba_unmultiplied(0x1f, 0x24, 0x2b, 0xf7))
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(8)
            .inner_margin(Margin { left: 10, right: 10, top: 8, bottom: 10 })
            .show(ui, |ui| {
                ui.set_width(300.0);
                ui.horizontal(|ui| {
                    ui.add(icons::image(m.kind.icon(), 18.0).tint(TEXT));
                    ui.label(RichText::new(&m.name).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::image(icons::image("close", 16.0).tint(TEXT))).on_hover_text("Cancel (Esc)").clicked() {
                            cancel = true;
                        }
                        if ui.add(egui::Button::image(icons::image("check", 16.0).tint(theme::OK_GREEN))).on_hover_text("OK (Enter)").clicked() {
                            ok = true;
                        }
                    });
                });
                egui::Grid::new("mate-grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label(RichText::new("Type").color(MUTED));
                    egui::ComboBox::from_id_salt("mate-kind").selected_text(edited.kind.label()).show_ui(ui, |ui| {
                        for k in MateKind::ALL {
                            ui.selectable_value(&mut edited.kind, k, k.label());
                        }
                    });
                    ui.end_row();
                    for (i, (label, n)) in [("First", &na), ("Second", &nb)].into_iter().enumerate() {
                        ui.label(RichText::new(label).color(MUTED));
                        let fr = egui::Frame::new().fill(theme::BG).stroke(Stroke::new(1.0, if active == i { theme::ACCENT } else { BORDER })).corner_radius(5).inner_margin(Margin::symmetric(6, 3));
                        let r = fr
                            .show(ui, |ui| {
                                ui.set_width(180.0);
                                match n {
                                    Some(n) => ui.label(RichText::new(n).size(12.0)),
                                    None => ui.label(RichText::new("Pick a face, edge or circle").italics().color(MUTED).size(12.0)),
                                };
                            })
                            .response;
                        if r.interact(Sense::click()).clicked() {
                            set_active = Some(i);
                        }
                        ui.end_row();
                    }
                    ui.label(RichText::new(format!("Offset ({})", units.label())).color(MUTED));
                    let mut off = edited.offset / units.scale();
                    if ui.add(egui::DragValue::new(&mut off).speed(0.5)).changed() {
                        edited.offset = off * units.scale();
                    }
                    ui.end_row();
                });
                ui.checkbox(&mut edited.flip, "Flip primary axis");
                ui.add_space(4.0);
                ui.label(RichText::new("Click a face, edge or circular edge on each part · Enter to accept").color(MUTED).size(12.0));
            });
    });
    if let Some(i) = set_active {
        if let Some(d) = &mut app.asm.dialog {
            d.active = i;
        }
    }
    let changed = edited.kind != m.kind || edited.flip != m.flip || edited.offset != m.offset;
    if changed {
        if let Some(a) = asm_mut(app) {
            if let Some(mm) = a.mates.iter_mut().find(|x| x.id == mid) {
                if edited.kind != mm.kind {
                    edited.name = mm.name.replace(mm.kind.label(), edited.kind.label());
                }
                *mm = edited;
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
