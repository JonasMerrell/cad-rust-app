//! The machine model in the Manufacture tab: choosing and importing one, the window for checking which axis
//! carries each part, and drawing the machine in the simulation.

use std::sync::mpsc::{Receiver, channel};
use std::sync::Arc;

use egui::{Color32, RichText, Ui};
use glam::Vec3;

use crate::app::{App, ToastKind};
use crate::cam::Cam;
use crate::cam_machine::{self, CARRIERS, Carrier, MachineModel};
use crate::render::SceneData;
use crate::theme;

#[derive(Default)]
pub struct MachineUi {
    /// A STEP file being read: (setup index, receiver).
    importing: Option<(usize, Receiver<Result<MachineModel, String>>)>,
    /// The machine being checked or edited in the window.
    pub editing: Option<MachineModel>,
    /// Parts being read to add onto the table: (group name, receiver).
    adding: Option<(String, Receiver<Result<Vec<cam_machine::StepPart>, String>>)>,
    /// How far to move each group (by name) in the window.
    moves: std::collections::HashMap<String, [f64; 3]>,
    /// Draw the machine in the simulation (on unless turned off).
    pub hide: bool,
    /// Keep the work still and move the whole machine around it, instead of the base staying put.
    pub follow_work: bool,
}

pub enum Action {
    Import,
    Edit,
}

/// The machine model rows of a setup's Machine section.
pub fn setup_rows(ui: &mut Ui, c: &mut Cam, importing: bool) -> Option<Action> {
    let mut act = None;
    let names = cam_machine::list();
    crate::cam_ui::row(ui, "Machine model", |ui| {
        let label = if c.machine.model.is_empty() { "none".to_string() } else { cam_machine::clean(&c.machine.model) };
        egui::ComboBox::from_id_salt("cam-machine-model").width(150.0).selected_text(label).show_ui(ui, |ui| {
            ui.selectable_value(&mut c.machine.model, String::new(), "none");
            for n in &names {
                ui.selectable_value(&mut c.machine.model, n.clone(), n);
            }
        })
        .response
        .on_hover_text("The mill's own geometry: the simulation shows it moving and checks the head, the table and the tool against each other, the stock and the fixtures");
    });
    ui.horizontal(|ui| {
        if importing {
            ui.spinner();
            ui.label(RichText::new("Reading the machine…").size(12.0));
        } else if ui.button("Import machine STEP…").on_hover_text("Read a STEP (or Parasolid .x_t) assembly of the machine; name its axes X/Y/Z axis and its frame Base, or set them afterwards").clicked() {
            act = Some(Action::Import);
        }
        if !c.machine.model.is_empty() && ui.button("Axes…").on_hover_text("Which axis moves each part, where the spindle and the table are, and the travel").clicked() {
            act = Some(Action::Edit);
        }
    });
    if !c.machine.model.is_empty() {
        crate::cam_ui::row(ui, "On the table X/Y/Z", |ui| {
            for v in c.on_table.iter_mut() {
                ui.add(egui::DragValue::new(v).speed(0.5).max_decimals(2).suffix(" mm"))
                    .on_hover_text("Where the stock's middle (at the bottom of the stock, or of the vise if it stands lower) sits, from the middle of the table's top");
            }
        });
    }
    act
}

/// Start reading a STEP file for setup `ci` in the background.
pub fn start_import(app: &mut App, ctx: &egui::Context, ci: usize) {
    let Some(path) = rfd::FileDialog::new().add_filter("STEP or Parasolid", &["step", "stp", "x_t", "STEP", "STP", "X_T"]).pick_file() else { return };
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Machine".into());
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(cam_machine::import_step(&path, &name));
        ctx.request_repaint();
    });
    app.cam.machine.importing = Some((ci, rx));
}

pub fn is_importing(app: &App) -> bool {
    app.cam.machine.importing.is_some()
}

pub fn edit(app: &mut App, name: &str) {
    match cam_machine::load(name) {
        Ok(l) => app.cam.machine.editing = Some(l.model.clone()),
        Err(e) => app.flash(e, ToastKind::Error),
    }
}

