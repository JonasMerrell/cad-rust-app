//! Notes pinned to model faces, and the pencil: draw on the model and send the drawing to
//! Claude. Both live in the Part Studio viewport.
//!
//! Notes are saved with the document (`Document::notes`) and Claude sees them with every
//! request. Pencil strokes are view state only: they stick to the model while the view turns,
//! and "Send to Claude" puts a picture of the viewport with the strokes, and the faces they
//! cross, on the chat's next request.

use egui::{Color32, Pos2, RichText, Stroke};
use glam::{DVec3, Vec2};

use crate::app::App;
use crate::doc::{BodyId, Note, TopoName};

const PIN: f32 = 9.0;
/// Pins stand off their point by this much, so the point itself stays visible.
const STEM: egui::Vec2 = egui::vec2(12.0, -16.0);

pub fn pin_color() -> Color32 {
    Color32::from_rgb(0xf5, 0x9e, 0x0b)
}

#[derive(Default)]
pub struct Notes {
    /// Clicking a face adds a note there.
    pub adding: bool,
    pub edit: Option<Edit>,
    /// The pin under the cursor.
    pub hover: Option<u64>,
    /// Where each pin is drawn (`Note::at`, or the same face after a rebuild moved it), by
    /// notes and model.
    cache: std::cell::RefCell<Option<(u64, Vec<(u64, DVec3)>)>>,
}

pub struct Edit {
    /// None while adding a note.
    pub id: Option<u64>,
    pub at: [f64; 3],
    pub normal: [f64; 3],
    pub face: TopoName,
    pub text: String,
    focus: bool,
}

pub fn toggle_adding(app: &mut App) {
    app.notes.adding = !app.notes.adding;
    if app.notes.adding {
        app.measure = None;
        app.pencil = None;
        app.flash_info("Click a face to pin a note to it", Some(std::time::Duration::from_secs(3)));
    }
    app.scene_dirty = true;
}

/// The persistent name of the face at a picked point, when the kernel gave it one.
fn face_name_at(app: &App, p: DVec3) -> TopoName {
    let hit = app.result.bodies.iter().find_map(|b| {
        b.indices.chunks(3).enumerate().find_map(|(ti, tri)| {
            let v = |i: u32| DVec3::new(b.positions[3 * i as usize] as f64, b.positions[3 * i as usize + 1] as f64, b.positions[3 * i as usize + 2] as f64);
            (crate::kernel::point_triangle_distance(p, v(tri[0]), v(tri[1]), v(tri[2])) < 1e-4 * (1.0 + p.length())).then(|| b.faces[b.tri_face[ti] as usize].name.clone())
        })
    });
    TopoName(hit.flatten().into_iter().collect())
}

/// A click on the model while adding notes: start a note on the face that was hit.
pub fn click(app: &mut App, hit: Option<crate::doc::Ref>) {
    // a note being written is kept when the next one starts
    if let Some(e) = app.notes.edit.take() {
        save(app, e);
    }
    if let Some(crate::doc::Ref::Face { point, normal, .. }) = hit {
        let face = face_name_at(app, DVec3::from(point));
        app.notes.edit = Some(Edit { id: None, at: point, normal, face, text: String::new(), focus: true });
    } else if hit.is_some() {
        app.flash_info("Notes go on faces: click a face", Some(std::time::Duration::from_secs(2)));
    } else {
        app.notes.edit = None;
    }
}

pub fn open(app: &mut App, id: u64) {
    if let Some(e) = app.notes.edit.take().filter(|e| e.id != Some(id)) {
        save(app, e);
    }
    if let Some(n) = app.doc.notes.iter().find(|n| n.id == id) {
        app.notes.edit = Some(Edit { id: Some(id), at: n.at, normal: n.normal, face: n.face.clone(), text: n.text.clone(), focus: true });
    }
}

fn save(app: &mut App, e: Edit) {
    let text = e.text.trim().to_string();
    match e.id {
        Some(id) => {
            let Some(i) = app.doc.notes.iter().position(|n| n.id == id) else { return };
            if text.is_empty() {
                return delete(app, id);
            }
            if app.doc.notes[i].text == text {
                return;
            }
            app.checkpoint();
            app.doc.notes[i].text = text;
        }
        None => {
            if text.is_empty() {
                return;
            }
            app.checkpoint();
            let id = app.doc.notes.iter().map(|n| n.id).max().unwrap_or(0) + 1;
            app.doc.notes.push(Note { id, text, at: e.at, normal: e.normal, face: e.face });
        }
    }
    app.session.poke = true;
}

