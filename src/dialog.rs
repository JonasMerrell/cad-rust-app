//! The feature dialog shown over the viewport's top-left corner while a feature
//! is created or edited. Selection fields take picks from the viewport.

use egui::{Color32, Margin, RichText, Stroke, Ui};

use crate::app::App;
use crate::doc::{BoolOp, BooleanKind, Document, EndType, HoleType, Op, Ref, kind_label};
use crate::expr::Kind;
use crate::icons;
use crate::theme;

#[derive(Clone, Copy)]
enum Accept {
    Profiles,
    Planar,
    Axis,
    Edges,
    Faces,
    Face,
    Points,
    Bodies,
    Path,
    Features,
    Curve,
    SheetProfile,
}

impl Accept {
    fn ok(self, r: &Ref) -> bool {
        match self {
            Accept::Profiles => matches!(r, Ref::Region { .. } | Ref::Face { .. }),
            Accept::Planar => r.is_planar(),
            Accept::Axis => matches!(r, Ref::Axis { .. } | Ref::SketchCurve { .. } | Ref::Edge { .. } | Ref::OriginPlane { .. } | Ref::FeaturePlane { .. } | Ref::Face { .. }),
            Accept::Edges => matches!(r, Ref::Edge { .. }),
            Accept::Faces | Accept::Face => matches!(r, Ref::Face { .. }),
            Accept::Points => matches!(r, Ref::SketchPoint { .. } | Ref::Vertex { .. }),
            Accept::Bodies => matches!(r, Ref::Body { .. } | Ref::Face { .. } | Ref::Edge { .. }),
            Accept::Path => matches!(r, Ref::SketchCurve { .. } | Ref::Edge { .. } | Ref::Curve { .. }),
            Accept::Features => matches!(r, Ref::Feature { .. }),
            Accept::SheetProfile => matches!(r, Ref::SketchCurve { .. } | Ref::Region { .. }),
            Accept::Curve => matches!(r, Ref::SketchCurve { .. } | Ref::Edge { .. } | Ref::Curve { .. } | Ref::Region { .. }),
        }
    }
    fn hint(self) -> &'static str {
        match self {
            Accept::Profiles => "sketch regions or faces",
            Accept::Planar => "a plane or planar face",
            Accept::Axis => "a line or straight edge",
            Accept::Edges => "edges",
            Accept::Faces | Accept::Face => "faces",
            Accept::Points => "sketch points",
            Accept::Bodies => "bodies (or a face of each)",
            Accept::Path => "sketch curves, edges or a helix",
            Accept::Features => "features in the feature list",
            Accept::SheetProfile => "connected sketch lines, or a region for a flat plate",
            Accept::Curve => "a curve or edge",
        }
    }
}

