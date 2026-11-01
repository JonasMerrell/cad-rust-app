//! Drawing tab: a sheet with hidden-line projected views of a Part Studio's parts or of an
//! assembly, dimensions attached to the model's edges, notes, and exports to PDF, DXF or SVG.

use std::collections::{HashMap, HashSet};

use egui::{Color32, Margin, Pos2, RichText, Sense, Stroke, Ui};
use glam::DVec2;

use crate::app::{App, ToastKind};
use crate::asm_ui::checkpoint;
use crate::doc::BodyId;
use crate::drawing::{Anchor, DimTol, DrawDim, DrawView, Drawing, Gdt, Note, GDT_KINDS};
use crate::kernel::{ProjReq, ProjView};
use crate::theme;
use crate::workspace::{Tab, Workspace};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DTool {
    #[default]
    Select,
    Dimension,
    Radial,
    Angle,
    Note,
    Gdt,
    Datum,
    /// Pick where a section cuts a view: a horizontal or a vertical cutting line.
    Section(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DSel {
    View(u64),
    Dim(u64),
    Note(u64),
    Gdt(u64),
}

/// Projections are stored by (Part Studio, or ASM_TAG + assembly index; key string).
pub type ProjKey = (usize, String);

#[derive(Default)]
pub struct DrawUi {
    /// Part geometry changed: projections must be fetched again.
    pub dirty: bool,
    pub proj: HashMap<ProjKey, ProjView>,
    pub requested: HashSet<ProjKey>,
    pub center: [f64; 2],
    pub zoom: f32,
    pub fitted: bool,
    pub tool: DTool,
    pub sel: Option<DSel>,
    pub drag: Option<(DSel, [f64; 2], [f64; 2])>,
    /// First point of a dimension being placed: (view, model point).
    pub dim_first: Option<(u64, [f64; 2])>,
    pub note_edit: Option<(Option<u64>, [f64; 2], String)>,
    pub hover_pt: Option<(u64, [f64; 2])>,
    /// Sheet shown (0-based).
    pub cur_sheet: usize,
    /// Circle or arc under the pointer for radial dimensions: (view, centre, point on it, full circle).
    pub hover_circle: Option<(u64, [f64; 2], [f64; 2], bool)>,
    /// Straight line under the pointer for angle dimensions, and the first one picked.
    pub hover_line: Option<(u64, [f64; 2], [f64; 2])>,
    pub angle_first: Option<(u64, [f64; 2], [f64; 2])>,
    /// Alignment guides while dragging a view (sheet x, sheet y).
    pub guides: (Option<f64>, Option<f64>),
}

pub const DIRS: &[(&str, &str)] = &[
    ("front", "Front"),
    ("top", "Top"),
    ("right", "Right"),
    ("back", "Back"),
    ("bottom", "Bottom"),
    ("left", "Left"),
    ("iso", "Isometric"),
    ("section:front", "Section (front)"),
    ("section:top", "Section (top)"),
    ("section:right", "Section (right)"),
    ("section:back", "Section (back)"),
    ("section:bottom", "Section (bottom)"),
    ("section:left", "Section (left)"),
];

/// Tag of assembly views' projections (plus the assembly index).
pub const ASM_TAG: usize = 1 << 20;

fn base_dir(dir: &str) -> &str {
    dir.strip_prefix("section:").unwrap_or(dir)
}

pub fn is_section(v: &DrawView) -> bool {
    v.dir.starts_with("section:")
}

pub fn dir_vectors(dir: &str) -> ([f64; 3], [f64; 3]) {
    let s = 1.0 / 3f64.sqrt();
    match base_dir(dir) {
        "top" => ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
        "right" => ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        "back" => ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        "bottom" => ([0.0, 0.0, -1.0], [0.0, -1.0, 0.0]),
        "left" => ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        "iso" => ([s, -s, s], [0.0, 0.0, 1.0]),
        _ => ([0.0, -1.0, 0.0], [0.0, 0.0, 1.0]),
    }
}

/// The view's 2D axes in model space: (x, y, towards the viewer).
fn view_axes(dir: &str) -> (glam::DVec3, glam::DVec3, glam::DVec3) {
    let (d, u) = dir_vectors(dir);
    let (d, u) = (glam::DVec3::from(d), glam::DVec3::from(u));
    let x = u.cross(d).normalize();
    (x, d.cross(x), d)
}

pub fn sheet_size(name: &str) -> [f64; 2] {
    match name {
        "A4" => [297.0, 210.0],
        "A2" => [594.0, 420.0],
        "A1" => [841.0, 594.0],
        "Letter" => [279.4, 215.9],
        "Tabloid" => [431.8, 279.4],
        _ => [420.0, 297.0],
    }
}

/// Size of the title block (it sits in the bottom-right corner, inside the 10 mm border).
pub const TITLE_W: f64 = 170.0;
pub const TITLE_H: f64 = 36.0;

pub fn title_rect(d: &Drawing) -> ([f64; 2], [f64; 2]) {
    let [w, _] = sheet_size(&d.sheet);
    ([w - 10.0 - TITLE_W, 10.0], [w - 10.0, 10.0 + TITLE_H])
}

fn dwg_index(app: &App) -> Option<usize> {
    match app.tab {
        Tab::Drawing(i) => Some(i),
        _ => None,
    }
}

fn dwg(app: &App) -> Option<&Drawing> {
    dwg_index(app).and_then(|i| app.ws.drawings.get(i))
}

fn dwg_mut(app: &mut App) -> Option<&mut Drawing> {
    let i = dwg_index(app)?;
    app.ws.drawings.get_mut(i)
}

fn units_of(app: &dyn DocCtx, studio: usize) -> crate::expr::Units {
    app.studio_doc(studio).map(|d| d.units).unwrap_or_default()
}

// ---------------------------------------------------------------- what a view shows

/// The view whose projection a view uses: a detail view shows part of its source view.
pub fn source_view<'a>(d: &'a Drawing, v: &'a DrawView) -> &'a DrawView {
    v.detail.and_then(|(src, _, _)| d.views.iter().find(|x| x.id == src && x.detail.is_none())).unwrap_or(v)
}

/// The parts of an assembly as (Part Studio, body, row-major 3x4 pose), in BOM order.
pub fn asm_items(ws: &Workspace, ai: usize) -> Vec<(usize, BodyId, [f64; 12])> {
    let Some(a) = ws.assemblies.get(ai) else { return vec![] };
    let mut items = vec![];
    for x in a.instances.iter().filter(|x| !x.hidden) {
        for l in crate::assembly::leaves(&ws.assemblies, x) {
            let p = x.pose.compose(&l.pose);
            let (m, t) = (p.m(), p.t);
            items.push((l.studio, l.body, [m.x_axis.x, m.y_axis.x, m.z_axis.x, t[0], m.x_axis.y, m.y_axis.y, m.z_axis.y, t[1], m.x_axis.z, m.y_axis.z, m.z_axis.z, t[2]]));
        }
    }
    items
}

/// What to ask the kernel for to draw a view.
pub fn view_req(ws: &Workspace, v: &DrawView) -> ProjReq {
    let (d, u) = dir_vectors(&v.dir);
    ProjReq {
        dir: d,
        up: u,
        section: is_section(v),
        cut: v.cut,
        bodies: v.body.into_iter().collect(),
        items: v.assembly.map(|ai| asm_items(ws, ai)).unwrap_or_default(),
    }
}

/// The key a view's projection is stored under; it changes whenever what the view shows does.
pub fn view_key(ws: &Workspace, d: &Drawing, v: &DrawView) -> ProjKey {
    let v = source_view(d, v);
    match v.assembly {
        Some(ai) => {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            format!("{:?}", asm_items(ws, ai)).hash(&mut h);
            (ASM_TAG + ai, format!("{}|{:?}|{:x}", v.dir, v.cut, h.finish()))
        }
        None if v.body.is_none() && v.cut.is_none() => (v.studio, v.dir.clone()),
        None => (v.studio, format!("{}|{:?}|{:?}", v.dir, v.body, v.cut)),
    }
}

/// The projection a view draws, when the kernel has made it.
pub fn view_proj<'a>(app: &'a dyn DocCtx, d: &Drawing, v: &DrawView) -> Option<&'a ProjView> {
    app.proj(&view_key(app.ws(), d, v))
}

/// Reply from the kernel.
pub fn projected(app: &mut App, tag: usize, key: String, views: Vec<ProjView>) {
    if let Some(v) = views.into_iter().next() {
        app.dwg.proj.insert((tag, key), v);
        // dimensions follow the new geometry, in every drawing
        for i in 0..app.ws.drawings.len() {
            let mut d = app.ws.drawings[i].clone();
            if refresh(app, &mut d) {
                app.ws.drawings[i] = d;
            }
        }
    }
}

/// Ask the kernel for any projections the open drawing still needs.
fn request_projections(app: &mut App) {
    if app.dwg.dirty {
        app.dwg.dirty = false;
        app.dwg.proj.clear();
        app.dwg.requested.clear();
    }
    let Some(d) = dwg(app) else { return };
    let mut need: Vec<(ProjKey, DrawView)> = vec![];
    for v in d.views.iter().filter(|v| v.detail.is_none()) {
        let k = view_key(&app.ws, d, v);
        if !app.dwg.requested.contains(&k) && !need.iter().any(|n| n.0 == k) {
            need.push((k, v.clone()));
        }
    }
    for (key, v) in need {
        let req = view_req(&app.ws, &v);
        if let Some(ai) = v.assembly {
            // wait until every Part Studio in the assembly has been built
            let built = req.items.iter().all(|(s, _, _)| !app.studio_bodies(*s).is_empty());
            if !built || ai >= app.ws.assemblies.len() {
                continue;
            }
            let docs = app.studio_docs();
            app.kernel.set_studios(docs);
            app.dwg.requested.insert(key.clone());
            app.kernel.project(key.0, key.1, &crate::doc::Document::default(), vec![req]);
            continue;
        }
        // wait until the Part Studio has been built
        if app.studio_bodies(v.studio).is_empty() {
            continue;
        }
        let Some(doc) = app.studio_doc(v.studio).cloned() else { continue };
        app.dwg.requested.insert(key.clone());
        app.kernel.project(key.0, key.1, &doc, vec![req]);
    }
}

/// Front, top, right and isometric views laid out third-angle (first-angle when the title block
/// says so), scaled to the sheet.
pub fn add_standard_views(app: &mut App) {
    let studio = app.cur_studio;
    let sheet = dwg(app).map(|d| sheet_size(&d.sheet)).unwrap_or([420.0, 297.0]);
    // model size for a sensible scale
    let mut lo = glam::DVec3::splat(f64::INFINITY);
    let mut hi = glam::DVec3::splat(f64::NEG_INFINITY);
    for b in app.studio_bodies(studio) {
        lo = lo.min(b.bbox[0].into());
        hi = hi.max(b.bbox[1].into());
    }
    let size = if lo.x.is_finite() { hi - lo } else { glam::DVec3::splat(50.0) };
    let need_w = size.x + size.y + size.y * 0.8;
    let need_h = size.z + size.y + size.z * 0.6;
    let raw = ((sheet[0] - 60.0) / need_w.max(1.0)).min((sheet[1] - 80.0) / need_h.max(1.0));
    let scale = nice_scale(raw);
    let (fw, fh) = (size.x * scale, size.z * scale);
    let gap = 25.0;
    let front = [40.0 + fw / 2.0, 60.0 + fh / 2.0];
    let sheet_no = app.dwg.cur_sheet;
    let Some(d) = dwg_mut(app) else { return };
    for (dir, pos) in [
        ("front", front),
        ("top", [front[0], front[1] + fh / 2.0 + gap + size.y * scale / 2.0]),
        ("right", [front[0] + fw / 2.0 + gap + size.y * scale / 2.0, front[1]]),
        ("iso", [sheet[0] - 30.0 - (size.x + size.y) * scale * 0.45, sheet[1] - 30.0 - (size.z + size.y) * scale * 0.45]),
    ] {
        d.next_id += 1;
        let id = d.next_id;
        d.views.push(DrawView { id, studio, dir: dir.into(), pos, scale, sheet: sheet_no, ..Default::default() });
    }
    if d.title.first_angle {
        mirror_projection(d);
    }
    app.dwg.fitted = false;
}

/// Switch a drawing's orthographic views between third-angle and first-angle placement: around each
/// front view (per sheet, part and assembly), the views beside it (right, left) and above or below it
/// (top, bottom) swap sides. Their positions are mirrored about the middle of the group, so the
/// group keeps the area it had. Section, detail and isometric views stay where they are.
pub fn mirror_projection(d: &mut Drawing) {
    let fronts: Vec<DrawView> = d.views.iter().filter(|v| v.dir == "front" && v.detail.is_none()).cloned().collect();
    for f in fronts {
        let same = |v: &DrawView| v.id != f.id && v.detail.is_none() && v.sheet == f.sheet && v.studio == f.studio && v.assembly == f.assembly && v.body == f.body;
        let side: Vec<u64> = d.views.iter().filter(|v| same(v) && matches!(v.dir.as_str(), "right" | "left") && (v.pos[1] - f.pos[1]).abs() < 1.0).map(|v| v.id).collect();
        let over: Vec<u64> = d.views.iter().filter(|v| same(v) && matches!(v.dir.as_str(), "top" | "bottom") && (v.pos[0] - f.pos[0]).abs() < 1.0).map(|v| v.id).collect();
        // along x the front view and the views beside it swap; the views above or below follow the
        // front view across (and the other way round along y)
        let mirror = |d: &mut Drawing, ids: &[u64], follow: &[u64], axis: usize| {
            if ids.is_empty() {
                return;
            }
            let all: Vec<f64> = d.views.iter().filter(|v| v.id == f.id || ids.contains(&v.id)).map(|v| v.pos[axis]).collect();
            let m = all.iter().cloned().fold(f64::INFINITY, f64::min) + all.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let shift = m - 2.0 * f.pos[axis];
            for v in d.views.iter_mut() {
                if v.id == f.id || ids.contains(&v.id) {
                    v.pos[axis] = m - v.pos[axis];
                } else if follow.contains(&v.id) {
                    v.pos[axis] += shift;
                }
            }
        };
        mirror(d, &side, &over, 0);
        mirror(d, &over, &side, 1);
    }
}

pub fn nice_scale(raw: f64) -> f64 {
    let steps = [100.0, 50.0, 20.0, 10.0, 5.0, 4.0, 2.0, 1.0, 0.5, 0.25, 0.2, 0.1, 0.05, 0.02, 0.01, 0.005];
    steps.iter().copied().find(|s| *s <= raw).unwrap_or(0.005)
}

fn fmt_scale(s: f64) -> String {
    if s >= 1.0 { format!("{}:1", crate::expr::trim(s, 2)) } else { format!("1:{}", crate::expr::trim(1.0 / s, 2)) }
}

/// The next free letter for a section or detail view (I, O and Q are skipped).
pub fn next_letter(d: &Drawing) -> String {
    ('A'..='Z').filter(|c| !matches!(c, 'I' | 'O' | 'Q')).map(|c| c.to_string()).find(|c| !d.views.iter().any(|v| v.letter == *c)).unwrap_or_else(|| "Z".into())
}

/// Where a view's model point lands on the sheet.
fn to_sheet(v: &DrawView, proj: Option<&ProjView>, p: [f64; 2]) -> [f64; 2] {
    let c = proj.map(|pr| [(pr.bbox[0][0] + pr.bbox[1][0]) / 2.0, (pr.bbox[0][1] + pr.bbox[1][1]) / 2.0]).unwrap_or([0.0; 2]);
    [v.pos[0] + (p[0] - c[0]) * v.scale, v.pos[1] + (p[1] - c[1]) * v.scale]
}

fn from_sheet(v: &DrawView, proj: Option<&ProjView>, s: [f64; 2]) -> [f64; 2] {
    let c = proj.map(|pr| [(pr.bbox[0][0] + pr.bbox[1][0]) / 2.0, (pr.bbox[0][1] + pr.bbox[1][1]) / 2.0]).unwrap_or([0.0; 2]);
    [(s[0] - v.pos[0]) / v.scale + c[0], (s[1] - v.pos[1]) / v.scale + c[1]]
}

// ---------------------------------------------------------------- dimensions attached to the model

fn anchor_tol(p: &ProjView) -> f64 {
    let ext = (p.bbox[1][0] - p.bbox[0][0]).abs().max((p.bbox[1][1] - p.bbox[0][1]).abs());
    1e-3 + 1e-5 * ext
}

/// Distance from `q` to a polyline, and the fraction of its length where the nearest point is.
fn polyline_nearest(pts: &[[f64; 2]], q: DVec2) -> (f64, f64) {
    let total: f64 = pts.windows(2).map(|w| DVec2::from(w[0]).distance(DVec2::from(w[1]))).sum();
    let (mut best, mut at, mut acc) = (f64::INFINITY, 0.0, 0.0);
    for w in pts.windows(2) {
        let (a, b) = (DVec2::from(w[0]), DVec2::from(w[1]));
        let d = b - a;
        let l = d.length();
        let t = if l < 1e-12 { 0.0 } else { ((q - a).dot(d) / (l * l)).clamp(0.0, 1.0) };
        let dist = (a + d * t - q).length();
        if dist < best {
            best = dist;
            at = acc + t * l;
        }
        acc += l;
    }
    (best, if total > 1e-12 { at / total } else { 0.0 })
}

fn polyline_at(pts: &[[f64; 2]], f: f64) -> [f64; 2] {
    let total: f64 = pts.windows(2).map(|w| DVec2::from(w[0]).distance(DVec2::from(w[1]))).sum();
    let target = f.clamp(0.0, 1.0) * total;
    let mut acc = 0.0;
    for w in pts.windows(2) {
        let (a, b) = (DVec2::from(w[0]), DVec2::from(w[1]));
        let l = a.distance(b);
        if acc + l >= target && l > 1e-12 {
            return a.lerp(b, (target - acc) / l).into();
        }
        acc += l;
    }
    pts[pts.len() - 1]
}

/// The model edge point a view point lies on: a circle's centre, an edge end, a point on a
/// circle (by angle) or along an edge (by length).
pub fn anchor_at(p: &ProjView, q: [f64; 2]) -> Option<Anchor> {
    let tol = anchor_tol(p);
    let qv = DVec2::from(q);
    let mk = |e: &crate::kernel::ProjEdge, at: u8, t: f64| Anchor { part: e.part, edge: e.index, kind: e.kind, at, t };
    for e in &p.edges {
        let c = e.circle.map(|c| [c[0], c[1]]).or(e.axis.map(|a| [a[0], a[1]]));
        if c.map(|c| DVec2::from(c).distance(qv) < tol).unwrap_or(false) {
            return Some(mk(e, 2, 0.0));
        }
    }
    for e in &p.edges {
        if DVec2::from(e.pts[0]).distance(qv) < tol {
            return Some(mk(e, 0, 0.0));
        }
        if DVec2::from(e.pts[e.pts.len() - 1]).distance(qv) < tol {
            return Some(mk(e, 1, 0.0));
        }
    }
    for e in &p.edges {
        if let Some([cx, cy, r]) = e.circle {
            let c = DVec2::new(cx, cy);
            if ((qv - c).length() - r).abs() < tol && polyline_nearest(&e.pts, qv).0 < tol + 0.02 * r {
                let a = qv - c;
                return Some(mk(e, 4, a.y.atan2(a.x)));
            }
        }
    }
    let mut best: Option<(f64, Anchor)> = None;
    for e in &p.edges {
        let (dist, f) = polyline_nearest(&e.pts, qv);
        if dist < tol && best.map(|b| dist < b.0).unwrap_or(true) {
            best = Some((dist, mk(e, 3, f)));
        }
    }
    best.map(|b| b.1)
}

/// Where an anchor is in a (new) projection; None when its edge is gone.
pub fn resolve(p: &ProjView, a: &Anchor) -> Option<[f64; 2]> {
    let e = p.edges.iter().find(|e| e.part == a.part && e.index == a.edge && e.kind == a.kind)?;
    match a.at {
        0 => Some(e.pts[0]),
        1 => Some(e.pts[e.pts.len() - 1]),
        2 => e.circle.map(|c| [c[0], c[1]]).or(e.axis.map(|x| [x[0], x[1]])),
        4 => e.circle.map(|c| [c[0] + c[2] * a.t.cos(), c[1] + c[2] * a.t.sin()]),
        _ => Some(polyline_at(&e.pts, a.t)),
    }
}

