//! The sidebar of an Advanced-mode study: crackh3d's boundary conditions, periodic pairs, body force and
//! solver (a child of `fea_ui`, using its helpers).

use egui::{RichText, Ui};

use super::{edit, faces_row, faces_text, index, study, trim};
use crate::app::App;
use crate::cam_ui::{row, section};
use crate::fea_adv::{BcType, SolverKind};
use crate::theme;

/// The Advanced mode's sections, in place of Supports and Loads.
pub(super) fn sections(app: &mut App, ui: &mut Ui) {
    section(ui, "Boundary conditions", |ui| bcs_ui(app, ui));
    if let Some(id) = app.fea.sel.filter(|id| study(app).is_some_and(|s| s.advanced.bcs.iter().any(|b| b.id == *id))) {
        section(ui, "Boundary condition", |ui| bc_ui(app, ui, id));
    }
    section(ui, "Periodic", |ui| periodic_list(app, ui));
    if let Some(id) = app.fea.sel.filter(|id| study(app).is_some_and(|s| s.advanced.periodic.iter().any(|b| b.id == *id))) {
        section(ui, "Periodic pair", |ui| periodic_ui(app, ui, id));
    }
}

/// The analysis row of an Advanced study, and its solver.
pub(super) fn study_rows(app: &mut App, ui: &mut Ui) {
    let Some(si) = index(app) else { return };
    let s = &mut app.ws.sims[si];
    row(ui, "Analysis", |ui| {
        ui.label(RichText::new("Static (crackh3d solver)").size(12.5)).on_hover_text("Advanced mode solves a static, linear elastic load case with crackh3d's solver");
    });
    row(ui, "Solver", |ui| {
        ui.radio_value(&mut s.advanced.solver, SolverKind::Iterative, "Iterative").on_hover_text("CG with an AMG preconditioner: less memory on big meshes (falls back to the direct solve)");
        ui.radio_value(&mut s.advanced.solver, SolverKind::Direct, "Direct").on_hover_text("Sparse Cholesky: quick for small and medium meshes");
    });
}

fn select(app: &mut App, id: u64) {
    app.fea.sel = if app.fea.sel == Some(id) { None } else { Some(id) };
    app.fea.side_b = false;
    app.fea.show_results = app.fea.sel.is_none();
    app.fea.scene_dirty = true;
}

/// A selectable list row with Suppress / Delete in its context menu; returns the action picked.
fn item_row(ui: &mut Ui, sel: bool, name: &str, suppressed: bool, info: String) -> Option<&'static str> {
    let mut action = None;
    ui.horizontal(|ui| {
        let mut text = RichText::new(name).size(12.5);
        if suppressed {
            text = text.color(theme::muted()).strikethrough();
        }
        let r = ui.add(egui::Button::selectable(sel, text).truncate());
        if r.clicked() {
            action = Some("select");
        }
        r.context_menu(|ui| {
            if ui.button(if suppressed { "Unsuppress" } else { "Suppress" }).clicked() {
                action = Some("suppress");
                ui.close();
            }
            if ui.button(RichText::new("Delete").color(theme::error_soft())).clicked() {
                action = Some("delete");
                ui.close();
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(info).color(theme::muted()).size(11.5));
        });
    });
    action
}