fn delete(app: &mut App, id: u64) {
    app.checkpoint();
    app.doc.notes.retain(|n| n.id != id);
    app.session.poke = true;
}

/// A cheap fingerprint of the built model, to notice rebuilds.
fn model_key(app: &App) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for b in &app.result.bodies {
        b.id.hash(&mut h);
        b.indices.len().hash(&mut h);
        b.volume.to_bits().hash(&mut h);
        for c in b.bbox.iter().flatten() {
            c.to_bits().hash(&mut h);
        }
    }
    serde_json::to_string(&app.doc.notes).unwrap_or_default().hash(&mut h);
    h.finish()
}

/// Where each note's pin goes: its own point while that is still on the model; else, when the
/// face it was on still exists (by name), the nearest spot on that face.
pub fn pin_points(app: &App) -> Vec<(u64, DVec3)> {
    if app.doc.notes.is_empty() {
        return vec![];
    }
    let key = model_key(app);
    if let Some((k, v)) = &*app.notes.cache.borrow() {
        if *k == key {
            return v.clone();
        }
    }
    let v: Vec<(u64, DVec3)> = app.doc.notes.iter().map(|n| (n.id, follow(app, n))).collect();
    *app.notes.cache.borrow_mut() = Some((key, v.clone()));
    v
}

fn follow(app: &App, n: &Note) -> DVec3 {
    let at = DVec3::from(n.at);
    let Some(name) = n.face.0.first() else { return at };
    let tol = 1e-4 * (1.0 + at.length());
    let mut best: Option<(f64, DVec3)> = None;
    for b in &app.result.bodies {
        for (ti, tri) in b.indices.chunks(3).enumerate() {
            if b.faces[b.tri_face[ti] as usize].name.as_ref() != Some(name) {
                continue;
            }
            let v = |i: u32| DVec3::new(b.positions[3 * i as usize] as f64, b.positions[3 * i as usize + 1] as f64, b.positions[3 * i as usize + 2] as f64);
            let (a, bb, c) = (v(tri[0]), v(tri[1]), v(tri[2]));
            if crate::kernel::point_triangle_distance(at, a, bb, c) < tol {
                return at;
            }
            let m = (a + bb + c) / 3.0;
            let d = (m - at).length();
            if best.is_none_or(|x| d < x.0) {
                best = Some((d, m));
            }
        }
    }
    best.map(|b| b.1).unwrap_or(at)
}

fn to_screen(app: &App, rect: egui::Rect, p: DVec3) -> Pos2 {
    let v = app.camera.project(p.as_vec3());
    Pos2::new(rect.min.x + v.x, rect.min.y + v.y)
}

/// The pin under screen point `p`.
pub fn pin_under(app: &App, rect: egui::Rect, p: Pos2) -> Option<u64> {
    pin_points(app).into_iter().rev().find(|(_, w)| (to_screen(app, rect, *w) + STEM - p).length() <= PIN + 2.0).map(|(id, _)| id)
}