/// Attach a dimension's points to the model edges they lie on (all or nothing).
pub fn anchor_dim(p: &ProjView, dm: &mut DrawDim) {
    let pts: Vec<[f64; 2]> = [dm.a, dm.b].into_iter().chain(dm.extra.iter().copied()).collect();
    let anchors: Vec<Option<Anchor>> = pts.iter().map(|q| anchor_at(p, *q)).collect();
    if anchors.iter().all(|a| a.is_some()) {
        dm.anchors = anchors;
        dm.dangling = false;
    }
}

/// After views are projected again: move anchored dimensions onto the model's new geometry
/// (flagging those whose edges are gone), attach older dimensions to the edges they sit on,
/// and regenerate auto-dimensions. Returns whether the drawing changed.
pub fn refresh(app: &dyn DocCtx, d: &mut Drawing) -> bool {
    let before = serde_json::to_string(&(&d.dims, d.next_id)).unwrap_or_default();
    let views: Vec<DrawView> = d.views.iter().filter(|v| v.detail.is_none()).cloned().collect();
    for v in &views {
        let Some(p) = view_proj(app, d, v) else { continue };
        if v.auto {
            regen_auto(d, v, p);
        }
        for dm in d.dims.iter_mut().filter(|x| x.view == v.id && x.auto.is_empty()) {
            if dm.anchors.is_empty() {
                // older dimensions: attach them while the geometry still matches
                if !dm.dangling {
                    anchor_dim(p, dm);
                }
                continue;
            }
            let got: Vec<Option<[f64; 2]>> = dm.anchors.iter().map(|a| a.as_ref().and_then(|a| resolve(p, a))).collect();
            if got.iter().any(|g| g.is_none()) {
                dm.dangling = true;
                continue;
            }
            dm.dangling = false;
            let got: Vec<[f64; 2]> = got.into_iter().flatten().collect();
            dm.a = got[0];
            dm.b = got[1];
            for (k, q) in dm.extra.iter_mut().enumerate() {
                if let Some(g) = got.get(2 + k) {
                    *q = *g;
                }
            }
        }
    }
    before != serde_json::to_string(&(&d.dims, d.next_id)).unwrap_or_default()
}

/// Replace a view's auto-dimensions with ones made from its current projection, keeping the
/// ids and offsets of those that are still there.
fn regen_auto(d: &mut Drawing, v: &DrawView, p: &ProjView) {
    let mut id = d.next_id;
    let fresh = auto_dims(p, v, &mut id);
    let mut out: Vec<DrawDim> = vec![];
    for mut n in fresh.into_iter().filter(|n| !v.auto_off.contains(&n.auto)) {
        if let Some(old) = d.dims.iter().find(|o| o.view == v.id && o.auto == n.auto) {
            n.id = old.id;
            n.offset = old.offset;
            n.tol = old.tol;
        } else {
            d.next_id += 1;
            n.id = d.next_id;
        }
        out.push(n);
    }
    // keep the order: other dimensions first, then this view's auto ones
    d.dims.retain(|o| !(o.view == v.id && !o.auto.is_empty()));
    d.dims.extend(out);
}

/// A circle or circular arc through a projected polyline: (centre, radius, full circle).
pub fn fit_circle(pl: &[[f64; 2]]) -> Option<([f64; 2], f64, bool)> {
    if pl.len() < 5 {
        return None;
    }
    let (a, b, c) = (DVec2::from(pl[0]), DVec2::from(pl[pl.len() / 3]), DVec2::from(pl[2 * pl.len() / 3]));
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if d.abs() < 1e-12 {
        return None;
    }
    let (a2, b2, c2) = (a.length_squared(), b.length_squared(), c.length_squared());
    let o = DVec2::new((a2 * (b.y - c.y) + b2 * (c.y - a.y) + c2 * (a.y - b.y)) / d, (a2 * (c.x - b.x) + b2 * (a.x - c.x) + c2 * (b.x - a.x)) / d);
    let r = (a - o).length();
    if !r.is_finite() || r < 1e-6 || pl.iter().any(|p| ((DVec2::from(*p) - o).length() - r).abs() > 1e-3 * r + 1e-4) {
        return None;
    }
    let full = DVec2::from(pl[0]).distance(DVec2::from(pl[pl.len() - 1])) < 1e-6 * (1.0 + r);
    Some((o.into(), r, full))
}

/// The angular span of an arc polyline about its centre: (start, end) with end > start,
/// counter-clockwise (radians).
pub fn arc_span(pl: &[[f64; 2]], c: [f64; 2]) -> (f64, f64) {
    let c = DVec2::from(c);
    let ang = |p: [f64; 2]| {
        let v = DVec2::from(p) - c;
        v.y.atan2(v.x)
    };
    let a0 = ang(pl[0]);
    let mut sweep = 0.0;
    let mut prev = a0;
    for p in &pl[1..] {
        let a = ang(*p);
        let mut da = a - prev;
        while da > std::f64::consts::PI {
            da -= std::f64::consts::TAU;
        }
        while da < -std::f64::consts::PI {
            da += std::f64::consts::TAU;
        }
        sweep += da;
        prev = a;
    }
    if sweep >= 0.0 { (a0, a0 + sweep) } else { (a0 + sweep, a0) }
}

/// Straight polylines of a projection (lines in the view) as segments.
fn straight(pl: &[[f64; 2]]) -> Option<([f64; 2], [f64; 2])> {
    let (a, b) = (DVec2::from(pl[0]), DVec2::from(pl[pl.len() - 1]));
    let d = b - a;
    if d.length() < 1e-9 {
        return None;
    }
    let n = d.perp().normalize();
    pl.iter().all(|p| (DVec2::from(*p) - a).dot(n).abs() < 1e-6 * (1.0 + d.length())).then_some((pl[0], pl[pl.len() - 1]))
}

/// Points a dimension can snap to: line ends and midpoints, circle and arc centres, and
/// quadrant points. Kind: 0 end, 1 midpoint, 2 centre, 3 quadrant.
pub fn snap_points(p: &ProjView) -> Vec<([f64; 2], u8)> {
    let mut out: Vec<([f64; 2], u8)> = vec![];
    let mut push = |q: [f64; 2], k: u8| {
        if !out.iter().any(|(x, _)| (x[0] - q[0]).abs() + (x[1] - q[1]).abs() < 1e-6) {
            out.push((q, k));
        }
    };
    for pl in &p.visible {
        if let Some((c, r, full)) = fit_circle(pl) {
            push(c, 2);
            let (a0, a1) = if full { (0.0, std::f64::consts::TAU) } else { arc_span(pl, c) };
            for k in 0..4 {
                let a = k as f64 * std::f64::consts::FRAC_PI_2;
                // quadrants that lie on the arc
                let inside = (0..2).any(|w| {
                    let a = a + w as f64 * std::f64::consts::TAU;
                    a >= a0 - 1e-9 && a <= a1 + 1e-9
                });
                if inside {
                    push([c[0] + r * a.cos(), c[1] + r * a.sin()], 3);
                }
            }
            if !full {
                push(pl[0], 0);
                push(pl[pl.len() - 1], 0);
            }
            continue;
        }
        push(pl[0], 0);
        push(pl[pl.len() - 1], 0);
        if straight(pl).is_some() {
            let (a, b) = (DVec2::from(pl[0]), DVec2::from(pl[pl.len() - 1]));
            push(((a + b) / 2.0).into(), 1);
        }
    }
    out
}

/// Full circles of a view grouped by centre: (centre, the largest radius).
fn circle_centres(p: &ProjView) -> Vec<([f64; 2], f64)> {
    let mut out: Vec<([f64; 2], f64)> = vec![];
    for pl in &p.visible {
        if let Some((c, r, true)) = fit_circle(pl) {
            match out.iter_mut().find(|x| DVec2::from(x.0).distance(DVec2::from(c)) < 1e-4 * (1.0 + r)) {
                Some(x) => x.1 = x.1.max(r),
                None => out.push((c, r)),
            }
        }
    }
    out
}