enum Slot<'a> {
    Many(&'a mut Vec<Ref>),
    One(&'a mut Option<Ref>),
}

/// Selection fields of an operation, in display order.
fn sel_fields(op: &mut Op) -> Vec<(&'static str, Accept, Slot<'_>)> {
    use Accept::*;
    match op {
        Op::Plane { base, mode, refs, .. } => match mode.as_str() {
            "angle" => vec![("Base plane", Planar, Slot::One(base)), ("Hinge line", Axis, Slot::Many(refs))],
            "threePoint" => vec![("Points", Points, Slot::Many(refs))],
            "normalToCurve" => vec![("Curve", Curve, Slot::Many(refs))],
            "midPlane" => vec![("First plane", Planar, Slot::One(base)), ("Second plane", Planar, Slot::Many(refs))],
            _ => vec![("Base plane", Planar, Slot::One(base))],
        },
        Op::Extrude { profiles, up_to, end, .. } => {
            let mut v = vec![("Profiles", Profiles, Slot::Many(profiles))];
            if *end == EndType::UpToFace {
                v.push(("Up to face", Face, Slot::One(up_to)));
            }
            v
        }
        Op::Revolve { profiles, axis, .. } => vec![("Profiles", Profiles, Slot::Many(profiles)), ("Axis", Axis, Slot::One(axis))],
        Op::Sweep { profiles, path, .. } => vec![("Profiles", Profiles, Slot::Many(profiles)), ("Path", Path, Slot::Many(path))],
        Op::Loft { profiles, .. } => vec![("Profiles", Profiles, Slot::Many(profiles))],
        Op::Fillet { edges, .. } | Op::Chamfer { edges, .. } => vec![("Edges", Edges, Slot::Many(edges))],
        Op::Shell { faces, .. } => vec![("Faces to remove", Faces, Slot::Many(faces))],
        Op::Hole { points, .. } => vec![("Hole points", Points, Slot::Many(points))],
        Op::Draft { faces, neutral, .. } => vec![("Faces", Faces, Slot::Many(faces)), ("Neutral plane", Planar, Slot::One(neutral))],
        Op::LinearPattern { kind, bodies, direction, second, .. } => vec![
            if kind == "feature" { ("Features", Features, Slot::Many(bodies)) } else { ("Bodies", Bodies, Slot::Many(bodies)) },
            ("Direction", Axis, Slot::One(direction)),
            ("Second direction", Axis, Slot::One(second)),
        ],
        Op::CircularPattern { kind, bodies, axis, .. } => {
            vec![if kind == "feature" { ("Features", Features, Slot::Many(bodies)) } else { ("Bodies", Bodies, Slot::Many(bodies)) }, ("Axis", Axis, Slot::One(axis))]
        }
        Op::Mirror { kind, bodies, plane } => {
            vec![if kind == "feature" { ("Features", Features, Slot::Many(bodies)) } else { ("Bodies", Bodies, Slot::Many(bodies)) }, ("Mirror plane", Planar, Slot::One(plane))]
        }
        Op::Thicken { faces, .. } => vec![("Faces", Profiles, Slot::Many(faces))],
        Op::Helix { axis, .. } => vec![("Axis", Axis, Slot::One(axis))],
        Op::Transform { bodies, mode, direction, axis, .. } => {
            if mode == "rotate" {
                vec![("Bodies", Bodies, Slot::Many(bodies)), ("Axis", Axis, Slot::One(axis))]
            } else {
                vec![("Bodies", Bodies, Slot::Many(bodies)), ("Direction", Axis, Slot::One(direction))]
            }
        }
        Op::DeleteFace { faces } => vec![("Faces", Faces, Slot::Many(faces))],
        Op::SheetMetal { profile, .. } => vec![("Profile", SheetProfile, Slot::Many(profile))],
        Op::MoveFace { faces, .. } => vec![("Faces", Faces, Slot::Many(faces))],
        Op::Rib { profile, .. } => vec![("Lines", Path, Slot::Many(profile))],
        Op::Boolean { target, tools, .. } => vec![("Target body", Bodies, Slot::One(target)), ("Tool bodies", Bodies, Slot::Many(tools))],
        Op::Split { target, plane } => vec![("Target body", Bodies, Slot::One(target)), ("Split plane", Planar, Slot::One(plane))],
        Op::DeleteBody { bodies } => vec![("Bodies", Bodies, Slot::Many(bodies))],
        Op::SurfaceExtrude { curves, .. } => vec![("Curves", Curve, Slot::Many(curves))],
        Op::SurfaceRevolve { curves, axis, .. } => vec![("Curves", Curve, Slot::Many(curves)), ("Axis", Axis, Slot::One(axis))],
        Op::SurfaceLoft { curves, .. } => vec![("Profiles", Curve, Slot::Many(curves))],
        Op::Fill { edges } => vec![("Boundary", Curve, Slot::Many(edges))],
        Op::OffsetSurface { faces, .. } => vec![("Faces", Bodies, Slot::Many(faces))],
        Op::Knit { bodies, .. } => vec![("Surfaces", Bodies, Slot::Many(bodies))],
        Op::Sketch { .. } | Op::Import { .. } | Op::Derived { .. } => vec![],
    }
}

fn normalize(r: Ref, accept: Accept, doc: &Document) -> Ref {
    // a sketch picked in the tree acts as its plane
    let _ = doc;
    match (accept, &r) {
        (Accept::Bodies, _) => r,
        _ => r,
    }
}

/// Put pre-selected geometry into the first fields that accept it.
pub fn seed_from_picks(op: &mut Op, picks: &[Ref], doc: &Document) {
    for p in picks {
        for (_, acc, slot) in sel_fields(op) {
            if acc.ok(p) {
                let p = normalize(p.clone(), acc, doc);
                match slot {
                    Slot::Many(v) => v.push(p),
                    Slot::One(o) => {
                        if o.is_some() {
                            continue;
                        }
                        *o = Some(p)
                    }
                }
                break;
            }
        }
    }
}

/// A viewport or tree pick while the dialog is open.
pub fn add_pick(app: &mut App, r: Ref) {
    let Some(d) = &mut app.dialog else { return };
    let fid = d.feature;
    let mut active = d.active_field;
    let Some(f) = app.doc.feature_mut(fid) else { return };
    let mut fields = sel_fields(&mut f.op);
    if fields.is_empty() {
        return;
    }
    // if the active field doesn't take this kind, try the others
    if !fields[active.min(fields.len() - 1)].1.ok(&r) {
        if let Some(i) = fields.iter().position(|f| f.1.ok(&r)) {
            active = i;
        } else {
            let hint = fields[active.min(fields.len() - 1)].1.hint();
            drop(fields);
            app.flash(format!("Select {hint}"), crate::app::ToastKind::Info);
            return;
        }
    }
    let (_, _, slot) = &mut fields[active];
    match slot {
        Slot::Many(v) => {
            if let Some(i) = v.iter().position(|x| *x == r) {
                v.remove(i);
            } else {
                v.push(r);
            }
        }
        Slot::One(o) => {
            if o.as_ref() == Some(&r) {
                **o = None;
            } else {
                **o = Some(r);
            }
        }
    }
    drop(fields);
    if let Some(d) = &mut app.dialog {
        d.active_field = active;
    }
    app.changed();
}

/// Everything referenced by the open dialog (for highlighting).
pub fn field_refs(app: &App) -> Vec<Ref> {
    let Some(d) = &app.dialog else { return vec![] };
    let Some(f) = app.doc.feature(d.feature) else { return vec![] };
    let mut op = f.op.clone();
    let mut out = vec![];
    for (_, _, slot) in sel_fields(&mut op) {
        match slot {
            Slot::Many(v) => out.extend(v.iter().cloned()),
            Slot::One(o) => out.extend(o.iter().cloned()),
        }
    }
    out
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let Some(d) = &app.dialog else { return };
    let fid = d.feature;
    let active = d.active_field;
    let Some(f) = app.doc.feature(fid) else {
        app.dialog = None;
        return;
    };
    let kind = f.op.kind();
    let name = f.name.clone();
    let error = app.errors.get(&fid).cloned();
    let rect = app.viewport_rect;
    let phone = app.phone;
    let mut op = f.op.clone();
    let vars = app.doc.var_values().0;
    let units = app.doc.units;
    let mut ok = false;
    let mut cancel = false;
    let mut new_active = None;
    let flat = app.result.flats.iter().find(|f| f.feature == fid).cloned();
    // Part Studios and their parts, for Derived
    let studios: Vec<(usize, String, Vec<([u64; 2], String)>)> = (0..app.ws.studios.len())
        .filter(|i| *i != app.cur_studio)
        .map(|i| {
            let parts = app.studio_bodies(i).iter().map(|b| ([b.id.0, b.id.1 as u64], b.name.clone())).collect();
            (i, app.ws.studios[i].name.clone(), parts)
        })
        .collect();
    let mut export_flat = false;

    let area = egui::Area::new(egui::Id::new("feature-dialog")).order(egui::Order::Middle);
    let area = if phone { area.pivot(egui::Align2::LEFT_BOTTOM).fixed_pos(egui::pos2(rect.left(), rect.bottom())) } else { area.fixed_pos(rect.min + egui::vec2(10.0, 10.0)) };
    area.show(ui.ctx(), |ui| {
        egui::Frame::new()
            .fill(theme::window())
            .stroke(Stroke::new(1.0, theme::border()))
            .corner_radius(8)
            .inner_margin(Margin { left: 10, right: 10, top: 8, bottom: 10 })
            .shadow(egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(0x59) })
            .show(ui, |ui| {
                ui.set_width(if phone { rect.width() - 20.0 } else { 300.0 });
                ui.horizontal(|ui| {
                    ui.add(icons::image(kind, 18.0).tint(theme::text()));
                    ui.label(RichText::new(&name).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::image(icons::image("close", 16.0).tint(theme::text()))).on_hover_text("Cancel (Esc)").clicked() {
                            cancel = true;
                        }
                        if ui.add(egui::Button::image(icons::image("check", 16.0).tint(theme::ok_green()))).on_hover_text("OK (Enter)").clicked() {
                            ok = true;
                        }
                    });
                });
                ui.add_space(4.0);

                // selection fields
                let mut op2 = op.clone();
                for (i, (label, acc, slot)) in sel_fields(&mut op2).into_iter().enumerate() {
                    let refs: Vec<Ref> = match &slot {
                        Slot::Many(v) => (*v).clone(),
                        Slot::One(o) => o.iter().cloned().collect(),
                    };
                    let is_active = i == active;
                    let resp = ui
                        .horizontal(|ui| {
                            ui.add_sized([96.0, 20.0], egui::Label::new(RichText::new(label).color(theme::muted())));
                            let fr = egui::Frame::new()
                                .fill(theme::bg())
                                .stroke(Stroke::new(1.0, if is_active { theme::accent() } else { theme::border() }))
                                .corner_radius(5)
                                .inner_margin(Margin::symmetric(6, 3));
                            fr.show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                if refs.is_empty() {
                                    ui.label(RichText::new(format!("Select {}", acc.hint())).color(theme::muted()).italics().size(12.0));
                                } else {
                                    let txt = if refs.len() == 1 { refs[0].describe() } else { format!("{} selected", refs.len()) };
                                    ui.label(RichText::new(txt).size(12.0));
                                }
                            })
                            .response
                        })
                        .inner;
                    if resp.interact(egui::Sense::click()).clicked() {
                        new_active = Some(i);
                    }
                }

                // value and option fields
                let mut changed = false;
                let val = |ui: &mut Ui, label: &str, s: &mut String, kind: Kind, changed: &mut bool| {
                    ui.horizontal(|ui| {
                        ui.add_sized([96.0, 20.0], egui::Label::new(RichText::new(label).color(theme::muted())));
                        let ok = crate::doc::eval(s, kind, &vars, units).is_ok();
                        let mut te = egui::TextEdit::singleline(s).desired_width(ui.available_width());
                        if !ok {
                            te = te.text_color(theme::error_soft());
                        }
                        let r = ui.add(te);
                        if r.changed() {
                            *changed = true;
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            ENTER.with(|e| e.set(true));
                        }
                        r
                    });
                };
                let check = |ui: &mut Ui, label: &str, b: &mut bool, changed: &mut bool| {
                    if ui.checkbox(b, label).changed() {
                        *changed = true;
                    }
                };
                fn pattern_kind(ui: &mut Ui, kind: &mut String, changed: &mut bool) {
                    let mut k = kind.clone();
                    combo(ui, "Pattern type", &mut k, &[("part".to_string(), "Part"), ("feature".to_string(), "Feature")], changed);
                    *kind = k;
                }
                fn combo<T: PartialEq + Clone>(ui: &mut Ui, label: &str, v: &mut T, opts: &[(T, &str)], changed: &mut bool) {
                    ui.horizontal(|ui| {
                        ui.add_sized([96.0, 20.0], egui::Label::new(RichText::new(label).color(theme::muted())));
                        let cur = opts.iter().find(|o| o.0 == *v).map(|o| o.1).unwrap_or("");
                        egui::ComboBox::from_id_salt(label).selected_text(cur).width(ui.available_width()).show_ui(ui, |ui| {
                            for (o, l) in opts {
                                if ui.selectable_label(*v == *o, *l).clicked() {
                                    *v = o.clone();
                                    *changed = true;
                                }
                            }
                        });
                    });
                }
                let bool_ops: Vec<(BoolOp, &str)> = BoolOp::ALL.iter().map(|b| (*b, b.label())).collect();
                match &mut op {
                    Op::Plane { offset, mode, angle, flip, .. } => {
                        let mut m = mode.clone();
                        combo(
                            ui,
                            "Type",
                            &mut m,
                            &[
                                ("offset".to_string(), "Offset"),
                                ("angle".to_string(), "Angle"),
                                ("threePoint".to_string(), "Three points"),
                                ("normalToCurve".to_string(), "Normal to curve"),
                                ("midPlane".to_string(), "Mid plane"),
                            ],
                            &mut changed,
                        );
                        *mode = m;
                        match mode.as_str() {
                            "offset" => val(ui, "Distance", offset, Kind::Length, &mut changed),
                            "angle" => val(ui, "Angle (°)", angle, Kind::Angle, &mut changed),
                            _ => {}
                        }
                        check(ui, "Flip normal", flip, &mut changed);
                    }
                    Op::Extrude { end, depth, opposite, second, depth2, op: bop, draft, .. } => {
                        combo(ui, "Operation", bop, &bool_ops, &mut changed);
                        let ends: Vec<(EndType, &str)> = EndType::ALL.iter().map(|e| (*e, e.label())).collect();
                        combo(ui, "End type", end, &ends, &mut changed);
                        if matches!(end, EndType::Blind | EndType::Symmetric) {
                            val(ui, "Depth", depth, Kind::Length, &mut changed);
                        }
                        check(ui, "Opposite direction", opposite, &mut changed);
                        if *end == EndType::Blind {
                            check(ui, "Second direction", second, &mut changed);
                            if *second {
                                val(ui, "Second depth", depth2, Kind::Length, &mut changed);
                            }
                        }
                        let _ = draft;
                    }
                    Op::Revolve { mode, angle, op: bop, .. } => {
                        combo(ui, "Operation", bop, &bool_ops, &mut changed);
                        let mut m = match mode.as_str() {
                            "one" => 1,
                            "symmetric" => 2,
                            _ => 0,
                        };
                        combo(ui, "Type", &mut m, &[(0, "Full"), (1, "One direction"), (2, "Symmetric")], &mut changed);
                        *mode = ["full", "one", "symmetric"][m].into();
                        if m != 0 {
                            val(ui, "Angle (°)", angle, Kind::Angle, &mut changed);
                        }
                    }
                    Op::Sweep { op: bop, .. } => combo(ui, "Operation", bop, &bool_ops, &mut changed),
                    Op::Loft { op: bop, ruled, .. } => {
                        combo(ui, "Operation", bop, &bool_ops, &mut changed);
                        check(ui, "Straight (ruled) sides", ruled, &mut changed);
                    }
                    Op::Fillet { radius, variable, end_radius, .. } => {
                        val(ui, "Radius", radius, Kind::Length, &mut changed);
                        check(ui, "Variable radius", variable, &mut changed);
                        if *variable {
                            val(ui, "End radius", end_radius, Kind::Length, &mut changed);
                        }
                    }
                    Op::Chamfer { distance, .. } => val(ui, "Distance", distance, Kind::Length, &mut changed),
                    Op::Shell { thickness, .. } => val(ui, "Thickness", thickness, Kind::Length, &mut changed),
                    Op::Hole { hole_type, diameter, depth, through, cb_diameter, cb_depth, cs_diameter, cs_angle, tapped, .. } => {
                        ui.horizontal(|ui| {
                            ui.add_sized([96.0, 20.0], egui::Label::new(RichText::new("ISO size").color(theme::muted())));
                            egui::ComboBox::from_id_salt("iso-hole").selected_text("Choose…").show_ui(ui, |ui| {
                                // (size, clearance (normal), tap drill, counterbore Ø, counterbore depth, countersink Ø)
                                for (m, clr, tap, cbd, cbh, csd) in ISO_HOLES {
                                    if ui.selectable_label(false, format!("M{m} clearance")).clicked() {
                                        *diameter = format!("{clr} mm");
                                        *cb_diameter = format!("{cbd} mm");
                                        *cb_depth = format!("{cbh} mm");
                                        *cs_diameter = format!("{csd} mm");
                                        *cs_angle = "90".into();
                                        *tapped = false;
                                        changed = true;
                                    }
                                    if ui.selectable_label(false, format!("M{m} tapped")).clicked() {
                                        *diameter = format!("{tap} mm");
                                        *tapped = true;
                                        *hole_type = HoleType::Simple;
                                        changed = true;
                                    }
                                }
                            });
                        });
                        combo(
                            ui,
                            "Hole type",
                            hole_type,
                            &[(HoleType::Simple, "Simple"), (HoleType::Counterbore, "Counterbore"), (HoleType::Countersink, "Countersink")],
                            &mut changed,
                        );
                        val(ui, "Diameter", diameter, Kind::Length, &mut changed);
                        check(ui, "Through all", through, &mut changed);
                        if !*through {
                            val(ui, "Depth", depth, Kind::Length, &mut changed);
                        }
                        match hole_type {
                            HoleType::Counterbore => {
                                val(ui, "Counterbore Ø", cb_diameter, Kind::Length, &mut changed);
                                val(ui, "Counterbore depth", cb_depth, Kind::Length, &mut changed);
                            }
                            HoleType::Countersink => {
                                val(ui, "Countersink Ø", cs_diameter, Kind::Length, &mut changed);
                                val(ui, "Countersink angle", cs_angle, Kind::Angle, &mut changed);
                            }
                            HoleType::Simple => {}
                        }
                        if ui.checkbox(tapped, "Tapped hole (cosmetic thread)").changed() {
                            changed = true;
                        }
                    }
                    Op::Draft { angle, .. } => val(ui, "Angle (°)", angle, Kind::Angle, &mut changed),
                    Op::LinearPattern { kind, spacing, count, second, spacing2, count2, .. } => {
                        pattern_kind(ui, kind, &mut changed);
                        val(ui, "Spacing", spacing, Kind::Length, &mut changed);
                        val(ui, "Instances", count, Kind::Count, &mut changed);
                        if second.is_some() {
                            val(ui, "Spacing (2nd)", spacing2, Kind::Length, &mut changed);
                            val(ui, "Instances (2nd)", count2, Kind::Count, &mut changed);
                        }
                    }
                    Op::CircularPattern { kind, count, angle, .. } => {
                        pattern_kind(ui, kind, &mut changed);
                        val(ui, "Instances", count, Kind::Count, &mut changed);
                        val(ui, "Total angle (°)", angle, Kind::Angle, &mut changed);
                    }
                    Op::Mirror { kind, .. } => pattern_kind(ui, kind, &mut changed),
                    Op::MoveFace { distance, .. } => val(ui, "Distance", distance, Kind::Length, &mut changed),
                    Op::SurfaceExtrude { depth, symmetric, opposite, .. } => {
                        val(ui, "Depth", depth, Kind::Length, &mut changed);
                        check(ui, "Symmetric", symmetric, &mut changed);
                        check(ui, "Opposite direction", opposite, &mut changed);
                    }
                    Op::SurfaceRevolve { angle, .. } => val(ui, "Angle (°)", angle, Kind::Angle, &mut changed),
                    Op::SurfaceLoft { ruled, .. } => check(ui, "Straight (ruled) sides", ruled, &mut changed),
                    Op::OffsetSurface { distance, .. } => val(ui, "Distance", distance, Kind::Length, &mut changed),
                    Op::Knit { solid, .. } => check(ui, "Make a solid if closed", solid, &mut changed),
                    Op::Derived { studio, bodies } => {
                        if studios.is_empty() {
                            ui.label(RichText::new("Add another Part Studio (the + tab) to derive parts from.").color(theme::muted()).size(12.0));
                        }
                        let cur = studios.iter().find(|s| s.0 == *studio).map(|s| s.1.clone()).unwrap_or_else(|| "Choose…".into());
                        ui.horizontal(|ui| {
                            ui.add_sized([96.0, 20.0], egui::Label::new(RichText::new("Part Studio").color(theme::muted())));
                            egui::ComboBox::from_id_salt("derive-studio").selected_text(cur).show_ui(ui, |ui| {
                                for (i, n, _) in &studios {
                                    if ui.selectable_label(*studio == *i, n).clicked() {
                                        *studio = *i;
                                        bodies.clear();
                                        changed = true;
                                    }
                                }
                            });
                        });
                        if let Some((_, _, parts)) = studios.iter().find(|s| s.0 == *studio) {
                            let mut all = bodies.is_empty();
                            if ui.checkbox(&mut all, "All parts").changed() {
                                bodies.clear();
                                if !all {
                                    bodies.extend(parts.iter().take(1).map(|p| (p.0[0], p.0[1] as u32)));
                                }
                                changed = true;
                            }
                            if !all {
                                for (id, n) in parts {
                                    let bid = (id[0], id[1] as u32);
                                    let mut on = bodies.contains(&bid);
                                    if ui.checkbox(&mut on, n).changed() {
                                        if on {
                                            bodies.push(bid);
                                        } else {
                                            bodies.retain(|b| *b != bid);
                                        }
                                        changed = true;
                                    }
                                }
                            }
                        }
                    }
                    Op::Rib { thickness, flip, .. } => {
                        val(ui, "Thickness", thickness, Kind::Length, &mut changed);
                        check(ui, "Other side of the line", flip, &mut changed);
                    }
                    Op::SheetMetal { thickness, radius, k_factor, depth, flip, op: bop, .. } => {
                        combo(ui, "Operation", bop, &bool_ops, &mut changed);
                        val(ui, "Thickness", thickness, Kind::Length, &mut changed);
                        val(ui, "Bend radius", radius, Kind::Length, &mut changed);
                        val(ui, "K-factor", k_factor, Kind::Count, &mut changed);
                        val(ui, "Depth", depth, Kind::Length, &mut changed);
                        check(ui, "Material on the other side", flip, &mut changed);
                        if let Some(f) = flat.as_ref() {
                            ui.label(RichText::new(format!("Flat pattern {} × {}", units.fmt_with(f.size[0]), units.fmt_with(f.size[1]))).color(theme::muted()).size(12.0));
                            if ui.button("Export flat pattern DXF…").clicked() {
                                export_flat = true;
                            }
                        }
                    }
                    Op::Thicken { thickness, opposite, op: bop, .. } => {
                        combo(ui, "Operation", bop, &bool_ops, &mut changed);
                        val(ui, "Thickness", thickness, Kind::Length, &mut changed);
                        check(ui, "Opposite direction", opposite, &mut changed);
                    }
                    Op::Helix { radius, pitch, turns, left, .. } => {
                        val(ui, "Radius", radius, Kind::Length, &mut changed);
                        val(ui, "Pitch", pitch, Kind::Length, &mut changed);
                        val(ui, "Turns", turns, Kind::Count, &mut changed);
                        check(ui, "Left-handed", left, &mut changed);
                    }
                    Op::Transform { mode, distance, angle, copy, .. } => {
                        let mut m = mode.clone();
                        combo(ui, "Type", &mut m, &[("translate".to_string(), "Translate"), ("rotate".to_string(), "Rotate")], &mut changed);
                        *mode = m;
                        if mode == "rotate" {
                            val(ui, "Angle (°)", angle, Kind::Angle, &mut changed);
                        } else {
                            val(ui, "Distance", distance, Kind::Length, &mut changed);
                        }
                        check(ui, "Copy", copy, &mut changed);
                    }
                    Op::Boolean { kind, keep_tools, .. } => {
                        combo(ui, "Operation", kind, &[(BooleanKind::Union, "Union"), (BooleanKind::Subtract, "Subtract"), (BooleanKind::Intersect, "Intersect")], &mut changed);
                        check(ui, "Keep tools", keep_tools, &mut changed);
                    }
                    _ => {}
                }
                if let Some(e) = &error {
                    ui.add_space(4.0);
                    ui.label(RichText::new(e).color(theme::error_soft()));
                }
                ui.add_space(4.0);
                ui.label(RichText::new(format!("{} · Enter to accept, Esc to cancel", hint_for(kind))).color(theme::muted()).size(12.0));
                if changed {
                    if let Some(f) = app.doc.feature_mut(fid) {
                        f.op = op.clone();
                    }
                    app.changed();
                }
            });
    });
    if export_flat {
        if let Some(f) = &flat {
            if let Some(p) = rfd::FileDialog::new().add_filter("DXF", &["dxf"]).set_file_name("flat-pattern.dxf").save_file() {
                match std::fs::write(&p, crate::kernel::flat_dxf(f)) {
                    Ok(()) => app.flash_info(format!("Exported {}", p.display()), Some(std::time::Duration::from_secs(3))),
                    Err(e) => app.flash(format!("Could not export: {e}"), crate::app::ToastKind::Error),
                }
            }
        }
    }
    if ENTER.with(|e| e.replace(false)) {
        ok = true;
    }
    if let Some(i) = new_active {
        if let Some(d) = &mut app.dialog {
            d.active_field = i;
        }
    }
    if ok {
        app.dialog_ok();
    } else if cancel {
        app.dialog_cancel();
    }
}