/// Draw the pins (dimmed behind the model), the hovered note's text and the note being edited.
pub fn paint(app: &mut App, ui: &egui::Ui, painter: &egui::Painter, rect: egui::Rect) {
    let pins = pin_points(app);
    let editing = app.notes.edit.as_ref().and_then(|e| e.id);
    for (i, (id, w)) in pins.iter().enumerate() {
        let s = to_screen(app, rect, *w);
        if !rect.contains(s) {
            continue;
        }
        let hidden = !app.xray && crate::viewport::occluded(app, *w);
        let a = if hidden { 0.35 } else { 1.0 };
        let hot = app.notes.hover == Some(*id) || editing == Some(*id);
        let col = pin_color().gamma_multiply(a);
        let c = s + STEM;
        painter.line_segment([s, c], Stroke::new(1.5, col));
        painter.circle_filled(s, 2.5, col);
        painter.circle(c, if hot { PIN + 1.5 } else { PIN }, col, Stroke::new(1.5, Color32::WHITE.gamma_multiply(a)));
        painter.text(c, egui::Align2::CENTER_CENTER, format!("{}", i + 1), egui::FontId::proportional(11.0), Color32::WHITE.gamma_multiply(a));
    }
    // the hovered note's text
    if let Some(id) = app.notes.hover.filter(|id| editing != Some(*id)) {
        if let (Some(n), Some((_, w))) = (app.doc.notes.iter().find(|n| n.id == id), pins.iter().find(|p| p.0 == id)) {
            let c = to_screen(app, rect, *w) + STEM + egui::vec2(PIN + 6.0, -8.0);
            let g = painter.layout(n.text.clone(), egui::FontId::proportional(12.5), crate::theme::text(), 260.0);
            let r = egui::Rect::from_min_size(c, g.size() + egui::vec2(14.0, 8.0));
            painter.rect(r, 6.0, crate::theme::label_bg(), Stroke::new(1.0, pin_color()), egui::StrokeKind::Inside);
            painter.galley(r.min + egui::vec2(7.0, 4.0), g, crate::theme::text());
        }
    }
    // the new note's point
    if let Some(e) = app.notes.edit.as_ref().filter(|e| e.id.is_none()) {
        let s = to_screen(app, rect, DVec3::from(e.at));
        painter.line_segment([s, s + STEM], Stroke::new(1.5, pin_color()));
        painter.circle(s + STEM, PIN, pin_color(), Stroke::new(1.5, Color32::WHITE));
        painter.text(s + STEM, egui::Align2::CENTER_CENTER, "+", egui::FontId::proportional(13.0), Color32::WHITE);
    }
    editor(app, ui, rect);
    if app.notes.adding {
        list(app, ui, rect);
    }
}

/// The note being written or edited, beside its pin.
fn editor(app: &mut App, ui: &egui::Ui, rect: egui::Rect) {
    let Some(e) = &app.notes.edit else { return };
    let at = match e.id {
        Some(id) => pin_points(app).into_iter().find(|p| p.0 == id).map(|p| p.1).unwrap_or(DVec3::from(e.at)),
        None => DVec3::from(e.at),
    };
    let s = to_screen(app, rect, at) + STEM + egui::vec2(PIN + 8.0, -12.0);
    let pos = Pos2::new(s.x.min(rect.right() - 290.0).max(rect.left() + 4.0), s.y.min(rect.bottom() - 150.0).max(rect.top() + 4.0));
    let number = e.id.and_then(|id| app.doc.notes.iter().position(|n| n.id == id)).map(|i| i + 1);
    let pal = crate::theme::pal();
    let (mut done, mut cancel, mut del) = (false, false, false);
    let edit = app.notes.edit.as_mut().unwrap();
    egui::Area::new(egui::Id::new("note-editor")).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
        egui::Frame::new().fill(pal.window).stroke(Stroke::new(1.0, pin_color())).corner_radius(8).shadow(ui.visuals().popup_shadow).inner_margin(egui::Margin::same(8)).show(
            ui,
            |ui| {
                ui.set_width(270.0);
                ui.horizontal(|ui| {
                    ui.add(crate::icons::image("note", 14.0).tint(pin_color()));
                    let title = match number {
                        Some(n) => format!("Note {n}"),
                        None => "New note".into(),
                    };
                    ui.label(RichText::new(title).size(12.5).strong().color(pal.text));
                });
                // before the text box, which takes Enter for itself
                let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter) && i.modifiers.command, i.key_pressed(egui::Key::Escape)));
                done |= enter;
                cancel |= esc;
                let r =
                    ui.add(egui::TextEdit::multiline(&mut edit.text).desired_rows(3).desired_width(f32::INFINITY).hint_text("What about this face? Claude sees your notes too."));
                if std::mem::take(&mut edit.focus) {
                    r.request_focus();
                }
                ui.horizontal(|ui| {
                    done |= ui.button("Save").on_hover_text("Ctrl+Enter").clicked();
                    cancel |= ui.button("Cancel").clicked();
                    if edit.id.is_some() {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            del = ui.add(egui::Button::image(crate::icons::image("delete", 14.0).tint(pal.error))).on_hover_text("Delete the note").clicked();
                        });
                    }
                });
            },
        );
    });
    if del {
        let id = app.notes.edit.take().and_then(|e| e.id);
        if let Some(id) = id {
            delete(app, id);
        }
    } else if done {
        let e = app.notes.edit.take().unwrap();
        save(app, e);
    } else if cancel {
        app.notes.edit = None;
    }
}