/// Auto-dimensions for a view: overall size, hole positions from the bottom-left corner, and
/// one callout per kind of hole (from its Hole feature when there is one).
pub fn auto_dims(p: &ProjView, v: &DrawView, next_id: &mut u64) -> Vec<DrawDim> {
    let mut out = vec![];
    let view = v.id;
    let [lo, hi] = p.bbox;
    let mut push = |kind: u8, a: [f64; 2], b: [f64; 2], offset: f64, prefix: String, suffix: String, key: String, out: &mut Vec<DrawDim>| {
        *next_id += 1;
        out.push(DrawDim { id: *next_id, view, a, b, kind, offset, prefix, suffix, auto: key, ..Default::default() });
    };
    let tol = anchor_tol(p).max(1e-3);
    // holes: circles with a Hole feature behind them, grouped by the feature's size and kind
    let circles: Vec<([f64; 2], f64)> = p.visible.iter().filter_map(|pl| fit_circle(pl).filter(|c| c.2).map(|c| (c.0, c.1))).collect();
    let mut used: Vec<usize> = vec![];
    let mut centres: Vec<[f64; 2]> = vec![];
    let mid = DVec2::new((lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0);
    // the leader points away from the middle of the view, out of the material
    let outward = |c: [f64; 2], r: f64| -> [f64; 2] {
        let dir = (DVec2::from(c) - mid).normalize_or(DVec2::new(1.0, 1.0).normalize());
        let dir = if dir.length() < 0.5 { DVec2::new(1.0, 1.0).normalize() } else { dir };
        let d45 = DVec2::new(dir.x.signum(), dir.y.signum()).normalize();
        (DVec2::from(c) + d45 * r).into()
    };
    // a leader long enough to take the callout's text out past the view's outline (sheet mm)
    let clear_lead = |c: [f64; 2], on: [f64; 2]| -> f64 {
        let (c, on) = (DVec2::from(c), DVec2::from(on));
        let dir = (on - c).normalize_or(DVec2::new(1.0, 1.0).normalize());
        let edge = if dir.y >= 0.0 { hi[1] - on.y } else { on.y - lo[1] };
        ((edge * v.scale + 5.0) / dir.y.abs().max(0.3)).clamp(6.0, 60.0)
    };
    let mut groups: Vec<(&crate::kernel::ProjHole, Vec<[f64; 2]>)> = vec![];
    for h in p.holes.iter().filter(|h| h.square) {
        let Some(ci) = circles.iter().position(|(c, r)| DVec2::from(*c).distance(DVec2::from(h.at)) < tol + 1e-3 && (r - h.dia / 2.0).abs() < tol + 1e-3) else { continue };
        // the counterbore or countersink circle belongs to the same callout
        for (k, (c, r)) in circles.iter().enumerate() {
            if DVec2::from(*c).distance(DVec2::from(h.at)) < tol + 1e-3 && (h.kind > 0 && (r - h.dia2 / 2.0).abs() < tol + 1e-3 || k == ci) {
                used.push(k);
            }
        }
        let same = |g: &crate::kernel::ProjHole| g.feature == h.feature && g.kind == h.kind && (g.dia - h.dia).abs() < 1e-9 && (g.dia2 - h.dia2).abs() < 1e-9 && g.through == h.through;
        match groups.iter_mut().find(|g| same(g.0)) {
            Some(g) => {
                if !g.1.iter().any(|q| DVec2::from(*q).distance(DVec2::from(h.at)) < tol) {
                    g.1.push(h.at);
                }
            }
            None => groups.push((h, vec![h.at])),
        }
        if !centres.iter().any(|q| DVec2::from(*q).distance(DVec2::from(h.at)) < tol) {
            centres.push(h.at);
        }
    }
    let num = |v: f64| crate::expr::trim(v, 3);
    for (gi, (h, cs)) in groups.iter().enumerate() {
        let mut cs = cs.clone();
        cs.sort_by(|a, b| (a[0], a[1]).partial_cmp(&(b[0], b[1])).unwrap_or(std::cmp::Ordering::Equal));
        let c = cs[cs.len() - 1];
        let r = h.dia / 2.0;
        let mut suffix = if h.through { " THRU".to_string() } else if h.depth > 0.0 { format!(" ↧{}", num(h.depth)) } else { String::new() };
        match h.kind {
            1 => suffix.push_str(&format!("\n⌴Ø{} ↧{}", num(h.dia2), num(h.v2))),
            2 => suffix.push_str(&format!("\n⌵Ø{} × {}°", num(h.dia2), num(h.v2))),
            _ => {}
        }
        let on = outward(c, r);
        push(4, c, on, clear_lead(c, on),  if cs.len() > 1 { format!("{}× ", cs.len()) } else { String::new() }, suffix, format!("hole:{}:{gi}", h.feature), &mut out);
    }
    // other full circles, grouped by radius
    let mut plain: Vec<(f64, Vec<[f64; 2]>)> = vec![];
    for (k, (c, r)) in circles.iter().enumerate() {
        if used.contains(&k) {
            continue;
        }
        // a smaller circle inside a bigger one at the same centre is a step of the same hole
        match plain.iter_mut().find(|g| (g.0 - r).abs() < 1e-4 * (1.0 + r)) {
            Some(g) => {
                if !g.1.iter().any(|q| (q[0] - c[0]).abs() + (q[1] - c[1]).abs() < 1e-6 * (1.0 + r)) {
                    g.1.push(*c);
                }
            }
            None => plain.push((*r, vec![*c])),
        }
        if !centres.iter().any(|q| DVec2::from(*q).distance(DVec2::from(*c)) < tol) {
            centres.push(*c);
        }
    }
    for (r, cs) in &plain {
        let c = cs[cs.len() - 1];
        let on = outward(c, *r);
        push(4, c, on, clear_lead(c, on), if cs.len() > 1 { format!("{}× ", cs.len()) } else { String::new() }, String::new(), format!("dia:{}", crate::expr::trim(*r * 2.0, 4)), &mut out);
    }
    // hole positions from the bottom-left corner: x above the view, y on its left
    let mut xs: Vec<f64> = vec![];
    let mut ys: Vec<f64> = vec![];
    for c in &centres {
        if (c[0] - lo[0]).abs() > tol && !xs.iter().any(|x| (x - c[0]).abs() < tol) {
            xs.push(c[0]);
        }
        if (c[1] - lo[1]).abs() > tol && !ys.iter().any(|y| (y - c[1]).abs() < tol) {
            ys.push(c[1]);
        }
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    xs.truncate(8);
    ys.truncate(8);
    let step = 7.0;
    for (i, x) in xs.iter().enumerate() {
        let c = centres.iter().find(|c| (c[0] - x).abs() < tol).copied().unwrap_or([*x, hi[1]]);
        push(1, [lo[0], hi[1]], c, 8.0 + i as f64 * step, String::new(), String::new(), format!("x:{}", crate::expr::trim(*x - lo[0], 4)), &mut out);
    }
    for (i, y) in ys.iter().enumerate() {
        let c = centres.iter().find(|c| (c[1] - y).abs() < tol).copied().unwrap_or([lo[0], *y]);
        push(2, [lo[0], lo[1]], c, 8.0 + i as f64 * step, String::new(), String::new(), format!("y:{}", crate::expr::trim(*y - lo[1], 4)), &mut out);
    }
    // overall size, outside the hole positions
    if hi[0] - lo[0] > 1e-6 {
        push(1, [lo[0], hi[1]], [hi[0], hi[1]], 8.0 + xs.len() as f64 * step + if xs.is_empty() { 2.0 } else { 0.0 }, String::new(), String::new(), "w".into(), &mut out);
    }
    if hi[1] - lo[1] > 1e-6 {
        push(2, [hi[0], lo[1]], [hi[0], hi[1]], -12.0, String::new(), String::new(), "h".into(), &mut out);
    }
    out
}

// ---------------------------------------------------------------- geometry for display and export

/// What the sheet renderer needs to know about the document (the GUI app or a headless session).
pub trait DocCtx {
    fn ws(&self) -> &crate::workspace::Workspace;
    fn bodies(&self, studio: usize) -> &[crate::kernel::BodyMesh];
    fn studio_doc(&self, studio: usize) -> Option<&crate::doc::Document>;
    fn proj(&self, key: &ProjKey) -> Option<&ProjView>;
}

impl DocCtx for App {
    fn ws(&self) -> &crate::workspace::Workspace {
        &self.ws
    }
    fn bodies(&self, studio: usize) -> &[crate::kernel::BodyMesh] {
        self.studio_bodies(studio)
    }
    fn studio_doc(&self, studio: usize) -> Option<&crate::doc::Document> {
        App::studio_doc(self, studio)
    }
    fn proj(&self, key: &ProjKey) -> Option<&ProjView> {
        self.dwg.proj.get(key)
    }
}

/// A drawing primitive in sheet millimetres.
pub enum Prim {
    Line { a: [f64; 2], b: [f64; 2], w: f64, dashed: bool, color: [u8; 3] },
    Text { at: [f64; 2], size: f64, text: String, anchor: u8, color: [u8; 3] },
    /// A circular arc counter-clockwise from `a0` to `a1` (radians); a full circle when they are a turn apart.
    Arc { c: [f64; 2], r: f64, a0: f64, a1: f64, w: f64, dashed: bool, color: [u8; 3] },
    /// A thin chain line (centre lines and cutting planes).
    Chain { a: [f64; 2], b: [f64; 2], w: f64, color: [u8; 3] },
    /// The primitives up to the next `End` draw one dimension (DXF writes it as a DIMENSION).
    Dim(DimInfo),
    End,
}

/// A dimension's definition points on the sheet, for exporters that keep dimensions as such.
#[derive(Clone, Debug, Default)]
pub struct DimInfo {
    /// 0 aligned, 1 horizontal, 2 vertical, 3 radius, 4 diameter, 5 angle.
    pub kind: u8,
    /// Linear: the two measured points; radial: centre and the point on the circle;
    /// angle: the two lines' points.
    pub pts: Vec<[f64; 2]>,
    /// A point on the dimension line (linear), or where the leader ends (radial).
    pub line: [f64; 2],
    pub text_at: [f64; 2],
    pub text: String,
}

impl Prim {
    /// Line pieces of an arc, for renderers without arcs.
    pub fn arc_points(c: [f64; 2], r: f64, a0: f64, a1: f64) -> Vec<[f64; 2]> {
        let n = (((a1 - a0).abs() / std::f64::consts::TAU * 96.0).ceil() as usize).clamp(4, 192);
        (0..=n).map(|i| {
            let t = a0 + (a1 - a0) * i as f64 / n as f64;
            [c[0] + r * t.cos(), c[1] + r * t.sin()]
        }).collect()
    }

    /// Dash pattern of chain lines (mm): long dash, gap, dot, gap.
    pub const CHAIN: [f64; 4] = [6.0, 1.2, 1.0, 1.2];

    /// The pieces of a chain line, for renderers without dash patterns.
    pub fn chain_pieces(a: [f64; 2], b: [f64; 2]) -> Vec<([f64; 2], [f64; 2])> {
        let (a, b) = (DVec2::from(a), DVec2::from(b));
        let len = a.distance(b);
        let d = (b - a).normalize_or_zero();
        let mut out = vec![];
        let mut s = 0.0;
        let mut k = 0;
        while s < len {
            let l = Self::CHAIN[k % 4];
            if k % 2 == 0 {
                out.push(((a + d * s).into(), (a + d * (s + l).min(len)).into()));
            }
            s += l;
            k += 1;
        }
        out
    }
}

const INK: [u8; 3] = [20, 20, 20];
pub const DIM_INK: [u8; 3] = [20, 60, 140];
const LOST: [u8; 3] = [200, 40, 40];
const THIN_INK: [u8; 3] = [110, 110, 110];

/// Polylines as lines, with circles and arcs kept as arcs.
fn curve_prims(out: &mut Vec<Prim>, v: &DrawView, p: &ProjView, pls: &[Vec<[f64; 2]>], w: f64, dashed: bool, color: [u8; 3]) {
    for pl in pls {
        if let Some((c, r, full)) = fit_circle(pl) {
            let (a0, a1) = if full { (0.0, std::f64::consts::TAU) } else { arc_span(pl, c) };
            out.push(Prim::Arc { c: to_sheet(v, Some(p), c), r: r * v.scale, a0, a1, w, dashed, color });
            continue;
        }
        for s in pl.windows(2) {
            out.push(Prim::Line { a: to_sheet(v, Some(p), s[0]), b: to_sheet(v, Some(p), s[1]), w, dashed, color });
        }
    }
}

/// The name, material and raw stock of what a sheet shows, for the title block.
fn shown_part(app: &dyn DocCtx, v: Option<&DrawView>) -> (String, String, String) {
    let Some(v) = v else { return Default::default() };
    if let Some(ai) = v.assembly {
        return (app.ws().assemblies.get(ai).map(|a| a.name.clone()).unwrap_or_default(), String::new(), String::new());
    }
    let bodies = app.bodies(v.studio);
    let studio = app.ws().studios.get(v.studio).map(|s| s.name.clone()).unwrap_or_default();
    let body = v.body.or_else(|| (bodies.len() == 1).then(|| bodies[0].id));
    let Some(b) = body else { return (studio, String::new(), String::new()) };
    let props = app.studio_doc(v.studio).and_then(|d| d.parts.get(&crate::doc::body_key(b))).cloned().unwrap_or_default();
    let name = props.name.clone().unwrap_or_else(|| {
        let mesh = bodies.iter().find(|m| m.id == b).map(|m| m.name.clone()).unwrap_or_default();
        if v.body.is_some() && bodies.len() > 1 { format!("{studio} / {mesh}") } else { studio.clone() }
    });
    let doc = app.studio_doc(v.studio);
    let stock = crate::stock_size::bom_text(bodies.iter().find(|m| m.id == b), &props, doc.map(|d| d.units).unwrap_or_default());
    (name, props.material.unwrap_or_default(), stock)
}

/// Text that may span lines ('\n') and hold the counterbore ⌴, countersink ⌵ and depth ↧
/// symbols, which are drawn as lines (fonts rarely have them). `at` is the first line's baseline.
fn text_prims(out: &mut Vec<Prim>, at: [f64; 2], size: f64, text: &str, anchor: u8, color: [u8; 3]) {
    const SYMS: [char; 3] = ['⌴', '⌵', '↧'];
    let cw = size * 0.6;
    for (li, line) in text.split('\n').enumerate() {
        let y = at[1] - li as f64 * size * 1.45;
        if !line.chars().any(|c| SYMS.contains(&c)) {
            out.push(Prim::Text { at: [at[0], y], size, text: line.to_string(), anchor, color });
            continue;
        }
        // lay the line out left to right with estimated widths
        let width = |s: &str| s.chars().map(|c| if SYMS.contains(&c) { size * 0.95 } else { cw }).sum::<f64>();
        let mut x = at[0] - match anchor {
            1 => width(line) / 2.0,
            2 => width(line),
            _ => 0.0,
        };
        let mut run = String::new();
        let flush = |out: &mut Vec<Prim>, run: &mut String, x: &mut f64| {
            if !run.is_empty() {
                out.push(Prim::Text { at: [*x, y], size, text: run.clone(), anchor: 0, color });
                *x += width(run);
                run.clear();
            }
        };
        for ch in line.chars() {
            if !SYMS.contains(&ch) {
                run.push(ch);
                continue;
            }
            flush(out, &mut run, &mut x);
            let s = size;
            let l = |out: &mut Vec<Prim>, a: [f64; 2], b: [f64; 2]| out.push(Prim::Line { a: [x + a[0] * s, y + a[1] * s], b: [x + b[0] * s, y + b[1] * s], w: 0.22, dashed: false, color });
            match ch {
                '⌴' => {
                    l(out, [0.1, 0.72], [0.1, 0.08]);
                    l(out, [0.1, 0.08], [0.8, 0.08]);
                    l(out, [0.8, 0.08], [0.8, 0.72]);
                }
                '⌵' => {
                    l(out, [0.08, 0.72], [0.45, 0.08]);
                    l(out, [0.45, 0.08], [0.82, 0.72]);
                }
                _ => {
                    l(out, [0.45, 0.8], [0.45, 0.1]);
                    l(out, [0.45, 0.1], [0.25, 0.38]);
                    l(out, [0.45, 0.1], [0.65, 0.38]);
                    l(out, [0.15, 0.04], [0.75, 0.04]);
                }
            }
            x += s * 0.95;
        }
        flush(out, &mut run, &mut x);
    }
}

/// Format a length for a dimension (mm in, document units out).
pub fn fmt_len(d: &Drawing, units: crate::expr::Units, mm: f64) -> String {
    match d.decimals {
        Some(n) => {
            let s = format!("{:.*}", n as usize, mm / units.scale());
            if s.trim_start_matches('-').chars().all(|c| c == '0' || c == '.') { s.trim_start_matches('-').to_string() } else { s }
        }
        None => units.fmt(mm),
    }
}

/// A dimension's value with its tolerance: (main text, stacked upper and lower lines).
fn dim_label(d: &Drawing, units: crate::expr::Units, mm: f64, tol: Option<DimTol>) -> (String, Option<(String, String)>) {
    let f = |v: f64| fmt_len(d, units, v);
    let sign = |v: f64| if v >= 0.0 { format!("+{}", f(v)) } else { format!("−{}", f(-v)) };
    match tol {
        None => (f(mm), None),
        Some(DimTol::Symmetric { value }) => (format!("{} ±{}", f(mm), f(value.abs())), None),
        Some(DimTol::Deviation { upper, lower }) => (f(mm), Some((sign(upper), sign(-lower.abs())))),
        Some(DimTol::Limits { upper, lower }) => (String::new(), Some((f(mm + upper), f(mm - lower.abs())))),
    }
}

/// A dimension's text: the main value and any stacked tolerance beside it (or the two limits
/// stacked in its place).
fn label_prims(out: &mut Vec<Prim>, at: [f64; 2], anchor: u8, main: &str, stack: &Option<(String, String)>, color: [u8; 3]) {
    let size = 3.5;
    let cw = size * 0.6;
    match stack {
        None => text_prims(out, at, size, main, anchor, color),
        Some((up, low)) if main.is_empty() => {
            let s = size * 0.85;
            out.push(Prim::Text { at: [at[0], at[1] + s * 0.6], size: s, text: up.clone(), anchor, color });
            out.push(Prim::Text { at: [at[0], at[1] - s * 0.6], size: s, text: low.clone(), anchor, color });
        }
        Some((up, low)) => {
            let first = main.split('\n').next().unwrap_or("");
            let mw = first.chars().count() as f64 * cw;
            let tw = up.chars().count().max(low.chars().count()) as f64 * cw * 0.7;
            // the main text's left edge, so the tolerance can go after it
            let left = at[0] - match anchor {
                1 => (mw + tw) / 2.0,
                2 => mw + tw + 0.6,
                _ => 0.0,
            };
            text_prims(out, [left, at[1]], size, main, 0, color);
            let s = size * 0.7;
            out.push(Prim::Text { at: [left + mw + 0.6, at[1] + s * 0.75], size: s, text: up.clone(), anchor: 0, color });
            out.push(Prim::Text { at: [left + mw + 0.6, at[1] - s * 0.35], size: s, text: low.clone(), anchor: 0, color });
        }
    }
}

/// Sheet-space rectangle a view occupies (min, max), when known.
pub fn view_extent(app: &dyn DocCtx, d: &Drawing, v: &DrawView) -> Option<([f64; 2], [f64; 2])> {
    if let Some((_, _, r)) = v.detail {
        let rr = r * v.scale;
        return Some(([v.pos[0] - rr, v.pos[1] - rr - 8.0], [v.pos[0] + rr, v.pos[1] + rr]));
    }
    let Some(p) = view_proj(app, d, v) else {
        // not projected yet: assume the part's full size around the view's centre
        let r = part_size(app, v) * v.scale / 2.0;
        return Some(([v.pos[0] - r, v.pos[1] - r - 8.0], [v.pos[0] + r, v.pos[1] + r]));
    };
    let (a, b) = (to_sheet(v, Some(p), p.bbox[0]), to_sheet(v, Some(p), p.bbox[1]));
    Some(([a[0].min(b[0]), a[1].min(b[1]) - 8.0], [a[0].max(b[0]), a[1].max(b[1])]))
}

/// Problems worth telling the user about: dimensions that lost their geometry, and views
/// running into the title block.
pub fn warnings(app: &dyn DocCtx, d: &Drawing) -> Vec<String> {
    let mut out = vec![];
    let lost: Vec<String> = d.dims.iter().filter(|x| x.dangling).map(|x| x.id.to_string()).collect();
    if !lost.is_empty() {
        out.push(format!("dimension{} {} lost {} geometry after a model change (shown in red); delete or redo {}", if lost.len() > 1 { "s" } else { "" }, lost.join(", "), if lost.len() > 1 { "their" } else { "its" }, if lost.len() > 1 { "them" } else { "it" }));
    }
    let (tlo, thi) = title_rect(d);
    for v in &d.views {
        if let Some((lo, hi)) = view_extent(app, d, v) {
            // the view or its label (in the strip under it) running into the title block
            if lo[0] < thi[0] && hi[0] > tlo[0] && lo[1] + 1.0 < thi[1] && hi[1] > tlo[1] {
                out.push(format!("{} (view {}) overlaps the title block", view_label(d, v), v.id));
            }
        }
    }
    out
}

/// Everything on the sheet, for both the screen and the exporters.
pub fn sheet_prims(app: &dyn DocCtx, d: &Drawing, sheet: usize) -> Vec<Prim> {
    let mut out = vec![];
    let [w, h] = sheet_size(&d.sheet);
    let line = |out: &mut Vec<Prim>, a: [f64; 2], b: [f64; 2], wid: f64| out.push(Prim::Line { a, b, w: wid, dashed: false, color: INK });
    // border
    let m = 10.0;
    for (a, b) in [([m, m], [w - m, m]), ([w - m, m], [w - m, h - m]), ([w - m, h - m], [m, h - m]), ([m, h - m], [m, m])] {
        line(&mut out, a, b, 0.5);
    }
    let on_sheet: Vec<&DrawView> = d.views.iter().filter(|v| v.sheet == sheet).collect();
    title_block(app, d, sheet, &on_sheet, &mut out);

    for v in on_sheet.iter().copied() {
        if let Some((src, c, r)) = v.detail {
            detail_prims(app, d, v, src, c, r, &mut out);
            continue;
        }
        let Some(p) = view_proj(app, d, v) else {
            out.push(Prim::Text { at: v.pos, size: 4.0, text: "projecting…".into(), anchor: 1, color: [130, 130, 130] });
            continue;
        };
        view_geometry(&mut out, v, p);
        let label = view_label(d, v);
        let mut bottom = to_sheet(v, Some(p), [0.0, p.bbox[0][1]])[1];
        // below horizontal dimensions placed under the view, when there is room before the next view
        let below = d.dims.iter().filter(|x| x.view == v.id && x.kind == 1).map(|dm| to_sheet(v, Some(p), dm.a)[1] + dm.offset - 4.0).fold(bottom, f64::min);
        let free = |y: f64| {
            on_sheet.iter().filter(|o| o.id != v.id).filter_map(|o| view_extent(app, d, o)).all(|(lo, hi)| !(lo[0] < v.pos[0] + 20.0 && hi[0] > v.pos[0] - 20.0 && lo[1] + 8.0 < y - 3.0 && hi[1] > y - 8.0))
        };
        if below < bottom && free(below) {
            bottom = below;
        }
        let title = if is_section(v) && !v.letter.is_empty() { format!("SECTION {0}-{0}", v.letter) } else { label.to_uppercase() };
        out.push(Prim::Text { at: [v.pos[0], bottom - 7.0], size: 3.2, text: format!("{title} ({})", fmt_scale(v.scale)), anchor: 1, color: [80, 80, 80] });
        if v.balloons {
            balloon_prims(app, v, p, &mut out);
        }
    }
    // cutting lines of section views, on the views they cut
    for s in d.views.iter().filter(|s| is_section(s) && !s.letter.is_empty() && s.detail.is_none()) {
        if let Some(pv) = section_parent(d, s).filter(|pv| pv.sheet == sheet) {
            cutting_line(app, d, s, pv, &mut out);
        }
    }
    for dm in &d.dims {
        let Some(v) = on_sheet.iter().find(|v| v.id == dm.view) else { continue };
        let p = view_proj(app, d, v);
        let (a, b) = (to_sheet(v, p, dm.a), to_sheet(v, p, dm.b));
        let units = units_of(app, v.studio);
        let color = if dm.dangling { LOST } else { DIM_INK };
        let mut info = DimInfo { kind: dm.kind, pts: vec![a, b], ..Default::default() };
        let mark = start_dim(&mut out);
        match dm.kind {
            3 | 4 => {
                let dia = dm.kind == 4;
                let r = DVec2::from(dm.a).distance(dm.b.into());
                let (main, stack) = dim_label(d, units, r * if dia { 2.0 } else { 1.0 }, dm.tol);
                let lost = if dm.dangling { " ?" } else { "" };
                let mut text = format!("{}{}{}", dm.prefix, if dia { "Ø" } else { "R" }, main);
                // the first suffix line goes on the value's line, the rest underneath
                text.push_str(&dm.suffix);
                text.push_str(lost);
                let arc = (!dia).then(|| arc_of(p, dm.a, r)).flatten();
                radial_prims(&mut out, a, b, dia, dm.offset, &text, &stack, color, arc, &mut info);
            }
            5 if dm.extra.len() == 2 => {
                angle_prims(&mut out, [a, b], [to_sheet(v, p, dm.extra[0]), to_sheet(v, p, dm.extra[1])], dm.offset, color, &mut info);
            }
            _ => {
                let lost = if dm.dangling { " ?" } else { "" };
                dim_prims(&mut out, a, b, dm.kind, dm.offset, color, &mut info, |len| {
                    let (main, stack) = dim_label(d, units, len / v.scale, dm.tol);
                    (if main.is_empty() { main } else { format!("{}{main}{}{lost}", dm.prefix, dm.suffix) }, stack)
                });
            }
        }
        finish_dim(&mut out, mark, info);
    }
    for n in d.notes.iter().filter(|n| n.sheet == sheet) {
        text_prims(&mut out, n.pos, 3.5, &n.text, 0, INK);
    }
    for g in d.gdts.iter().filter(|g| g.sheet == sheet) {
        gdt_prims(&mut out, g);
    }
    if let Some((ai, pos)) = d.bom {
        if let Some(a) = app.ws().assemblies.get(ai).filter(|_| d.bom_sheet == sheet) {
            bom_prims(app, a, pos, &mut out);
        }
    }
    out
}

/// Open a dimension group; `finish_dim` closes it with its definition.
fn start_dim(out: &mut Vec<Prim>) -> usize {
    out.push(Prim::Dim(DimInfo::default()));
    out.len() - 1
}

fn finish_dim(out: &mut Vec<Prim>, mark: usize, info: DimInfo) {
    out[mark] = Prim::Dim(info);
    out.push(Prim::End);
}

/// A view's lines: visible edges, thin tangent edges, hidden edges, hatching, centre marks
/// on circles seen square on and centre lines on holes and cylinders seen from the side.
fn view_geometry(out: &mut Vec<Prim>, v: &DrawView, p: &ProjView) {
    curve_prims(out, v, p, &p.visible, 0.5, false, INK);
    curve_prims(out, v, p, &p.smooth, 0.18, false, THIN_INK);
    if v.hidden_lines {
        curve_prims(out, v, p, &p.hidden, 0.25, true, [90, 90, 90]);
    }
    for h in &p.hatch {
        out.push(Prim::Line { a: to_sheet(v, Some(p), h[0]), b: to_sheet(v, Some(p), h[1]), w: 0.18, dashed: false, color: [70, 70, 70] });
    }
    if v.dir == "iso" {
        return;
    }
    let chain = [60, 60, 60];
    for (c, r) in circle_centres(p) {
        let cs = DVec2::from(to_sheet(v, Some(p), c));
        let rs = r * v.scale;
        if rs < 1.5 {
            continue;
        }
        let e = rs + 2.0;
        for d in [DVec2::X, DVec2::Y] {
            out.push(Prim::Chain { a: (cs - d * e).into(), b: (cs + d * e).into(), w: 0.18, color: chain });
        }
    }
    // circles seen edge on, sharing an axis line: holes and cylinders from the side
    let mut axes: Vec<(DVec2, DVec2, f64, f64, f64)> = vec![]; // point, direction, min, max along, radius
    for e in &p.edges {
        let Some([cx, cy, ax, ay]) = e.axis else { continue };
        let (c, d) = (DVec2::new(cx, cy), DVec2::new(ax, ay).normalize_or_zero());
        if d.length() < 0.5 {
            continue;
        }
        let r = e.pts.iter().map(|q| (DVec2::from(*q) - c).length()).fold(0.0, f64::max);
        match axes.iter_mut().find(|x| x.1.perp_dot(d).abs() < 1e-6 && (c - x.0).perp_dot(x.1).abs() < 1e-4 * (1.0 + r)) {
            Some(x) => {
                let s = (c - x.0).dot(x.1);
                x.2 = x.2.min(s);
                x.3 = x.3.max(s);
                x.4 = x.4.max(r);
            }
            None => axes.push((c, d, 0.0, 0.0, r)),
        }
    }
    for (c, d, s0, s1, r) in axes {
        if s1 - s0 < 1e-6 || r * v.scale < 1.0 {
            continue;
        }
        let ext = 2.0 / v.scale;
        let (a, b) = (c + d * (s0 - ext), c + d * (s1 + ext));
        out.push(Prim::Chain { a: to_sheet(v, Some(p), a.into()), b: to_sheet(v, Some(p), b.into()), w: 0.18, color: chain });
    }
}

/// The arc (centre, radius, start and end angles) a radius dimension measures, if the view shows one.
fn arc_of(p: Option<&ProjView>, c: [f64; 2], r: f64) -> Option<(f64, f64)> {
    let p = p?;
    for pl in &p.visible {
        if let Some((cc, rr, false)) = fit_circle(pl) {
            if DVec2::from(cc).distance(DVec2::from(c)) < 1e-3 * (1.0 + r) && (rr - r).abs() < 1e-3 * (1.0 + r) {
                return Some(arc_span(pl, cc));
            }
        }
    }
    None
}

/// The point at the middle of the arc of radius `r` about `c` in a view (for radius dimensions
/// given by centre and radius), else the point at 45°.
pub fn arc_mid_point(p: Option<&ProjView>, c: [f64; 2], r: f64) -> [f64; 2] {
    match arc_of(p, c, r) {
        Some((a0, a1)) => {
            let m = (a0 + a1) / 2.0;
            [c[0] + r * m.cos(), c[1] + r * m.sin()]
        }
        None => [c[0] + r * 0.7071, c[1] + r * 0.7071],
    }
}

/// The view a section's cutting line is drawn on: its parent, else a view of the same thing
/// that sees the cutting plane edge on.
pub fn section_parent<'a>(d: &'a Drawing, s: &DrawView) -> Option<&'a DrawView> {
    if let Some(p) = s.parent.and_then(|id| d.views.iter().find(|v| v.id == id)) {
        return Some(p);
    }
    let (_, _, ds) = view_axes(&s.dir);
    d.views.iter().find(|v| {
        v.id != s.id && v.detail.is_none() && !is_section(v) && v.dir != "iso" && v.studio == s.studio && v.assembly == s.assembly && view_axes(&v.dir).2.dot(ds).abs() < 1e-6
    })
}

/// The cutting line of section `s` drawn on view `pv`: a chain line with thick ends, arrows in
/// the direction of sight, and the section's letter at both ends.
fn cutting_line(app: &dyn DocCtx, d: &Drawing, s: &DrawView, pv: &DrawView, out: &mut Vec<Prim>) {
    let Some(pp) = view_proj(app, d, pv) else { return };
    let Some(cut) = s.cut.or_else(|| view_proj(app, d, s).and_then(|p| p.cut)) else { return };
    let (vx, vy, _) = view_axes(&pv.dir);
    let (_, _, ds) = view_axes(&s.dir);
    let n = DVec2::new(vx.dot(ds), vy.dot(ds));
    if (n.length() - 1.0).abs() > 1e-6 {
        return;
    }
    let t = n.perp();
    let q0 = n * cut;
    let corners = [pp.bbox[0], [pp.bbox[1][0], pp.bbox[0][1]], pp.bbox[1], [pp.bbox[0][0], pp.bbox[1][1]]];
    let along: Vec<f64> = corners.iter().map(|c| (DVec2::from(*c) - q0).dot(t)).collect();
    let ext = 4.0 / pv.scale;
    let (s0, s1) = (along.iter().cloned().fold(f64::INFINITY, f64::min) - ext, along.iter().cloned().fold(f64::NEG_INFINITY, f64::max) + ext);
    let a = DVec2::from(to_sheet(pv, Some(pp), (q0 + t * s0).into()));
    let b = DVec2::from(to_sheet(pv, Some(pp), (q0 + t * s1).into()));
    let ink = INK;
    out.push(Prim::Chain { a: a.into(), b: b.into(), w: 0.25, color: ink });
    let dir = (b - a).normalize_or_zero();
    // the viewer of the section looks against its direction
    let sight = -n;
    for (end, inward) in [(a, dir), (b, -dir)] {
        out.push(Prim::Line { a: end.into(), b: (end + inward * 5.0).into(), w: 0.7, dashed: false, color: ink });
        let tip = end + inward * 2.0 + sight * 7.0;
        let tail = end + inward * 2.0;
        out.push(Prim::Line { a: tail.into(), b: tip.into(), w: 0.35, dashed: false, color: ink });
        arrow(out, tip, -sight, ink);
        // the letter beside the arrow head, on the outer side, so close cutting lines keep apart
        let at = tip - inward * 3.5 + DVec2::new(0.0, -2.5);
        out.push(Prim::Text { at: [at.x, at.y], size: 5.0, text: s.letter.clone(), anchor: 1, color: ink });
    }
}

