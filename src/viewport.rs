//! The 3D viewport: navigation, scene assembly, picking and model-mode overlays.

use std::sync::Arc;

use egui::{Color32, Pos2, Sense, Stroke, Ui};
use glam::{DVec2, DVec3, Vec2, Vec3};

use crate::app::{App, Measure};
use crate::doc::{Op, OriginPlane, Ref};
use crate::regions::find_regions;
use crate::render::{SceneData, ViewportCallback};
use crate::sketch::{Frame, Geom};
use crate::theme::{to_rgba, vp};

pub struct Nav {
    pub orbit: bool,
    pub moved: f32,
    pub last: Pos2,
    pub button: egui::PointerButton,
}

fn rgba(c: Color32, a: f32) -> [f32; 4] {
    to_rgba(c, a)
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    if app.viewport_rect != rect {
        app.viewport_rect = rect;
        app.scene_dirty = true;
    }
    app.camera.size = Vec2::new(rect.width().max(1.0), rect.height().max(1.0));
    ui.painter().rect_filled(rect, 0.0, vp::BACKGROUND);

    navigate(app, ui, &resp);
    let local = |p: Pos2| Vec2::new(p.x - rect.min.x, p.y - rect.min.y);

    // hover + clicks
    let pointer = resp.hover_pos();
    if app.sketch.is_some() {
        crate::sketcher::viewport_input(app, ui, &resp, rect);
    } else if app.nav.is_none() {
        let hit = pointer.and_then(|p| pick(app, local(p)));
        if hit != app.hover {
            app.hover = hit.clone();
            app.scene_dirty = true;
        }
        if resp.clicked_by(egui::PointerButton::Primary) {
            let add = ui.input(|i| i.modifiers.shift || i.modifiers.command);
            click(app, hit, add);
        }
    }
    if resp.secondary_clicked() {
        if let Some(p) = pointer {
            app.context_menu = Some((p, app.hover.clone()));
        }
    }

    if app.scene_dirty {
        app.scene_dirty = false;
        let mut s = build_scene(app);
        s.version = app.scene.version.wrapping_add(1);
        app.scene = Arc::new(s);
    }

    let ppp = ui.ctx().pixels_per_point();
    let clip = match app.section {
        Some(s) => {
            let n = [Vec3::Z, -Vec3::Y, Vec3::X][s.plane as usize];
            let (n, w) = if s.flip { (-n, -(s.offset as f32)) } else { (n, s.offset as f32) };
            [n.x, n.y, n.z, w]
        }
        None => [0.0; 4],
    };
    let cb = ViewportCallback {
        scene: app.scene.clone(),
        view_proj: app.camera.view_proj(),
        eye_dir: app.camera.back,
        clip,
        size_px: [rect.width() * ppp, rect.height() * ppp],
        pixels_per_point: ppp,
    };
    ui.painter().add(eframe::egui_wgpu::Callback::new_paint_callback(rect, cb));

    // 2D overlays
    let painter = ui.painter_at(rect);
    if let Some(s) = app.sketch.take() {
        crate::sketcher::overlay(app, &s, ui, &painter, rect);
        app.sketch = Some(s);
    } else {
        model_overlay(app, &painter, rect);
    }
    axis_triad(app, &painter, rect);
    context_menu(app, ui);
}