/// While adding notes: every note of the Part Studio, bottom left of the viewport.
fn list(app: &mut App, ui: &egui::Ui, rect: egui::Rect) {
    let pal = crate::theme::pal();
    let mut open_id = None;
    let mut close = false;
    egui::Area::new(egui::Id::new("notes-list")).order(egui::Order::Middle).pivot(egui::Align2::LEFT_BOTTOM).fixed_pos(Pos2::new(rect.left() + 10.0, rect.bottom() - 44.0)).show(
        ui.ctx(),
        |ui| {
            egui::Frame::new().fill(pal.panel).stroke(Stroke::new(1.0, pal.border)).corner_radius(8).shadow(ui.visuals().popup_shadow).inner_margin(egui::Margin::same(8)).show(
                ui,
                |ui| {
                    ui.set_width(250.0);
                    ui.horizontal(|ui| {
                        ui.add(crate::icons::image("note", 14.0).tint(pin_color()));
                        ui.label(RichText::new("Notes").size(13.0).strong().color(pal.text));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close = ui.add(egui::Button::image(crate::icons::image("close", 13.0).tint(pal.text)).frame(false)).on_hover_text("Done adding notes (Esc)").clicked();
                        });
                    });
                    if app.doc.notes.is_empty() {
                        ui.label(RichText::new("Click a face to pin a note to it.").size(12.0).color(pal.muted));
                    } else {
                        ui.label(RichText::new("Click a face to add one, a pin or a row to edit.").size(11.5).color(pal.muted));
                        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                            for (i, n) in app.doc.notes.iter().enumerate() {
                                let first = n.text.lines().next().unwrap_or("");
                                let r = ui.add(egui::Button::new(RichText::new(format!("{}  {first}", i + 1)).size(12.0)).truncate().frame(false).min_size(egui::vec2(234.0, 0.0)));
                                if r.on_hover_text(&n.text).clicked() {
                                    open_id = Some(n.id);
                                }
                            }
                        });
                    }
                },
            );
        },
    );
    if close {
        app.notes.adding = false;
        app.notes.edit = None;
    }
    if let Some(id) = open_id {
        open(app, id);
    }
}

/// The notes as Claude is told them with each request.
pub fn context(app: &App) -> Option<String> {
    if app.doc.notes.is_empty() {
        return None;
    }
    let pins = pin_points(app);
    let mut out = vec!["Notes the user pinned to faces of this Part Studio (numbered as the pins show; the face is a face pick you can pass to tools):".to_string()];
    for (i, n) in app.doc.notes.iter().enumerate() {
        let at = pins.iter().find(|p| p.0 == n.id).map(|p| p.1.to_array()).unwrap_or(n.at);
        let r = crate::doc::Ref::Face { point: at, normal: n.normal, name: n.face.clone() };
        out.push(format!("{}. \"{}\" on face {}", i + 1, n.text.replace('\n', " / "), serde_json::to_string(&r).unwrap_or_default()));
    }
    Some(out.join("\n"))
}

// ---------------------------------------------------------------- pencil

pub const INKS: [(Color32, &str); 4] = [
    (Color32::from_rgb(0xe5, 0x39, 0x35), "Red"),
    (Color32::from_rgb(0x1e, 0x88, 0xe5), "Blue"),
    (Color32::from_rgb(0x2e, 0x9d, 0x4a), "Green"),
    (Color32::from_rgb(0x22, 0x22, 0x22), "Black"),
];

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Capture {
    #[default]
    Idle,
    /// Hide the pencil's bar this frame, and take the screenshot of it.
    Hide,
    Waiting,
}

#[derive(Default)]
pub struct Pencil {
    pub strokes: Vec<PencilStroke>,
    cur: Option<PencilStroke>,
    pub ink: usize,
    capture: Capture,
    /// Depth along the view ray of the last point that hit the model: strokes off the model
    /// stay at that depth.
    depth: Option<f64>,
}

#[derive(Clone)]
pub struct PencilStroke {
    pub pts: Vec<DVec3>,
    pub color: Color32,
    /// Faces the stroke crosses: (body, face index, a point on it).
    pub faces: Vec<(BodyId, usize, DVec3)>,
}