/// Item balloons of an assembly view: one per BOM line, on the first part of that kind.
fn balloon_prims(app: &dyn DocCtx, v: &DrawView, p: &ProjView, out: &mut Vec<Prim>) {
    let Some(ai) = v.assembly else { return };
    let Some(a) = app.ws().assemblies.get(ai) else { return };
    let rows = crate::asm_ui::bom(app, a);
    let keys: Vec<(usize, BodyId)> = asm_items(app.ws(), ai).iter().map(|x| (x.0, x.1)).collect();
    let row_keys: Vec<(usize, BodyId)> = {
        // the BOM's rows in order: first leaf of each (studio, body)
        let mut k: Vec<(usize, BodyId)> = vec![];
        for x in &a.instances {
            for l in crate::assembly::leaves_with(&app.ws().assemblies, x, true) {
                if !k.contains(&(l.studio, l.body)) {
                    k.push((l.studio, l.body));
                }
            }
        }
        k
    };
    let mid = DVec2::from(to_sheet(v, Some(p), [(p.bbox[0][0] + p.bbox[1][0]) / 2.0, (p.bbox[0][1] + p.bbox[1][1]) / 2.0]));
    let lo = DVec2::from(to_sheet(v, Some(p), p.bbox[0]));
    let hi = DVec2::from(to_sheet(v, Some(p), p.bbox[1]));
    let reach = (hi - lo).abs().length() / 2.0 + 10.0;
    let mut placed: Vec<DVec2> = vec![];
    for (ri, key) in row_keys.iter().enumerate() {
        let Some(k) = keys.iter().position(|x| x == key) else { continue };
        let Some(q) = p.items.get(k) else { continue };
        let Some(row) = rows.get(ri) else { continue };
        let at = DVec2::from(to_sheet(v, Some(p), *q));
        let mut dir = (at - mid).normalize_or(DVec2::new(1.0, 1.0).normalize());
        // keep balloons apart
        let mut c = mid + dir * reach;
        for _ in 0..24 {
            if placed.iter().all(|o| o.distance(c) > 10.0) {
                break;
            }
            dir = DVec2::from_angle(0.25).rotate(dir);
            c = mid + dir * reach;
        }
        placed.push(c);
        let r = 4.0;
        out.push(Prim::Arc { c: c.into(), r, a0: 0.0, a1: std::f64::consts::TAU, w: 0.3, dashed: false, color: INK });
        let from = c + (at - c).normalize_or_zero() * r;
        out.push(Prim::Line { a: from.into(), b: at.into(), w: 0.25, dashed: false, color: INK });
        out.push(Prim::Arc { c: at.into(), r: 0.5, a0: 0.0, a1: std::f64::consts::TAU, w: 0.5, dashed: false, color: INK });
        out.push(Prim::Text { at: [c.x, c.y - 1.5], size: 4.0, text: row.item.to_string(), anchor: 1, color: INK });
    }
}

/// Border cells of the title block with their fields.
fn title_block(app: &dyn DocCtx, d: &Drawing, sheet: usize, on_sheet: &[&DrawView], out: &mut Vec<Prim>) {
    let ([x0, y0], [x1, y1]) = title_rect(d);
    let line = |out: &mut Vec<Prim>, a: [f64; 2], b: [f64; 2], w: f64| out.push(Prim::Line { a, b, w, dashed: false, color: INK });
    let main = on_sheet.iter().copied().find(|v| v.detail.is_none()).or_else(|| d.views.iter().find(|v| v.detail.is_none()));
    let units = main.map(|v| units_of(app, v.studio)).unwrap_or_default();
    let scale = main.map(|v| fmt_scale(v.scale)).unwrap_or_default();
    let (part, material, stock) = shown_part(app, main);
    let t = &d.title;
    let part = if t.part.is_empty() { part } else { t.part.clone() };
    let material = if t.material.is_empty() { material } else { t.material.clone() };
    let stock = match t.stock.trim() {
        "" => stock,
        s if s.eq_ignore_ascii_case("none") => String::new(),
        s => s.to_string(),
    };
    // outline and rows
    for (a, b) in [([x0, y0], [x0, y1]), ([x0, y1], [x1, y1]), ([x0, y0 + 12.0], [x1, y0 + 12.0]), ([x0, y0 + 24.0], [x1 - 24.0, y0 + 24.0])] {
        line(out, a, b, 0.35);
    }
    let cell = |out: &mut Vec<Prim>, x: f64, y: f64, label: &str, value: &str, size: f64| {
        out.push(Prim::Text { at: [x + 1.5, y + 9.2], size: 2.0, text: label.into(), anchor: 0, color: [110, 110, 110] });
        if !value.is_empty() {
            out.push(Prim::Text { at: [x + 1.5, y + 2.2], size, text: value.into(), anchor: 0, color: INK });
        }
    };
    // top row: drawing name; projection symbol on the right of the upper two rows
    out.push(Prim::Text { at: [x0 + 3.0, y0 + 27.0], size: 6.0, text: d.name.clone(), anchor: 0, color: INK });
    line(out, [x1 - 24.0, y0 + 12.0], [x1 - 24.0, y1], 0.25);
    projection_symbol(out, [x1 - 12.0, y0 + 26.5], t.first_angle);
    out.push(Prim::Text { at: [x1 - 12.0, y0 + 13.6], size: 2.0, text: if t.first_angle { "FIRST ANGLE" } else { "THIRD ANGLE" }.into(), anchor: 1, color: [110, 110, 110] });
    // middle row
    let mut x = x0;
    for (wd, label, value) in [(64.0, "PART", part.as_str()), (38.0, "PART NO.", t.part_no.as_str()), (44.0, "MATERIAL", material.as_str())] {
        if label == "MATERIAL" && !stock.is_empty() {
            // the stock in small type under the material
            cell(out, x, y0 + 12.0, label, "", 3.0);
            out.push(Prim::Text { at: [x + 1.5, y0 + 17.4], size: if value.chars().count() as f64 * 1.8 > wd { 2.5 } else { 3.0 }, text: value.into(), anchor: 0, color: INK });
            out.push(Prim::Text { at: [x + 1.5, y0 + 13.6], size: if stock.chars().count() as f64 * 1.3 > wd { 1.8 } else { 2.2 }, text: stock.clone(), anchor: 0, color: INK });
        } else {
            cell(out, x, y0 + 12.0, label, value, if value.chars().count() as f64 * 1.9 > wd { 2.6 } else { 3.2 });
        }
        x += wd;
        line(out, [x, y0 + 12.0], [x, y0 + 24.0], 0.25);
    }
    // bottom row
    let sheet_txt = format!("{} of {}", sheet + 1, d.sheet_count());
    let mut x = x0;
    let row = [
        (24.0, "SCALE", scale.as_str()),
        (18.0, "UNITS", units.label()),
        (14.0, "REV", t.revision.as_str()),
        (23.0, "DATE", t.date.as_str()),
        (32.0, "DRAWN", t.drawn_by.as_str()),
        (35.0, "GENERAL TOL.", t.tolerance.as_str()),
        (24.0, "SHEET", sheet_txt.as_str()),
    ];
    for (i, (wd, label, value)) in row.iter().enumerate() {
        cell(out, x, y0, label, value, if value.chars().count() as f64 * 1.9 > *wd { 2.6 } else { 3.2 });
        x += wd;
        if i + 1 < row.len() {
            line(out, [x, y0], [x, y0 + 12.0], 0.25);
        }
    }
}

/// The ISO projection symbol: a truncated cone's side view and end view, centred at `c`.
fn projection_symbol(out: &mut Vec<Prim>, c: [f64; 2], first_angle: bool) {
    let (cone_x, circ_x) = if first_angle { (c[0] + 3.5, c[0] - 5.5) } else { (c[0] - 5.5, c[0] + 4.5) };
    let (h0, h1, len) = (1.6, 3.0, 7.0);
    let l = |out: &mut Vec<Prim>, a: [f64; 2], b: [f64; 2]| out.push(Prim::Line { a, b, w: 0.25, dashed: false, color: INK });
    let (xa, xb) = (cone_x - len / 2.0, cone_x + len / 2.0);
    // the cone's side view, wide end on the left and narrow end on the right (ISO 5456-2, the same
    // in both symbols); its end view, seen from the narrow end, beside it: on the right in
    // third-angle (the cone points at it), on the left in first-angle
    let (ha, hb) = (h1, h0);
    l(out, [xa, c[1] - ha], [xa, c[1] + ha]);
    l(out, [xb, c[1] - hb], [xb, c[1] + hb]);
    l(out, [xa, c[1] + ha], [xb, c[1] + hb]);
    l(out, [xa, c[1] - ha], [xb, c[1] - hb]);
    for r in [h0, h1] {
        out.push(Prim::Arc { c: [circ_x, c[1]], r, a0: 0.0, a1: std::f64::consts::TAU, w: 0.25, dashed: false, color: INK });
    }
    let (lo, hi) = (xa.min(circ_x - h1) - 1.0, xb.max(circ_x + h1) + 1.0);
    out.push(Prim::Chain { a: [lo, c[1]], b: [hi, c[1]], w: 0.15, color: INK });
}

fn arrow(out: &mut Vec<Prim>, tip: DVec2, dir: DVec2, color: [u8; 3]) {
    // `dir` points from the tip back along the shaft
    let d = dir.normalize_or_zero();
    let back = tip + d * 3.0;
    out.push(Prim::Line { a: tip.into(), b: (back + d.perp() * 0.9).into(), w: 0.25, dashed: false, color });
    out.push(Prim::Line { a: tip.into(), b: (back - d.perp() * 0.9).into(), w: 0.25, dashed: false, color });
}

/// Radius or diameter: leader from the circle outwards with a shoulder and the text. A radius
/// of an arc points from the arc's centre through the middle of the arc when the picked point
/// is off the arc.
#[allow(clippy::too_many_arguments)]
fn radial_prims(out: &mut Vec<Prim>, c: [f64; 2], on: [f64; 2], diameter: bool, offset: f64, text: &str, stack: &Option<(String, String)>, color: [u8; 3], arc: Option<(f64, f64)>, info: &mut DimInfo) {
    let (c, on) = (DVec2::from(c), DVec2::from(on));
    let mut dir = (on - c).normalize_or(DVec2::X);
    if let Some((a0, a1)) = arc {
        let a = dir.y.atan2(dir.x);
        let on_arc = (0..3).any(|k| {
            let a = a + (k as f64 - 1.0) * std::f64::consts::TAU;
            a >= a0 - 1e-6 && a <= a1 + 1e-6
        });
        if !on_arc {
            dir = DVec2::from_angle((a0 + a1) / 2.0);
        }
    }
    let r = (on - c).length();
    let p = c + dir * r;
    let end = p + dir * offset.max(2.0);
    let start = if diameter { c - dir * r } else { c };
    out.push(Prim::Line { a: start.into(), b: end.into(), w: 0.25, dashed: false, color });
    arrow(out, p, -dir, color);
    if diameter {
        arrow(out, c - dir * r, dir, color);
    }
    let sh = if dir.x >= 0.0 { 4.0 } else { -4.0 };
    let tip = end + DVec2::new(sh, 0.0);
    out.push(Prim::Line { a: end.into(), b: tip.into(), w: 0.25, dashed: false, color });
    let at = tip + DVec2::new(if sh > 0.0 { 0.8 } else { -0.8 }, -1.2);
    label_prims(out, at.into(), if sh > 0.0 { 0 } else { 2 }, text, stack, color);
    info.pts = vec![c.into(), p.into()];
    info.line = end.into();
    info.text_at = at.into();
    info.text = text.split('\n').next().unwrap_or("").to_string();
}

/// Angle between two lines, drawn as an arc of radius `r` about their intersection.
fn angle_prims(out: &mut Vec<Prim>, l1: [[f64; 2]; 2], l2: [[f64; 2]; 2], r: f64, color: [u8; 3], info: &mut DimInfo) {
    let (a0, a1, b0, b1) = (DVec2::from(l1[0]), DVec2::from(l1[1]), DVec2::from(l2[0]), DVec2::from(l2[1]));
    let (da, db) = (a1 - a0, b1 - b0);
    let den = da.perp_dot(db);
    if den.abs() < 1e-9 {
        return;
    }
    let t = (b0 - a0).perp_dot(db) / den;
    let x = a0 + da * t;
    // each ray points from the intersection towards its line's far end
    let ray = |p: DVec2, q: DVec2| if (p - x).length() > (q - x).length() { (p - x).normalize() } else { (q - x).normalize() };
    let (u, v) = (ray(a0, a1), ray(b0, b1));
    let ang = u.angle_to(v);
    let n = 32;
    let r = r.max(3.0);
    let pts: Vec<DVec2> = (0..=n).map(|i| x + DVec2::from_angle(ang * i as f64 / n as f64).rotate(u) * r).collect();
    for w in pts.windows(2) {
        out.push(Prim::Line { a: w[0].into(), b: w[1].into(), w: 0.25, dashed: false, color });
    }
    let s = ang.signum();
    arrow(out, pts[0], u.perp() * s, color);
    arrow(out, pts[n], -v.perp() * s, color);
    // extension lines where the arc lies beyond a line's extent
    for (p, q, e) in [(a0, a1, pts[0]), (b0, b1, pts[n])] {
        let near = if (p - e).length() < (q - e).length() { p } else { q };
        let along = (e - x).dot((near - x).normalize_or_zero());
        if along > (near - x).length() + 0.5 {
            out.push(Prim::Line { a: (near + (e - near).normalize() * 1.0).into(), b: (e + (e - near).normalize() * 2.0).into(), w: 0.25, dashed: false, color });
        }
    }
    let mid = x + DVec2::from_angle(ang / 2.0).rotate(u) * (r + 2.5);
    let text = format!("{}°", crate::expr::trim(ang.abs().to_degrees(), 1));
    out.push(Prim::Text { at: (mid + DVec2::new(0.0, -1.2)).into(), size: 3.5, text: text.clone(), anchor: 1, color });
    info.pts = vec![a0.into(), a1.into(), b0.into(), b1.into()];
    info.line = pts[n / 2].into();
    info.text_at = mid.into();
    info.text = text;
}

const GDT_H: f64 = 7.0;

/// Sizes of a feature control frame's cells.
fn gdt_cells(g: &Gdt) -> Vec<f64> {
    let tol_w = (g.tol.chars().count() as f64 * 2.2 + 3.0).max(GDT_H);
    let mut cells = vec![GDT_H, tol_w];
    for d in g.datums.split_whitespace() {
        cells.push((d.chars().count() as f64 * 2.2 + 3.0).max(GDT_H));
    }
    cells
}

/// Sheet rectangle of a GD&T annotation (for picking).
pub fn gdt_rect(g: &Gdt) -> ([f64; 2], [f64; 2]) {
    if g.kind == "datum" {
        return (g.pos, [g.pos[0] + GDT_H, g.pos[1] + GDT_H]);
    }
    let w: f64 = gdt_cells(g).iter().sum();
    (g.pos, [g.pos[0] + w, g.pos[1] + GDT_H])
}

fn gdt_prims(out: &mut Vec<Prim>, g: &Gdt) {
    use DVec2;
    let ink = INK;
    let line = |out: &mut Vec<Prim>, a: DVec2, b: DVec2| out.push(Prim::Line { a: a.into(), b: b.into(), w: 0.3, dashed: false, color: ink });
    let rect = |out: &mut Vec<Prim>, lo: DVec2, hi: DVec2| {
        line(out, lo, DVec2::new(hi.x, lo.y));
        line(out, DVec2::new(hi.x, lo.y), hi);
        line(out, hi, DVec2::new(lo.x, hi.y));
        line(out, DVec2::new(lo.x, hi.y), lo);
    };
    let p = DVec2::from(g.pos);
    if g.kind == "datum" {
        rect(out, p, p + DVec2::splat(GDT_H));
        out.push(Prim::Text { at: [p.x + GDT_H / 2.0, p.y + 1.6], size: 4.0, text: g.tol.clone(), anchor: 1, color: ink });
        if let Some(l) = g.leader {
            let l = DVec2::from(l);
            let top = p + DVec2::new(GDT_H / 2.0, 0.0);
            let top = if l.y > p.y + GDT_H { p + DVec2::new(GDT_H / 2.0, GDT_H) } else { top };
            let d = (l - top).normalize_or(DVec2::NEG_Y);
            let base = l - d * 2.6;
            line(out, top, base);
            // filled datum triangle on the feature
            let s = d.perp() * 1.5;
            for k in 0..=6 {
                let f = k as f64 / 6.0;
                line(out, base + s * (1.0 - 2.0 * f), l);
            }
            line(out, base + s, base - s);
        }
        return;
    }
    let cells = gdt_cells(g);
    let w: f64 = cells.iter().sum();
    rect(out, p, p + DVec2::new(w, GDT_H));
    let mut x = p.x;
    for c in &cells[..cells.len() - 1] {
        x += c;
        line(out, DVec2::new(x, p.y), DVec2::new(x, p.y + GDT_H));
    }
    gdt_symbol(out, &g.kind, p + DVec2::splat(GDT_H / 2.0));
    out.push(Prim::Text { at: [p.x + GDT_H + 1.5, p.y + 1.8], size: 3.5, text: g.tol.clone(), anchor: 0, color: ink });
    let mut x = p.x + cells[0] + cells[1];
    for (i, d) in g.datums.split_whitespace().enumerate() {
        out.push(Prim::Text { at: [x + cells[2 + i] / 2.0, p.y + 1.8], size: 3.5, text: d.into(), anchor: 1, color: ink });
        x += cells[2 + i];
    }
    if let Some(l) = g.leader {
        let l = DVec2::from(l);
        let start = if l.x < p.x { p + DVec2::new(0.0, GDT_H / 2.0) } else { p + DVec2::new(w, GDT_H / 2.0) };
        let knee = DVec2::new(start.x + if l.x < p.x { -4.0 } else { 4.0 }, start.y);
        line(out, start, knee);
        line(out, knee, l);
        arrow(out, l, knee - l, ink);
    }
}

/// The characteristic symbol of a GD&T frame, drawn in lines about `c` (cell 7 mm).
fn gdt_symbol(out: &mut Vec<Prim>, kind: &str, c: DVec2) {
    use DVec2;
    let mut segs: Vec<(DVec2, DVec2)> = vec![];
    let circle = |segs: &mut Vec<(DVec2, DVec2)>, o: DVec2, r: f64, a0: f64, a1: f64| {
        let n = 24;
        for i in 0..n {
            let (t0, t1) = (a0 + (a1 - a0) * i as f64 / n as f64, a0 + (a1 - a0) * (i + 1) as f64 / n as f64);
            segs.push((o + DVec2::from_angle(t0) * r, o + DVec2::from_angle(t1) * r));
        }
    };
    let v = |x: f64, y: f64| c + DVec2::new(x, y);
    let tau = std::f64::consts::TAU;
    let pi = std::f64::consts::PI;
    match kind {
        "straightness" => segs.push((v(-2.5, 0.0), v(2.5, 0.0))),
        "flatness" => {
            let q = [v(-2.5, -1.2), v(1.5, -1.2), v(2.5, 1.2), v(-1.5, 1.2)];
            for i in 0..4 {
                segs.push((q[i], q[(i + 1) % 4]));
            }
        }
        "circularity" => circle(&mut segs, c, 2.0, 0.0, tau),
        "cylindricity" => {
            circle(&mut segs, c, 1.6, 0.0, tau);
            segs.push((v(-2.6, -2.2), v(-0.6, 2.2)));
            segs.push((v(0.6, -2.2), v(2.6, 2.2)));
        }
        "perpendicularity" => {
            segs.push((v(-2.5, -2.0), v(2.5, -2.0)));
            segs.push((v(0.0, -2.0), v(0.0, 2.5)));
        }
        "parallelism" => {
            segs.push((v(-2.0, -2.2), v(0.0, 2.2)));
            segs.push((v(0.0, -2.2), v(2.0, 2.2)));
        }
        "angularity" => {
            segs.push((v(-2.5, -2.0), v(2.5, -2.0)));
            segs.push((v(-2.5, -2.0), v(2.0, 2.2)));
        }
        "position" => {
            circle(&mut segs, c, 1.7, 0.0, tau);
            segs.push((v(-2.7, 0.0), v(2.7, 0.0)));
            segs.push((v(0.0, -2.7), v(0.0, 2.7)));
        }
        "concentricity" => {
            circle(&mut segs, c, 1.0, 0.0, tau);
            circle(&mut segs, c, 2.2, 0.0, tau);
        }
        "symmetry" => {
            segs.push((v(-2.6, 0.0), v(2.6, 0.0)));
            segs.push((v(-1.6, 1.5), v(1.6, 1.5)));
            segs.push((v(-1.6, -1.5), v(1.6, -1.5)));
        }
        "profileLine" => circle(&mut segs, v(0.0, -1.2), 2.5, 0.0, pi),
        "profileSurface" => {
            circle(&mut segs, v(0.0, -1.2), 2.5, 0.0, pi);
            segs.push((v(-2.5, -1.2), v(2.5, -1.2)));
        }
        "runout" | "totalRunout" => {
            segs.push((v(-1.5, -2.2), v(1.5, 2.2)));
            segs.push((v(1.5, 2.2), v(1.6, 0.6)));
            segs.push((v(1.5, 2.2), v(0.2, 1.4)));
            if kind == "totalRunout" {
                segs.push((v(-0.2, -2.2), v(2.8, 2.2)));
                segs.push((v(-2.5, -2.2), v(2.5, -2.2)));
            }
        }
        _ => {}
    }
    for (a, b) in segs {
        out.push(Prim::Line { a: a.into(), b: b.into(), w: 0.3, dashed: false, color: INK });
    }
}