fn bcs_ui(app: &mut App, ui: &mut Ui) {
    let Some(s) = study(app).cloned() else { return };
    if s.advanced.bcs.is_empty() {
        ui.label(RichText::new("Add boundary conditions below, then click the faces each acts on.").color(theme::muted()).size(12.0));
    }
    let mut act: Option<(&str, u64)> = None;
    for b in &s.advanced.bcs {
        let info = if b.bc.on_faces() { format!("{} · {}", short(&b.bc), faces_text(b.faces.len())) } else { short(&b.bc).to_string() };
        if let Some(a) = item_row(ui, app.fea.sel == Some(b.id), &b.name, b.suppressed, info) {
            act = Some((a, b.id));
        }
    }
    let mut add = None;
    egui::ComboBox::from_id_salt("fea-adv-add").selected_text("Add…").width(150.0).show_ui(ui, |ui| {
        for k in BcType::KINDS {
            if ui.selectable_label(false, BcType::label_of(k)).clicked() {
                add = Some(k);
            }
        }
    });
    if let Some(k) = add {
        let mut id = None;
        edit(app, |s| id = s.add_adv_bc(k));
        if let Some(id) = id {
            app.fea.sel = None;
            select(app, id);
        }
    }
    // body force
    let mut g = s.advanced.gravity;
    ui.horizontal(|ui| {
        let mut on = g.is_some();
        if ui.checkbox(&mut on, "Gravity").on_hover_text("An acceleration of the whole part (in g), such as its own weight").changed() {
            g = if on { Some([0.0, 0.0, -1.0]) } else { None };
        }
        if let Some(v) = &mut g {
            vec3(ui, v, 0.05, " g");
        }
    });
    if g != s.advanced.gravity {
        edit(app, |s| s.advanced.gravity = g);
    }
    if let Some((a, id)) = act {
        match a {
            "select" => select(app, id),
            "suppress" => edit(app, |s| {
                if let Some(b) = s.advanced.bc_mut(id) {
                    b.suppressed = !b.suppressed;
                }
            }),
            _ => {
                edit(app, |s| s.advanced.bcs.retain(|b| b.id != id));
                if app.fea.sel == Some(id) {
                    app.fea.sel = None;
                }
            }
        }
    }
}

/// Short summary of a BC's values for its list row.
fn short(b: &BcType) -> String {
    let v = |x: &[f64; 3]| format!("({}, {}, {})", trim(x[0]), trim(x[1]), trim(x[2]));
    match b {
        BcType::Fixed => "fixed".into(),
        BcType::Symmetry => "symmetry".into(),
        BcType::Displacement { .. } => "displacement".into(),
        BcType::Pressure { p } => format!("{} MPa", trim(*p)),
        BcType::Traction { t } => format!("{} MPa", v(t)),
        BcType::Force { f } | BcType::PointForce { f, .. } => format!("{} N", v(f)),
        BcType::Spring { .. } => "spring".into(),
        BcType::RemoteRigid { .. } => "rigid".into(),
        BcType::RemoteDistributing { .. } => "distributing".into(),
    }
}

fn vec3(ui: &mut Ui, v: &mut [f64; 3], speed: f64, suffix: &str) {
    for (k, x) in v.iter_mut().enumerate() {
        ui.add(egui::DragValue::new(x).speed(speed).max_decimals(6).prefix(["x ", "y ", "z "][k]).suffix(suffix));
    }
}

/// Optional components: a checkbox each (ticked: prescribed) and its value.
fn opt3(ui: &mut Ui, v: &mut [Option<f64>; 3], suffix: &str, tip: &str) {
    for (k, x) in v.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let mut on = x.is_some();
            if ui.checkbox(&mut on, ["x", "y", "z"][k]).on_hover_text(tip).changed() {
                *x = if on { Some(0.0) } else { None };
            }
            match x {
                Some(val) => {
                    ui.add(egui::DragValue::new(val).speed(0.001).max_decimals(6).suffix(suffix));
                }
                None => {
                    ui.label(RichText::new("free").color(theme::muted()).size(12.0));
                }
            }
        });
    }
}