/// The viewport is being photographed for Claude: controls over it stay hidden.
pub fn capturing(app: &App) -> bool {
    app.pencil.as_ref().is_some_and(|p| p.capture != Capture::Idle)
}

pub fn toggle_pencil(app: &mut App) {
    if app.pencil.is_some() {
        app.pencil = None;
    } else if app.sketch.is_none() {
        app.pencil = Some(Pencil::default());
        app.measure = None;
        app.notes.adding = false;
        app.notes.edit = None;
        app.hover = None;
    }
    app.scene_dirty = true;
}

/// Ctrl+Z while drawing: the last stroke goes. False when there was none.
pub fn undo_stroke(app: &mut App) -> bool {
    app.pencil.as_mut().is_some_and(|p| p.strokes.pop().is_some())
}

/// Drawing with the primary button (the other buttons still turn and pan the view).
pub fn input(app: &mut App, ui: &egui::Ui, resp: &egui::Response, rect: egui::Rect) {
    let Some(pen) = &app.pencil else { return };
    if pen.capture != Capture::Idle {
        return;
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    let down = ui.input(|i| i.pointer.primary_down());
    let pos = ui.input(|i| i.pointer.latest_pos());
    if resp.drag_started_by(egui::PointerButton::Primary) || (resp.clicked_by(egui::PointerButton::Primary) && pen.cur.is_none()) {
        let color = INKS[pen.ink.min(INKS.len() - 1)].0;
        app.pencil.as_mut().unwrap().cur = Some(PencilStroke { pts: vec![], color, faces: vec![] });
        // from where the button went down: a drag starts a few pixels later
        if let Some(p) = ui.input(|i| i.pointer.press_origin()).or(pos) {
            add_point(app, rect, p);
        }
    }
    if app.pencil.as_ref().is_some_and(|p| p.cur.is_some()) {
        if let Some(p) = pos.filter(|_| down) {
            add_point(app, rect, p);
        }
        if !down {
            let pen = app.pencil.as_mut().unwrap();
            if let Some(s) = pen.cur.take().filter(|s| !s.pts.is_empty()) {
                pen.strokes.push(s);
            }
        }
    }
}

fn add_point(app: &mut App, rect: egui::Rect, p: Pos2) {
    let s = Vec2::new(p.x - rect.min.x, p.y - rect.min.y);
    let cam = &app.camera;
    if let Some(last) = app.pencil.as_ref().and_then(|pen| pen.cur.as_ref()).and_then(|c| c.pts.last()) {
        if (cam.project(last.as_vec3()) - s).length() < 1.5 {
            return;
        }
    }
    let (o, d) = cam.ray(s);
    let (o, d) = (o.as_dvec3(), d.as_dvec3().normalize());
    let hit = crate::viewport::face_hit(app, s);
    let pen = app.pencil.as_mut().unwrap();
    let w = match hit {
        Some((t, body, face)) => {
            pen.depth = Some(t);
            let w = o + d * t;
            let cur = pen.cur.as_mut().unwrap();
            if !cur.faces.iter().any(|f| f.0 == body && f.1 == face) {
                cur.faces.push((body, face, w));
            }
            w
        }
        None => {
            let t = pen.depth.unwrap_or_else(|| (app.camera.target.as_dvec3() - o).dot(d));
            o + d * t
        }
    };
    pen.cur.as_mut().unwrap().pts.push(w);
}

/// The strokes, and the pencil's bar at the top of the viewport.
pub fn paint_pencil(app: &mut App, ui: &egui::Ui, painter: &egui::Painter, rect: egui::Rect) {
    let Some(pen) = &app.pencil else { return };
    let cam = &app.camera;
    let screen = |p: &DVec3| {
        let v = cam.project(p.as_vec3());
        Pos2::new(rect.min.x + v.x, rect.min.y + v.y)
    };
    for s in pen.strokes.iter().chain(pen.cur.iter()) {
        let pts: Vec<Pos2> = s.pts.iter().map(screen).collect();
        if pts.len() == 1 {
            painter.circle_filled(pts[0], 2.0, s.color);
        } else {
            painter.add(egui::Shape::line(pts, Stroke::new(3.0, s.color)));
        }
    }
    match pen.capture {
        Capture::Idle => bar(app, ui, rect),
        Capture::Hide => {
            // this frame has no bar: take it
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new("pencil")));
            app.pencil.as_mut().unwrap().capture = Capture::Waiting;
            ui.ctx().request_repaint();
        }
        Capture::Waiting => ui.ctx().request_repaint(),
    }
}