/// A circle or circular arc through a projected polyline: (centre, radius, full circle).

fn bom_prims(app: &dyn DocCtx, a: &crate::assembly::Assembly, pos: [f64; 2], out: &mut Vec<Prim>) {
    let rows = crate::asm_ui::bom(app, a);
    // the stock column only when some part is cut from stock
    let cols: &[f64] = if rows.iter().any(|r| !r.stock.is_empty()) { &[12.0, 60.0, 14.0, 34.0, 56.0] } else { &[12.0, 60.0, 14.0, 34.0] };
    let rh = 6.0;
    let w: f64 = cols.iter().sum();
    let n = rows.len() + 1;
    let (x0, y0) = (pos[0], pos[1]);
    for i in 0..=n {
        let y = y0 - i as f64 * rh;
        out.push(Prim::Line { a: [x0, y], b: [x0 + w, y], w: if i <= 1 { 0.35 } else { 0.2 }, dashed: false, color: INK });
    }
    let mut x = x0;
    for c in cols.iter().chain(std::iter::once(&0.0)) {
        out.push(Prim::Line { a: [x, y0], b: [x, y0 - n as f64 * rh], w: 0.25, dashed: false, color: INK });
        x += c;
    }
    let cells = |i: usize, vals: Vec<String>, out: &mut Vec<Prim>| {
        let mut x = x0;
        for (k, v) in vals.into_iter().take(cols.len()).enumerate() {
            out.push(Prim::Text { at: [x + 1.5, y0 - (i as f64 + 1.0) * rh + 1.7], size: 3.0, text: v, anchor: 0, color: INK });
            x += cols[k];
        }
    };
    cells(0, ["ITEM", "PART", "QTY", "MATERIAL", "STOCK"].map(String::from).to_vec(), out);
    for (i, r) in rows.iter().enumerate() {
        cells(i + 1, vec![r.item.to_string(), r.name.clone(), r.qty.to_string(), r.material.clone(), r.stock.clone()], out);
    }
}


/// A detail view: part of another view, clipped to a circle and enlarged.
fn detail_prims(app: &dyn DocCtx, d: &Drawing, v: &DrawView, src: u64, c: [f64; 2], r: f64, out: &mut Vec<Prim>) {
    let Some(sv) = d.views.iter().find(|x| x.id == src) else { return };
    let Some(p) = view_proj(app, d, sv) else { return };
    let cc = DVec2::from(c);
    let map = |q: DVec2| -> [f64; 2] { [v.pos[0] + (q.x - cc.x) * v.scale, v.pos[1] + (q.y - cc.y) * v.scale] };
    let clipped = |pls: &[Vec<[f64; 2]>], w: f64, dashed: bool, color: [u8; 3], out: &mut Vec<Prim>| {
        for pl in pls {
            for s in pl.windows(2) {
                let (a, b) = (DVec2::from(s[0]), DVec2::from(s[1]));
                if let Some((a, b)) = clip_circle(a, b, cc, r) {
                    out.push(Prim::Line { a: map(a), b: map(b), w, dashed, color });
                }
            }
        }
    };
    clipped(&p.visible, 0.5, false, INK, out);
    clipped(&p.smooth, 0.18, false, THIN_INK, out);
    // the detail shows hidden lines when it or its source view does
    if v.hidden_lines || sv.hidden_lines {
        clipped(&p.hidden, 0.25, true, [90, 90, 90], out);
    }
    let hatch: Vec<Vec<[f64; 2]>> = p.hatch.iter().map(|h| vec![h[0], h[1]]).collect();
    clipped(&hatch, 0.18, false, [70, 70, 70], out);
    // boundary circle here and on the source view
    let grey = [90, 90, 90];
    out.push(Prim::Arc { c: v.pos, r: r * v.scale, a0: 0.0, a1: std::f64::consts::TAU, w: 0.25, dashed: true, color: grey });
    if sv.sheet == v.sheet {
        out.push(Prim::Arc { c: to_sheet(sv, Some(p), c), r: r * sv.scale, a0: 0.0, a1: std::f64::consts::TAU, w: 0.25, dashed: true, color: grey });
        if !v.letter.is_empty() {
            let at = DVec2::from(to_sheet(sv, Some(p), c)) + DVec2::new(1.0, 1.0).normalize() * (r * sv.scale + 2.5);
            out.push(Prim::Text { at: at.into(), size: 5.0, text: v.letter.clone(), anchor: 0, color: INK });
        }
    }
    let title = if v.letter.is_empty() { "DETAIL".to_string() } else { format!("DETAIL {}", v.letter) };
    out.push(Prim::Text { at: [v.pos[0], v.pos[1] - r * v.scale - 7.0], size: 3.2, text: format!("{title} ({})", fmt_scale(v.scale)), anchor: 1, color: [80, 80, 80] });
}

fn clip_circle(a: DVec2, b: DVec2, c: DVec2, r: f64) -> Option<(DVec2, DVec2)> {
    let d = b - a;
    let f = a - c;
    let (qa, qb, qc) = (d.dot(d), 2.0 * f.dot(d), f.dot(f) - r * r);
    if qa < 1e-18 {
        return (qc <= 0.0).then_some((a, b));
    }
    let disc = qb * qb - 4.0 * qa * qc;
    if disc <= 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    let (t0, t1) = (((-qb - sq) / (2.0 * qa)).max(0.0), ((-qb + sq) / (2.0 * qa)).min(1.0));
    (t0 < t1).then(|| (a + d * t0, a + d * t1))
}


/// A linear dimension: extension lines, the dimension line with arrows, and the text from `fmt`
/// (given the measured sheet length).
#[allow(clippy::too_many_arguments)]
fn dim_prims(out: &mut Vec<Prim>, a: [f64; 2], b: [f64; 2], kind: u8, offset: f64, color: [u8; 3], info: &mut DimInfo, fmt: impl Fn(f64) -> (String, Option<(String, String)>)) {
    let (a, b) = (DVec2::from(a), DVec2::from(b));
    let (dir, len) = match kind {
        1 => (DVec2::X, (b.x - a.x).abs()),
        2 => (DVec2::Y, (b.y - a.y).abs()),
        _ => ((b - a).normalize_or(DVec2::X), (b - a).length()),
    };
    let n = dir.perp();
    // dimension line endpoints: feet of a and b on the line offset from a
    let base = a + n * offset;
    let pa = base;
    let pb = base + dir * (b - a).dot(dir);
    let ext = |out: &mut Vec<Prim>, from: DVec2, to: DVec2| {
        let d = (to - from).normalize_or_zero();
        out.push(Prim::Line { a: (from + d * 1.0).into(), b: (to + d * 2.0).into(), w: 0.25, dashed: false, color });
    };
    ext(out, a, pa);
    ext(out, b, pb);
    out.push(Prim::Line { a: pa.into(), b: pb.into(), w: 0.25, dashed: false, color });
    // arrowheads
    let d = (pb - pa).normalize_or_zero();
    for (p, s) in [(pa, 1.0), (pb, -1.0)] {
        let tip = p;
        let back = p + d * 3.0 * s;
        out.push(Prim::Line { a: tip.into(), b: (back + d.perp() * 0.9).into(), w: 0.25, dashed: false, color });
        out.push(Prim::Line { a: tip.into(), b: (back - d.perp() * 0.9).into(), w: 0.25, dashed: false, color });
    }
    let mid = (pa + pb) / 2.0;
    let (main, stack) = fmt(len);
    let (at, anchor) = if kind == 2 || (kind == 0 && d.x.abs() < 0.3) {
        // vertical-ish: text beside the line
        (mid + DVec2::new(-1.5, -1.5), 2)
    } else {
        (mid + n.abs() * 1.2, 1)
    };
    label_prims(out, at.into(), anchor, &main, &stack, color);
    info.pts = vec![a.into(), b.into()];
    info.line = pa.into();
    info.text_at = at.into();
    info.text = if main.is_empty() { stack.map(|s| format!("{}/{}", s.0, s.1)).unwrap_or_default() } else { main.split('\n').next().unwrap_or("").to_string() };
}


// ---------------------------------------------------------------- toolbar / sidebar

pub fn toolbar(app: &mut App, ui: &mut Ui) {
    App::group(ui, |ui| {
        ui.menu_button(RichText::new("Insert view").size(12.5), |ui| {
            // the directions of one Part Studio (or one part of it); sections looking back are left to the cut tools
            let dirs = |app: &mut App, ui: &mut Ui, si: usize, body: Option<BodyId>| {
                for (dir, label) in DIRS.iter().filter(|d| !matches!(d.0, "section:back" | "section:bottom" | "section:left")) {
                    if ui.button(*label).clicked() {
                        insert_view(app, si, dir, body, None);
                        ui.close();
                    }
                }
            };
            let one = app.ws.studios.len() == 1;
            for si in 0..app.ws.studios.len() {
                let name = app.ws.studios[si].name.clone();
                // one part of a Part Studio with several
                let parts: Vec<(BodyId, String)> = app.studio_bodies(si).iter().map(|b| (b.id, part_name(app, si, b.id))).collect();
                let body = |app: &mut App, ui: &mut Ui| {
                    dirs(app, ui, si, None);
                    if parts.len() > 1 {
                        ui.separator();
                        for (id, pname) in &parts {
                            ui.menu_button(format!("Only {pname}"), |ui| dirs(app, ui, si, Some(*id)));
                        }
                    }
                };
                if one {
                    body(app, ui);
                } else {
                    ui.menu_button(name, |ui| body(app, ui));
                }
            }
            if !app.ws.assemblies.is_empty() {
                ui.separator();
                for ai in 0..app.ws.assemblies.len() {
                    let name = app.ws.assemblies[ai].name.clone();
                    ui.menu_button(format!("Assembly {name}"), |ui| {
                        for (dir, label) in DIRS.iter().filter(|d| !d.0.starts_with("section:")) {
                            if ui.button(*label).clicked() {
                                insert_view(app, 0, dir, None, Some(ai));
                                ui.close();
                            }
                        }
                    });
                }
            }
            ui.separator();
            if ui.button("Standard views").clicked() {
                checkpoint(app);
                add_standard_views(app);
                ui.close();
            }
            if ui.button("Section: horizontal cut…").on_hover_text("Click where a view should be cut; the section looks across the cutting line").clicked() {
                app.dwg.tool = DTool::Section(true);
                ui.close();
            }
            if ui.button("Section: vertical cut…").on_hover_text("Click where a view should be cut; the section looks across the cutting line").clicked() {
                app.dwg.tool = DTool::Section(false);
                ui.close();
            }
            if ui.button("Detail of selected view").on_hover_text("Select a view first; the detail is centred on it at twice the scale").clicked() {
                add_detail(app);
                ui.close();
            }
            if !app.ws.assemblies.is_empty() {
                ui.separator();
                for (i, a) in app.ws.assemblies.iter().enumerate() {
                    if ui.button(format!("BOM table: {}", a.name)).clicked() {
                        let sheet = dwg(app).map(|d| sheet_size(&d.sheet)).unwrap_or([420.0, 297.0]);
                        checkpoint(app);
                        let cur = app.dwg.cur_sheet;
                        if let Some(d) = dwg_mut(app) {
                            d.bom = Some((i, [sheet[0] - 10.0 - 120.0, sheet[1] - 10.0]));
                            d.bom_sheet = cur;
                        }
                        ui.close();
                        break;
                    }
                }
            }
        });
    });
    App::group(ui, |ui| {
        let t = app.dwg.tool;
        if app.tool_button(ui, "select", "Select", "Select and move (S)", false, t == DTool::Select, true).clicked() {
            app.dwg.tool = DTool::Select;
        }
        if app.tool_button(ui, "dimension", "Dimension", "Dimension: click two points on a view (corners, line midpoints, circle centres or quadrants) (D)", true, t == DTool::Dimension, true).clicked() {
            app.dwg.tool = DTool::Dimension;
            app.dwg.dim_first = None;
        }
        if app.tool_button(ui, "dimRadial", "Radius / diameter", "Radius or diameter: click a circle or arc on a view (R)", false, t == DTool::Radial, true).clicked() {
            app.dwg.tool = DTool::Radial;
        }
        if app.tool_button(ui, "dimAngle", "Angle", "Angle: click two straight edges on a view (A)", false, t == DTool::Angle, true).clicked() {
            app.dwg.tool = DTool::Angle;
            app.dwg.angle_first = None;
        }
        let has_view = matches!(app.dwg.sel, Some(DSel::View(_)));
        if app.tool_button(ui, "autoDim", "Auto-dimension", "Add overall size, hole positions and hole callouts to the selected view; they follow the model", false, false, has_view).clicked() {
            auto_dimension(app);
        }
        if app.tool_button(ui, "text", "Note", "Note: click to place text (N)", false, t == DTool::Note, true).clicked() {
            app.dwg.tool = DTool::Note;
        }
        if app.tool_button(ui, "gdt", "Tolerance", "Geometric tolerance (GD&T) frame: click the feature it applies to", false, t == DTool::Gdt, true).clicked() {
            app.dwg.tool = DTool::Gdt;
        }
        if app.tool_button(ui, "datum", "Datum", "Datum feature symbol: click the feature", false, t == DTool::Datum, true).clicked() {
            app.dwg.tool = DTool::Datum;
        }
        if app.tool_button(ui, "delete", "Delete", "Delete the selection (Del)", false, false, app.dwg.sel.is_some()).clicked() {
            delete_sel(app);
        }
    });
    App::group(ui, |ui| {
        ui.menu_button(RichText::new("Export…").size(12.5), |ui| {
            for (f, l) in [("pdf", "PDF"), ("dxf", "DXF"), ("svg", "SVG")] {
                if ui.button(l).clicked() {
                    export(app, f);
                    ui.close();
                }
            }
        });
    });
}

/// A part's name in a Part Studio (its own name, else the body's).
fn part_name(app: &App, studio: usize, id: BodyId) -> String {
    app.studio_doc(studio).and_then(|d| d.parts.get(&crate::doc::body_key(id))).and_then(|p| p.name.clone()).or_else(|| app.studio_bodies(studio).iter().find(|b| b.id == id).map(|b| b.name.clone())).unwrap_or_default()
}

fn auto_dimension(app: &mut App) {
    let Some(DSel::View(vid)) = app.dwg.sel else { return };
    let Some(v) = dwg(app).and_then(|d| d.views.iter().find(|v| v.id == vid)).cloned() else { return };
    if v.detail.is_some() || v.dir == "iso" {
        app.flash("Auto-dimension works on orthographic views", ToastKind::Info);
        return;
    }
    if dwg(app).and_then(|d| view_proj(app, d, &v)).is_none() {
        return;
    }
    checkpoint(app);
    let Some(i) = dwg_index(app) else { return };
    let mut d = app.ws.drawings[i].clone();
    if let Some(x) = d.views.iter_mut().find(|x| x.id == vid) {
        x.auto = true;
        x.auto_off.clear();
    }
    refresh(app, &mut d);
    let n = d.dims.iter().filter(|x| x.view == vid && !x.auto.is_empty()).count();
    app.ws.drawings[i] = d;
    app.flash_info(format!("{n} dimensions; they follow the model"), Some(std::time::Duration::from_secs(2)));
}

fn add_detail(app: &mut App) {
    let Some(DSel::View(src)) = app.dwg.sel else {
        app.flash("Select a view to take the detail from", ToastKind::Info);
        return;
    };
    let Some(d) = dwg(app) else { return };
    let Some(sv) = d.views.iter().find(|v| v.id == src).cloned() else { return };
    let Some(p) = view_proj(app, d, &sv).cloned() else { return };
    let c = [(p.bbox[0][0] + p.bbox[1][0]) / 2.0, (p.bbox[0][1] + p.bbox[1][1]) / 2.0];
    let r = ((p.bbox[1][0] - p.bbox[0][0]).max(p.bbox[1][1] - p.bbox[0][1]) * 0.2).max(1.0);
    let size = r * sv.scale * 2.0 * 2.0;
    let pos = free_spot(app, d, sv.sheet, [size + 10.0, size + 16.0]).unwrap_or([sv.pos[0] + (p.bbox[1][0] - p.bbox[0][0]) * sv.scale * 0.5 + r * sv.scale * 2.0 + 20.0, sv.pos[1]]);
    let letter = next_letter(d);
    checkpoint(app);
    if let Some(d) = dwg_mut(app) {
        d.next_id += 1;
        let id = d.next_id;
        d.views.push(DrawView { id, studio: sv.studio, dir: sv.dir.clone(), pos, scale: sv.scale * 2.0, hidden_lines: sv.hidden_lines, detail: Some((src, c, r)), sheet: sv.sheet, letter, ..Default::default() });
        app.dwg.sel = Some(DSel::View(id));
    }
}

/// A view's name: its direction, numbered when the drawing has several of that kind ("Isometric 2").
pub fn view_label(d: &Drawing, v: &DrawView) -> String {
    if is_section(v) && !v.letter.is_empty() {
        return format!("Section {0}-{0}", v.letter);
    }
    let kind = |x: &DrawView| if x.detail.is_some() { "Detail".to_string() } else { DIRS.iter().find(|k| k.0 == x.dir).map(|k| k.1).unwrap_or("View").to_string() };
    let base = kind(v);
    let same: Vec<u64> = d.views.iter().filter(|x| kind(x) == base).map(|x| x.id).collect();
    if same.len() > 1 {
        let n = same.iter().position(|i| *i == v.id).unwrap_or(0) + 1;
        format!("{base} {n}")
    } else {
        base
    }
}

/// Largest extent of what a view shows (the diagonal, so any view fits), mm.
fn part_size(app: &dyn DocCtx, v: &DrawView) -> f64 {
    model_box(app, v).map(|(lo, hi)| (hi - lo).length()).unwrap_or(40.0)
}

/// Width and height of what a view shows, in model mm along the view's axes.
fn view_size(app: &dyn DocCtx, v: &DrawView) -> [f64; 2] {
    let Some((lo, hi)) = model_box(app, v) else { return [40.0, 40.0] };
    let (vx, vy, _) = view_axes(&v.dir);
    let corners = (0..8).map(|c| glam::DVec3::new([lo.x, hi.x][c & 1], [lo.y, hi.y][(c >> 1) & 1], [lo.z, hi.z][(c >> 2) & 1]));
    let span = |a: glam::DVec3| {
        let t: Vec<f64> = corners.clone().map(|p| p.dot(a)).collect();
        t.iter().cloned().fold(f64::NEG_INFINITY, f64::max) - t.iter().cloned().fold(f64::INFINITY, f64::min)
    };
    [span(vx), span(vy)]
}