fn bc_ui(app: &mut App, ui: &mut Ui, id: u64) {
    let Some(si) = index(app) else { return };
    let Some(b) = app.ws.sims[si].advanced.bc_mut(id) else { return };
    row(ui, "Name", |ui| {
        ui.text_edit_singleline(&mut b.name);
    });
    row(ui, "Type", |ui| {
        ui.label(RichText::new(b.bc.label()).size(12.5));
    });
    match &mut b.bc {
        BcType::Fixed => {
            ui.label(RichText::new("The faces cannot move at all.").color(theme::muted()).size(12.0));
        }
        BcType::Symmetry => {
            ui.label(RichText::new("The faces cannot move along their normals (a symmetry plane; inclined and curved faces too).").color(theme::muted()).size(12.0));
        }
        BcType::Displacement { u } => {
            row(ui, "Displacement", |ui| {
                ui.vertical(|ui| opt3(ui, u, " mm", "Prescribe this component (unticked: free)"));
            });
        }
        BcType::Pressure { p } => {
            row(ui, "Pressure", |ui| {
                ui.add(egui::DragValue::new(p).speed(0.01).max_decimals(6).suffix(" MPa")).on_hover_text("Pushes into the faces");
            });
        }
        BcType::Traction { t } => {
            row(ui, "Traction", |ui| {
                ui.vertical(|ui| vec3(ui, t, 0.01, " MPa"));
            });
        }
        BcType::Force { f } => {
            row(ui, "Force", |ui| {
                ui.vertical(|ui| vec3(ui, f, 1.0, " N"));
            });
            ui.label(RichText::new("The total force, spread evenly over the faces' area.").color(theme::muted()).size(11.5));
        }
        BcType::Spring { k } => {
            row(ui, "Stiffness", |ui| {
                ui.vertical(|ui| vec3(ui, k, 0.1, " N/mm³"));
            });
            ui.label(RichText::new("Per unit area of the faces, along x, y and z (an elastic foundation).").color(theme::muted()).size(11.5));
        }
        BcType::RemoteRigid { point, force, moment, u, rot } => {
            row(ui, "Point", |ui| {
                ui.vertical(|ui| vec3(ui, point, 0.5, " mm"));
            });
            row(ui, "Motion", |ui| {
                ui.vertical(|ui| opt3(ui, u, " mm", "Prescribe the point's movement along this axis (unticked: free, under the force)"));
            });
            row(ui, "Rotation", |ui| {
                ui.vertical(|ui| opt3(ui, rot, " rad", "Prescribe the rotation about this axis (unticked: free, under the moment)"));
            });
            row(ui, "Force", |ui| {
                ui.vertical(|ui| vec3(ui, force, 1.0, " N"));
            });
            row(ui, "Moment", |ui| {
                ui.vertical(|ui| vec3(ui, moment, 10.0, " N·mm"));
            });
            ui.label(RichText::new("The faces move as one rigid body with the point. The force and moment act on its free components.").color(theme::muted()).size(11.5));
        }
        BcType::RemoteDistributing { point, force, moment } => {
            row(ui, "Point", |ui| {
                ui.vertical(|ui| vec3(ui, point, 0.5, " mm"));
            });
            row(ui, "Force", |ui| {
                ui.vertical(|ui| vec3(ui, force, 1.0, " N"));
            });
            row(ui, "Moment", |ui| {
                ui.vertical(|ui| vec3(ui, moment, 10.0, " N·mm"));
            });
            ui.label(RichText::new("A force and moment about the point, spread over the faces; they stay free to deform.").color(theme::muted()).size(11.5));
        }
        BcType::PointForce { at, f } => {
            row(ui, "At", |ui| {
                ui.vertical(|ui| vec3(ui, at, 0.5, " mm"));
            });
            row(ui, "Force", |ui| {
                ui.vertical(|ui| vec3(ui, f, 1.0, " N"));
            });
            ui.label(RichText::new("On the mesh node nearest the point: the stress right there is singular.").color(theme::muted()).size(11.5));
        }
    }
    if b.bc.on_faces() {
        faces_row(app, ui, id);
    }
}