fn bar(app: &mut App, ui: &egui::Ui, rect: egui::Rect) {
    let pal = crate::theme::pal();
    let top = rect.top() + if app.isolation.is_some() { 46.0 } else { 10.0 };
    let (mut undo, mut clear, mut send, mut close) = (false, false, false, false);
    let n = app.pencil.as_ref().map(|p| p.strokes.len()).unwrap_or(0);
    let mut ink = app.pencil.as_ref().map(|p| p.ink).unwrap_or(0);
    egui::Area::new(egui::Id::new("pencil-bar")).order(egui::Order::Middle).pivot(egui::Align2::CENTER_TOP).fixed_pos(Pos2::new(rect.center().x, top)).show(ui.ctx(), |ui| {
        egui::Frame::new()
            .fill(pal.panel)
            .stroke(Stroke::new(1.0, pal.active_stroke))
            .corner_radius(8)
            .shadow(ui.visuals().popup_shadow)
            .inner_margin(egui::Margin::symmetric(10, 5))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(crate::icons::image("pencil", 15.0).tint(pal.active_text));
                    ui.label(RichText::new(if n == 0 { "Draw on the part" } else { "Pencil" }).size(12.5).color(pal.text));
                    ui.add_space(4.0);
                    for (i, (c, name)) in INKS.iter().enumerate() {
                        let (r, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
                        let on = i == ink;
                        ui.painter().circle(r.center(), if on { 7.5 } else { 6.0 }, *c, Stroke::new(if on { 2.0 } else { 1.0 }, if on { pal.text } else { pal.border }));
                        if resp.on_hover_text(*name).clicked() {
                            ink = i;
                        }
                    }
                    ui.add_space(4.0);
                    undo = ui.add_enabled(n > 0, egui::Button::image(crate::icons::image("undo", 15.0).tint(pal.text))).on_hover_text("Undo the last stroke (Ctrl+Z)").clicked();
                    clear = ui.add_enabled(n > 0, egui::Button::image(crate::icons::image("delete", 15.0).tint(pal.text))).on_hover_text("Clear the drawing").clicked();
                    send = ui
                        .add_enabled(n > 0, egui::Button::image_and_text(crate::icons::image("chat", 15.0).tint(pal.active_text), RichText::new("Send to Claude").size(12.5)))
                        .on_hover_text("Attach a picture of the view with your drawing, and the faces it crosses, to your next Claude message")
                        .clicked();
                    close = ui.add(egui::Button::image(crate::icons::image("close", 14.0).tint(pal.text)).frame(false)).on_hover_text("Put the pencil away (Esc)").clicked();
                });
            });
    });
    let Some(pen) = app.pencil.as_mut() else { return };
    pen.ink = ink;
    if undo {
        pen.strokes.pop();
    }
    if clear {
        pen.strokes.clear();
    }
    if send {
        pen.capture = Capture::Hide;
        ui.ctx().request_repaint();
    }
    if close {
        app.pencil = None;
    }
}

/// The screenshot for "Send to Claude" arrived: hand it, cropped to the viewport, to the chat.
pub fn handle_capture(app: &mut App, ctx: &egui::Context) {
    if app.pencil.as_ref().map(|p| p.capture) != Some(Capture::Waiting) {
        return;
    }
    let img = ctx.input(|i| {
        i.raw.events.iter().find_map(|e| match e {
            egui::Event::Screenshot { image, user_data, .. } if user_data.data.as_ref().and_then(|d| d.downcast_ref::<&str>()) == Some(&"pencil") => Some(image.clone()),
            _ => None,
        })
    });
    let Some(img) = img else { return };
    let region = img.region(&app.viewport_rect, Some(ctx.pixels_per_point()));
    let (w, h) = (region.size[0] as u32, region.size[1] as u32);
    let bytes: Vec<u8> = region.pixels.iter().flat_map(|c| c.to_array()).collect();
    let Some(mut buf) = image::RgbaImage::from_raw(w, h, bytes) else { return };
    // the API scales larger pictures down anyway
    let long = w.max(h);
    if long > 1568 {
        let k = 1568.0 / long as f32;
        buf = image::imageops::resize(&buf, ((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1), image::imageops::FilterType::Triangle);
    }
    let mut png = vec![];
    if buf.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).is_err() {
        return;
    }
    let pen = app.pencil.take().unwrap();
    let text = describe(app, &pen);
    crate::claude_ui::attach_sketch(app, ctx, png, text);
}