/// The model-space box around what a view shows.
fn model_box(app: &dyn DocCtx, v: &DrawView) -> Option<(glam::DVec3, glam::DVec3)> {
    let mut lo = glam::DVec3::splat(f64::INFINITY);
    let mut hi = glam::DVec3::splat(f64::NEG_INFINITY);
    match v.assembly {
        Some(ai) => {
            for (s, b, m) in asm_items(app.ws(), ai) {
                if let Some(mesh) = app.bodies(s).iter().find(|x| x.id == b) {
                    for c in 0..8 {
                        let p = glam::DVec3::new(mesh.bbox[c & 1][0], mesh.bbox[(c >> 1) & 1][1], mesh.bbox[(c >> 2) & 1][2]);
                        let w = glam::DVec3::new(m[0] * p.x + m[1] * p.y + m[2] * p.z + m[3], m[4] * p.x + m[5] * p.y + m[6] * p.z + m[7], m[8] * p.x + m[9] * p.y + m[10] * p.z + m[11]);
                        lo = lo.min(w);
                        hi = hi.max(w);
                    }
                }
            }
        }
        None => {
            for b in app.bodies(v.studio).iter().filter(|b| v.body.map(|x| x == b.id).unwrap_or(true)) {
                lo = lo.min(b.bbox[0].into());
                hi = hi.max(b.bbox[1].into());
            }
        }
    }
    lo.x.is_finite().then_some((lo, hi))
}

/// Centre of an empty area of `size` mm on a sheet, clear of views, the BOM and the title block.
fn free_spot(app: &dyn DocCtx, d: &Drawing, sheet: usize, size: [f64; 2]) -> Option<[f64; 2]> {
    let [w, h] = sheet_size(&d.sheet);
    let mut taken: Vec<([f64; 2], [f64; 2])> = d.views.iter().filter(|v| v.sheet == sheet).filter_map(|v| view_extent(app, d, v)).collect();
    taken.push(title_rect(d));
    // dimensions, notes and tolerance frames on this sheet
    for dm in &d.dims {
        if let Some(v) = d.views.iter().find(|v| v.id == dm.view && v.sheet == sheet) {
            let pr = view_proj(app, d, v);
            let (a, b) = (to_sheet(v, pr, dm.a), to_sheet(v, pr, dm.b));
            let m = dm.offset.abs() + 6.0;
            taken.push(([a[0].min(b[0]) - m, a[1].min(b[1]) - m], [a[0].max(b[0]) + m, a[1].max(b[1]) + m]));
        }
    }
    for n in d.notes.iter().filter(|n| n.sheet == sheet) {
        let lines = n.text.split('\n').count() as f64;
        let wide = n.text.split('\n').map(|l| l.chars().count()).max().unwrap_or(0) as f64;
        taken.push(([n.pos[0], n.pos[1] - 2.0 - (lines - 1.0) * 5.1], [n.pos[0] + 3.0 * wide, n.pos[1] + 6.0]));
    }
    for g in d.gdts.iter().filter(|g| g.sheet == sheet) {
        let (lo, hi) = gdt_rect(g);
        taken.push((lo, hi));
    }
    // no fully free area: settle for the spot that covers the least of what is already there
    let (hw, hh) = (size[0].min(w - 30.0) / 2.0, size[1].min(h - 30.0) / 2.0);
    let step = 5.0;
    let mut best: Option<(f64, [f64; 2])> = None;
    let mut y = h - 15.0 - hh;
    while y >= 15.0 + hh {
        let mut x = 15.0 + hw;
        while x <= w - 15.0 - hw {
            let (lo, hi) = ([x - hw - 4.0, y - hh - 4.0], [x + hw + 4.0, y + hh + 4.0]);
            let cover: f64 = taken
                .iter()
                .map(|(a, b)| (hi[0].min(b[0]) - lo[0].max(a[0])).max(0.0) * (hi[1].min(b[1]) - lo[1].max(a[1])).max(0.0))
                .sum();
            if cover == 0.0 {
                return Some([x, y]);
            }
            if best.is_none_or(|(c, _)| cover < c) {
                best = Some((cover, [x, y]));
            }
            x += step;
        }
        y -= step;
    }
    best.map(|b| b.1)
}

fn insert_view(app: &mut App, studio: usize, dir: &str, body: Option<BodyId>, assembly: Option<usize>) {
    checkpoint(app);
    let scale = dwg(app).and_then(|d| d.views.first()).map(|v| v.scale).unwrap_or(1.0);
    let mut v = DrawView { studio, dir: dir.into(), scale, body, assembly, balloons: assembly.is_some(), sheet: app.dwg.cur_sheet, ..Default::default() };
    // size from the part's extent: iso views are about as wide as the part's diagonal
    let [sw, sh] = view_size(app, &v);
    let pos = dwg(app).and_then(|d| free_spot(app, d, v.sheet, [sw * scale + 4.0, sh * scale + 4.0])).unwrap_or(app.dwg.center);
    let letter = dwg(app).map(next_letter).unwrap_or_default();
    if let Some(d) = dwg_mut(app) {
        d.next_id += 1;
        v.id = d.next_id;
        v.pos = pos;
        if dir.starts_with("section:") {
            v.letter = letter;
        }
        let id = v.id;
        d.views.push(v);
        app.dwg.sel = Some(DSel::View(id));
    }
}

/// A section view through a point picked on view `parent`, cut along a horizontal or vertical
/// line of that view and looking across it.
fn add_section(app: &mut App, parent: &DrawView, q: [f64; 2], horizontal: bool) {
    let (vx, vy, _) = view_axes(&parent.dir);
    // the cutting plane's normal, as one of the standard directions
    let n = if horizontal { vy } else { vx };
    let pick = ["front", "top", "right", "back", "bottom", "left"].into_iter().map(|k| (k, view_axes(k).2)).max_by(|a, b| a.1.dot(n).abs().partial_cmp(&b.1.dot(n).abs()).unwrap_or(std::cmp::Ordering::Equal));
    let Some((name, ds)) = pick else { return };
    // prefer looking from the front, top or right
    let (name, ds) = match name {
        "back" => ("front", -ds),
        "bottom" => ("top", -ds),
        "left" => ("right", -ds),
        _ => (name, ds),
    };
    let p3 = vx * q[0] + vy * q[1];
    let cut = p3.dot(ds);
    checkpoint(app);
    let scale = parent.scale;
    let mut v = DrawView { studio: parent.studio, dir: format!("section:{name}"), scale, body: parent.body, assembly: parent.assembly, cut: Some(cut), parent: Some(parent.id), sheet: parent.sheet, ..Default::default() };
    let [sw, sh] = view_size(app, &v);
    let pos = dwg(app).and_then(|d| free_spot(app, d, v.sheet, [sw * scale + 4.0, sh * scale + 4.0])).unwrap_or(app.dwg.center);
    let letter = dwg(app).map(next_letter).unwrap_or_default();
    if let Some(d) = dwg_mut(app) {
        d.next_id += 1;
        v.id = d.next_id;
        v.pos = pos;
        v.letter = letter;
        let id = v.id;
        d.views.push(v);
        app.dwg.sel = Some(DSel::View(id));
    }
    app.dwg.tool = DTool::Select;
}

fn delete_sel(app: &mut App) {
    let Some(s) = app.dwg.sel.take() else { return };
    checkpoint(app);
    if let Some(d) = dwg_mut(app) {
        match s {
            DSel::View(id) => {
                d.views.retain(|v| v.id != id);
                d.dims.retain(|x| x.view != id);
            }
            DSel::Dim(id) => {
                // a deleted auto-dimension stays deleted when the others are regenerated
                if let Some(x) = d.dims.iter().find(|x| x.id == id && !x.auto.is_empty()).map(|x| (x.view, x.auto.clone())) {
                    if let Some(v) = d.views.iter_mut().find(|v| v.id == x.0) {
                        v.auto_off.push(x.1);
                    }
                }
                d.dims.retain(|x| x.id != id)
            }
            DSel::Note(id) => d.notes.retain(|x| x.id != id),
            DSel::Gdt(id) => d.gdts.retain(|x| x.id != id),
        }
    }
}

pub fn shortcuts(app: &mut App, ctx: &egui::Context) {
    // no Ctrl combinations here yet; Ctrl+letter is not the letter's tool
    if ctx.egui_wants_keyboard_input() || crate::app::key_mods(ctx).0 {
        return;
    }
    let pressed = |k: egui::Key| ctx.input(|i| i.key_pressed(k));
    if pressed(egui::Key::Escape) {
        app.dwg.dim_first = None;
        app.dwg.angle_first = None;
        app.dwg.sel = None;
        app.dwg.tool = DTool::Select;
    }
    if pressed(egui::Key::Delete) || pressed(egui::Key::Backspace) {
        delete_sel(app);
    }
    if pressed(egui::Key::D) {
        app.dwg.tool = DTool::Dimension;
    }
    if pressed(egui::Key::S) {
        app.dwg.tool = DTool::Select;
    }
    if pressed(egui::Key::N) {
        app.dwg.tool = DTool::Note;
    }
    if pressed(egui::Key::R) {
        app.dwg.tool = DTool::Radial;
    }
    if pressed(egui::Key::A) {
        app.dwg.tool = DTool::Angle;
        app.dwg.angle_first = None;
    }
    if pressed(egui::Key::F) {
        app.dwg.fitted = false;
    }
}

fn field(ui: &mut Ui, label: &str, value: &mut String, hint: &str) -> bool {
    ui.horizontal(|ui| {
        ui.add_sized([74.0, 18.0], egui::Label::new(RichText::new(label).size(12.0)));
        ui.add(egui::TextEdit::singleline(value).desired_width(ui.available_width()).hint_text(hint)).changed()
    })
    .inner
}

pub fn sidebar(app: &mut App, ui: &mut Ui) {
    let Some(di) = dwg_index(app) else { return };
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("SHEET").color(theme::muted()).size(11.0).strong());
            let mut sheet = app.ws.drawings[di].sheet.clone();
            egui::ComboBox::from_id_salt("sheet").selected_text(&sheet).show_ui(ui, |ui| {
                for s in ["A4", "A3", "A2", "A1", "Letter", "Tabloid"] {
                    ui.selectable_value(&mut sheet, s.to_string(), s);
                }
            });
            if sheet != app.ws.drawings[di].sheet {
                checkpoint(app);
                app.ws.drawings[di].sheet = sheet;
                app.dwg.fitted = false;
            }
            ui.add_space(4.0);
            let count = app.ws.drawings[di].sheet_count();
            let mut remove = None;
            for i in 0..count {
                let r = ui.add(egui::Button::selectable(app.dwg.cur_sheet == i, RichText::new(format!("Sheet {}", i + 1)).size(12.5)).min_size(egui::vec2(ui.available_width(), 20.0)));
                if r.clicked() {
                    app.dwg.cur_sheet = i;
                    app.dwg.sel = None;
                }
                if count > 1 {
                    r.context_menu(|ui| {
                        if ui.button(RichText::new("Delete sheet").color(theme::error_soft())).clicked() {
                            remove = Some(i);
                            ui.close();
                        }
                    });
                }
            }
            if ui.button("+ Add sheet").clicked() {
                checkpoint(app);
                app.ws.drawings[di].sheets = count + 1;
                app.dwg.cur_sheet = count;
                app.dwg.sel = None;
            }
            if let Some(i) = remove {
                checkpoint(app);
                let d = &mut app.ws.drawings[di];
                let gone: Vec<u64> = d.views.iter().filter(|v| v.sheet == i).map(|v| v.id).collect();
                d.views.retain(|v| v.sheet != i);
                d.dims.retain(|x| !gone.contains(&x.view));
                d.notes.retain(|n| n.sheet != i);
                d.gdts.retain(|g| g.sheet != i);
                if d.bom_sheet == i {
                    d.bom = None;
                }
                let fix = |k: &mut usize| {
                    if *k > i {
                        *k -= 1
                    }
                };
                d.views.iter_mut().for_each(|v| fix(&mut v.sheet));
                d.notes.iter_mut().for_each(|v| fix(&mut v.sheet));
                d.gdts.iter_mut().for_each(|v| fix(&mut v.sheet));
                fix(&mut d.bom_sheet);
                d.sheets = count - 1;
                app.dwg.cur_sheet = app.dwg.cur_sheet.min(count - 2);
                app.dwg.sel = None;
            }
            // title block
            ui.add_space(8.0);
            egui::CollapsingHeader::new(RichText::new("TITLE BLOCK").color(theme::muted()).size(11.0).strong()).id_salt("title-block").show(ui, |ui| {
                let mut t = app.ws.drawings[di].title.clone();
                let mut name = app.ws.drawings[di].name.clone();
                let mut changed = field(ui, "Title", &mut name, "");
                changed |= field(ui, "Part", &mut t.part, "from the view");
                changed |= field(ui, "Part no.", &mut t.part_no, "");
                changed |= field(ui, "Material", &mut t.material, "from the part");
                changed |= field(ui, "Stock", &mut t.stock, "from the part (none: leave out)");
                changed |= field(ui, "Revision", &mut t.revision, "A");
                changed |= field(ui, "Date", &mut t.date, "2026-10-28");
                changed |= field(ui, "Drawn by", &mut t.drawn_by, "");
                changed |= field(ui, "Gen. tol.", &mut t.tolerance, "ISO 2768-m");
                changed |= ui.checkbox(&mut t.first_angle, "First-angle projection").changed();
                let mut dec = app.ws.drawings[di].decimals.map(|d| d as i32).unwrap_or(-1);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Decimals").size(12.0));
                    changed |= ui.add(egui::DragValue::new(&mut dec).range(-1..=4).custom_formatter(|v, _| if v < 0.0 { "auto".into() } else { format!("{v}") })).changed();
                });
                if changed {
                    let d = &mut app.ws.drawings[di];
                    if t.first_angle != d.title.first_angle {
                        // the views move to the other projection's places
                        mirror_projection(d);
                    }
                    d.title = t;
                    d.name = name;
                    d.decimals = (dec >= 0).then_some(dec as u8);
                }
            });
            ui.add_space(8.0);
            ui.label(RichText::new("VIEWS").color(theme::muted()).size(11.0).strong());
            let cur = app.dwg.cur_sheet;
            let views: Vec<(u64, f64, bool)> = app.ws.drawings[di].views.iter().filter(|v| v.sheet == cur).map(|v| (v.id, v.scale, v.hidden_lines)).collect();
            for (id, scale, hidden) in views {
                let label = app.ws.drawings[di].views.iter().find(|v| v.id == id).map(|v| view_label(&app.ws.drawings[di], v)).unwrap_or_default();
                let label = label.as_str();
                let sel = app.dwg.sel == Some(DSel::View(id));
                ui.horizontal(|ui| {
                    if ui.selectable_label(sel, label).clicked() {
                        app.dwg.sel = Some(DSel::View(id));
                    }
                    let mut s = scale;
                    let r = ui.add(egui::DragValue::new(&mut s).range(0.001..=100.0).speed(0.01).prefix("scale "));
                    let mut hl = hidden;
                    let r2 = ui.checkbox(&mut hl, "hidden").on_hover_text("Show hidden lines");
                    if r.changed() || r2.changed() {
                        if let Some(v) = app.ws.drawings[di].views.iter_mut().find(|v| v.id == id) {
                            v.scale = s;
                            v.hidden_lines = hl;
                        }
                    }
                });
            }
            if let Some(DSel::View(id)) = app.dwg.sel {
                view_props(app, ui, di, id);
            }
            ui.add_space(8.0);
            ui.label(RichText::new(format!("{} dimensions · {} notes · {} tolerances", app.ws.drawings[di].dims.len(), app.ws.drawings[di].notes.len(), app.ws.drawings[di].gdts.len())).color(theme::muted()).size(12.0));
            for w in warnings(app, &app.ws.drawings[di]) {
                ui.label(RichText::new(format!("⚠ {w}")).color(theme::error_soft()).size(12.0));
            }
            // the selected tolerance frame or datum
            if let Some(DSel::Gdt(id)) = app.dwg.sel {
                if let Some(mut g) = app.ws.drawings[di].gdts.iter().find(|g| g.id == id).cloned() {
                    ui.add_space(8.0);
                    let datum = g.kind == "datum";
                    ui.label(RichText::new(if datum { "DATUM" } else { "TOLERANCE" }).color(theme::muted()).size(11.0).strong());
                    if datum {
                        ui.horizontal(|ui| {
                            ui.label("Letter");
                            ui.add(egui::TextEdit::singleline(&mut g.tol).desired_width(40.0));
                        });
                    } else {
                        let label = GDT_KINDS.iter().find(|k| k.0 == g.kind).map(|k| k.1).unwrap_or("?");
                        egui::ComboBox::from_id_salt("gdt-kind").selected_text(label).show_ui(ui, |ui| {
                            for (k, l) in GDT_KINDS {
                                ui.selectable_value(&mut g.kind, k.to_string(), *l);
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Tolerance");
                            ui.add(egui::TextEdit::singleline(&mut g.tol).desired_width(90.0));
                            if ui.small_button("Ø").on_hover_text("Diameter zone").clicked() && !g.tol.starts_with('Ø') {
                                g.tol.insert(0, 'Ø');
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Datums");
                            ui.add(egui::TextEdit::singleline(&mut g.datums).desired_width(90.0).hint_text("A B C"));
                        });
                    }
                    let mut has_leader = g.leader.is_some();
                    if ui.checkbox(&mut has_leader, "Leader").changed() {
                        g.leader = if has_leader { Some([g.pos[0] - 10.0, g.pos[1] - 8.0]) } else { None };
                    }
                    if let Some(cur) = app.ws.drawings[di].gdts.iter_mut().find(|x| x.id == id) {
                        if *cur != g {
                            *cur = g;
                        }
                    }
                }
            }
            if let Some(DSel::Dim(id)) = app.dwg.sel {
                dim_props(app, ui, di, id);
            }
        });
    });
}

/// Settings of the selected view: which part, section cut and letter, balloons.
fn view_props(app: &mut App, ui: &mut Ui, di: usize, id: u64) {
    let Some(mut v) = app.ws.drawings[di].views.iter().find(|v| v.id == id).cloned() else { return };
    let before = format!("{v:?}");
    ui.add_space(6.0);
    if v.assembly.is_none() && v.detail.is_none() {
        let parts: Vec<(BodyId, String)> = app.studio_bodies(v.studio).iter().map(|b| (b.id, part_name(app, v.studio, b.id))).collect();
        if parts.len() > 1 || v.body.is_some() {
            let shown = v.body.and_then(|b| parts.iter().find(|p| p.0 == b)).map(|p| p.1.clone()).unwrap_or_else(|| "All parts".into());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Shows").size(12.0));
                egui::ComboBox::from_id_salt("view-part").selected_text(shown).show_ui(ui, |ui| {
                    ui.selectable_value(&mut v.body, None, "All parts");
                    for (b, n) in &parts {
                        ui.selectable_value(&mut v.body, Some(*b), n);
                    }
                });
            });
        }
    }
    if is_section(&v) || v.detail.is_some() {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Letter").size(12.0));
            ui.add(egui::TextEdit::singleline(&mut v.letter).desired_width(30.0));
        });
    }
    if is_section(&v) {
        let mid = view_proj(app, &app.ws.drawings[di], &v).and_then(|p| p.cut).unwrap_or(0.0);
        let mut middle = v.cut.is_none();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Cut at").size(12.0));
            let mut c = v.cut.unwrap_or(mid);
            if ui.add_enabled(!middle, egui::DragValue::new(&mut c).speed(0.5).suffix(" mm")).on_hover_text("Position of the cutting plane along the view direction").changed() {
                v.cut = Some(c);
            }
            if ui.checkbox(&mut middle, "middle").changed() {
                v.cut = if middle { None } else { Some(mid) };
            }
        });
    }
    if v.assembly.is_some() {
        ui.checkbox(&mut v.balloons, "Item balloons");
    }
    if format!("{v:?}") != before {
        checkpoint(app);
        if let Some(x) = app.ws.drawings[di].views.iter_mut().find(|x| x.id == id) {
            *x = v;
        }
    }
}