/// Finish an import, and the window for checking the machine.
pub fn window(app: &mut App, ctx: &egui::Context) {
    if let Some((ci, rx)) = &app.cam.machine.importing {
        let ci = *ci;
        if let Ok(r) = rx.try_recv() {
            app.cam.machine.importing = None;
            match r.and_then(|m| cam_machine::save(&m).map(|_| m)) {
                Ok(m) => {
                    if let Some(c) = app.ws.cams.get_mut(ci) {
                        c.machine.model = m.name.clone();
                    }
                    app.cam.sim = None;
                    app.cam.scene_dirty = true;
                    app.flash_info(format!("Read {} parts of {}: check which axis moves each", m.parts.len(), m.name), Some(std::time::Duration::from_secs(4)));
                    app.cam.machine.editing = Some(m);
                }
                Err(e) => app.flash(format!("Could not read the machine: {e}"), ToastKind::Error),
            }
        }
    }
    let Some(mut m) = app.cam.machine.editing.take() else { return };
    if let Some((g, rx)) = &app.cam.machine.adding {
        if let Ok(r) = rx.try_recv() {
            let g = g.clone();
            app.cam.machine.adding = None;
            match r {
                Ok(parts) => {
                    let n = parts.len();
                    m.add_on_table(&g, parts, glam::DVec3::ZERO);
                    app.flash_info(format!("Added {n} parts of {g} on the middle of the table: move them where they stand, then Save"), Some(std::time::Duration::from_secs(4)));
                }
                Err(e) => app.flash(format!("Could not read the parts: {e}"), ToastKind::Error),
            }
        }
    }
    let mut add_step = false;
    let mut open = true;
    let mut save = false;
    let mut close = false;
    egui::Window::new(format!("Machine: {}", m.name))
        .id(egui::Id::new("cam-machine-window"))
        .open(&mut open)
        .default_size([460.0, 560.0])
        .default_pos(ctx.content_rect().center() - egui::vec2(230.0, 280.0))
        .constrain(true)
        .collapsible(false)
        .show(ctx, |ui| {
            if let Some(p) = m.problem() {
                ui.label(RichText::new(format!("⚠ {p}")).color(theme::error()));
            }
            ui.label(RichText::new("AXES").color(theme::muted()).size(11.0).strong());
            for a in 0..3 {
                let c = [Carrier::X, Carrier::Y, Carrier::Z][a];
                crate::cam_ui::row(ui, &format!("{} rides on", c.short()), |ui| {
                    egui::ComboBox::from_id_salt(("cam-machine-rides", a)).width(130.0).selected_text(m.rides[a].label()).show_ui(ui, |ui| {
                        for k in CARRIERS.into_iter().filter(|k| *k != c) {
                            ui.selectable_value(&mut m.rides[a], k, k.label());
                        }
                    });
                });
            }
            for (label, v, tip) in [
                ("Spindle on", &mut m.spindle_on, "What carries the spindle (usually the Z axis)"),
                ("Table on", &mut m.table_on, "What carries the work: the table's axis, or the base on a gantry router"),
            ] {
                crate::cam_ui::row(ui, label, |ui| {
                    egui::ComboBox::from_id_salt(("cam-machine-on", label)).width(130.0).selected_text(v.label()).show_ui(ui, |ui| {
                        for k in CARRIERS {
                            ui.selectable_value(v, k, k.label());
                        }
                    })
                    .response
                    .on_hover_text(tip);
                });
            }
            for (a, n) in ["X", "Y", "Z"].iter().enumerate() {
                crate::cam_ui::row(ui, &format!("{n} travel"), |ui| {
                    ui.add(egui::DragValue::new(&mut m.travel[a][0]).speed(1.0).max_decimals(1).suffix(" mm"));
                    ui.label("to");
                    ui.add(egui::DragValue::new(&mut m.travel[a][1]).speed(1.0).max_decimals(1).suffix(" mm"));
                });
            }
            ui.label(RichText::new("Travel is from where each axis is in the model (equal ends: not checked). The first guess is how far its guides reach: set your soft limits.").color(theme::muted()).size(11.5));
            ui.add_space(4.0);
            ui.label(RichText::new("SPINDLE AND TABLE").color(theme::muted()).size(11.0).strong());
            crate::cam_ui::row(ui, "Tool comes out at", |ui| {
                for v in m.spindle.iter_mut() {
                    ui.add(egui::DragValue::new(v).speed(0.5).max_decimals(2));
                }
            });
            ui.checkbox(&mut m.holders, "Tools hang their holder below this point")
                .on_hover_text("On: the model ends at the spindle face and each tool's holder adds its length. Off: the model has the collet nut, and tools stick out of it by their stickout.");
            crate::cam_ui::row(ui, "Table top middle", |ui| {
                for v in m.table.iter_mut() {
                    ui.add(egui::DragValue::new(v).speed(0.5).max_decimals(2));
                }
            });
            ui.add_space(4.0);
            ui.label(RichText::new("PARTS").color(theme::muted()).size(11.0).strong());
            // by top-level assembly, each with the carrier of its parts (mixed: shown per part)
            let mut groups: Vec<String> = vec![];
            for p in &m.parts {
                let g = p.name.split('/').next().unwrap_or("").to_string();
                if !groups.contains(&g) {
                    groups.push(g);
                }
            }
            let mut gone: Option<String> = None;
            let mut shift: Option<(String, [f64; 3])> = None;
            let moves = &mut app.cam.machine.moves;
            egui::ScrollArea::vertical().max_height(260.0).id_salt("cam-machine-parts").show(ui, |ui| {
                for g in &groups {
                    let ids: Vec<usize> = (0..m.parts.len()).filter(|&i| m.parts[i].name.split('/').next() == Some(g.as_str())).collect();
                    let first = m.parts[ids[0]].carrier;
                    let mixed = ids.iter().any(|&i| m.parts[i].carrier != first);
                    ui.horizontal(|ui| {
                        let mut pick = None;
                        egui::ComboBox::from_id_salt(("cam-machine-group", g)).width(110.0).selected_text(if mixed { "mixed" } else { first.short() }).show_ui(ui, |ui| {
                            for k in CARRIERS {
                                if ui.selectable_label(!mixed && k == first, k.label()).clicked() {
                                    pick = Some(k);
                                }
                            }
                        });
                        // a pick sets every part of the group, also one already on it in a mixed group
                        if let Some(pick) = pick {
                            for &i in &ids {
                                m.parts[i].carrier = pick;
                            }
                        }
                        ui.label(RichText::new(cam_machine::clean(g)).size(12.5));
                        if ui.small_button("✕").on_hover_text("Take these parts off the machine (something it doesn't have)").clicked() {
                            gone = Some(g.clone());
                        }
                    });
                    egui::CollapsingHeader::new(RichText::new("Move").size(11.5).color(theme::muted())).id_salt(("cam-machine-move", g)).show(ui, |ui| {
                        let d = moves.entry(g.clone()).or_insert([0.0; 3]);
                        ui.horizontal(|ui| {
                            for v in d.iter_mut() {
                                ui.add(egui::DragValue::new(v).speed(0.5).max_decimals(2).suffix(" mm"));
                            }
                            if ui.small_button("Move").on_hover_text("Move these parts by X/Y/Z (model coordinates)").clicked() {
                                shift = Some((g.clone(), *d));
                            }
                        });
                    });
                    if ids.len() > 1 {
                        egui::CollapsingHeader::new(RichText::new(format!("{} parts", ids.len())).size(11.5).color(theme::muted())).id_salt(("cam-machine-sub", g)).show(ui, |ui| {
                            for &i in &ids {
                                ui.horizontal(|ui| {
                                    let p = &mut m.parts[i];
                                    egui::ComboBox::from_id_salt(("cam-machine-part", i)).width(90.0).selected_text(p.carrier.short()).show_ui(ui, |ui| {
                                        for k in CARRIERS {
                                            ui.selectable_value(&mut p.carrier, k, k.label());
                                        }
                                    });
                                    let leaf = p.name.split_once('/').map(|x| x.1).filter(|l| !l.starts_with("=>")).unwrap_or("(its own body)");
                                    ui.label(RichText::new(leaf).size(11.5));
                                });
                            }
                        });
                    }
                }
            });
            if let Some(g) = gone {
                let full = m.parts.iter().find(|p| p.name.split('/').next() == Some(g.as_str())).map(|p| p.name.split('/').next().unwrap_or("").to_string()).unwrap_or(g);
                m.remove(&full);
            }
            if let Some((g, d)) = shift {
                m.shift_group(&g, glam::DVec3::from(d));
                app.cam.machine.moves.remove(&g);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if app.cam.machine.adding.is_some() {
                    ui.spinner();
                } else if ui.button("Add parts from STEP…").on_hover_text("From a STEP or Parasolid .x_t file: put something that stands on the table (a vise, a tool setter, a fixture plate) on the machine; it rides with the table").clicked() {
                    add_step = true;
                }
                if ui.button("Save").on_hover_text("Keep these settings for the machine (every setup that uses it)").clicked() {
                    save = true;
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
    if add_step {
        if let Some(path) = rfd::FileDialog::new().add_filter("STEP or Parasolid", &["step", "stp", "x_t", "STEP", "STP", "X_T"]).pick_file() {
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Fixture".into());
            let (tx, rx) = channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(cam_machine::read_step(&path));
                ctx.request_repaint();
            });
            app.cam.machine.adding = Some((name, rx));
        }
    }
    if save {
        match cam_machine::save(&m) {
            Ok(_) => {
                cam_machine::update_cached(&m.name, Arc::new(cam_machine::Loaded::new(m.clone())));
                app.cam.sim = None;
                app.cam.scene_dirty = true;
                app.flash_info(format!("Saved machine {}", m.name), Some(std::time::Duration::from_secs(2)));
            }
            Err(e) => app.flash(format!("Could not save the machine: {e}"), ToastKind::Error),
        }
    }
    if open && !close {
        app.cam.machine.editing = Some(m);
    }
}

/// The simulation's checkbox rows for the machine.
pub fn sim_rows(app: &mut App, ui: &mut Ui) {
    let Some(sim) = &app.cam.sim else { return };
    let Some(m) = &sim.machine else { return };
    let overlapping = m.placed.overlapping.clone();
    let mut show = !app.cam.machine.hide;
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui.checkbox(&mut show, "Show the machine").changed();
        changed |= ui.checkbox(&mut app.cam.machine.follow_work, "Follow the work").on_hover_text("Keep the work still and move the machine around it, instead of the base standing still").changed();
    });
    if changed {
        app.cam.machine.hide = !show;
        app.cam.scene_dirty = true;
    }
    for (a, b) in overlapping {
        ui.label(RichText::new(format!("{} and {} overlap as modelled, so they aren't checked against each other", a.short(), b.short())).color(theme::muted()).size(11.5));
    }
}

