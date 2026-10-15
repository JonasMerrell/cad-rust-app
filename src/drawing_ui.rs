//! Drawing tab: a sheet with hidden-line projected views of a Part Studio,
//! dimensions and notes, exported to PDF, DXF or SVG.

use std::collections::{HashMap, HashSet};

use egui::{Color32, Margin, Pos2, RichText, Sense, Stroke, Ui};

use crate::app::{App, ToastKind};
use crate::asm_ui::checkpoint;
use crate::drawing::{DrawDim, DrawView, Drawing, Note};
use crate::kernel::ProjView;
use crate::theme;
use crate::workspace::Tab;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DTool {
    #[default]
    Select,
    Dimension,
    Note,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DSel {
    View(u64),
    Dim(u64),
    Note(u64),
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

fn units_of(app: &App, studio: usize) -> crate::expr::Units {
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
    let Some(d) = dwg_mut(app) else { return };
    for (dir, pos) in [
        ("front", front),
        ("top", [front[0], front[1] + fh / 2.0 + gap + size.y * scale / 2.0]),
        ("right", [front[0] + fw / 2.0 + gap + size.y * scale / 2.0, front[1]]),
        ("iso", [sheet[0] - 30.0 - (size.x + size.y) * scale * 0.45, sheet[1] - 30.0 - (size.z + size.y) * scale * 0.45]),
    ] {
        d.next_id += 1;
        let id = d.next_id;
        d.views.push(DrawView { id, studio, dir: dir.into(), pos, scale, hidden_lines: false, detail: None });
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

/// A drawing primitive in sheet millimetres.
pub enum Prim {
    Line { a: [f64; 2], b: [f64; 2], w: f64, dashed: bool, color: [u8; 3] },
    Text { at: [f64; 2], size: f64, text: String, anchor: u8, color: [u8; 3] },
}

const INK: [u8; 3] = [20, 20, 20];
const DIM_INK: [u8; 3] = [20, 60, 140];

/// Everything on the sheet, for both the screen and the exporters.
pub fn sheet_prims(app: &App, d: &Drawing) -> Vec<Prim> {
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
    let units = d.views.first().map(|v| units_of(app, v.studio)).unwrap_or_default();
    let scale = d.views.first().map(|v| fmt_scale(v.scale)).unwrap_or_default();
    let studio = d.views.first().and_then(|v| app.ws.studios.get(v.studio)).map(|s| s.name.clone()).unwrap_or_default();
    out.push(Prim::Text { at: [tx + 4.0, ty + 24.0], size: 6.0, text: d.name.clone(), anchor: 0, color: INK });
    out.push(Prim::Text { at: [tx + 4.0, ty + 6.0], size: 3.2, text: format!("Part: {studio}"), anchor: 0, color: INK });
    out.push(Prim::Text { at: [tx + 114.0, ty + 11.0], size: 3.2, text: format!("Scale {scale}"), anchor: 0, color: INK });
    out.push(Prim::Text { at: [tx + 114.0, ty + 4.0], size: 3.2, text: format!("Units {}", units.label()), anchor: 0, color: INK });
    out.push(Prim::Text { at: [w - m - 4.0, ty + 24.0], size: 3.0, text: "cad-app".into(), anchor: 2, color: [110, 110, 110] });

    for v in &d.views {
        if let Some((src, c, r)) = v.detail {
            detail_prims(app, d, v, src, c, r, &mut out);
            continue;
        }
        let proj = app.dwg.proj.get(&(v.studio, v.dir.clone()));
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
        let label = DIRS.iter().find(|x| x.0 == v.dir).map(|x| x.1).unwrap_or("");
        let bottom = to_sheet(v, Some(p), [0.0, p.bbox[0][1]])[1];
        out.push(Prim::Text { at: [v.pos[0], bottom - 7.0], size: 3.2, text: format!("{} ({})", label.to_uppercase(), fmt_scale(v.scale)), anchor: 1, color: [80, 80, 80] });
    }
    for dm in &d.dims {
        let Some(v) = d.views.iter().find(|v| v.id == dm.view) else { continue };
        let p = app.dwg.proj.get(&(v.studio, v.dir.clone()));
        let (a, b) = (to_sheet(v, p, dm.a), to_sheet(v, p, dm.b));
        let units = units_of(app, v.studio);
        dim_prims(&mut out, a, b, dm.kind, dm.offset, |len| units.fmt(len / v.scale));
    }
    for n in &d.notes {
        out.push(Prim::Text { at: n.pos, size: 3.5, text: n.text.clone(), anchor: 0, color: INK });
    }
    if let Some((ai, pos)) = d.bom {
        if let Some(a) = app.ws.assemblies.get(ai) {
            bom_prims(app, a, pos, &mut out);
        }
    }
    out
}

fn bom_prims(app: &App, a: &crate::assembly::Assembly, pos: [f64; 2], out: &mut Vec<Prim>) {
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
fn detail_prims(app: &App, d: &Drawing, v: &DrawView, src: u64, c: [f64; 2], r: f64, out: &mut Vec<Prim>) {
    let Some(sv) = d.views.iter().find(|x| x.id == src) else { return };
    let Some(p) = app.dwg.proj.get(&(sv.studio, sv.dir.clone())) else { return };
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
                        if let Some(d) = dwg_mut(app) {
                            d.bom = Some((i, [sheet[0] - 10.0 - 120.0, sheet[1] - 10.0]));
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
        if app.tool_button(ui, "text", "Note", "Note: click to place text (N)", false, t == DTool::Note, true).clicked() {
            app.dwg.tool = DTool::Note;
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
    checkpoint(app);
    if let Some(d) = dwg_mut(app) {
        d.next_id += 1;
        let id = d.next_id;
        let pos = [sv.pos[0] + (p.bbox[1][0] - p.bbox[0][0]) * sv.scale * 0.5 + r * sv.scale * 2.0 + 20.0, sv.pos[1]];
        d.views.push(DrawView { id, studio: sv.studio, dir: sv.dir.clone(), pos, scale: sv.scale * 2.0, hidden_lines: false, detail: Some((src, c, r)) });
        app.dwg.sel = Some(DSel::View(id));
    }
}

fn insert_view(app: &mut App, studio: usize, dir: &str) {
    checkpoint(app);
    let center = app.dwg.center;
    let scale = dwg(app).and_then(|d| d.views.first()).map(|v| v.scale).unwrap_or(1.0);
    if let Some(d) = dwg_mut(app) {
        d.next_id += 1;
        let id = d.next_id;
        d.views.push(DrawView { id, studio, dir: dir.into(), pos: center, scale, hidden_lines: false, detail: None });
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
            ui.add_space(8.0);
            ui.label(RichText::new("VIEWS").color(theme::muted()).size(11.0).strong());
            let views: Vec<(u64, String, f64, bool, bool)> = app.ws.drawings[di].views.iter().map(|v| (v.id, v.dir.clone(), v.scale, v.hidden_lines, v.detail.is_some())).collect();
            for (id, dir, scale, hidden, detail) in views {
                let label = if detail { "Detail" } else { DIRS.iter().find(|x| x.0 == dir).map(|x| x.1).unwrap_or("View") };
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
            ui.label(RichText::new(format!("{} dimensions · {} notes", app.ws.drawings[di].dims.len(), app.ws.drawings[di].notes.len())).color(theme::muted()).size(12.0));
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
    if resp.dragged_by(egui::PointerButton::Middle) || resp.dragged_by(egui::PointerButton::Secondary) {
        let dlt = resp.drag_delta();
        app.dwg.center[0] -= (dlt.x / zoom) as f64;
        app.dwg.center[1] += (dlt.y / zoom) as f64;
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, if theme::is_dark() { theme::vp::background() } else { theme::hex(0xdfe3e8) });
    let sheet_rect = egui::Rect::from_two_pos(to_screen([0.0, 0.0]), to_screen([sw, sh]));
    painter.rect_filled(sheet_rect.translate(egui::vec2(3.0, 3.0)), 0.0, Color32::from_black_alpha(if theme::is_dark() { 90 } else { 40 }));
    painter.rect_filled(sheet_rect, 0.0, Color32::from_rgb(250, 250, 248));

    let prims = sheet_prims(app, &d);
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

    // snapping to view geometry for dimensions
    let pointer = resp.hover_pos();
    app.dwg.hover_pt = None;
    if app.dwg.tool == DTool::Dimension {
        if let Some(p) = pointer {
            let mut best: Option<(f32, u64, [f64; 2])> = None;
            for v in &d.views {
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
                    for v in &d.views {
                        if view_rect(v).map(|r| r.contains(p)).unwrap_or(false) {
                            app.dwg.sel = Some(DSel::View(v.id));
                        }
                    }
                    for n in &d.notes {
                        if (to_screen(n.pos) - p).length() < 20.0 {
                            app.dwg.sel = Some(DSel::Note(n.id));
                        }
                    }
                    for dm in &d.dims {
                        if let Some(v) = d.views.iter().find(|v| v.id == dm.view) {
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
                                    dd.dims.push(DrawDim { id, view: vid, a, b: q, kind, offset: 8.0 });
                                }
                                app.dwg.dim_first = None;
                            }
                            _ => app.dwg.dim_first = Some((vid, q)),
                        }
                    }
                }
                DTool::Note => app.dwg.note_edit = Some((None, sp, String::new())),
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
                if let Some(dd) = dwg_mut(app) {
                    match sel {
                        DSel::View(id) => {
                            if let Some(v) = dd.views.iter_mut().find(|v| v.id == id) {
                                v.pos = [orig[0] + dlt[0], orig[1] + dlt[1]];
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
                                dd.notes.push(Note { id: nid, pos, text });
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
    };
    painter.text(rect.left_bottom() + egui::vec2(10.0, -10.0), egui::Align2::LEFT_BOTTOM, hint, egui::FontId::proportional(12.0), theme::muted());
    let _ = (theme::border(), theme::text(), from_sheet);
}

// ---------------------------------------------------------------- export

fn export(app: &mut App, format: &str) {
    let Some(d) = dwg(app).cloned() else { return };
    let ext = format;
    let Some(path) = rfd::FileDialog::new().add_filter(ext.to_uppercase(), &[ext]).set_file_name(format!("{}.{ext}", d.name)).save_file() else { return };
    let prims = sheet_prims(app, &d);
    let size = sheet_size(&d.sheet);
    let data = match format {
        "pdf" => crate::drawing_export::pdf(&prims, size),
        "dxf" => crate::drawing_export::dxf(&prims).into_bytes(),
        _ => crate::drawing_export::svg(&prims, size).into_bytes(),
    };
    match std::fs::write(&path, data) {
        Ok(()) => app.flash_info(format!("Exported {}", path.display()), Some(std::time::Duration::from_secs(3))),
        Err(e) => app.flash(format!("Could not export: {e}"), ToastKind::Error),
    }
}