fn navigate(app: &mut App, ui: &Ui, resp: &egui::Response) {
    let rect = resp.rect;
    let (pos, scroll, pinch) = ui.input(|i| (i.pointer.hover_pos(), i.smooth_scroll_delta.y, i.zoom_delta()));
    if let Some(p) = pos {
        if rect.contains(p) {
            let local = Vec2::new(p.x - rect.min.x, p.y - rect.min.y);
            if scroll.abs() > 0.0 {
                app.camera.zoom_at(local, (scroll * 0.0035).exp());
                app.scene_dirty = true;
            }
            if (pinch - 1.0).abs() > 1e-4 {
                app.camera.zoom_at(local, pinch);
                app.scene_dirty = true;
            }
        }
    }
    let (down_sec, down_mid, shift, delta, latest) = ui.input(|i| {
        (
            i.pointer.button_down(egui::PointerButton::Secondary),
            i.pointer.button_down(egui::PointerButton::Middle),
            i.modifiers.shift,
            i.pointer.delta(),
            i.pointer.latest_pos(),
        )
    });
    if app.nav.is_none() && (resp.drag_started_by(egui::PointerButton::Secondary) || resp.drag_started_by(egui::PointerButton::Middle)) {
        let button = if down_mid { egui::PointerButton::Middle } else { egui::PointerButton::Secondary };
        app.nav = Some(Nav { orbit: button == egui::PointerButton::Secondary && !shift, moved: 0.0, last: latest.unwrap_or_default(), button });
    }
    if let Some(n) = &mut app.nav {
        let still = match n.button {
            egui::PointerButton::Middle => down_mid,
            _ => down_sec,
        };
        if !still {
            app.nav = None;
        } else if delta != egui::Vec2::ZERO {
            n.moved += delta.length();
            if n.orbit {
                app.camera.orbit(delta.x, delta.y);
            } else {
                app.camera.pan(delta.x, delta.y);
            }
            app.scene_dirty = true;
            ui.ctx().set_cursor_icon(if n.orbit { egui::CursorIcon::AllScroll } else { egui::CursorIcon::Grabbing });
        }
    }
}

// ---------------------------------------------------------------- picking

