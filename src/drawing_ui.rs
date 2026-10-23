//! Drawing tab: a sheet with hidden-line projected views of a Part Studio,
//! dimensions and notes, exported to PDF, DXF or SVG.

use std::collections::{HashMap, HashSet};

use egui::{Color32, Margin, Pos2, RichText, Sense, Stroke, Ui};

use crate::app::{App, ToastKind};
use crate::asm_ui::checkpoint;
use crate::drawing::{DrawDim, DrawView, Drawing, Gdt, Note, GDT_KINDS};
use crate::kernel::ProjView;
use crate::theme;
use crate::workspace::Tab;

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DSel {
    View(u64),
    Dim(u64),
    Note(u64),
    Gdt(u64),
}

#[derive(Default)]
pub struct DrawUi {
    /// Part geometry changed: projections must be fetched again.
    pub dirty: bool,
    pub proj: HashMap<(usize, String), ProjView>,
    pub requested: HashSet<(usize, String)>,
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
];

fn base_dir(dir: &str) -> &str {
    dir.strip_prefix("section:").unwrap_or(dir)
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

/// Reply from the kernel.
pub fn projected(app: &mut App, tag: usize, key: String, views: Vec<ProjView>) {
    if let Some(v) = views.into_iter().next() {
        app.dwg.proj.insert((tag, key), v);
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
    let need: Vec<(usize, String)> = d.views.iter().map(|v| (v.studio, v.dir.clone())).filter(|k| !app.dwg.requested.contains(k)).collect();
    for (studio, dir) in need {
        // wait until the Part Studio has been built
        if app.studio_bodies(studio).is_empty() {
            continue;
        }
        let Some(doc) = app.studio_doc(studio).cloned() else { continue };
        app.dwg.requested.insert((studio, dir.clone()));
        let (d, u) = dir_vectors(&dir);
        app.kernel.project(studio, dir.clone(), &doc, vec![(d, u, dir.starts_with("section:"))]);
    }
}

/// Front, top, right and isometric views laid out third-angle, scaled to the sheet.
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
        d.views.push(DrawView { id, studio, dir: dir.into(), pos, scale, hidden_lines: false, detail: None, sheet: sheet_no });
    }
    app.dwg.fitted = false;
}

fn nice_scale(raw: f64) -> f64 {
    let steps = [100.0, 50.0, 20.0, 10.0, 5.0, 4.0, 2.0, 1.0, 0.5, 0.25, 0.2, 0.1, 0.05, 0.02, 0.01, 0.005];
    steps.iter().copied().find(|s| *s <= raw).unwrap_or(0.005)
}