/// Settings of the selected dimension: text around the value, type and tolerance.
fn dim_props(app: &mut App, ui: &mut Ui, di: usize, id: u64) {
    let Some(mut dm) = app.ws.drawings[di].dims.iter().find(|x| x.id == id).cloned() else { return };
    let before = serde_json::to_string(&dm).unwrap_or_default();
    ui.add_space(8.0);
    ui.label(RichText::new("DIMENSION").color(theme::muted()).size(11.0).strong());
    if dm.dangling {
        ui.label(RichText::new("Its geometry is gone after a model change: delete it or dimension again").color(theme::error_soft()).size(12.0));
    }
    if !dm.auto.is_empty() {
        ui.label(RichText::new("Auto-dimension: follows the model").color(theme::muted()).size(12.0));
    }
    ui.horizontal(|ui| {
        ui.label("Prefix");
        ui.add(egui::TextEdit::singleline(&mut dm.prefix).desired_width(80.0).hint_text("e.g. 4× "));
    });
    ui.horizontal(|ui| {
        ui.label("Suffix");
        ui.add(egui::TextEdit::singleline(&mut dm.suffix).desired_width(100.0).hint_text("e.g.  THRU"));
    });
    match dm.kind {
        0..=2 => {
            egui::ComboBox::from_id_salt("dim-kind").selected_text(["Aligned", "Horizontal", "Vertical"][dm.kind as usize]).show_ui(ui, |ui| {
                for (k, l) in ["Aligned", "Horizontal", "Vertical"].iter().enumerate() {
                    ui.selectable_value(&mut dm.kind, k as u8, *l);
                }
            });
        }
        3 | 4 => {
            let mut dia = dm.kind == 4;
            if ui.checkbox(&mut dia, "Diameter").changed() {
                dm.kind = if dia { 4 } else { 3 };
            }
        }
        _ => {}
    }
    if dm.kind != 5 {
        let kinds = ["None", "± symmetric", "+/− deviation", "Limits"];
        let mut k = match dm.tol {
            None => 0,
            Some(DimTol::Symmetric { .. }) => 1,
            Some(DimTol::Deviation { .. }) => 2,
            Some(DimTol::Limits { .. }) => 3,
        };
        // tolerances are kept in mm and edited in the document's units, as the dimension shows them
        let studio = app.ws.drawings[di].views.iter().find(|v| v.id == dm.view).map(|v| v.studio);
        let units = studio.map(|s| units_of(&*app, s)).unwrap_or_default();
        let us = units.scale();
        let (up0, low0) = match dm.tol {
            Some(DimTol::Symmetric { value }) => (value, value),
            Some(DimTol::Deviation { upper, lower }) | Some(DimTol::Limits { upper, lower }) => (upper, lower),
            None => (0.1, 0.1),
        };
        let (mut up, mut low) = (up0 / us, low0 / us);
        let unit = format!(" {}", units.label());
        ui.horizontal(|ui| {
            ui.label("Tolerance");
            egui::ComboBox::from_id_salt("dim-tol").selected_text(kinds[k]).show_ui(ui, |ui| {
                for (i, l) in kinds.iter().enumerate() {
                    ui.selectable_value(&mut k, i, *l);
                }
            });
        });
        if k > 0 {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut up).speed(0.01 / us).range(0.0..=1000.0 / us).prefix(if k == 1 { "±" } else { "+" }).suffix(unit.as_str()));
                if k > 1 {
                    ui.add(egui::DragValue::new(&mut low).speed(0.01 / us).range(0.0..=1000.0 / us).prefix("−").suffix(unit.as_str()));
                }
            });
        }
        // back to mm, leaving untouched values exactly as they were
        let up = if up == up0 / us { up0 } else { up * us };
        let low = if low == low0 / us { low0 } else { low * us };
        dm.tol = match k {
            1 => Some(DimTol::Symmetric { value: up }),
            2 => Some(DimTol::Deviation { upper: up, lower: low }),
            3 => Some(DimTol::Limits { upper: up, lower: low }),
            _ => None,
        };
    }
    if serde_json::to_string(&dm).unwrap_or_default() != before {
        if let Some(x) = app.ws.drawings[di].dims.iter_mut().find(|x| x.id == id) {
            *x = dm;
        }
    }
}

// ---------------------------------------------------------------- canvas

/// A new dimension's points attached to the view's model edges.
fn place_dim(app: &mut App, d: &Drawing, mut dm: DrawDim) -> u64 {
    if let Some(p) = d.views.iter().find(|v| v.id == dm.view).and_then(|v| view_proj(app, d, v)) {
        anchor_dim(p, &mut dm);
    }
    checkpoint(app);
    let mut id = 0;
    if let Some(dd) = dwg_mut(app) {
        dd.next_id += 1;
        id = dd.next_id;
        dm.id = id;
        dd.dims.push(dm);
    }
    id
}