/// What is under the cursor in model mode.
pub fn pick(app: &App, s: Vec2) -> Option<Ref> {
    let cam = &app.camera;
    let px = 7.0;
    let to_screen = |p: DVec3| cam.project(p.as_vec3());
    let seg_px = |a: Vec2, b: Vec2| {
        let d = b - a;
        let t = if d.length_squared() < 1e-9 { 0.0 } else { ((s - a).dot(d) / d.length_squared()).clamp(0.0, 1.0) };
        (a + d * t - s).length()
    };
    let rolled = rolled_ids(app);
    // sketch points (for holes etc.)
    let mut best: Option<(f32, Ref)> = None;
    for f in &app.doc.features {
        if f.hidden || rolled.contains(&f.id) {
            continue;
        }
        if let Op::Sketch { sketch, frame, .. } = &f.op {
            for p in &sketch.points {
                let d = (to_screen(frame.to_world(p.v())) - s).length();
                if d < px && best.as_ref().map(|b| d < b.0).unwrap_or(true) {
                    best = Some((d, Ref::SketchPoint { sketch: f.id, point: p.id }));
                }
            }
        }
    }
    if let Some((_, r)) = best {
        return Some(r);
    }
    // model edges
    let mut best: Option<(f32, Ref)> = None;
    for b in &app.result.bodies {
        for e in &b.edges {
            let pts: Vec<Vec2> = e.pts.iter().map(|p| cam.project(Vec3::from(*p))).collect();
            for w in pts.windows(2) {
                let d = seg_px(w[0], w[1]);
                if d < px * 0.85 && best.as_ref().map(|b| d < b.0).unwrap_or(true) {
                    best = Some((d, Ref::Edge { point: e.mid }));
                }
            }
        }
    }
    if let Some((_, r)) = best {
        return Some(r);
    }
    // sketch curves
    let mut best: Option<(f32, Ref)> = None;
    for f in &app.doc.features {
        if f.hidden || rolled.contains(&f.id) {
            continue;
        }
        if let Op::Sketch { sketch, frame, .. } = &f.op {
            for c in &sketch.curves {
                let pts: Vec<Vec2> = sketch.tessellate(c, 2.0).iter().map(|p| to_screen(frame.to_world(*p))).collect();
                for w in pts.windows(2) {
                    let d = seg_px(w[0], w[1]);
                    if d < px * 0.7 && best.as_ref().map(|b| d < b.0).unwrap_or(true) {
                        best = Some((d, Ref::SketchCurve { sketch: f.id, curve: c.id }));
                    }
                }
            }
        }
    }
    if let Some((_, r)) = best {
        return Some(r);
    }
    // faces, regions and planes by depth along the view ray
    let (o, d) = cam.ray(s);
    let (o, d) = (o.as_dvec3(), d.as_dvec3());
    let mut best: Option<(f64, Ref)> = None;
    fn consider(best: &mut Option<(f64, Ref)>, t: f64, r: Ref) {
        if t > 0.0 && best.as_ref().map(|b| t < b.0).unwrap_or(true) {
            *best = Some((t, r));
        }
    }
    if !app.xray {
        for b in &app.result.bodies {
            for (ti, tri) in b.indices.chunks(3).enumerate() {
                let v = |i: u32| DVec3::new(b.positions[3 * i as usize] as f64, b.positions[3 * i as usize + 1] as f64, b.positions[3 * i as usize + 2] as f64);
                if let Some(t) = ray_tri(o, d, v(tri[0]), v(tri[1]), v(tri[2])) {
                    if section_keeps(app, o + d * t) {
                        let fi = b.tri_face[ti] as usize;
                        let fa = &b.faces[fi];
                        consider(&mut best, t, Ref::Face { point: (o + d * t).into(), normal: fa.normal });
                    }
                }
            }
        }
    }
    for f in &app.doc.features {
        if f.hidden || rolled.contains(&f.id) {
            continue;
        }
        if let Op::Sketch { sketch, frame, .. } = &f.op {
            if let Some((t, p)) = ray_plane(o, d, frame) {
                for r in find_regions(sketch) {
                    if r.contains(p) {
                        // regions sit on faces; prefer them on ties
                        consider(&mut best, t - 1e-3, Ref::Region { sketch: f.id, point: r.sample.into() });
                        break;
                    }
                }
            }
        }
    }
    // origin planes only when nothing else is under the cursor
    if best.is_some() {
        return best.map(|b| b.1);
    }
    let size = plane_size(app) as f64;
    for p in OriginPlane::ALL {
        let fr = p.frame();
        if let Some((t, l)) = ray_plane(o, d, &fr) {
            if l.x >= 0.0 && l.y >= 0.0 && l.x <= size && l.y <= size {
                consider(&mut best, t + 1e-3, Ref::OriginPlane { plane: p });
            }
        }
    }
    for f in &app.doc.features {
        if let (Op::Plane { .. }, Some(fr)) = (&f.op, app.result.frames.get(&f.id)) {
            if f.hidden || rolled.contains(&f.id) {
                continue;
            }
            if let Some((t, l)) = ray_plane(o, d, fr) {
                if l.x.abs() <= size / 2.0 && l.y.abs() <= size / 2.0 {
                    consider(&mut best, t + 1e-3, Ref::FeaturePlane { feature: f.id });
                }
            }
        }
    }
    best.map(|b| b.1)
}

fn section_keeps(app: &App, p: DVec3) -> bool {
    match app.section {
        None => true,
        Some(s) => {
            let n = [DVec3::Z, -DVec3::Y, DVec3::X][s.plane as usize];
            let (n, w) = if s.flip { (-n, -s.offset) } else { (n, s.offset) };
            w - n.dot(p) >= 0.0
        }
    }
}

