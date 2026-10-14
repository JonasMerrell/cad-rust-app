//! The feature dialog shown over the viewport's top-left corner while a feature
//! is created or edited. Selection fields take picks from the viewport.

use egui::{Color32, Margin, RichText, Stroke, Ui};

use crate::app::App;
use crate::doc::{kind_label, BoolOp, BooleanKind, Document, EndType, HoleType, Op, Ref};
use crate::expr::Kind;
use crate::icons;
use crate::theme::{self, ACCENT, BORDER, MUTED, TEXT};

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
            Accept::Curve => matches!(r, Ref::SketchCurve { .. } | Ref::Edge { .. } | Ref::Curve { .. }),
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
        Op::CircularPattern { kind, bodies, axis, .. } => vec![
            if kind == "feature" { ("Features", Features, Slot::Many(bodies)) } else { ("Bodies", Bodies, Slot::Many(bodies)) },
            ("Axis", Axis, Slot::One(axis)),
        ],
        Op::Mirror { kind, bodies, plane } => vec![
            if kind == "feature" { ("Features", Features, Slot::Many(bodies)) } else { ("Bodies", Bodies, Slot::Many(bodies)) },
            ("Mirror plane", Planar, Slot::One(plane)),
        ],
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
        Op::Boolean { target, tools, .. } => vec![("Target body", Bodies, Slot::One(target)), ("Tool bodies", Bodies, Slot::Many(tools))],
        Op::Split { target, plane } => vec![("Target body", Bodies, Slot::One(target)), ("Split plane", Planar, Slot::One(plane))],
        Op::DeleteBody { bodies } => vec![("Bodies", Bodies, Slot::Many(bodies))],
        Op::Sketch { .. } | Op::Import { .. } => vec![],
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

    let area = egui::Area::new(egui::Id::new("feature-dialog")).order(egui::Order::Middle);
    let area = if phone {
        area.pivot(egui::Align2::LEFT_BOTTOM).fixed_pos(egui::pos2(rect.left(), rect.bottom()))
    } else {
        area.fixed_pos(rect.min + egui::vec2(10.0, 10.0))
    };
    area.show(ui.ctx(), |ui| {
        egui::Frame::new()
            .fill(Color32::from_rgba_unmultiplied(0x1f, 0x24, 0x2b, 0xf7))
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(8)
            .inner_margin(Margin { left: 10, right: 10, top: 8, bottom: 10 })
            .shadow(egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(0x59) })
            .show(ui, |ui| {
                ui.set_width(if phone { rect.width() - 20.0 } else { 300.0 });
                ui.horizontal(|ui| {
                    ui.add(icons::image(kind, 18.0).tint(TEXT));
                    ui.label(RichText::new(&name).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::image(icons::image("close", 16.0).tint(TEXT))).on_hover_text("Cancel (Esc)").clicked() {
                            cancel = true;
                        }
                        if ui.add(egui::Button::image(icons::image("check", 16.0).tint(theme::OK_GREEN))).on_hover_text("OK (Enter)").clicked() {
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
                            ui.add_sized([96.0, 20.0], egui::Label::new(RichText::new(label).color(MUTED)));
                            let fr = egui::Frame::new()
                                .fill(theme::BG)
                                .stroke(Stroke::new(1.0, if is_active { ACCENT } else { BORDER }))
                                .corner_radius(5)
                                .inner_margin(Margin::symmetric(6, 3));
                            fr.show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                if refs.is_empty() {
                                    ui.label(RichText::new(format!("Select {}", acc.hint())).color(MUTED).italics().size(12.0));
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
                        ui.add_sized([96.0, 20.0], egui::Label::new(RichText::new(label).color(MUTED)));
                        let ok = crate::doc::eval(s, kind, &vars, units).is_ok();
                        let mut te = egui::TextEdit::singleline(s).desired_width(ui.available_width());
                        if !ok {
                            te = te.text_color(theme::ERROR_SOFT);
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
                        ui.add_sized([96.0, 20.0], egui::Label::new(RichText::new(label).color(MUTED)));
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
                        combo(ui, "Type", &mut m, &[
                            ("offset".to_string(), "Offset"),
                            ("angle".to_string(), "Angle"),
                            ("threePoint".to_string(), "Three points"),
                            ("normalToCurve".to_string(), "Normal to curve"),
                            ("midPlane".to_string(), "Mid plane"),
                        ], &mut changed);
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
                        combo(ui, "Hole type", hole_type, &[(HoleType::Simple, "Simple"), (HoleType::Counterbore, "Counterbore"), (HoleType::Countersink, "Countersink")], &mut changed);
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
                    ui.label(RichText::new(e).color(theme::ERROR_SOFT));
                }
                ui.add_space(4.0);
                ui.label(RichText::new(format!("{} · Enter to accept, Esc to cancel", hint_for(kind))).color(MUTED).size(12.0));
                if changed {
                    if let Some(f) = app.doc.feature_mut(fid) {
                        f.op = op.clone();
                    }
                    app.changed();
                }
            });
    });
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