/// ISO metric: (size, normal clearance Ø, tap drill Ø, counterbore Ø, counterbore depth, countersink Ø)
const ISO_HOLES: &[(&str, f64, f64, f64, f64, f64)] = &[
    ("2", 2.4, 1.6, 4.4, 2.0, 4.4),
    ("2.5", 2.9, 2.05, 5.4, 2.5, 5.5),
    ("3", 3.4, 2.5, 6.5, 3.3, 6.3),
    ("4", 4.5, 3.3, 8.0, 4.4, 9.4),
    ("5", 5.5, 4.2, 10.0, 5.4, 10.4),
    ("6", 6.6, 5.0, 11.0, 6.4, 12.6),
    ("8", 9.0, 6.8, 15.0, 8.6, 17.3),
    ("10", 11.0, 8.5, 18.0, 10.6, 20.0),
    ("12", 13.5, 10.2, 20.0, 12.6, 24.0),
    ("16", 17.5, 14.0, 26.0, 16.6, 31.0),
    ("20", 22.0, 17.5, 33.0, 20.6, 38.0),
];

thread_local! {
    static ENTER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn hint_for(kind: &str) -> String {
    match kind {
        "extrude" | "revolve" | "sweep" | "loft" => "Click sketch regions or faces in the viewport".into(),
        "fillet" | "chamfer" => "Click model edges".into(),
        "hole" => "Click sketch points where holes go".into(),
        _ => format!("{} options", kind_label(kind)),
    }
}