pub fn ray_plane(o: DVec3, d: DVec3, fr: &Frame) -> Option<(f64, DVec2)> {
    let n = fr.normal();
    let den = d.dot(n);
    if den.abs() < 1e-9 {
        return None;
    }
    let t = (DVec3::from(fr.origin) - o).dot(n) / den;
    Some((t, fr.to_local(o + d * t)))
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

fn click(app: &mut App, hit: Option<Ref>, add: bool) {
    if let Some(m) = &mut app.measure {
        match hit {
            Some(r) => {
                if m.picks.len() >= 2 {
                    m.picks.clear();
                }
                m.picks.push(r)
            }
            None => m.picks.clear(),
        }
        app.scene_dirty = true;
        return;
    }
    if app.dialog.is_some() {
        if let Some(r) = hit {
            crate::dialog::add_pick(app, r);
        }
        return;
    }
    match hit {
        Some(r) => {
            if let Some(i) = app.picks.iter().position(|x| *x == r) {
                app.picks.remove(i);
            } else {
                if !add && !can_multi(&app.picks, &r) {
                    app.picks.clear();
                }
                app.picks.push(r);
            }
        }
        None => app.picks.clear(),
    }
    app.scene_dirty = true;
}

/// Clicking adds to the selection (like the original) while the kinds are compatible.
fn can_multi(picks: &[Ref], r: &Ref) -> bool {
    let kind = |r: &Ref| std::mem::discriminant(r);
    picks.iter().all(|p| kind(p) == kind(r))
}

pub fn rolled_ids(app: &App) -> std::collections::HashSet<u64> {
    let rb = app.doc.rollback_index();
    app.doc.features.iter().enumerate().filter(|(i, f)| *i >= rb || f.suppressed).map(|(_, f)| f.id).collect()
}

pub fn plane_size(app: &App) -> f32 {
    let (lo, hi) = app.model_bounds();
    (((hi - lo).max_element() as f32) * 0.6).clamp(100.0, 1e5)
}

// ---------------------------------------------------------------- scene

fn is_picked(app: &App, r: &Ref) -> bool {
    app.picks.contains(r)
        || app.measure.as_ref().map(|m| m.picks.contains(r)).unwrap_or(false)
        || crate::dialog::field_refs(app).iter().any(|x| x == r)
}

fn build_scene(app: &App) -> SceneData {
    let mut s = SceneData::default();
    let in_sketch = app.sketch.as_ref().map(|k| k.feature);
    let rolled = rolled_ids(app);
    let size = plane_size(app);

    // origin planes and axes
    for p in OriginPlane::ALL {
        let r = Ref::OriginPlane { plane: p };
        let fr = p.frame();
        let hov = app.hover.as_ref() == Some(&r);
        let picked = is_picked(app, &r);
        if in_sketch.is_some() && !picked {
            continue;
        }
        let col = if picked { vp::PICKED } else if hov { vp::HOVER } else { vp::PLANE };
        let q = |x: f32, y: f32| (fr.to_world(DVec2::new(x as f64, y as f64))).as_vec3();
        let quad = [q(0.0, 0.0), q(size, 0.0), q(size, size), q(0.0, size)];
        s.quad(true, quad, rgba(col, if picked || hov { 0.22 } else { 0.07 }));
        let mut ring = quad.to_vec();
        ring.push(quad[0]);
        s.polyline(&ring, 1.2, rgba(col, if picked || hov { 0.95 } else { 0.55 }), false);
    }
    let al = size * 0.45;
    s.line(Vec3::ZERO, Vec3::X * al, 1.6, rgba(vp::AXIS_X, 0.8));
    s.line(Vec3::ZERO, Vec3::Y * al, 1.6, rgba(vp::AXIS_Y, 0.8));
    s.line(Vec3::ZERO, Vec3::Z * al, 1.6, rgba(vp::AXIS_Z, 0.8));

    // plane features
    for f in &app.doc.features {
        if let (Op::Plane { .. }, Some(fr)) = (&f.op, app.result.frames.get(&f.id)) {
            if f.hidden || rolled.contains(&f.id) {
                continue;
            }
            let r = Ref::FeaturePlane { feature: f.id };
            let hov = app.hover.as_ref() == Some(&r);
            let picked = is_picked(app, &r) || app.dialog.as_ref().map(|d| d.feature == f.id).unwrap_or(false);
            let col = if picked { vp::PICKED } else if hov { vp::HOVER } else { vp::PLANE };
            let h = (size / 2.0) as f64;
            let q = |x: f64, y: f64| fr.to_world(DVec2::new(x, y)).as_vec3();
            let quad = [q(-h, -h), q(h, -h), q(h, h), q(-h, h)];
            s.quad(true, quad, rgba(col, if picked || hov { 0.22 } else { 0.08 }));
            let mut ring = quad.to_vec();
            ring.push(quad[0]);
            s.polyline(&ring, 1.2, rgba(col, 0.7), false);
        }
    }

    // bodies
    let hover_face = match &app.hover {
        Some(Ref::Face { point, .. }) => Some(DVec3::from(*point)),
        _ => None,
    };
    let picked_faces: Vec<DVec3> = app
        .picks
        .iter()
        .chain(app.measure.iter().flat_map(|m| m.picks.iter()))
        .chain(crate::dialog::field_refs(app).iter())
        .filter_map(|r| if let Ref::Face { point, .. } = r { Some(DVec3::from(*point)) } else { None })
        .collect();
    let body_alpha = if app.xray || in_sketch.is_some() { 0.35 } else { 1.0 };
    for b in &app.result.bodies {
        let body_picked = is_picked(app, &Ref::Body { body: b.id });
        // which faces are highlighted
        let face_of = |p: DVec3| -> Option<usize> {
            let mut best = (f64::INFINITY, None);
            for (ti, tri) in b.indices.chunks(3).enumerate() {
                let v = |i: u32| DVec3::new(b.positions[3 * i as usize] as f64, b.positions[3 * i as usize + 1] as f64, b.positions[3 * i as usize + 2] as f64);
                let d = crate::kernel::point_triangle_distance(p, v(tri[0]), v(tri[1]), v(tri[2]));
                if d < best.0 {
                    best = (d, Some(b.tri_face[ti] as usize));
                }
            }
            if best.0 < 1e-3 * (1.0 + p.length()) { best.1 } else { None }
        };
        let hov_f = hover_face.and_then(face_of);
        let picked_f: Vec<usize> = picked_faces.iter().filter_map(|p| face_of(*p)).collect();
        let transparent = body_alpha < 1.0;
        let list_len = if transparent { s.transparent.len() } else { s.opaque.len() };
        let _ = list_len;
        for (ti, tri) in b.indices.chunks(3).enumerate() {
            let fi = b.tri_face[ti] as usize;
            let col = if body_picked || picked_f.contains(&fi) {
                vp::PICKED
            } else if hov_f == Some(fi) {
                vp::HOVER
            } else {
                vp::BODY
            };
            let list = if transparent { &mut s.transparent } else { &mut s.opaque };
            for &i in tri {
                let i = i as usize;
                list.push(crate::render::MeshVertex {
                    pos: [b.positions[3 * i], b.positions[3 * i + 1], b.positions[3 * i + 2]],
                    normal: [b.normals[3 * i], b.normals[3 * i + 1], b.normals[3 * i + 2]],
                    color: rgba(col, body_alpha),
                });
            }
        }
        for e in &b.edges {
            let r = Ref::Edge { point: e.mid };
            let hov = app.hover.as_ref() == Some(&r);
            let picked = is_picked(app, &r);
            let (col, w) = if picked {
                (rgba(vp::PICKED, 1.0), 3.0)
            } else if hov {
                (rgba(vp::HOVER, 1.0), 3.0)
            } else {
                (rgba(vp::EDGE, 1.0), 1.4)
            };
            let pts: Vec<Vec3> = e.pts.iter().map(|p| Vec3::from(*p)).collect();
            s.polyline(&pts, w, col, picked || hov || app.xray);
        }
    }

    // sketches (the one being edited is drawn by the sketcher)
    for f in &app.doc.features {
        if Some(f.id) == in_sketch || f.hidden || rolled.contains(&f.id) {
            continue;
        }
        if let Op::Sketch { sketch, frame, .. } = &f.op {
            let sk_picked = is_picked(app, &Ref::FeaturePlane { feature: f.id });
            for r in find_regions(sketch) {
                let rr = Ref::Region { sketch: f.id, point: r.sample.into() };
                let hov = app.hover.as_ref().map(|h| matches!(h, Ref::Region { sketch, point } if *sketch == f.id && r.contains(DVec2::from(*point)))).unwrap_or(false);
                let picked = is_picked(app, &rr)
                    || app.picks.iter().chain(crate::dialog::field_refs(app).iter()).any(|x| matches!(x, Ref::Region { sketch, point } if *sketch == f.id && r.contains(DVec2::from(*point))));
                let (col, a) = if picked { (vp::PICKED, 0.45) } else if hov { (vp::HOVER, 0.35) } else { (vp::SKETCH, 0.10) };
                // lift slightly towards the viewer so regions on faces stay visible
                let lift = frame.normal().as_vec3() * 0.01;
                for t in &r.tris {
                    let w = t.map(|p| frame.to_world(p).as_vec3() + lift);
                    s.tri(true, w, rgba(col, a));
                }
            }
            for c in &sketch.curves {
                let r = Ref::SketchCurve { sketch: f.id, curve: c.id };
                let hov = app.hover.as_ref() == Some(&r);
                let picked = is_picked(app, &r);
                let col = if picked || sk_picked {
                    vp::PICKED
                } else if hov {
                    vp::HOVER
                } else if c.construction {
                    vp::CONSTRUCTION
                } else {
                    vp::SKETCH
                };
                let pts: Vec<Vec3> = sketch.tessellate(c, 1.0).iter().map(|p| frame.to_world(*p).as_vec3()).collect();
                if c.construction {
                    dashed(&mut s, &pts, 1.2, rgba(col, 0.9), app.camera.wpp() * 6.0);
                } else {
                    s.polyline(&pts, if hov || picked { 2.4 } else { 1.6 }, rgba(col, 1.0), hov || picked);
                }
            }
            for p in &sketch.points {
                let r = Ref::SketchPoint { sketch: f.id, point: p.id };
                let hov = app.hover.as_ref() == Some(&r);
                let picked = is_picked(app, &r);
                if hov || picked {
                    s.point(frame.to_world(p.v()).as_vec3(), 7.0, rgba(if picked { vp::PICKED } else { vp::HOVER }, 1.0));
                } else if !sketch.curves.iter().any(|c| matches!(c.geom, Geom::Circle { .. } | Geom::Arc { .. }) && crate::sketch::Sketch::curve_points(&c.geom)[0] == p.id) {
                    s.point(frame.to_world(p.v()).as_vec3(), 3.5, rgba(vp::SKETCH, 0.9));
                }
            }
        }
    }

    // sketch curves picked as references
    if let Some(sess) = &app.sketch {
        crate::sketcher::scene(app, sess, &mut s);
    }

    // measurement lines
    if let Some(m) = &app.measure {
        if let Some((a, b, _)) = measure_pair(app, m) {
            s.polyline(&[a.as_vec3(), b.as_vec3()], 1.5, rgba(vp::DIM, 1.0), true);
            s.point(a.as_vec3(), 6.0, rgba(vp::DIM, 1.0));
            s.point(b.as_vec3(), 6.0, rgba(vp::DIM, 1.0));
        }
    }
    s
}

pub fn dashed(s: &mut SceneData, pts: &[Vec3], w: f32, col: [f32; 4], dash: f32) {
    let mut on = true;
    let mut left = dash;
    for win in pts.windows(2) {
        let (mut a, b) = (win[0], win[1]);
        let mut seg = (b - a).length();
        while seg > 1e-9 {
            let step = left.min(seg);
            let p = a + (b - a).normalize() * step;
            if on {
                s.lines_top.push(crate::render::LineInst { a: a.into(), b: p.into(), width: w, bias: 0.0, color: col });
            }
            a = p;
            seg -= step;
            left -= step;
            if left <= 1e-9 {
                on = !on;
                left = dash;
            }
        }
    }
}

// ---------------------------------------------------------------- overlays

fn model_overlay(app: &mut App, painter: &egui::Painter, rect: egui::Rect) {
    let cam = app.camera.clone();
    let to_screen = |p: DVec3| {
        let v = cam.project(p.as_vec3());
        Pos2::new(rect.min.x + v.x, rect.min.y + v.y)
    };
    // sketch dimensions on the model
    if app.show_dims {
        let rolled = rolled_ids(app);
        for f in &app.doc.features {
            if f.hidden || rolled.contains(&f.id) {
                continue;
            }
            if let Op::Sketch { sketch, frame, .. } = &f.op {
                crate::sketcher::draw_dims(app, sketch, frame, painter, rect, None, None);
            }
        }
    }
    if let Some(m) = &app.measure {
        if let Some((a, b, label)) = measure_pair(app, m) {
            let mid = to_screen((a + b) / 2.0);
            label_box(painter, mid + egui::vec2(0.0, -14.0), &label, vp::DIM);
        }
        for (a, b, l) in &m.kept {
            let (sa, sb) = (to_screen((*a).into()), to_screen((*b).into()));
            painter.line_segment([sa, sb], Stroke::new(1.2, vp::DIM));
            label_box(painter, sa + (sb - sa) / 2.0, l, vp::DIM);
        }
    }
    // hover label
    if let (Some(h), Some(p)) = (&app.hover, painter.ctx().pointer_hover_pos()) {
        if rect.contains(p) && app.nav.is_none() {
            let text = hover_text(app, h);
            label_box(painter, p + egui::vec2(14.0, 18.0), &text, crate::theme::TEXT);
        }
    }
}

fn hover_text(app: &App, r: &Ref) -> String {
    match r {
        Ref::Edge { point } => {
            for b in &app.result.bodies {
                if let Some(e) = b.edges.iter().find(|e| e.mid == *point) {
                    let kind = if e.linear { "line" } else if e.circular { "circle" } else { "curve" };
                    return format!("{kind} edge · {}", app.doc.units.fmt_with(e.length));
                }
            }
            "edge".into()
        }
        Ref::Face { .. } => "face".into(),
        _ => r.describe(),
    }
}

pub fn label_box(painter: &egui::Painter, at: Pos2, text: &str, color: Color32) {
    let font = egui::FontId::proportional(12.0);
    let g = painter.layout_no_wrap(text.to_string(), font, color);
    let r = egui::Rect::from_center_size(at, g.size() + egui::vec2(12.0, 4.0));
    painter.rect(r, 4.0, Color32::from_rgba_unmultiplied(0x15, 0x18, 0x1c, 0xd9), Stroke::new(1.0, crate::theme::BORDER), egui::StrokeKind::Inside);
    painter.galley(r.min + egui::vec2(6.0, 2.0), g, color);
}

fn axis_triad(app: &App, painter: &egui::Painter, rect: egui::Rect) {
    let c = Pos2::new(rect.left() + 36.0, rect.bottom() - 36.0);
    let cam = &app.camera;
    for (dir, col, l) in [(Vec3::X, vp::AXIS_X, "X"), (Vec3::Y, vp::AXIS_Y, "Y"), (Vec3::Z, vp::AXIS_Z, "Z")] {
        let x = dir.dot(cam.right());
        let y = dir.dot(cam.up);
        let e = c + egui::vec2(x, -y) * 22.0;
        painter.line_segment([c, e], Stroke::new(2.0, col));
        painter.text(e + egui::vec2(x, -y) * 7.0, egui::Align2::CENTER_CENTER, l, egui::FontId::proportional(10.0), col);
    }
}

fn context_menu(app: &mut App, ui: &mut Ui) {
    let Some((pos, target)) = app.context_menu.clone() else { return };
    if app.nav.as_ref().map(|n| n.moved > 3.0).unwrap_or(false) {
        app.context_menu = None;
        return;
    }
    let mut close = false;
    let area = egui::Area::new(egui::Id::new("ctx-menu")).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
        egui::Frame::menu(ui.style()).fill(crate::theme::PANEL_2).show(ui, |ui| {
            ui.set_min_width(200.0);
            if let Some(t) = &target {
                ui.label(egui::RichText::new(t.describe()).color(crate::theme::MUTED).size(11.0));
                if ui.button("Select").clicked() {
                    if !app.picks.contains(t) {
                        app.picks.push(t.clone());
                    }
                    close = true;
                }
                if t.is_planar() && ui.button("New sketch here").clicked() {
                    app.picks = vec![t.clone()];
                    app.start_feature("sketch");
                    close = true;
                }
                if let Ref::Face { point, normal } = t {
                    if ui.button("View normal to").clicked() {
                        app.camera.set_orientation(DVec3::from(*normal).as_vec3(), if normal[2].abs() > 0.9 { Vec3::Y } else { Vec3::Z });
                        app.camera.target = DVec3::from(*point).as_vec3();
                        app.scene_dirty = true;
                        close = true;
                    }
                }
                let sk = match t {
                    Ref::FeaturePlane { feature } | Ref::Region { sketch: feature, .. } | Ref::SketchCurve { sketch: feature, .. } | Ref::SketchPoint { sketch: feature, .. } => Some(*feature),
                    _ => None,
                };
                if let Some(f) = sk {
                    if ui.button("Edit feature").clicked() {
                        app.edit_feature(f);
                        close = true;
                    }
                }
                ui.separator();
            }
            if ui.button("Zoom to fit").clicked() {
                app.fit_view();
                close = true;
            }
            if !app.picks.is_empty() && ui.button("Clear selection").clicked() {
                app.picks.clear();
                app.scene_dirty = true;
                close = true;
            }
        });
    });
    let clicked_elsewhere = ui.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer() && !ui.input(|i| i.pointer.secondary_pressed());
    if close || clicked_elsewhere || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.context_menu = None;
    }
}