fn fmt_scale(s: f64) -> String {
    if s >= 1.0 { format!("{}:1", crate::expr::trim(s, 2)) } else { format!("1:{}", crate::expr::trim(1.0 / s, 2)) }
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

// ---------------------------------------------------------------- geometry for display and export

/// What the sheet renderer needs to know about the document (the GUI app or a headless session).
pub trait DocCtx {
    fn ws(&self) -> &crate::workspace::Workspace;
    fn bodies(&self, studio: usize) -> &[crate::kernel::BodyMesh];
    fn studio_doc(&self, studio: usize) -> Option<&crate::doc::Document>;
    fn proj(&self, studio: usize, dir: &str) -> Option<&ProjView>;
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
    fn proj(&self, studio: usize, dir: &str) -> Option<&ProjView> {
        self.dwg.proj.get(&(studio, dir.to_string()))
    }
}

/// A drawing primitive in sheet millimetres.
pub enum Prim {
    Line { a: [f64; 2], b: [f64; 2], w: f64, dashed: bool, color: [u8; 3] },
    Text { at: [f64; 2], size: f64, text: String, anchor: u8, color: [u8; 3] },
}

const INK: [u8; 3] = [20, 20, 20];
const DIM_INK: [u8; 3] = [20, 60, 140];

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
    // title block
    let (tw, th) = (170.0, 36.0);
    let (tx, ty) = (w - m - tw, m);
    for (a, b) in [([tx, ty], [tx, ty + th]), ([tx, ty + th], [w - m, ty + th]), ([tx, ty + 18.0], [w - m, ty + 18.0]), ([tx + 110.0, ty], [tx + 110.0, ty + 18.0])] {
        line(&mut out, a, b, 0.35);
    }
    let on_sheet: Vec<&DrawView> = d.views.iter().filter(|v| v.sheet == sheet).collect();
    let units = on_sheet.first().map(|v| units_of(app, v.studio)).unwrap_or_default();
    let scale = on_sheet.first().map(|v| fmt_scale(v.scale)).unwrap_or_default();
    let studio = on_sheet.first().and_then(|v| app.ws().studios.get(v.studio)).map(|s| s.name.clone()).unwrap_or_default();
    out.push(Prim::Text { at: [tx + 4.0, ty + 24.0], size: 6.0, text: d.name.clone(), anchor: 0, color: INK });
    out.push(Prim::Text { at: [tx + 4.0, ty + 6.0], size: 3.2, text: format!("Part: {studio}"), anchor: 0, color: INK });
    out.push(Prim::Text { at: [tx + 114.0, ty + 11.0], size: 3.2, text: format!("Scale {scale}"), anchor: 0, color: INK });
    out.push(Prim::Text { at: [tx + 114.0, ty + 4.0], size: 3.2, text: format!("Units {}", units.label()), anchor: 0, color: INK });
    out.push(Prim::Text { at: [w - m - 4.0, ty + 24.0], size: 3.0, text: "cad-app".into(), anchor: 2, color: [110, 110, 110] });
    out.push(Prim::Text { at: [w - m - 4.0, ty + 4.0], size: 3.2, text: format!("Sheet {} of {}", sheet + 1, d.sheet_count()), anchor: 2, color: INK });

    for v in on_sheet.iter().copied() {
        if let Some((src, c, r)) = v.detail {
            detail_prims(app, d, v, src, c, r, &mut out);
            continue;
        }
        let proj = app.proj(v.studio, &v.dir);
        let Some(p) = proj else {
            out.push(Prim::Text { at: v.pos, size: 4.0, text: "projecting…".into(), anchor: 1, color: [130, 130, 130] });
            continue;
        };
        for pl in &p.visible {
            for s in pl.windows(2) {
                out.push(Prim::Line { a: to_sheet(v, Some(p), s[0]), b: to_sheet(v, Some(p), s[1]), w: 0.5, dashed: false, color: INK });
            }
        }
        if v.hidden_lines {
            for pl in &p.hidden {
                for s in pl.windows(2) {
                    out.push(Prim::Line { a: to_sheet(v, Some(p), s[0]), b: to_sheet(v, Some(p), s[1]), w: 0.25, dashed: true, color: [90, 90, 90] });
                }
            }
        }
        for h in &p.hatch {
            out.push(Prim::Line { a: to_sheet(v, Some(p), h[0]), b: to_sheet(v, Some(p), h[1]), w: 0.18, dashed: false, color: [70, 70, 70] });
        }
        let label = view_label(d, v);
        let bottom = to_sheet(v, Some(p), [0.0, p.bbox[0][1]])[1];
        out.push(Prim::Text { at: [v.pos[0], bottom - 7.0], size: 3.2, text: format!("{} ({})", label.to_uppercase(), fmt_scale(v.scale)), anchor: 1, color: [80, 80, 80] });
    }
    for dm in &d.dims {
        let Some(v) = on_sheet.iter().find(|v| v.id == dm.view) else { continue };
        let p = app.proj(v.studio, &v.dir);
        let (a, b) = (to_sheet(v, p, dm.a), to_sheet(v, p, dm.b));
        let units = units_of(app, v.studio);
        match dm.kind {
            3 | 4 => radial_prims(&mut out, a, b, dm.kind == 4, dm.offset, &format!("{}{}{}", dm.prefix, if dm.kind == 4 { "Ø" } else { "R" }, units.fmt(glam::DVec2::from(dm.a).distance(dm.b.into()) * if dm.kind == 4 { 2.0 } else { 1.0 }))),
            5 if dm.extra.len() == 2 => angle_prims(&mut out, [a, b], [to_sheet(v, p, dm.extra[0]), to_sheet(v, p, dm.extra[1])], dm.offset),
            _ => dim_prims(&mut out, a, b, dm.kind, dm.offset, |len| format!("{}{}", dm.prefix, units.fmt(len / v.scale))),
        }
    }
    for n in d.notes.iter().filter(|n| n.sheet == sheet) {
        out.push(Prim::Text { at: n.pos, size: 3.5, text: n.text.clone(), anchor: 0, color: INK });
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

fn arrow(out: &mut Vec<Prim>, tip: glam::DVec2, dir: glam::DVec2, color: [u8; 3]) {
    // `dir` points from the tip back along the shaft
    let d = dir.normalize_or_zero();
    let back = tip + d * 3.0;
    out.push(Prim::Line { a: tip.into(), b: (back + d.perp() * 0.9).into(), w: 0.25, dashed: false, color });
    out.push(Prim::Line { a: tip.into(), b: (back - d.perp() * 0.9).into(), w: 0.25, dashed: false, color });
}

/// Radius or diameter: leader from the circle outwards with a shoulder and the text.
fn radial_prims(out: &mut Vec<Prim>, c: [f64; 2], on: [f64; 2], diameter: bool, offset: f64, text: &str) {
    let (c, on) = (glam::DVec2::from(c), glam::DVec2::from(on));
    let dir = (on - c).normalize_or(glam::DVec2::X);
    let r = (on - c).length();
    let p = c + dir * r;
    let end = p + dir * offset.max(2.0);
    let start = if diameter { c - dir * r } else { c };
    out.push(Prim::Line { a: start.into(), b: end.into(), w: 0.25, dashed: false, color: DIM_INK });
    arrow(out, p, -dir, DIM_INK);
    if diameter {
        arrow(out, c - dir * r, dir, DIM_INK);
    }
    let sh = if dir.x >= 0.0 { 4.0 } else { -4.0 };
    let tip = end + glam::DVec2::new(sh, 0.0);
    out.push(Prim::Line { a: end.into(), b: tip.into(), w: 0.25, dashed: false, color: DIM_INK });
    out.push(Prim::Text { at: (tip + glam::DVec2::new(if sh > 0.0 { 0.8 } else { -0.8 }, -1.2)).into(), size: 3.5, text: text.into(), anchor: if sh > 0.0 { 0 } else { 2 }, color: DIM_INK });
}

/// Angle between two lines, drawn as an arc of radius `r` about their intersection.
fn angle_prims(out: &mut Vec<Prim>, l1: [[f64; 2]; 2], l2: [[f64; 2]; 2], r: f64) {
    use glam::DVec2;
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
        out.push(Prim::Line { a: w[0].into(), b: w[1].into(), w: 0.25, dashed: false, color: DIM_INK });
    }
    let s = ang.signum();
    arrow(out, pts[0], u.perp() * s, DIM_INK);
    arrow(out, pts[n], -v.perp() * s, DIM_INK);
    // extension lines where the arc lies beyond a line's extent
    for (p, q, e) in [(a0, a1, pts[0]), (b0, b1, pts[n])] {
        let near = if (p - e).length() < (q - e).length() { p } else { q };
        let along = (e - x).dot((near - x).normalize_or_zero());
        if along > (near - x).length() + 0.5 {
            out.push(Prim::Line { a: (near + (e - near).normalize() * 1.0).into(), b: (e + (e - near).normalize() * 2.0).into(), w: 0.25, dashed: false, color: DIM_INK });
        }
    }
    let mid = x + DVec2::from_angle(ang / 2.0).rotate(u) * (r + 2.5);
    out.push(Prim::Text { at: (mid + DVec2::new(0.0, -1.2)).into(), size: 3.5, text: format!("{}°", crate::expr::trim(ang.abs().to_degrees(), 1)), anchor: 1, color: DIM_INK });
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
    use glam::DVec2;
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
fn gdt_symbol(out: &mut Vec<Prim>, kind: &str, c: glam::DVec2) {
    use glam::DVec2;
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
pub fn fit_circle(pl: &[[f64; 2]]) -> Option<([f64; 2], f64, bool)> {
    use glam::DVec2;
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

/// Straight polylines of a projection (lines in the view) as segments.
fn straight(pl: &[[f64; 2]]) -> Option<([f64; 2], [f64; 2])> {
    let (a, b) = (glam::DVec2::from(pl[0]), glam::DVec2::from(pl[pl.len() - 1]));
    let d = b - a;
    if d.length() < 1e-9 {
        return None;
    }
    let n = d.perp().normalize();
    pl.iter().all(|p| (glam::DVec2::from(*p) - a).dot(n).abs() < 1e-6 * (1.0 + d.length())).then_some((pl[0], pl[pl.len() - 1]))
}

/// Overall size and one diameter per distinct hole size for a view.
pub fn auto_dims(p: &ProjView, view: u64, next_id: &mut u64) -> Vec<DrawDim> {
    let mut out = vec![];
    let [lo, hi] = p.bbox;
    let mut push = |kind: u8, a: [f64; 2], b: [f64; 2], offset: f64, prefix: String, out: &mut Vec<DrawDim>| {
        *next_id += 1;
        out.push(DrawDim { id: *next_id, view, a, b, kind, offset, extra: vec![], prefix });
    };
    if hi[0] - lo[0] > 1e-6 {
        // above the view, clear of the view label underneath
        push(1, [lo[0], hi[1]], [hi[0], hi[1]], 10.0, String::new(), &mut out);
    }
    if hi[1] - lo[1] > 1e-6 {
        push(2, [hi[0], lo[1]], [hi[0], hi[1]], -10.0, String::new(), &mut out);
    }
    // full circles, grouped by radius
    let mut groups: Vec<(f64, Vec<[f64; 2]>)> = vec![];
    for pl in &p.visible {
        if let Some((c, r, true)) = fit_circle(pl) {
            match groups.iter_mut().find(|g| (g.0 - r).abs() < 1e-4 * (1.0 + r)) {
                Some(g) => {
                    if !g.1.iter().any(|q| (q[0] - c[0]).abs() + (q[1] - c[1]).abs() < 1e-6 * (1.0 + r)) {
                        g.1.push(c);
                    }
                }
                None => groups.push((r, vec![c])),
            }
        }
    }
    for (r, cs) in groups {
        let c = cs[0];
        let on = [c[0] + r * 0.7071, c[1] + r * 0.7071];
        push(4, c, on, 6.0, if cs.len() > 1 { format!("{}× ", cs.len()) } else { String::new() }, &mut out);
    }
    out
}

fn bom_prims(app: &dyn DocCtx, a: &crate::assembly::Assembly, pos: [f64; 2], out: &mut Vec<Prim>) {
    let rows = crate::asm_ui::bom(app, a);
    let cols = [12.0, 60.0, 14.0, 34.0];
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
    let cells = |i: usize, vals: [String; 4], out: &mut Vec<Prim>| {
        let mut x = x0;
        for (k, v) in vals.into_iter().enumerate() {
            out.push(Prim::Text { at: [x + 1.5, y0 - (i as f64 + 1.0) * rh + 1.7], size: 3.0, text: v, anchor: 0, color: INK });
            x += cols[k];
        }
    };
    cells(0, ["ITEM".into(), "PART".into(), "QTY".into(), "MATERIAL".into()], out);
    for (i, (item, name, q, m, _)) in rows.iter().enumerate() {
        cells(i + 1, [item.to_string(), name.clone(), q.to_string(), m.clone()], out);
    }
}

/// A detail view: part of another view, clipped to a circle and enlarged.
fn detail_prims(app: &dyn DocCtx, d: &Drawing, v: &DrawView, src: u64, c: [f64; 2], r: f64, out: &mut Vec<Prim>) {
    let Some(sv) = d.views.iter().find(|x| x.id == src) else { return };
    let Some(p) = app.proj(sv.studio, &sv.dir) else { return };
    let cc = glam::DVec2::from(c);
    let map = |q: glam::DVec2| -> [f64; 2] { [v.pos[0] + (q.x - cc.x) * v.scale, v.pos[1] + (q.y - cc.y) * v.scale] };
    for pl in &p.visible {
        for s in pl.windows(2) {
            let (a, b) = (glam::DVec2::from(s[0]), glam::DVec2::from(s[1]));
            if let Some((a, b)) = clip_circle(a, b, cc, r) {
                out.push(Prim::Line { a: map(a), b: map(b), w: 0.5, dashed: false, color: INK });
            }
        }
    }
    // boundary circle here and on the source view
    let n = 64;
    for i in 0..n {
        let t0 = i as f64 / n as f64 * std::f64::consts::TAU;
        let t1 = (i + 1) as f64 / n as f64 * std::f64::consts::TAU;
        let (q0, q1) = (cc + glam::DVec2::from_angle(t0) * r, cc + glam::DVec2::from_angle(t1) * r);
        out.push(Prim::Line { a: map(q0), b: map(q1), w: 0.25, dashed: true, color: [90, 90, 90] });
        out.push(Prim::Line { a: to_sheet(sv, Some(p), q0.into()), b: to_sheet(sv, Some(p), q1.into()), w: 0.25, dashed: true, color: [90, 90, 90] });
    }
    out.push(Prim::Text { at: [v.pos[0], v.pos[1] - r * v.scale - 7.0], size: 3.2, text: format!("DETAIL ({})", fmt_scale(v.scale)), anchor: 1, color: [80, 80, 80] });
}

fn clip_circle(a: glam::DVec2, b: glam::DVec2, c: glam::DVec2, r: f64) -> Option<(glam::DVec2, glam::DVec2)> {
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

fn dim_prims(out: &mut Vec<Prim>, a: [f64; 2], b: [f64; 2], kind: u8, offset: f64, fmt: impl Fn(f64) -> String) {
    let (a, b) = (glam::DVec2::from(a), glam::DVec2::from(b));
    let (dir, len) = match kind {
        1 => (glam::DVec2::X, (b.x - a.x).abs()),
        2 => (glam::DVec2::Y, (b.y - a.y).abs()),
        _ => ((b - a).normalize_or(glam::DVec2::X), (b - a).length()),
    };
    let n = dir.perp();
    // dimension line endpoints: feet of a and b on the line offset from a
    let base = a + n * offset;
    let pa = base;
    let pb = base + dir * (b - a).dot(dir);
    let ext = |out: &mut Vec<Prim>, from: glam::DVec2, to: glam::DVec2| {
        let d = (to - from).normalize_or_zero();
        out.push(Prim::Line { a: (from + d * 1.0).into(), b: (to + d * 2.0).into(), w: 0.25, dashed: false, color: DIM_INK });
    };
    ext(out, a, pa);
    ext(out, b, pb);
    out.push(Prim::Line { a: pa.into(), b: pb.into(), w: 0.25, dashed: false, color: DIM_INK });
    // arrowheads
    let d = (pb - pa).normalize_or_zero();
    for (p, s) in [(pa, 1.0), (pb, -1.0)] {
        let tip = p;
        let back = p + d * 3.0 * s;
        out.push(Prim::Line { a: tip.into(), b: (back + d.perp() * 0.9).into(), w: 0.25, dashed: false, color: DIM_INK });
        out.push(Prim::Line { a: tip.into(), b: (back - d.perp() * 0.9).into(), w: 0.25, dashed: false, color: DIM_INK });
    }
    let mid = (pa + pb) / 2.0;
    if kind == 2 || (kind == 0 && d.x.abs() < 0.3) {
        // vertical-ish: text beside the line
        out.push(Prim::Text { at: (mid + glam::DVec2::new(-1.5, -1.5)).into(), size: 3.5, text: fmt(len), anchor: 2, color: DIM_INK });
    } else {
        out.push(Prim::Text { at: (mid + n.abs() * 1.2).into(), size: 3.5, text: fmt(len), anchor: 1, color: DIM_INK });
    }
}

// ---------------------------------------------------------------- toolbar / sidebar

pub fn toolbar(app: &mut App, ui: &mut Ui) {
    App::group(ui, |ui| {
        ui.menu_button(RichText::new("Insert view").size(12.5), |ui| {
            for si in 0..app.ws.studios.len() {
                let name = app.ws.studios[si].name.clone();
                ui.label(RichText::new(name).color(theme::muted()).size(11.0));
                for (dir, label) in DIRS {
                    if ui.button(*label).clicked() {
                        insert_view(app, si, dir);
                        ui.close();
                    }
                }
            }
            ui.separator();
            if ui.button("Standard views").clicked() {
                checkpoint(app);
                add_standard_views(app);
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
        if app.tool_button(ui, "dimension", "Dimension", "Dimension: click two points on a view, then place it (D)", true, t == DTool::Dimension, true).clicked() {
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
        if app.tool_button(ui, "autoDim", "Auto-dimension", "Add overall size and hole diameters to the selected view", false, false, has_view).clicked() {
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

fn auto_dimension(app: &mut App) {
    let Some(DSel::View(vid)) = app.dwg.sel else { return };
    let Some(v) = dwg(app).and_then(|d| d.views.iter().find(|v| v.id == vid)).cloned() else { return };
    if v.detail.is_some() || v.dir == "iso" {
        app.flash("Auto-dimension works on orthographic views", ToastKind::Info);
        return;
    }
    let Some(p) = app.dwg.proj.get(&(v.studio, v.dir.clone())).cloned() else { return };
    checkpoint(app);
    if let Some(d) = dwg_mut(app) {
        let dims = auto_dims(&p, vid, &mut d.next_id);
        let n = dims.len();
        d.dims.extend(dims);
        app.flash_info(format!("Added {n} dimensions"), Some(std::time::Duration::from_secs(2)));
    }
}

fn add_detail(app: &mut App) {
    let Some(DSel::View(src)) = app.dwg.sel else {
        app.flash("Select a view to take the detail from", ToastKind::Info);
        return;
    };
    let Some(d) = dwg(app) else { return };
    let Some(sv) = d.views.iter().find(|v| v.id == src).cloned() else { return };
    let Some(p) = app.dwg.proj.get(&(sv.studio, sv.dir.clone())).cloned() else { return };
    let c = [(p.bbox[0][0] + p.bbox[1][0]) / 2.0, (p.bbox[0][1] + p.bbox[1][1]) / 2.0];
    let r = ((p.bbox[1][0] - p.bbox[0][0]).max(p.bbox[1][1] - p.bbox[0][1]) * 0.2).max(1.0);
    let size = r * sv.scale * 2.0 * 2.0;
    let pos = free_spot(app, d, sv.sheet, [size + 10.0, size + 16.0]).unwrap_or([sv.pos[0] + (p.bbox[1][0] - p.bbox[0][0]) * sv.scale * 0.5 + r * sv.scale * 2.0 + 20.0, sv.pos[1]]);
    checkpoint(app);
    if let Some(d) = dwg_mut(app) {
        d.next_id += 1;
        let id = d.next_id;
        d.views.push(DrawView { id, studio: sv.studio, dir: sv.dir.clone(), pos, scale: sv.scale * 2.0, hidden_lines: false, detail: Some((src, c, r)), sheet: sv.sheet });
        app.dwg.sel = Some(DSel::View(id));
    }
}

/// Sheet-space rectangle a view occupies (min, max), when known.
fn view_extent(app: &App, v: &DrawView) -> Option<([f64; 2], [f64; 2])> {
    if let Some((_, _, r)) = v.detail {
        let rr = r * v.scale;
        return Some(([v.pos[0] - rr, v.pos[1] - rr - 8.0], [v.pos[0] + rr, v.pos[1] + rr]));
    }
    let Some(p) = app.dwg.proj.get(&(v.studio, v.dir.clone())) else {
        // not projected yet: assume the part's full size around the view's centre
        let r = part_size(app, v.studio) * v.scale / 2.0;
        return Some(([v.pos[0] - r, v.pos[1] - r - 8.0], [v.pos[0] + r, v.pos[1] + r]));
    };
    let (a, b) = (to_sheet(v, Some(p), p.bbox[0]), to_sheet(v, Some(p), p.bbox[1]));
    Some(([a[0].min(b[0]), a[1].min(b[1]) - 8.0], [a[0].max(b[0]), a[1].max(b[1])]))
}

/// A view's name: its direction, numbered when the drawing has several of that kind ("Isometric 2").
pub fn view_label(d: &Drawing, v: &DrawView) -> String {
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

/// Largest extent of a Part Studio's parts (the diagonal, so any view fits), mm.
fn part_size(app: &App, studio: usize) -> f64 {
    let mut lo = glam::DVec3::splat(f64::INFINITY);
    let mut hi = glam::DVec3::splat(f64::NEG_INFINITY);
    for b in app.studio_bodies(studio) {
        lo = lo.min(b.bbox[0].into());
        hi = hi.max(b.bbox[1].into());
    }
    if lo.x.is_finite() { (hi - lo).length() } else { 40.0 }
}

/// Centre of an empty area of `size` mm on a sheet, clear of views, the BOM and the title block.
fn free_spot(app: &App, d: &Drawing, sheet: usize, size: [f64; 2]) -> Option<[f64; 2]> {
    let [w, h] = sheet_size(&d.sheet);
    let mut taken: Vec<([f64; 2], [f64; 2])> = d.views.iter().filter(|v| v.sheet == sheet).filter_map(|v| view_extent(app, v)).collect();
    taken.push(([w - 10.0 - 170.0, 10.0], [w - 10.0, 46.0]));
    // dimensions, notes and tolerance frames on this sheet
    for dm in &d.dims {
        if let Some(v) = d.views.iter().find(|v| v.id == dm.view && v.sheet == sheet) {
            let pr = app.dwg.proj.get(&(v.studio, v.dir.clone()));
            let (a, b) = (to_sheet(v, pr, dm.a), to_sheet(v, pr, dm.b));
            let m = dm.offset.abs() + 6.0;
            taken.push(([a[0].min(b[0]) - m, a[1].min(b[1]) - m], [a[0].max(b[0]) + m, a[1].max(b[1]) + m]));
        }
    }
    for n in d.notes.iter().filter(|n| n.sheet == sheet) {
        taken.push(([n.pos[0], n.pos[1] - 2.0], [n.pos[0] + 3.0 * n.text.chars().count() as f64, n.pos[1] + 6.0]));
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

fn insert_view(app: &mut App, studio: usize, dir: &str) {
    checkpoint(app);
    let scale = dwg(app).and_then(|d| d.views.first()).map(|v| v.scale).unwrap_or(1.0);
    // size from the part's extent: iso views are about as wide as the part's diagonal
    let ext = part_size(app, studio) * scale * if dir == "iso" { 0.9 } else { 0.8 };
    let sheet = app.dwg.cur_sheet;
    let pos = dwg(app).and_then(|d| free_spot(app, d, sheet, [ext, ext])).unwrap_or(app.dwg.center);
    if let Some(d) = dwg_mut(app) {
        d.next_id += 1;
        let id = d.next_id;
        d.views.push(DrawView { id, studio, dir: dir.into(), pos, scale, hidden_lines: false, detail: None, sheet });
        app.dwg.sel = Some(DSel::View(id));
    }
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
            DSel::Dim(id) => d.dims.retain(|x| x.id != id),
            DSel::Note(id) => d.notes.retain(|x| x.id != id),
            DSel::Gdt(id) => d.gdts.retain(|x| x.id != id),
        }
    }
}

pub fn shortcuts(app: &mut App, ctx: &egui::Context) {
    if ctx.egui_wants_keyboard_input() {
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
            ui.add_space(8.0);
            ui.label(RichText::new("VIEWS").color(theme::muted()).size(11.0).strong());
            let cur = app.dwg.cur_sheet;
            let views: Vec<(u64, String, f64, bool, bool)> = app.ws.drawings[di].views.iter().filter(|v| v.sheet == cur).map(|v| (v.id, v.dir.clone(), v.scale, v.hidden_lines, v.detail.is_some())).collect();
            for (id, dir, scale, hidden, detail) in views {
                let _ = (&dir, detail);
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
            ui.add_space(8.0);
            ui.label(RichText::new(format!("{} dimensions · {} notes · {} tolerances", app.ws.drawings[di].dims.len(), app.ws.drawings[di].notes.len(), app.ws.drawings[di].gdts.len())).color(theme::muted()).size(12.0));
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
            // prefix of the selected dimension (e.g. "4× ")
            if let Some(DSel::Dim(id)) = app.dwg.sel {
                if let Some(dm) = app.ws.drawings[di].dims.iter_mut().find(|x| x.id == id) {
                    ui.add_space(8.0);
                    ui.label(RichText::new("DIMENSION").color(theme::muted()).size(11.0).strong());
                    ui.horizontal(|ui| {
                        ui.label("Prefix");
                        ui.add(egui::TextEdit::singleline(&mut dm.prefix).desired_width(80.0).hint_text("e.g. 4× "));
                    });
                    if dm.kind == 3 || dm.kind == 4 {
                        let mut dia = dm.kind == 4;
                        if ui.checkbox(&mut dia, "Diameter").changed() {
                            dm.kind = if dia { 4 } else { 3 };
                        }
                    }
                }
            }
        });
    });
}

// ---------------------------------------------------------------- canvas

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
    for p in &prims {
        match p {
            Prim::Line { a, b, w, dashed, color } => {
                let stroke = Stroke::new((*w as f32 * zoom * 0.6).clamp(0.6, 4.0), Color32::from_rgb(color[0], color[1], color[2]));
                if *dashed {
                    painter.extend(egui::Shape::dashed_line(&[to_screen(*a), to_screen(*b)], stroke, 4.0, 3.0));
                } else {
                    painter.line_segment([to_screen(*a), to_screen(*b)], stroke);
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
        }
    }

    // selection highlight
    let view_rect = |v: &DrawView| -> Option<egui::Rect> {
        if let Some((_, _, r)) = v.detail {
            let rr = r * v.scale;
            return Some(egui::Rect::from_two_pos(to_screen([v.pos[0] - rr, v.pos[1] - rr]), to_screen([v.pos[0] + rr, v.pos[1] + rr])));
        }
        let p = app.dwg.proj.get(&(v.studio, v.dir.clone()))?;
        Some(egui::Rect::from_two_pos(to_screen(to_sheet(v, Some(p), p.bbox[0])), to_screen(to_sheet(v, Some(p), p.bbox[1]))).expand(6.0))
    };
    if let Some(DSel::View(id)) = app.dwg.sel {
        if let Some(r) = d.views.iter().find(|v| v.id == id).and_then(|v| view_rect(v)) {
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
            let Some(pr) = app.dwg.proj.get(&(v.studio, v.dir.clone())) else { continue };
            let sp = from_sheet(v, Some(pr), to_sheet_pt(p));
            for pl in &pr.visible {
                if app.dwg.tool == DTool::Radial {
                    if let Some((c, r, full)) = fit_circle(pl) {
                        let dd = ((glam::DVec2::from(sp) - glam::DVec2::from(c)).length() - r).abs() * v.scale * zoom as f64;
                        // arcs: only near the arc itself
                        let near_arc = full || pl.iter().any(|q| glam::DVec2::from(*q).distance(sp.into()) * v.scale * (zoom as f64) < 14.0);
                        if dd < 8.0 && near_arc && best_c.map(|b| (dd as f32) < b.0).unwrap_or(true) {
                            // the leader point snapped onto the circle
                            let on = glam::DVec2::from(c) + (glam::DVec2::from(sp) - glam::DVec2::from(c)).normalize_or(glam::DVec2::X) * r;
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
            let pr = app.dwg.proj.get(&(v.studio, v.dir.clone()));
            let r = glam::DVec2::from(c).distance(sp.into());
            let n = 48;
            let pts: Vec<Pos2> = (0..=n)
                .map(|i| {
                    let q = glam::DVec2::from(c) + glam::DVec2::from_angle(i as f64 / n as f64 * std::f64::consts::TAU) * r;
                    to_screen(to_sheet(v, pr, q.into()))
                })
                .collect();
            painter.add(egui::Shape::line(pts, Stroke::new(2.0, theme::vp::hover())));
        }
        if let Some((_, vid, a, b)) = best_l {
            app.dwg.hover_line = Some((vid, a, b));
            let v = views.iter().find(|v| v.id == vid).unwrap();
            let pr = app.dwg.proj.get(&(v.studio, v.dir.clone()));
            painter.line_segment([to_screen(to_sheet(v, pr, a)), to_screen(to_sheet(v, pr, b))], Stroke::new(2.5, theme::vp::hover()));
        }
        if let Some((vid, a, b)) = app.dwg.angle_first {
            if let Some(v) = views.iter().find(|v| v.id == vid) {
                let pr = app.dwg.proj.get(&(v.studio, v.dir.clone()));
                painter.line_segment([to_screen(to_sheet(v, pr, a)), to_screen(to_sheet(v, pr, b))], Stroke::new(2.5, theme::accent()));
            }
        }
    }
    if matches!(app.dwg.tool, DTool::Dimension | DTool::Gdt | DTool::Datum) {
        if let Some(p) = pointer {
            let mut best: Option<(f32, u64, [f64; 2])> = None;
            for v in &views {
                let Some(pr) = app.dwg.proj.get(&(v.studio, v.dir.clone())) else { continue };
                for pl in &pr.visible {
                    for q in [pl[0], pl[pl.len() - 1]] {
                        let s = to_screen(to_sheet(v, Some(pr), q));
                        let dd = (s - p).length();
                        if dd < 10.0 && best.map(|b| dd < b.0).unwrap_or(true) {
                            best = Some((dd, v.id, q));
                        }
                    }
                }
            }
            if let Some((_, vid, q)) = best {
                app.dwg.hover_pt = Some((vid, q));
                let v = d.views.iter().find(|v| v.id == vid).unwrap();
                let s = to_screen(to_sheet(v, app.dwg.proj.get(&(v.studio, v.dir.clone())), q));
                painter.circle_filled(s, 4.0, theme::vp::hover());
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
                        if view_rect(v).map(|r| r.contains(p)).unwrap_or(false) {
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
                            let pr = app.dwg.proj.get(&(v.studio, v.dir.clone()));
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
                                checkpoint(app);
                                if let Some(dd) = dwg_mut(app) {
                                    dd.next_id += 1;
                                    let id = dd.next_id;
                                    dd.dims.push(DrawDim { id, view: vid, a, b: q, kind, offset: 8.0, extra: vec![], prefix: String::new() });
                                }
                                app.dwg.dim_first = None;
                            }
                            _ => app.dwg.dim_first = Some((vid, q)),
                        }
                    }
                }
                DTool::Note => app.dwg.note_edit = Some((None, sp, String::new())),
                DTool::Radial => {
                    if let Some((vid, c, on, full)) = app.dwg.hover_circle {
                        checkpoint(app);
                        if let Some(dd) = dwg_mut(app) {
                            dd.next_id += 1;
                            let id = dd.next_id;
                            dd.dims.push(DrawDim { id, view: vid, a: c, b: on, kind: if full { 4 } else { 3 }, offset: 8.0, extra: vec![], prefix: String::new() });
                            app.dwg.sel = Some(DSel::Dim(id));
                        }
                    }
                }
                DTool::Angle => {
                    if let Some((vid, a, b)) = app.dwg.hover_line {
                        match app.dwg.angle_first {
                            Some((v0, a0, b0)) if v0 == vid && (a0, b0) != (a, b) => {
                                let (u, w) = (glam::DVec2::from(b0) - glam::DVec2::from(a0), glam::DVec2::from(b) - glam::DVec2::from(a));
                                if u.perp_dot(w).abs() < 1e-9 * u.length() * w.length() {
                                    app.flash("Those lines are parallel", ToastKind::Info);
                                } else {
                                    checkpoint(app);
                                    if let Some(dd) = dwg_mut(app) {
                                        dd.next_id += 1;
                                        let id = dd.next_id;
                                        dd.dims.push(DrawDim { id, view: vid, a: a0, b: b0, kind: 5, offset: 12.0, extra: vec![a, b], prefix: String::new() });
                                        app.dwg.sel = Some(DSel::Dim(id));
                                    }
                                }
                                app.dwg.angle_first = None;
                            }
                            _ => app.dwg.angle_first = Some((vid, a, b)),
                        }
                    }
                }
                DTool::Gdt | DTool::Datum => {
                    // the leader goes to snapped geometry, else to the click itself
                    let leader = app.dwg.hover_pt.and_then(|(vid, q)| d.views.iter().find(|v| v.id == vid).map(|v| to_sheet(v, app.dwg.proj.get(&(v.studio, v.dir.clone())), q))).unwrap_or(sp);
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

    // note editor
    if let Some((id, pos, mut text)) = app.dwg.note_edit.clone() {
        let mut done = None;
        egui::Area::new(egui::Id::new("note-edit")).order(egui::Order::Foreground).fixed_pos(to_screen(pos)).show(ui.ctx(), |ui| {
            egui::Frame::new().fill(theme::panel()).stroke(Stroke::new(1.0, theme::accent())).corner_radius(4).inner_margin(4).show(ui, |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut text).hint_text("Note").desired_width(200.0));
                if !r.has_focus() && !r.lost_focus() {
                    r.request_focus();
                }
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = Some(true);
                } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        match done {
            Some(true) => {
                app.dwg.note_edit = None;
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
        DTool::Select => "Drag views, notes or dimensions to move them · wheel zooms, right-drag pans",
        DTool::Dimension if app.dwg.dim_first.is_some() => "Click the second point on the same view",
        DTool::Dimension => "Click a corner or line end on a view",
        DTool::Note => "Click where the note goes",
        DTool::Radial => "Click a circle (diameter) or an arc (radius); drag the dimension to lengthen its leader",
        DTool::Angle if app.dwg.angle_first.is_some() => "Click the second line",
        DTool::Angle => "Click a straight edge in a view",
        DTool::Gdt => "Click the feature the tolerance frame points at; edit it in the sidebar",
        DTool::Datum => "Click the feature to mark as a datum",
    };
    painter.text(rect.left_bottom() + egui::vec2(10.0, -10.0), egui::Align2::LEFT_BOTTOM, hint, egui::FontId::proportional(12.0), theme::muted());
    let _ = (theme::border(), theme::text(), from_sheet);
}

// ---------------------------------------------------------------- export

fn export(app: &mut App, format: &str) {
    let Some(d) = dwg(app).cloned() else { return };
    let ext = format;
    let Some(path) = rfd::FileDialog::new().add_filter(ext.to_uppercase(), &[ext]).set_file_name(format!("{}.{ext}", d.name)).save_file() else { return };
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

    fn circle(c: [f64; 2], r: f64, a0: f64, a1: f64) -> Vec<[f64; 2]> {
        (0..=40).map(|i| {
            let t = a0 + (a1 - a0) * i as f64 / 40.0;
            [c[0] + r * t.cos(), c[1] + r * t.sin()]
        }).collect()
    }

    #[test]
    fn circles_auto_dims_and_annotations() {
        let tau = std::f64::consts::TAU;
        assert!(matches!(fit_circle(&circle([3.0, 4.0], 2.5, 0.0, tau)), Some((c, r, true)) if (c[0] - 3.0).abs() < 1e-9 && (r - 2.5).abs() < 1e-9));
        assert!(matches!(fit_circle(&circle([0.0, 0.0], 5.0, 0.0, 1.0)), Some((_, _, false))));
        assert!(fit_circle(&[[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [3.0, 0.0], [4.0, 0.0]]).is_none());
        // a 40x20 plate with two 5 mm holes
        let p = ProjView {
            visible: vec![
                vec![[0.0, 0.0], [40.0, 0.0]],
                vec![[40.0, 0.0], [40.0, 20.0]],
                circle([10.0, 10.0], 2.5, 0.0, tau),
                circle([30.0, 10.0], 2.5, 0.0, tau),
            ],
            hidden: vec![],
            hatch: vec![],
            bbox: [[0.0, 0.0], [40.0, 20.0]],
        };
        let mut id = 0;
        let dims = auto_dims(&p, 1, &mut id);
        assert_eq!(dims.len(), 3);
        assert_eq!(dims[2].kind, 4);
        assert_eq!(dims[2].prefix, "2× ");
        // annotations draw without trouble
        let mut out = vec![];
        angle_prims(&mut out, [[0.0, 0.0], [10.0, 0.0]], [[0.0, 0.0], [10.0, 10.0]], 8.0);
        assert!(out.iter().any(|p| matches!(p, Prim::Text { text, .. } if text == "45°")));
        for (k, _) in GDT_KINDS {
            gdt_prims(&mut out, &Gdt { id: 1, sheet: 0, pos: [0.0, 0.0], kind: k.to_string(), tol: "Ø0.1".into(), datums: "A B".into(), leader: Some([-5.0, -5.0]) });
        }
        let pdf = crate::drawing_export::pdf_pages(&[&out, &out], [297.0, 210.0]);
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/Count 2"));
        assert!(text.contains("\\330")); // Ø in WinAnsi
    }
}