pub fn show(app: &mut App, ui: &mut Ui) {
    request_projections(app);
    let Some(d) = dwg(app).cloned() else { return };
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    app.viewport_rect = rect;
    let [sw, sh] = sheet_size(&d.sheet);
    if !app.dwg.fitted || app.dwg.zoom <= 0.0 {
        app.dwg.fitted = true;
        app.dwg.zoom = ((rect.width() - 40.0) / sw as f32).min((rect.height() - 40.0) / sh as f32);
        app.dwg.center = [sw / 2.0, sh / 2.0];
    }
    // navigation: wheel zoom at cursor, middle/right drag pans
    let zoom = app.dwg.zoom;
    let center = app.dwg.center;
    let to_screen = |p: [f64; 2]| -> Pos2 { Pos2::new(rect.center().x + ((p[0] - center[0]) as f32) * zoom, rect.center().y - ((p[1] - center[1]) as f32) * zoom) };
    let to_sheet_pt = |s: Pos2| -> [f64; 2] { [center[0] + ((s.x - rect.center().x) / zoom) as f64, center[1] - ((s.y - rect.center().y) / zoom) as f64] };
    if let Some(p) = resp.hover_pos() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 {
            let before = to_sheet_pt(p);
            app.dwg.zoom = (zoom * (scroll * 0.0035).exp()).clamp(0.2, 200.0);
            let z = app.dwg.zoom;
            app.dwg.center = [before[0] - ((p.x - rect.center().x) / z) as f64, before[1] + ((p.y - rect.center().y) / z) as f64];
        }
    }
    // touch: pinch zooms, one finger on empty sheet pans
    let pinch = ui.input(|i| i.zoom_delta());
    if (pinch - 1.0).abs() > 1e-4 {
        if let Some(p) = resp.hover_pos() {
            let before = to_sheet_pt(p);
            app.dwg.zoom = (zoom * pinch).clamp(0.2, 200.0);
            let z = app.dwg.zoom;
            app.dwg.center = [before[0] - ((p.x - rect.center().x) / z) as f64, before[1] + ((p.y - rect.center().y) / z) as f64];
        }
    }
    let touch_pan = (app.phone || ui.input(|i| i.any_touches())) && app.dwg.tool == DTool::Select && app.dwg.sel.is_none() && app.dwg.drag.is_none() && resp.dragged_by(egui::PointerButton::Primary);
    if touch_pan || resp.dragged_by(egui::PointerButton::Middle) || resp.dragged_by(egui::PointerButton::Secondary) {
        let dlt = resp.drag_delta();
        app.dwg.center[0] -= (dlt.x / zoom) as f64;
        app.dwg.center[1] += (dlt.y / zoom) as f64;
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, if theme::is_dark() { theme::vp::background() } else { theme::hex(0xdfe3e8) });
    let sheet_rect = egui::Rect::from_two_pos(to_screen([0.0, 0.0]), to_screen([sw, sh]));
    painter.rect_filled(sheet_rect.translate(egui::vec2(3.0, 3.0)), 0.0, Color32::from_black_alpha(if theme::is_dark() { 90 } else { 40 }));
    painter.rect_filled(sheet_rect, 0.0, Color32::from_rgb(250, 250, 248));

    if app.dwg.cur_sheet >= d.sheet_count() {
        app.dwg.cur_sheet = 0;
    }
    let cur = app.dwg.cur_sheet;
    let prims = sheet_prims(app, &d, cur);
    let stroke_of = |w: f64, color: &[u8; 3]| Stroke::new((w as f32 * zoom * 0.6).clamp(0.6, 4.0), Color32::from_rgb(color[0], color[1], color[2]));
    for p in &prims {
        match p {
            Prim::Line { a, b, w, dashed, color } => {
                let stroke = stroke_of(*w, color);
                if *dashed {
                    painter.extend(egui::Shape::dashed_line(&[to_screen(*a), to_screen(*b)], stroke, 4.0, 3.0));
                } else {
                    painter.line_segment([to_screen(*a), to_screen(*b)], stroke);
                }
            }
            Prim::Arc { c, r, a0, a1, w, dashed, color } => {
                let pts: Vec<Pos2> = Prim::arc_points(*c, *r, *a0, *a1).into_iter().map(to_screen).collect();
                let stroke = stroke_of(*w, color);
                if *dashed {
                    painter.extend(egui::Shape::dashed_line(&pts, stroke, 4.0, 3.0));
                } else {
                    painter.add(egui::Shape::line(pts, stroke));
                }
            }
            Prim::Chain { a, b, w, color } => {
                let stroke = stroke_of(*w, color);
                for (p, q) in Prim::chain_pieces(*a, *b) {
                    painter.line_segment([to_screen(p), to_screen(q)], stroke);
                }
            }
            Prim::Text { at, size, text, anchor, color } => {
                let al = match anchor {
                    1 => egui::Align2::CENTER_BOTTOM,
                    2 => egui::Align2::RIGHT_BOTTOM,
                    _ => egui::Align2::LEFT_BOTTOM,
                };
                painter.text(to_screen(*at), al, text, egui::FontId::proportional((*size as f32 * zoom * 1.2).max(4.0)), Color32::from_rgb(color[0], color[1], color[2]));
            }
            Prim::Dim(_) | Prim::End => {}
        }
    }

    // selection highlight
    let view_rect = |app: &App, v: &DrawView| -> Option<egui::Rect> {
        if let Some((_, _, r)) = v.detail {
            let rr = r * v.scale;
            return Some(egui::Rect::from_two_pos(to_screen([v.pos[0] - rr, v.pos[1] - rr]), to_screen([v.pos[0] + rr, v.pos[1] + rr])));
        }
        let p = view_proj(app, &d, v)?;
        Some(egui::Rect::from_two_pos(to_screen(to_sheet(v, Some(p), p.bbox[0])), to_screen(to_sheet(v, Some(p), p.bbox[1]))).expand(6.0))
    };
    if let Some(DSel::View(id)) = app.dwg.sel {
        if let Some(r) = d.views.iter().find(|v| v.id == id).and_then(|v| view_rect(app, v)) {
            painter.rect_stroke(r, 2.0, Stroke::new(1.5, theme::accent()), egui::StrokeKind::Outside);
        }
    }
    if let Some(DSel::Gdt(id)) = app.dwg.sel {
        if let Some(g) = d.gdts.iter().find(|g| g.id == id) {
            let (lo, hi) = gdt_rect(g);
            painter.rect_stroke(egui::Rect::from_two_pos(to_screen(lo), to_screen(hi)).expand(3.0), 2.0, Stroke::new(1.5, theme::accent()), egui::StrokeKind::Outside);
        }
    }
    // alignment guides while dragging a view
    let guide = Stroke::new(1.0, theme::accent().gamma_multiply(0.7));
    if app.dwg.drag.is_some() {
        if let Some(x) = app.dwg.guides.0 {
            painter.extend(egui::Shape::dashed_line(&[to_screen([x, 0.0]), to_screen([x, sh])], guide, 5.0, 4.0));
        }
        if let Some(y) = app.dwg.guides.1 {
            painter.extend(egui::Shape::dashed_line(&[to_screen([0.0, y]), to_screen([sw, y])], guide, 5.0, 4.0));
        }
    }

    // snapping to view geometry for dimensions
    let pointer = resp.hover_pos();
    app.dwg.hover_pt = None;
    app.dwg.hover_circle = None;
    app.dwg.hover_line = None;
    let views: Vec<&DrawView> = d.views.iter().filter(|v| v.sheet == cur && v.detail.is_none()).collect();
    if let (DTool::Radial | DTool::Angle, Some(p)) = (app.dwg.tool, pointer) {
        let mut best_c: Option<(f32, u64, [f64; 2], [f64; 2], bool)> = None;
        let mut best_l: Option<(f32, u64, [f64; 2], [f64; 2])> = None;
        for v in &views {
            let Some(pr) = view_proj(app, &d, v) else { continue };
            let sp = from_sheet(v, Some(pr), to_sheet_pt(p));
            for pl in &pr.visible {
                if app.dwg.tool == DTool::Radial {
                    if let Some((c, r, full)) = fit_circle(pl) {
                        let dd = ((DVec2::from(sp) - DVec2::from(c)).length() - r).abs() * v.scale * zoom as f64;
                        // arcs: only near the arc itself
                        let near_arc = full || pl.iter().any(|q| DVec2::from(*q).distance(sp.into()) * v.scale * (zoom as f64) < 14.0);
                        if dd < 8.0 && near_arc && best_c.map(|b| (dd as f32) < b.0).unwrap_or(true) {
                            // the leader point snapped onto the circle
                            let on = DVec2::from(c) + (DVec2::from(sp) - DVec2::from(c)).normalize_or(DVec2::X) * r;
                            best_c = Some((dd as f32, v.id, c, on.into(), full));
                        }
                    }
                } else if let Some((a, b)) = straight(pl) {
                    let (sa, sb) = (to_screen(to_sheet(v, Some(pr), a)), to_screen(to_sheet(v, Some(pr), b)));
                    let dv = sb - sa;
                    let t = ((p - sa).dot(dv) / dv.length_sq().max(1e-6)).clamp(0.0, 1.0);
                    let dd = (sa + dv * t - p).length();
                    if dd < 8.0 && best_l.map(|x| dd < x.0).unwrap_or(true) {
                        best_l = Some((dd, v.id, a, b));
                    }
                }
            }
        }
        if let Some((_, vid, c, sp, full)) = best_c {
            app.dwg.hover_circle = Some((vid, c, sp, full));
            let v = views.iter().find(|v| v.id == vid).unwrap();
            let pr = view_proj(app, &d, v);
            let r = DVec2::from(c).distance(sp.into());
            let n = 48;
            let pts: Vec<Pos2> = (0..=n)
                .map(|i| {
                    let q = DVec2::from(c) + DVec2::from_angle(i as f64 / n as f64 * std::f64::consts::TAU) * r;
                    to_screen(to_sheet(v, pr, q.into()))
                })
                .collect();
            painter.add(egui::Shape::line(pts, Stroke::new(2.0, theme::vp::hover())));
        }
        if let Some((_, vid, a, b)) = best_l {
            app.dwg.hover_line = Some((vid, a, b));
            let v = views.iter().find(|v| v.id == vid).unwrap();
            let pr = view_proj(app, &d, v);
            painter.line_segment([to_screen(to_sheet(v, pr, a)), to_screen(to_sheet(v, pr, b))], Stroke::new(2.5, theme::vp::hover()));
        }
        if let Some((vid, a, b)) = app.dwg.angle_first {
            if let Some(v) = views.iter().find(|v| v.id == vid) {
                let pr = view_proj(app, &d, v);
                painter.line_segment([to_screen(to_sheet(v, pr, a)), to_screen(to_sheet(v, pr, b))], Stroke::new(2.5, theme::accent()));
            }
        }
    }
    if matches!(app.dwg.tool, DTool::Dimension | DTool::Gdt | DTool::Datum) {
        if let Some(p) = pointer {
            // line ends and midpoints, circle centres and quadrants
            let mut best: Option<(f32, u64, [f64; 2], u8)> = None;
            for v in &views {
                let Some(pr) = view_proj(app, &d, v) else { continue };
                for (q, k) in snap_points(pr) {
                    let s = to_screen(to_sheet(v, Some(pr), q));
                    let dd = (s - p).length();
                    if dd < 10.0 && best.map(|b| dd < b.0).unwrap_or(true) {
                        best = Some((dd, v.id, q, k));
                    }
                }
            }
            if let Some((_, vid, q, k)) = best {
                app.dwg.hover_pt = Some((vid, q));
                let v = d.views.iter().find(|v| v.id == vid).unwrap();
                let s = to_screen(to_sheet(v, view_proj(app, &d, v), q));
                let col = theme::vp::hover();
                match k {
                    2 => {
                        // centre: a cross
                        painter.line_segment([s - egui::vec2(6.0, 0.0), s + egui::vec2(6.0, 0.0)], Stroke::new(2.0, col));
                        painter.line_segment([s - egui::vec2(0.0, 6.0), s + egui::vec2(0.0, 6.0)], Stroke::new(2.0, col));
                    }
                    1 => {
                        painter.add(egui::Shape::convex_polygon(vec![s + egui::vec2(0.0, -5.0), s + egui::vec2(5.0, 4.0), s + egui::vec2(-5.0, 4.0)], col, Stroke::NONE));
                    }
                    3 => {
                        painter.add(egui::Shape::convex_polygon(vec![s + egui::vec2(0.0, -5.0), s + egui::vec2(5.0, 0.0), s + egui::vec2(0.0, 5.0), s + egui::vec2(-5.0, 0.0)], col, Stroke::NONE));
                    }
                    _ => {
                        painter.circle_filled(s, 4.0, col);
                    }
                }
                let what = ["end", "midpoint", "centre", "quadrant"][k as usize];
                painter.text(s + egui::vec2(8.0, -8.0), egui::Align2::LEFT_BOTTOM, what, egui::FontId::proportional(11.0), theme::muted());
            }
        }
    }
    // the view a section tool would cut, and the cutting line
    if let (DTool::Section(horizontal), Some(p)) = (app.dwg.tool, pointer) {
        for v in views.iter().filter(|v| v.dir != "iso" && !is_section(v)) {
            if view_rect(app, v).map(|r| r.contains(p)).unwrap_or(false) {
                let r = view_rect(app, v).unwrap();
                let stroke = Stroke::new(1.5, theme::accent());
                if horizontal {
                    painter.extend(egui::Shape::dashed_line(&[Pos2::new(r.left() - 10.0, p.y), Pos2::new(r.right() + 10.0, p.y)], stroke, 8.0, 3.0));
                } else {
                    painter.extend(egui::Shape::dashed_line(&[Pos2::new(p.x, r.top() - 10.0), Pos2::new(p.x, r.bottom() + 10.0)], stroke, 8.0, 3.0));
                }
            }
        }
    }

    // clicks
    if resp.clicked_by(egui::PointerButton::Primary) {
        if let Some(p) = pointer {
            let sp = to_sheet_pt(p);
            match app.dwg.tool {
                DTool::Select => {
                    app.dwg.sel = None;
                    for v in d.views.iter().filter(|v| v.sheet == cur) {
                        if view_rect(app, v).map(|r| r.contains(p)).unwrap_or(false) {
                            app.dwg.sel = Some(DSel::View(v.id));
                        }
                    }
                    for g in d.gdts.iter().filter(|g| g.sheet == cur) {
                        let (lo, hi) = gdt_rect(g);
                        if egui::Rect::from_two_pos(to_screen(lo), to_screen(hi)).expand(3.0).contains(p) {
                            app.dwg.sel = Some(DSel::Gdt(g.id));
                        }
                    }
                    for n in d.notes.iter().filter(|n| n.sheet == cur) {
                        if (to_screen(n.pos) - p).length() < 20.0 {
                            app.dwg.sel = Some(DSel::Note(n.id));
                        }
                    }
                    for dm in &d.dims {
                        if let Some(v) = d.views.iter().find(|v| v.id == dm.view && v.sheet == cur) {
                            let pr = view_proj(app, &d, v);
                            let (a, b) = (to_sheet(v, pr, dm.a), to_sheet(v, pr, dm.b));
                            let mid = to_screen([(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]);
                            if (mid - p).length() < 25.0 {
                                app.dwg.sel = Some(DSel::Dim(dm.id));
                            }
                        }
                    }
                }
                DTool::Dimension => {
                    if let Some((vid, q)) = app.dwg.hover_pt {
                        match app.dwg.dim_first {
                            None => app.dwg.dim_first = Some((vid, q)),
                            Some((v0, a)) if v0 == vid && (a[0] - q[0]).abs() + (a[1] - q[1]).abs() > 1e-9 => {
                                // horizontal/vertical when nearly axis aligned in the view
                                let (dx, dy) = ((q[0] - a[0]).abs(), (q[1] - a[1]).abs());
                                let kind = if dy < 1e-6 * (1.0 + dx) {
                                    1
                                } else if dx < 1e-6 * (1.0 + dy) {
                                    2
                                } else {
                                    0
                                };
                                let id = place_dim(app, &d, DrawDim { view: vid, a, b: q, kind, offset: 8.0, ..Default::default() });
                                app.dwg.sel = Some(DSel::Dim(id));
                                app.dwg.dim_first = None;
                            }
                            _ => app.dwg.dim_first = Some((vid, q)),
                        }
                    }
                }
                DTool::Note => app.dwg.note_edit = Some((None, sp, String::new())),
                DTool::Radial => {
                    if let Some((vid, c, on, full)) = app.dwg.hover_circle {
                        let id = place_dim(app, &d, DrawDim { view: vid, a: c, b: on, kind: if full { 4 } else { 3 }, offset: 8.0, ..Default::default() });
                        app.dwg.sel = Some(DSel::Dim(id));
                    }
                }
                DTool::Angle => {
                    if let Some((vid, a, b)) = app.dwg.hover_line {
                        match app.dwg.angle_first {
                            Some((v0, a0, b0)) if v0 == vid && (a0, b0) != (a, b) => {
                                let (u, w) = (DVec2::from(b0) - DVec2::from(a0), DVec2::from(b) - DVec2::from(a));
                                if u.perp_dot(w).abs() < 1e-9 * u.length() * w.length() {
                                    app.flash("Those lines are parallel", ToastKind::Info);
                                } else {
                                    let id = place_dim(app, &d, DrawDim { view: vid, a: a0, b: b0, kind: 5, offset: 12.0, extra: vec![a, b], ..Default::default() });
                                    app.dwg.sel = Some(DSel::Dim(id));
                                }
                                app.dwg.angle_first = None;
                            }
                            _ => app.dwg.angle_first = Some((vid, a, b)),
                        }
                    }
                }
                DTool::Section(horizontal) => {
                    let hit = views.iter().find(|v| v.dir != "iso" && !is_section(v) && view_rect(app, v).map(|r| r.contains(p)).unwrap_or(false)).map(|v| (*v).clone());
                    match hit {
                        Some(v) => {
                            let q = from_sheet(&v, view_proj(app, &d, &v), sp);
                            add_section(app, &v, q, horizontal);
                        }
                        None => app.flash("Click inside a front, top or side view", ToastKind::Info),
                    }
                }
                DTool::Gdt | DTool::Datum => {
                    // the leader goes to snapped geometry, else to the click itself
                    let leader = app.dwg.hover_pt.and_then(|(vid, q)| d.views.iter().find(|v| v.id == vid).map(|v| to_sheet(v, view_proj(app, &d, v), q))).unwrap_or(sp);
                    let datum = app.dwg.tool == DTool::Datum;
                    let used: Vec<String> = d.gdts.iter().filter(|g| g.kind == "datum").map(|g| g.tol.clone()).collect();
                    let letter = ('A'..='Z').map(|c| c.to_string()).find(|c| !used.contains(c)).unwrap_or_else(|| "A".into());
                    checkpoint(app);
                    if let Some(dd) = dwg_mut(app) {
                        dd.next_id += 1;
                        let id = dd.next_id;
                        let g = if datum {
                            Gdt { id, sheet: cur, pos: [leader[0] - GDT_H / 2.0, leader[1] + 10.0], kind: "datum".into(), tol: letter, datums: String::new(), leader: Some(leader) }
                        } else {
                            Gdt { id, sheet: cur, pos: [leader[0] + 10.0, leader[1] + 8.0], kind: "position".into(), tol: "Ø0.1".into(), datums: "A B".into(), leader: Some(leader) }
                        };
                        dd.gdts.push(g);
                        app.dwg.sel = Some(DSel::Gdt(id));
                    }
                    app.dwg.tool = DTool::Select;
                }
            }
        }
    }
    if resp.double_clicked_by(egui::PointerButton::Primary) && app.dwg.tool == DTool::Select {
        // edit a note in place
        if let Some(DSel::Note(id)) = app.dwg.sel {
            if let Some(n) = d.notes.iter().find(|n| n.id == id) {
                app.dwg.note_edit = Some((Some(id), n.pos, n.text.clone()));
            }
        }
    }
    // dragging views, notes and dimension offsets
    if app.dwg.tool == DTool::Select {
        if resp.drag_started_by(egui::PointerButton::Primary) {
            if let (Some(sel), Some(p)) = (app.dwg.sel, ui.input(|i| i.pointer.press_origin())) {
                let orig = match sel {
                    DSel::View(id) => d.views.iter().find(|v| v.id == id).map(|v| v.pos),
                    DSel::Note(id) => d.notes.iter().find(|n| n.id == id).map(|n| n.pos),
                    DSel::Dim(id) => d.dims.iter().find(|x| x.id == id).map(|x| [x.offset, 0.0]),
                    DSel::Gdt(id) => d.gdts.iter().find(|x| x.id == id).map(|x| x.pos),
                };
                if let Some(o) = orig {
                    checkpoint(app);
                    app.dwg.drag = Some((sel, to_sheet_pt(p), o));
                }
            }
        }
        if let Some((sel, start, orig)) = app.dwg.drag {
            if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
                let cur = to_sheet_pt(p);
                let dlt = [cur[0] - start[0], cur[1] - start[1]];
                let di = dwg_index(app);
                if let Some(dd) = di.and_then(|i| app.ws.drawings.get_mut(i)) {
                    match sel {
                        DSel::View(id) => {
                            // snap into line with other views on the sheet
                            let mut pos = [orig[0] + dlt[0], orig[1] + dlt[1]];
                            let tol = 6.0 / zoom as f64;
                            let mut guides = (None, None);
                            let sheet = dd.views.iter().find(|v| v.id == id).map(|v| v.sheet).unwrap_or(0);
                            for o in dd.views.iter().filter(|v| v.id != id && v.sheet == sheet) {
                                if (o.pos[0] - pos[0]).abs() < tol {
                                    pos[0] = o.pos[0];
                                    guides.0 = Some(o.pos[0]);
                                }
                                if (o.pos[1] - pos[1]).abs() < tol {
                                    pos[1] = o.pos[1];
                                    guides.1 = Some(o.pos[1]);
                                }
                            }
                            app.dwg.guides = guides;
                            if let Some(v) = dd.views.iter_mut().find(|v| v.id == id) {
                                v.pos = pos;
                            }
                        }
                        DSel::Gdt(id) => {
                            if let Some(g) = dd.gdts.iter_mut().find(|g| g.id == id) {
                                g.pos = [orig[0] + dlt[0], orig[1] + dlt[1]];
                            }
                        }
                        DSel::Note(id) => {
                            if let Some(n) = dd.notes.iter_mut().find(|n| n.id == id) {
                                n.pos = [orig[0] + dlt[0], orig[1] + dlt[1]];
                            }
                        }
                        DSel::Dim(id) => {
                            if let Some(x) = dd.dims.iter_mut().find(|x| x.id == id) {
                                x.offset = orig[0] + dlt[1] + dlt[0] * 0.0;
                            }
                        }
                    }
                }
            }
            if !ui.input(|i| i.pointer.primary_down()) {
                app.dwg.drag = None;
            }
        }
    }

    // note editor: Enter finishes, Shift+Enter starts a new line
    if let Some((id, pos, mut text)) = app.dwg.note_edit.clone() {
        let mut done = None;
        egui::Area::new(egui::Id::new("note-edit")).order(egui::Order::Foreground).fixed_pos(to_screen(pos)).show(ui.ctx(), |ui| {
            egui::Frame::new().fill(theme::panel()).stroke(Stroke::new(1.0, theme::accent())).corner_radius(4).inner_margin(4).show(ui, |ui| {
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
                let r = ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .hint_text("Note (Shift+Enter for a new line)")
                        .desired_width(220.0)
                        .desired_rows(1)
                        .return_key(egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::Enter)),
                );
                if !r.has_focus() && !r.lost_focus() {
                    r.request_focus();
                }
                if enter {
                    done = Some(true);
                } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        match done {
            Some(true) => {
                app.dwg.note_edit = None;
                let text = text.trim_end_matches('\n').to_string();
                if !text.trim().is_empty() {
                    checkpoint(app);
                    if let Some(dd) = dwg_mut(app) {
                        match id {
                            Some(id) => {
                                if let Some(n) = dd.notes.iter_mut().find(|n| n.id == id) {
                                    n.text = text;
                                }
                            }
                            None => {
                                dd.next_id += 1;
                                let nid = dd.next_id;
                                dd.notes.push(Note { id: nid, pos, text, sheet: cur });
                            }
                        }
                    }
                }
            }
            Some(false) => app.dwg.note_edit = None,
            None => app.dwg.note_edit = Some((id, pos, text)),
        }
    }

    // status hints
    let hint = match app.dwg.tool {
        DTool::Select => "Drag views, notes or dimensions to move them · double-click a note to edit it · wheel zooms, right-drag pans",
        DTool::Dimension if app.dwg.dim_first.is_some() => "Click the second point on the same view",
        DTool::Dimension => "Click a corner, line midpoint, circle centre or quadrant on a view",
        DTool::Note => "Click where the note goes",
        DTool::Radial => "Click a circle (diameter) or an arc (radius); drag the dimension to lengthen its leader",
        DTool::Angle if app.dwg.angle_first.is_some() => "Click the second line",
        DTool::Angle => "Click a straight edge in a view",
        DTool::Gdt => "Click the feature the tolerance frame points at; edit it in the sidebar",
        DTool::Datum => "Click the feature to mark as a datum",
        DTool::Section(true) => "Click on a view where the horizontal cutting line goes (Esc cancels)",
        DTool::Section(false) => "Click on a view where the vertical cutting line goes (Esc cancels)",
    };
    painter.text(rect.left_bottom() + egui::vec2(10.0, -10.0), egui::Align2::LEFT_BOTTOM, hint, egui::FontId::proportional(12.0), theme::muted());
    let _ = (theme::border(), theme::text());
}

// ---------------------------------------------------------------- export

fn export(app: &mut App, format: &str) {
    let Some(d) = dwg(app).cloned() else { return };
    let ext = format;
    let Some(path) = app.save_file_dialog(&ext.to_uppercase(), &[ext], format!("{}.{ext}", d.name)) else { return };
    let prims = sheet_prims(app, &d, app.dwg.cur_sheet);
    let size = sheet_size(&d.sheet);
    let data = match format {
        "pdf" => {
            let pages: Vec<Vec<Prim>> = (0..d.sheet_count()).map(|i| sheet_prims(app, &d, i)).collect();
            let refs: Vec<&[Prim]> = pages.iter().map(|p| p.as_slice()).collect();
            crate::drawing_export::pdf_pages(&refs, size)
        }
        "dxf" => crate::drawing_export::dxf(&prims).into_bytes(),
        _ => crate::drawing_export::svg(&prims, size).into_bytes(),
    };
    match std::fs::write(&path, data) {
        Ok(()) => app.flash_info(format!("Exported {}", path.display()), Some(std::time::Duration::from_secs(3))),
        Err(e) => app.flash(format!("Could not export: {e}"), ToastKind::Error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::{ProjEdge, ProjHole};

    #[test]
    fn projection_symbols_follow_iso() {
        // the cone's narrow end (the shorter vertical line) and the end view's circles
        let parts = |first: bool| {
            let mut out = vec![];
            projection_symbol(&mut out, [0.0, 0.0], first);
            let mut ends: Vec<(f64, f64)> = out.iter().filter_map(|p| match p {
                Prim::Line { a, b, .. } if (a[0] - b[0]).abs() < 1e-9 => Some((a[0], (a[1] - b[1]).abs())),
                _ => None,
            }).collect();
            ends.sort_by(|x, y| x.1.total_cmp(&y.1));
            let circles = out.iter().find_map(|p| match p {
                Prim::Arc { c, .. } => Some(c[0]),
                _ => None,
            }).unwrap();
            let wide = ends[1].0;
            (ends[0].0, wide, circles)
        };
        for first in [false, true] {
            let (narrow, wide, circles) = parts(first);
            // the same cone in both: wide end on the left, narrow end on the right
            assert!(narrow > wide);
            // third-angle: the end view to the right, where the cone points; first-angle: to the left
            assert_eq!(circles > narrow, !first, "first angle {first}");
            assert_eq!(circles < wide, first, "first angle {first}");
        }
    }

    #[test]
    fn first_angle_mirrors_the_standard_views() {
        let mut d = Drawing::default();
        for (id, dir, pos) in [(1, "front", [60.0, 80.0]), (2, "top", [60.0, 140.0]), (3, "right", [130.0, 80.0]), (4, "iso", [300.0, 200.0])] {
            d.views.push(DrawView { id, dir: dir.into(), pos, scale: 1.0, ..Default::default() });
        }
        let before: Vec<[f64; 2]> = d.views.iter().map(|v| v.pos).collect();
        mirror_projection(&mut d);
        let pos = |id: u64| d.views.iter().find(|v| v.id == id).unwrap().pos;
        // the top view under the front view, the right view on its left, in the same area
        assert!(pos(2)[1] < pos(1)[1] && pos(2)[0] == pos(1)[0]);
        assert!(pos(3)[0] < pos(1)[0] && pos(3)[1] == pos(1)[1]);
        assert_eq!(pos(1), [130.0, 140.0]);
        assert_eq!(pos(4), [300.0, 200.0]);
        mirror_projection(&mut d);
        assert_eq!(d.views.iter().map(|v| v.pos).collect::<Vec<_>>(), before);
    }

    fn circle(c: [f64; 2], r: f64, a0: f64, a1: f64) -> Vec<[f64; 2]> {
        (0..=40).map(|i| {
            let t = a0 + (a1 - a0) * i as f64 / 40.0;
            [c[0] + r * t.cos(), c[1] + r * t.sin()]
        }).collect()
    }

    /// A 40x20 plate seen from the top with two counterbored 5 mm holes (8 mm counterbores).
    fn plate(thick: f64, r: f64) -> ProjView {
        let tau = std::f64::consts::TAU;
        let mut edges = vec![];
        let mut add = |index: u32, kind: u8, pts: Vec<[f64; 2]>, circle: Option<[f64; 3]>| edges.push(ProjEdge { part: (2, 0), index, kind, pts, circle, axis: None });
        add(0, 0, vec![[0.0, 0.0], [40.0, 0.0]], None);
        add(1, 0, vec![[40.0, 0.0], [40.0, thick]], None);
        add(2, 1, circle([10.0, 10.0], r, 0.0, tau), Some([10.0, 10.0, r]));
        add(3, 1, circle([30.0, 10.0], r, 0.0, tau), Some([30.0, 10.0, r]));
        let hole = |x: f64| ProjHole { feature: 7, at: [x, 10.0], square: true, dia: 2.0 * r, through: true, depth: 0.0, kind: 1, dia2: 8.0, v2: 3.0 };
        ProjView {
            visible: vec![
                vec![[0.0, 0.0], [40.0, 0.0]],
                vec![[40.0, 0.0], [40.0, thick]],
                circle([10.0, 10.0], r, 0.0, tau),
                circle([30.0, 10.0], r, 0.0, tau),
                circle([10.0, 10.0], 4.0, 0.0, tau),
                circle([30.0, 10.0], 4.0, 0.0, tau),
            ],
            bbox: [[0.0, 0.0], [40.0, 20.0]],
            edges,
            holes: vec![hole(10.0), hole(30.0)],
            ..Default::default()
        }
    }

    #[test]
    fn circles_auto_dims_and_annotations() {
        let tau = std::f64::consts::TAU;
        assert!(matches!(fit_circle(&circle([3.0, 4.0], 2.5, 0.0, tau)), Some((c, r, true)) if (c[0] - 3.0).abs() < 1e-9 && (r - 2.5).abs() < 1e-9));
        assert!(matches!(fit_circle(&circle([0.0, 0.0], 5.0, 0.0, 1.0)), Some((_, _, false))));
        assert!(fit_circle(&[[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [3.0, 0.0], [4.0, 0.0]]).is_none());
        let p = plate(20.0, 2.5);
        let mut id = 0;
        let v = DrawView { id: 1, scale: 1.0, ..Default::default() };
        let dims = auto_dims(&p, &v, &mut id);
        // one counterbore callout from the Hole feature, not one per circle
        let callouts: Vec<&DrawDim> = dims.iter().filter(|x| x.kind == 4).collect();
        assert_eq!(callouts.len(), 1, "{dims:?}");
        assert_eq!(callouts[0].prefix, "2× ");
        assert_eq!(callouts[0].suffix, " THRU\n⌴Ø8 ↧3");
        // hole positions: x 10 and 30 from the left, y 10 from the bottom; overall width and height
        let keys: Vec<&str> = dims.iter().map(|x| x.auto.as_str()).collect();
        for k in ["x:10", "x:30", "y:10", "w", "h"] {
            assert!(keys.contains(&k), "{keys:?}");
        }
        // annotations draw without trouble
        let mut out = vec![];
        let mut info = DimInfo::default();
        angle_prims(&mut out, [[0.0, 0.0], [10.0, 0.0]], [[0.0, 0.0], [10.0, 10.0]], 8.0, DIM_INK, &mut info);
        assert!(out.iter().any(|p| matches!(p, Prim::Text { text, .. } if text == "45°")));
        for (k, _) in GDT_KINDS {
            gdt_prims(&mut out, &Gdt { id: 1, sheet: 0, pos: [0.0, 0.0], kind: k.to_string(), tol: "Ø0.1".into(), datums: "A B".into(), leader: Some([-5.0, -5.0]) });
        }
        // symbols are drawn as lines, the rest as text runs
        let n = out.len();
        text_prims(&mut out, [0.0, 0.0], 3.5, "2× Ø10 THRU\n⌴Ø16 ↧5", 0, INK);
        let texts: Vec<String> = out[n..].iter().filter_map(|p| if let Prim::Text { text, .. } = p { Some(text.clone()) } else { None }).collect();
        assert_eq!(texts, vec!["2× Ø10 THRU", "Ø16 ", "5"]);
        let pdf = crate::drawing_export::pdf_pages(&[&out, &out], [297.0, 210.0]);
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/Count 2"));
        assert!(text.contains("\\330")); // Ø in WinAnsi
    }

    #[test]
    fn dimensions_follow_the_model() {
        // a dimension to the plate's top right corner and one on a hole circle
        let before = plate(20.0, 2.5);
        let mut dm = DrawDim { view: 1, a: [40.0, 0.0], b: [40.0, 20.0], kind: 2, ..Default::default() };
        anchor_dim(&before, &mut dm);
        assert_eq!(dm.anchors.len(), 2);
        let mut dia = DrawDim { view: 1, a: [10.0, 10.0], b: [10.0 + 2.5 * 0.6, 10.0 + 2.5 * 0.8], kind: 4, ..Default::default() };
        anchor_dim(&before, &mut dia);
        assert_eq!(dia.anchors[0].unwrap().at, 2);
        assert_eq!(dia.anchors[1].unwrap().at, 4);
        // the plate gets thicker and the holes bigger
        let after = plate(28.0, 3.0);
        let got: Vec<[f64; 2]> = dm.anchors.iter().map(|a| resolve(&after, a.as_ref().unwrap()).unwrap()).collect();
        assert!((got[1][1] - 28.0).abs() < 1e-9, "{got:?}");
        let c = resolve(&after, dia.anchors[0].as_ref().unwrap()).unwrap();
        let on = resolve(&after, dia.anchors[1].as_ref().unwrap()).unwrap();
        assert!((DVec2::from(c).distance(on.into()) - 3.0).abs() < 1e-9);
        // an edge that is gone leaves the dimension dangling
        let mut gone = plate(20.0, 2.5);
        gone.edges.retain(|e| e.index != 1);
        assert!(resolve(&gone, dm.anchors[1].as_ref().unwrap()).is_none());
    }

    #[test]
    fn snaps_tolerances_and_arcs() {
        let p = plate(20.0, 2.5);
        let s = snap_points(&p);
        // centres, quadrants and the midpoint of a line
        assert!(s.iter().any(|(q, k)| *k == 2 && (q[0] - 10.0).abs() < 1e-9 && (q[1] - 10.0).abs() < 1e-9));
        assert!(s.iter().any(|(q, k)| *k == 3 && (q[0] - 14.0).abs() < 1e-9 && (q[1] - 10.0).abs() < 1e-9));
        assert!(s.iter().any(|(q, k)| *k == 1 && (q[0] - 20.0).abs() < 1e-9 && q[1].abs() < 1e-9));
        // a corner fillet from 270° to 360°: its radius leader goes through the middle of the arc
        let arc = circle([5.0, 5.0], 5.0, -std::f64::consts::FRAC_PI_2, 0.0);
        let (a0, a1) = arc_span(&arc, [5.0, 5.0]);
        assert!((a1 - a0 - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        let pv = ProjView { visible: vec![arc], ..Default::default() };
        let m = arc_mid_point(Some(&pv), [5.0, 5.0], 5.0);
        assert!(m[0] > 5.0 && m[1] < 5.0, "{m:?}");
        // tolerances and decimals
        let mut d = Drawing { decimals: Some(2), ..Default::default() };
        let u = crate::expr::Units::Mm;
        assert_eq!(dim_label(&d, u, 20.0, Some(DimTol::Symmetric { value: 0.1 })).0, "20.00 ±0.10");
        assert_eq!(dim_label(&d, u, 20.0, Some(DimTol::Deviation { upper: 0.1, lower: 0.05 })).1, Some(("+0.10".into(), "−0.05".into())));
        d.decimals = None;
        assert_eq!(dim_label(&d, u, 20.0, Some(DimTol::Limits { upper: 0.1, lower: 0.2 })).1, Some(("20.1".into(), "19.8".into())));
    }
}