/// Draw the machine where the simulation is, and move the whole scene so its base stands still (unless
/// following the work).
pub fn draw(app: &App, s: &mut SceneData) {
    if app.cam.machine.hide {
        return;
    }
    let Some(sim) = &app.cam.sim else { return };
    let Some(sm) = &sim.machine else { return };
    let placed = &sm.placed;
    let ci = sim.current_cutter().unwrap_or(0);
    let rel = placed.pose(sim.tool_pos(), sim.gauge(ci));
    // parts in a crash at this move turn red
    let bad: Vec<usize> = sm.hit_parts.iter().filter(|(step, _)| step + 1 >= sim.done && *step < sim.done + 1).flat_map(|(_, p)| p.iter().copied()).collect();
    let m = &placed.machine.model;
    let head = m.chain(m.spindle_on);
    for (i, p) in m.parts.iter().enumerate() {
        let k = CARRIERS.iter().position(|c| *c == p.carrier).unwrap_or(0);
        let off = (placed.shift + rel[k]).as_vec3();
        // (a black frame would read as a hole in the view: lift the darkest colours a little)
        let lift = |c: u8| (c as u16 * 3 / 4 + 48) as u8;
        let col = if bad.contains(&i) { Color32::from_rgb(0xe0, 0x40, 0x40) } else { Color32::from_rgb(lift(p.color[0]), lift(p.color[1]), lift(p.color[2])) };
        // the head see-through, so the cut stays in view under it
        let see_through = head.contains(&p.carrier);
        let c = crate::viewport::rgba(col, if see_through { 0.35 } else { 1.0 });
        for t in &placed.machine.tris[i] {
            s.tri(see_through, [t[0] + off, t[1] + off, t[2] + off], c);
        }
    }
    if !app.cam.machine.follow_work {
        let base = CARRIERS.iter().position(|c| *c == Carrier::Base).unwrap_or(0);
        translate(s, (-rel[base]).as_vec3());
    }
}

fn translate(s: &mut SceneData, d: Vec3) {
    if d == Vec3::ZERO {
        return;
    }
    let add = |p: &mut [f32; 3]| *p = (Vec3::from(*p) + d).into();
    for v in s.opaque.iter_mut().chain(s.transparent.iter_mut()) {
        add(&mut v.pos);
    }
    for l in s.lines.iter_mut().chain(s.lines_top.iter_mut()) {
        add(&mut l.a);
        add(&mut l.b);
    }
    for p in s.points.iter_mut() {
        add(&mut p.p);
    }
}