/// What the drawing covers, in words and face picks.
fn describe(app: &App, pen: &Pencil) -> String {
    let mut lines = vec![format!(
        "The user drew {} pencil stroke{} on the viewport (attached picture: the view as they see it, strokes in {}).",
        pen.strokes.len(),
        if pen.strokes.len() == 1 { "" } else { "s" },
        {
            let mut cs: Vec<&str> = vec![];
            for s in &pen.strokes {
                if let Some((_, n)) = INKS.iter().find(|i| i.0 == s.color) {
                    if !cs.contains(n) {
                        cs.push(n);
                    }
                }
            }
            cs.join(", ").to_lowercase()
        }
    )];
    let mut seen: Vec<(BodyId, usize)> = vec![];
    for (k, s) in pen.strokes.iter().enumerate() {
        let ink = INKS.iter().find(|i| i.0 == s.color).map(|i| i.1.to_lowercase()).unwrap_or_default();
        if s.faces.is_empty() {
            lines.push(format!("Stroke {} ({ink}) is off the model.", k + 1));
            continue;
        }
        lines.push(format!("Stroke {} ({ink}) crosses these faces:", k + 1));
        for (body, fi, p) in &s.faces {
            let Some(b) = app.result.bodies.iter().find(|b| b.id == *body) else { continue };
            let Some(f) = b.faces.get(*fi) else { continue };
            let again = seen.contains(&(*body, *fi));
            seen.push((*body, *fi));
            let kind = match f.surf.as_ref().map(|s| s.0) {
                _ if f.planar => "planar".to_string(),
                Some(1) => format!("cylinder{}", f.surf.as_ref().and_then(|s| s.1).map(|a| format!(" r {:.3}", a.2)).unwrap_or_default()),
                Some(2) => "cone".into(),
                Some(3) => "sphere".into(),
                Some(4) => "torus".into(),
                _ => "curved".into(),
            };
            let part = app.doc.parts.get(&crate::doc::body_key(*body)).and_then(|p| p.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| b.name.clone());
            let r = crate::doc::Ref::Face { point: p.to_array(), normal: f.normal, name: TopoName(f.name.clone().into_iter().collect()) };
            lines.push(format!(
                "- {kind} face of \"{part}\", area {:.1} mm²{}: {}",
                f.area,
                if again { " (also crossed by an earlier stroke)" } else { "" },
                serde_json::to_string(&r).unwrap_or_default()
            ));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use crate::doc::{Document, FaceName, Note, TopoName};

    fn note() -> Note {
        Note { id: 1, text: "make this wall 3 mm".into(), at: [1.0, 2.0, 3.0], normal: [0.0, 0.0, 1.0], face: TopoName(vec![FaceName { feature: 4, role: "end".into(), n: 0 }]) }
    }

    #[test]
    fn notes_are_saved_with_the_document() {
        let mut d = Document::default();
        d.notes.push(note());
        let j = serde_json::to_string(&d).unwrap();
        let back: Document = serde_json::from_str(&j).unwrap();
        assert_eq!(back.notes, d.notes);
        // documents without notes don't mention them, and older ones still load
        let plain = serde_json::to_string(&Document::default()).unwrap();
        assert!(!plain.contains("notes"));
        assert!(serde_json::from_str::<Document>(&plain).unwrap().notes.is_empty());
    }

    #[test]
    fn notes_leave_the_build_cache_key_alone() {
        let plain = Document::default();
        let mut noted = plain.clone();
        noted.notes.push(note());
        assert_eq!(crate::build_cache::key(&plain, String::new), crate::build_cache::key(&noted, String::new));
    }

    #[test]
    fn a_note_is_a_face_pick() {
        let r = serde_json::to_value(note().face_ref()).unwrap();
        assert_eq!(r["kind"], "face");
        assert_eq!(r["name"][0]["role"], "end");
    }
}