fn periodic_list(app: &mut App, ui: &mut Ui) {
    let Some(s) = study(app).cloned() else { return };
    let mut act: Option<(&str, u64)> = None;
    for p in &s.advanced.periodic {
        let info = format!("{} ↔ {}", faces_text(p.a.len()), faces_text(p.b.len()));
        if let Some(a) = item_row(ui, app.fea.sel == Some(p.id), &p.name, p.suppressed, info) {
            act = Some((a, p.id));
        }
    }
    if ui.small_button("Add periodic pair").on_hover_text("Two opposite sides that move together: side B follows side A").clicked() {
        let mut id = 0;
        edit(app, |s| id = s.add_periodic());
        app.fea.sel = None;
        select(app, id);
    }
    if let Some((a, id)) = act {
        match a {
            "select" => select(app, id),
            "suppress" => edit(app, |s| {
                if let Some(b) = s.advanced.periodic_mut(id) {
                    b.suppressed = !b.suppressed;
                }
            }),
            _ => {
                edit(app, |s| s.advanced.periodic.retain(|b| b.id != id));
                if app.fea.sel == Some(id) {
                    app.fea.sel = None;
                }
            }
        }
    }
}

fn periodic_ui(app: &mut App, ui: &mut Ui, id: u64) {
    let Some(s) = study(app).cloned() else { return };
    let Some(p) = s.advanced.periodic.iter().find(|p| p.id == id).cloned() else { return };
    let input = super::study_input(app, &s);
    let auto = crate::fea_adv::offset_of(&crate::fea_adv::Periodic { offset: None, ..p.clone() }, &input);
    let mut q = p.clone();
    row(ui, "Name", |ui| {
        ui.text_edit_singleline(&mut q.name);
    });
    // which side clicks add faces to
    row(ui, "Picking", |ui| {
        ui.vertical(|ui| {
        if ui.radio(!app.fea.side_b, format!("Side A ({})", faces_text(p.a.len()))).on_hover_text("The side that leads").clicked() {
            app.fea.side_b = false;
        }
        if ui.radio(app.fea.side_b, format!("Side B ({})", faces_text(p.b.len()))).on_hover_text("The side that follows A").clicked() {
            app.fea.side_b = true;
        }
        });
    });
    ui.horizontal(|ui| {
        if !p.a.is_empty() && ui.small_button("Clear A").clicked() {
            q.a.clear();
        }
        if !p.b.is_empty() && ui.small_button("Clear B").clicked() {
            q.b.clear();
        }
    });
    ui.label(RichText::new("Click faces in the viewport to add them to the side being picked.").color(theme::muted()).size(12.0));
    row(ui, "Offset A → B", |ui| {
        ui.vertical(|ui| {
            let mut own = q.offset.is_some();
            if ui.checkbox(&mut own, "Set it yourself").on_hover_text("Unticked: from the centre of side A's faces to the centre of side B's").changed() {
                q.offset = if own { Some(auto.map(|d| d.to_array()).unwrap_or([0.0; 3])) } else { None };
            }
            match &mut q.offset {
                Some(o) => vec3(ui, o, 0.1, " mm"),
                None => {
                    let text = auto.map(|d| format!("({}, {}, {}) mm", trim(d.x), trim(d.y), trim(d.z))).unwrap_or_else(|| "pick both sides".into());
                    ui.label(RichText::new(text).color(theme::muted()).size(12.0));
                }
            }
        });
    });
    row(ui, "B moves by", |ui| {
        ui.vertical(|ui| vec3(ui, &mut q.jump, 0.001, " mm")).response.on_hover_text("B's displacement relative to A's: zero for plain periodicity, or a mean strain times the offset");
    });
    // the offset the solve uses: the pair's own when set
    if let Some(d) = q.offset.map(glam::DVec3::from).or(auto) {
        let l = d.length();
        if l > 0.0 {
            let e = glam::DVec3::from(q.jump).dot(d) / (l * l);
            ui.label(RichText::new(format!("Mean strain along the offset: {:.4e}", e)).color(theme::muted()).size(11.5));
        }
    }
    if q != p {
        edit(app, |s| {
            if let Some(x) = s.advanced.periodic_mut(id) {
                *x = q;
            }
        });
    }
}