// ---------------------------------------------------------------- measure

fn ref_point(app: &App, r: &Ref) -> Option<(DVec3, Option<DVec3>)> {
    match r {
        Ref::Face { point, normal } => Some((DVec3::from(*point), Some(DVec3::from(*normal)))),
        Ref::Edge { point } | Ref::Vertex { point } => Some((DVec3::from(*point), None)),
        Ref::SketchPoint { sketch, point } => {
            let f = app.doc.feature(*sketch)?;
            if let Op::Sketch { sketch, frame, .. } = &f.op {
                Some((frame.to_world(sketch.point(*point)?.v()), None))
            } else {
                None
            }
        }
        Ref::OriginPlane { plane } => Some((DVec3::ZERO, Some(plane.frame().normal()))),
        _ => None,
    }
}

/// Two picked things → the points to draw between and the label.
pub fn measure_pair(app: &App, m: &Measure) -> Option<(DVec3, DVec3, String)> {
    let u = app.doc.units;
    if m.picks.len() == 1 {
        if let Ref::Edge { point } = &m.picks[0] {
            for b in &app.result.bodies {
                if let Some(e) = b.edges.iter().find(|e| e.mid == *point) {
                    let a = DVec3::from(e.pts[0].map(|x| x as f64));
                    let z = DVec3::from(e.pts[e.pts.len() - 1].map(|x| x as f64));
                    return Some((a, z, format!("length {}", u.fmt_with(e.length))));
                }
            }
        }
        return None;
    }
    if m.picks.len() < 2 {
        return None;
    }
    let (a, na) = ref_point(app, &m.picks[0])?;
    let (b, nb) = ref_point(app, &m.picks[1])?;
    if let (Some(na), Some(nb)) = (na, nb) {
        if na.cross(nb).length() < 1e-6 {
            // parallel planar faces: normal distance
            let d = (b - a).dot(na).abs();
            let foot = b - na * (b - a).dot(na);
            return Some((foot, b, format!("distance {}", u.fmt_with(d))));
        }
        let ang = na.angle_between(nb).to_degrees();
        return Some((a, b, format!("angle {}°", crate::expr::trim(ang, 2))));
    }
    let d = (b - a).length();
    let dd = b - a;
    Some((
        a,
        b,
        format!("distance {} (dx {}, dy {}, dz {})", u.fmt_with(d), u.fmt(dd.x.abs()), u.fmt(dd.y.abs()), u.fmt(dd.z.abs())),
    ))
}

pub fn measure_text(app: &App, m: &Measure) -> String {
    match measure_pair(app, m) {
        Some((_, _, l)) => format!("Measure: {l} · click two things, Esc to stop"),
        None => "Measure: click a face, edge or point, then another".into(),
    }
}
