//! Setup sheet: a printable HTML page for the operator with the stock, work zero, fixtures, tools and
//! operations of a setup, and a picture of the part on the machine.

use std::collections::HashMap;
use std::fmt::Write;

use base64::Engine;
use glam::DVec3;

use crate::cam::{fmt_mm, Cam, OpKind, ToolKind};
use crate::cam_paths::{self, Move, Part, Toolpath};
use crate::cam_sim::{class_rgb, Sim, CLASS_GOUGE, CLASS_PATH, CLASS_STOCK};
use crate::kernel::BodyMesh;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn time(s: f64) -> String {
    let s = s.max(0.0).round() as i64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn xyz(v: DVec3) -> String {
    format!("{}, {}, {}", fmt_mm(v.x), fmt_mm(v.y), fmt_mm(v.z))
}

const BG: [u8; 3] = [0xff, 0xff, 0xff];
const EDGE: [u8; 3] = [0x3a, 0x42, 0x4d];
const PART_RGB: [u8; 3] = [0xa9, 0xc0, 0xdc];
const JAW_RGB: [u8; 3] = [0x86, 0xae, 0xe0];
const FIXTURE_RGB: [u8; 3] = [0xe6, 0xb4, 0x5e];
const STOCK_LINE: [u8; 3] = [0x55, 0x5e, 0x6b];
const RAPID_RGB: [u8; 3] = [0xe0, 0x44, 0x44];
const UNCUT_RGB: [u8; 3] = [0xe6, 0xde, 0xcb];
const CUT_RGB: [u8; 3] = [0xc6, 0xcc, 0xd5];
const AXIS_RGB: [[u8; 3]; 3] = [[0xd8, 0x32, 0x32], [0x2a, 0xa1, 0x4a], [0x2f, 0x6f, 0xe0]];

/// A picture for the sheet, with labels placed on it (fractions of its width and height, text, CSS class).
pub struct Shot {
    pub png: Vec<u8>,
    pub labels: Vec<([f32; 2], String, &'static str)>,
}

/// What a picture shows (setup coordinates): lit meshes, coloured lines (depth tested or on top) and labelled points.
#[derive(Default)]
struct Scene {
    meshes: Vec<(BodyMesh, [u8; 3])>,
    lines: Vec<(Vec<DVec3>, [u8; 3], bool)>,
    labels: Vec<(DVec3, String, &'static str)>,
}

impl Scene {
    fn bounds(&self) -> (DVec3, DVec3) {
        let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
        for (m, _) in self.meshes.iter().filter(|m| !m.0.indices.is_empty()) {
            lo = lo.min(DVec3::from(m.bbox[0]));
            hi = hi.max(DVec3::from(m.bbox[1]));
        }
        for p in self.lines.iter().flat_map(|l| l.0.iter()).chain(self.labels.iter().map(|l| &l.0)) {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        (lo, hi)
    }

    /// Rendered from `view`, framed on what it shows (or `fit`) with a margin for the labels.
    fn shoot(&self, view: &str, w: u32, h: u32, fit: Option<(DVec3, DVec3)>) -> Option<Shot> {
        let (lo, hi) = fit.unwrap_or_else(|| self.bounds());
        if !lo.x.is_finite() {
            return None;
        }
        let pad = (hi - lo).length() * 0.04;
        let items: Vec<_> = self.meshes.iter().filter(|m| !m.0.indices.is_empty()).map(|(m, c)| (m, crate::assembly::Pose::default(), *c)).collect();
        let look = crate::agent::Look { bg: BG, edge: EDGE, lift: (hi - lo).length() * 0.004, scale: 2, fit: Some((lo - DVec3::splat(pad), hi + DVec3::splat(pad))), tight: true };
        let anchors: Vec<DVec3> = self.labels.iter().map(|l| l.0).collect();
        let (png, at) = crate::agent::raster_look(&items, &self.lines, view, None, w, h, &look, &anchors).ok()?;
        Some(Shot { png, labels: at.into_iter().zip(&self.labels).map(|(a, l)| (a, l.1.clone(), l.2)).collect() })
    }

    fn wire_box(&mut self, lo: DVec3, hi: DVec3, col: [u8; 3]) {
        let c = |k: usize| DVec3::new([lo.x, hi.x][k & 1], [lo.y, hi.y][(k >> 1) & 1], [lo.z, hi.z][(k >> 2) & 1]);
        self.lines.push(([0, 1, 3, 2, 0, 4, 5, 7, 6, 4].iter().map(|&k| c(k)).collect(), col, true));
        for (a, b) in [(1, 5), (3, 7), (2, 6)] {
            self.lines.push((vec![c(a), c(b)], col, true));
        }
    }

    /// The X, Y and Z arrows from the work zero, on top of everything.
    fn axes(&mut self, o: DVec3, len: f64) {
        for (k, d) in [DVec3::X, DVec3::Y, DVec3::Z].into_iter().enumerate() {
            self.lines.push((vec![o, o + d * len], AXIS_RGB[k], false));
            self.labels.push((o + d * len * 1.18, ["X", "Y", "Z"][k].into(), ["ax x", "ax y", "ax z"][k]));
        }
        self.labels.push((o, "zero".into(), "wcs"));
    }
}

fn shade(c: [u8; 3], f: f32) -> [u8; 3] {
    c.map(|v| (v as f32 * f).min(255.0) as u8)
}

/// Halfway to white.
fn tint(c: [u8; 3]) -> [u8; 3] {
    c.map(|v| ((v as u16 + 255) / 2) as u8)
}

/// A mesh of triangles with its sharp edges (over 25° between faces, or open) drawn.
fn mesh(tris: &[[DVec3; 3]], creases: bool) -> BodyMesh {
    let mut m = crate::cam_jaws::body_mesh("", tris);
    if !creases {
        return m;
    }
    type K = (i64, i64, i64);
    let key = |p: DVec3| -> K { ((p.x * 1e3).round() as i64, (p.y * 1e3).round() as i64, (p.z * 1e3).round() as i64) };
    let mut by: HashMap<(K, K), (DVec3, DVec3, Vec<DVec3>)> = HashMap::new();
    for t in tris {
        let n = (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
        if n == DVec3::ZERO {
            continue;
        }
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let (ka, kb) = (key(a), key(b));
            let e = by.entry(if ka < kb { (ka, kb) } else { (kb, ka) }).or_insert((a, b, vec![]));
            e.2.push(n);
        }
    }
    let cos = 25f64.to_radians().cos();
    for (a, b, ns) in by.into_values() {
        if ns.len() != 2 || ns[0].dot(ns[1]) < cos {
            m.edges.push(crate::kernel::EdgeInfo { pts: vec![a.as_vec3().to_array(), b.as_vec3().to_array()], ..Default::default() });
        }
    }
    m
}

fn box_tris(lo: DVec3, hi: DVec3) -> Vec<[DVec3; 3]> {
    let c = |k: usize| DVec3::new([lo.x, hi.x][k & 1], [lo.y, hi.y][(k >> 1) & 1], [lo.z, hi.z][(k >> 2) & 1]);
    [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]].iter().flat_map(|q| [[c(q[0]), c(q[1]), c(q[2])], [c(q[0]), c(q[2]), c(q[3])]]).collect()
}

/// The parts of a triangle inside a box, as triangles (Sutherland-Hodgman against the six sides).
fn clip_tri(t: [DVec3; 3], lo: DVec3, hi: DVec3, out: &mut Vec<[DVec3; 3]>) {
    let mut poly = t.to_vec();
    for k in 0..6 {
        let (axis, keep_below, v) = (k % 3, k >= 3, if k >= 3 { hi[k % 3] } else { lo[k % 3] });
        let inside = |p: &DVec3| if keep_below { p[axis] <= v } else { p[axis] >= v };
        let mut next = vec![];
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            if inside(&a) {
                next.push(a);
            }
            if inside(&a) != inside(&b) {
                let f = (v - a[axis]) / (b[axis] - a[axis]);
                next.push(a + (b - a) * f);
            }
        }
        poly = next;
        if poly.len() < 3 {
            return;
        }
    }
    for i in 1..poly.len() - 1 {
        out.push([poly[0], poly[i], poly[i + 1]]);
    }
}

/// What holds the work: soft jaws, fixtures, and (with `machine`) the vise and table of the machine model
/// near the stock.
fn holding(cam: &Cam, part: &Part, sim: Option<&Sim>, machine: bool, scene: &mut Scene) {
    let o = cam_paths::work_origin(cam, part);
    if let Some(j) = crate::cam_jaws::jaws(cam, part) {
        scene.meshes.push((mesh(&crate::cam_jaws::jaw_tris(&j), true), JAW_RGB));
    }
    for f in &cam.fixtures {
        let (a, b) = (DVec3::from(f.min) + o, DVec3::from(f.max) + o);
        scene.meshes.push((mesh(&box_tris(a.min(b), a.max(b)), true), FIXTURE_RGB));
    }
    let Some(sm) = sim.and_then(|s| s.machine.as_ref()).filter(|_| machine) else { return };
    let (lo, hi) = cam_paths::stock_box(cam, part);
    let m = &sm.placed.machine.model;
    let chain = m.chain(m.table_on);
    let margin = ((hi - lo).truncate().max_element() * 0.35).max(35.0);
    let (blo, bhi) = (DVec3::new(lo.x - margin, lo.y - margin, lo.z - margin), DVec3::new(hi.x + margin, hi.y + margin, hi.z + 5.0));
    let mut by_colour: Vec<([u8; 3], Vec<[DVec3; 3]>)> = vec![];
    for (i, p) in m.parts.iter().enumerate() {
        if !(chain.contains(&p.carrier) || chain.is_empty() && p.carrier == crate::cam_machine::Carrier::Base) {
            continue;
        }
        let mut tris = vec![];
        for t in &sm.placed.machine.tris[i] {
            clip_tri(t.map(|v| v.as_dvec3() + sm.placed.shift), blo, bhi, &mut tris);
        }
        if tris.is_empty() {
            continue;
        }
        match by_colour.iter_mut().find(|c| c.0 == p.color) {
            Some(c) => c.1.extend(tris),
            None => by_colour.push((p.color, tris)),
        }
    }
    for (c, tris) in by_colour {
        scene.meshes.push((mesh(&tris, true), c));
    }
}

/// The setup as on the machine (spindle above), from `view`: the part, what holds it, the stock outline and
/// the work zero, and the stock's sizes along the sides that view shows square on.
fn setup_shot(cam: &Cam, part: &Part, sim: Option<&Sim>, view: &str, w: u32, h: u32) -> Option<Shot> {
    let mut sc = Scene::default();
    if !part.tris.is_empty() {
        sc.meshes.push((mesh(&part.tris, true), PART_RGB));
    }
    holding(cam, part, sim, view == "iso", &mut sc);
    let (lo, hi) = cam_paths::stock_box(cam, part);
    sc.wire_box(lo, hi, STOCK_LINE);
    let size = hi - lo;
    sc.axes(cam_paths::work_origin(cam, part), size.max_element() * 0.18);
    let off = size.max_element() * 0.09;
    let mid = (lo + hi) / 2.0;
    let mm = |v: f64| format!("{} mm", fmt_mm(v));
    match view {
        "top" => {
            sc.labels.push((DVec3::new(mid.x, lo.y - off, hi.z), mm(size.x), "dim"));
            sc.labels.push((DVec3::new(lo.x - off, mid.y, hi.z), mm(size.y), "dim"));
        }
        "front" => {
            sc.labels.push((DVec3::new(mid.x, lo.y, lo.z - off), mm(size.x), "dim"));
            sc.labels.push((DVec3::new(hi.x + off, lo.y, mid.z), mm(size.z), "dim"));
        }
        _ => {}
    }
    sc.shoot(view, w, h, None)
}

/// A toolpath as polylines: (cuts, rapids).
fn path_lines(tp: &Toolpath) -> (Vec<Vec<DVec3>>, Vec<Vec<DVec3>>) {
    let (mut feeds, mut rapids) = (vec![], vec![]);
    let mut cur: Vec<DVec3> = vec![];
    let mut prev: Option<DVec3> = None;
    for m in &tp.moves {
        let Some(p) = prev else {
            prev = Some(m.end(DVec3::ZERO));
            continue;
        };
        match *m {
            Move::Feed(q, _) => {
                if cur.is_empty() {
                    cur.push(p);
                }
                cur.push(DVec3::from(q));
            }
            Move::Rapid(q) => {
                if cur.len() > 1 {
                    feeds.push(std::mem::take(&mut cur));
                }
                cur.clear();
                rapids.push(vec![p, DVec3::from(q)]);
            }
            Move::Tap { z, .. } => {
                feeds.push(vec![p, DVec3::new(p.x, p.y, z)]);
            }
            Move::Dwell(_) => {}
        }
        prev = Some(m.end(p));
    }
    if cur.len() > 1 {
        feeds.push(cur);
    }
    (feeds, rapids)
}

/// The simulated stock as meshes, coloured by `colour` of each mesh class.
fn sim_meshes(sim: &Sim, cells: usize, colour: impl Fn(u8) -> [u8; 3], sc: &mut Scene) {
    let (tris, cls) = sim.mesh(cells);
    let mut by: Vec<(u8, Vec<[DVec3; 3]>)> = vec![];
    for (t, c) in tris.into_iter().zip(cls) {
        match by.iter_mut().find(|b| b.0 == c) {
            Some(b) => b.1.push(t),
            None => by.push((c, vec![t])),
        }
    }
    for (c, t) in by {
        sc.meshes.push((mesh(&t, false), colour(c)));
    }
}

/// Pictures for the sheet.
pub struct Visuals {
    pub iso: Option<Shot>,
    pub top: Option<Shot>,
    pub front: Option<Shot>,
    /// Per toolpath (same order): the stock after it with its path drawn.
    pub ops: Vec<Option<Shot>>,
    /// The finished simulation, from above at an angle and from the top, and what it found.
    pub sim: Option<(Option<Shot>, Option<Shot>)>,
    pub events: Vec<crate::cam_sim::SimEvent>,
}

/// The simulation for the sheet: from what the setup before left (`left`), with the setup's machine model
/// if it has one. Not run yet.
pub fn new_sim(cam: &Cam, part: &Part, paths: &[Toolpath], left: Option<&crate::cam_stock::StockMap>) -> Sim {
    let stock = cam_paths::stock_box(cam, part);
    let mut sim = Sim::new(cam, part, stock, paths, 320);
    if let Some(m) = left {
        sim.start_from(m);
    }
    if !cam.machine.model.is_empty() {
        if let Ok(l) = crate::cam_machine::load(&cam.machine.model) {
            let placed = std::sync::Arc::new(Sim::place_machine(cam, part, stock, l));
            sim.set_machine(cam, part, placed);
        }
    }
    sim
}

/// Render the sheet's pictures, running `sim` (from `new_sim`) through the toolpaths on the way.
pub fn visuals(cam: &Cam, part: &Part, paths: &[Toolpath], sim: Option<Sim>) -> Visuals {
    let iso = setup_shot(cam, part, sim.as_ref(), "iso", 760, 540);
    let top = setup_shot(cam, part, sim.as_ref(), "top", 400, 300);
    let front = setup_shot(cam, part, sim.as_ref(), "front", 400, 300);
    let mut v = Visuals { iso, top, front, ops: vec![], sim: None, events: vec![] };
    let Some(mut sim) = sim else {
        v.ops = paths.iter().map(|_| None).collect();
        return v;
    };
    let (lo, hi) = cam_paths::stock_box(cam, part);
    let fit = (lo, hi + DVec3::Z * cam.clearance.max(1.0));
    let mut end = 0;
    for (i, tp) in paths.iter().enumerate() {
        end += tp.moves.len();
        sim.run_to(end, usize::MAX);
        if tp.moves.is_empty() {
            v.ops.push(None);
            continue;
        }
        let me = CLASS_PATH.saturating_add(i as u8);
        let col = class_rgb(me);
        let mut sc = Scene::default();
        sim_meshes(
            &sim,
            200,
            |c| match c {
                CLASS_GOUGE => class_rgb(CLASS_GOUGE),
                CLASS_STOCK => UNCUT_RGB,
                c if c == me => tint(col),
                _ => CUT_RGB,
            },
            &mut sc,
        );
        holding(cam, part, None, false, &mut sc);
        sc.wire_box(lo, hi, STOCK_LINE);
        let (feeds, rapids) = path_lines(tp);
        sc.lines.extend(rapids.into_iter().map(|l| (l, RAPID_RGB, true)));
        sc.lines.extend(feeds.into_iter().map(|l| (l, shade(col, 0.72), true)));
        v.ops.push(sc.shoot("iso", 460, 340, Some(fit)));
    }
    sim.run_to(usize::MAX, usize::MAX);
    let mut sc = Scene::default();
    sim_meshes(&sim, 300, class_rgb, &mut sc);
    holding(cam, part, None, false, &mut sc);
    for e in &sim.events {
        let p = DVec3::from(e.at);
        let r = (hi - lo).max_element() * 0.025;
        sc.lines.push((vec![p - DVec3::X * r, p + DVec3::X * r], class_rgb(CLASS_GOUGE), false));
        sc.lines.push((vec![p - DVec3::Y * r, p + DVec3::Y * r], class_rgb(CLASS_GOUGE), false));
    }
    v.sim = Some((sc.shoot("iso", 560, 420, None), sc.shoot("top", 400, 300, None)));
    v.events = sim.events.clone();
    v
}

/// A side view of the tool in its holder, to scale, as inline SVG `height` px tall.
fn tool_svg(t: &crate::cam::Tool, height: f64) -> String {
    let prof = t.profile();
    let r = t.radius();
    let len = prof.last().map(|p| p.1).unwrap_or(t.flute_length);
    let shank = if t.shank_diameter > 0.0 { t.shank_diameter / 2.0 } else { r };
    let stick = t.stickout.max(len + 1.0);
    let holder = t.holder_outline();
    // only the bottom of a tall holder
    let hh = holder.last().map(|p| p.1).unwrap_or(0.0).clamp(stick * 0.35, stick * 0.6);
    let total = stick + hh;
    let wide = prof.iter().map(|p| p.0).chain(holder.iter().filter(|p| p.1 < hh).map(|p| p.0)).fold(shank.max(r), f64::max);
    let s = height / total;
    let (w, cx) = (2.0 * wide * s + 8.0, wide * s + 4.0);
    let at = |x: f64, z: f64| format!("{:.1},{:.1}", cx + x * s, height + 2.0 - z * s);
    let shape = |right: &[(f64, f64)], fill: &str| -> String {
        let pts: Vec<String> = right.iter().map(|p| at(p.0, p.1)).chain(right.iter().rev().map(|p| at(-p.0, p.1))).collect();
        format!("<polygon points=\"{}\" fill=\"{fill}\" stroke=\"#3a424d\" stroke-width=\"0.8\" stroke-linejoin=\"round\"/>", pts.join(" "))
    };
    let mut hold: Vec<(f64, f64)> = holder.iter().map(|p| (p.0, stick + p.1)).filter(|p| p.1 < total).collect();
    if let Some(&(hr, _)) = hold.last() {
        hold.push((hr, total));
    }
    hold.insert(0, (0.0, stick));
    let mut cut = prof.clone();
    cut.insert(0, (0.0, 0.0));
    cut.push((0.0, len));
    format!(
        "<svg class=\"tool\" viewBox=\"0 0 {w:.1} {:.1}\" width=\"{w:.0}\" height=\"{:.0}\" role=\"img\" aria-label=\"{}\">{}{}{}</svg>",
        height + 4.0,
        height + 4.0,
        esc(&t.describe()),
        shape(&hold, "#a3abb6"),
        shape(&[(0.0, len), (shank, len), (shank, stick), (0.0, stick)], "#d3d8df"),
        shape(&cut, "#6f7d90"),
    )
}

fn b64(png: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(png)
}

/// A picture with its labels over it, and a caption.
fn figure(shot: &Option<Shot>, alt: &str, caption: &str) -> String {
    let Some(sh) = shot else { return String::new() };
    let mut s = format!("<figure><div class=\"shot\"><img alt=\"{}\" src=\"data:image/png;base64,{}\">", esc(alt), b64(&sh.png));
    for (at, text, class) in &sh.labels {
        if (0.0..=1.0).contains(&at[0]) && (0.0..=1.0).contains(&at[1]) {
            let _ = write!(s, "<span class=\"l {class}\" style=\"left:{:.2}%;top:{:.2}%\">{}</span>", at[0] * 100.0, at[1] * 100.0, esc(text));
        }
    }
    s.push_str("</div>");
    if !caption.is_empty() {
        let _ = write!(s, "<figcaption>{caption}</figcaption>");
    }
    s.push_str("</figure>");
    s
}

fn rgb(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// The setup sheet as a self-contained HTML page, with pictures of the setup, each operation and the
/// simulation (run here: give `new_sim`, or None for the setup pictures only).
pub fn sheet(cam: &Cam, part: &Part, paths: &[Toolpath], sim: Option<Sim>) -> String {
    html(cam, part, paths, &visuals(cam, part, paths, sim))
}

/// The setup sheet as a self-contained HTML page.
pub fn html(cam: &Cam, part: &Part, paths: &[Toolpath], vis: &Visuals) -> String {
    let o = cam_paths::work_origin(cam, part);
    let (lo, hi) = cam_paths::stock_box(cam, part);
    let total: f64 = paths.iter().map(|p| p.time).sum();
    let used: Vec<u64> = cam.tools.iter().map(|t| t.id).filter(|id| paths.iter().any(|p| p.tool == *id && !p.moves.is_empty())).collect();
    let mut s = String::new();
    let _ = write!(
        s,
        r#"<!doctype html><html><head><meta charset="utf-8"><title>{name} - setup sheet</title><style>
body{{font:13px/1.45 system-ui,sans-serif;color:#1d2330;margin:24px auto;max-width:1100px;padding:0 16px;background:#fff}}
h1{{font-size:22px;margin:0 0 4px}} h2{{font-size:15px;margin:22px 0 6px;border-bottom:2px solid #1d2330;padding-bottom:2px}}
.muted{{color:#667}} table{{border-collapse:collapse;width:100%}} th,td{{text-align:left;padding:4px 8px;border-bottom:1px solid #d6dae2;vertical-align:top}}
th{{background:#eef1f5;font-weight:600}} td.n{{text-align:right;font-variant-numeric:tabular-nums}}
.grid{{display:grid;grid-template-columns:1fr 1fr;gap:18px;align-items:start}} .kv td:first-child{{color:#556;width:42%}}
img{{max-width:100%;display:block;border-radius:6px}} .warn{{color:#b3401a}} .ok{{color:#1d7a3e}}
.kpis{{display:grid;grid-template-columns:repeat(auto-fit,minmax(130px,1fr));gap:8px;margin:14px 0 4px}}
.kpi{{border:1px solid #d6dae2;border-radius:8px;padding:8px 10px}} .kpi b{{display:block;font-size:17px;font-variant-numeric:tabular-nums}} .kpi span{{color:#667;font-size:11px;text-transform:uppercase;letter-spacing:.04em}}
.kpi.bad{{border-color:#e5a593;background:#fdf1ec}} .kpi.bad b{{color:#b3401a}}
figure{{margin:0}} figcaption{{color:#667;font-size:12px;margin-top:4px}}
figure{{text-align:center}} figcaption{{text-align:left}}
.shot{{position:relative;display:inline-block;max-width:100%;border:1px solid #d6dae2;border-radius:6px;vertical-align:top}} .shot img{{max-width:100%;max-height:360px;width:auto;border-radius:6px}}
.views .shot img{{max-height:300px}} .op .shot img{{max-height:280px}}
.l{{position:absolute;transform:translate(-50%,-50%);font-weight:700;font-size:12px;white-space:nowrap;pointer-events:none;text-shadow:0 0 3px #fff,0 0 3px #fff}}
.l.x{{color:#d83232}} .l.y{{color:#2aa14a}} .l.z{{color:#2f6fe0}} .l.wcs{{transform:translate(-110%,40%);font-size:11px;color:#1d2330}}
.l.dim{{background:#fff;border:1px solid #9aa3ae;border-radius:4px;padding:0 4px;font-weight:600;color:#1d2330;text-shadow:none}}
.views{{display:grid;grid-template-columns:1fr 1fr;gap:12px;margin-top:12px}}
.ops{{display:grid;grid-template-columns:1fr 1fr;gap:14px}} .op{{border:1px solid #d6dae2;border-radius:8px;padding:10px;break-inside:avoid}}
.op h3{{font-size:14px;margin:0 0 6px;display:flex;gap:8px;align-items:center}} .sw{{width:12px;height:12px;border-radius:3px;display:inline-block;flex:none}}
.op .body{{display:grid;grid-template-columns:1fr auto;gap:10px;margin-top:8px}} .op table td{{padding:2px 6px}}
svg.tool{{display:block;max-width:90px;height:auto}} td.draw{{width:60px}} td.draw svg{{max-height:110px;width:auto}}
.legend{{display:flex;flex-wrap:wrap;gap:6px 14px;margin:8px 0}} .legend span{{display:flex;gap:6px;align-items:center}}
@media (max-width:700px){{.grid,.views,.ops{{grid-template-columns:1fr}} table{{display:block;overflow-x:auto}}}}
@media print{{body{{margin:0;max-width:none}} h2{{break-after:avoid}} tr,figure,.kpis{{break-inside:avoid}} .ops{{grid-template-columns:1fr 1fr}}}}
</style></head><body><h1>{name}</h1><div class="muted">Setup sheet · program {program} · {post}</div>"#,
        name = esc(&cam.name),
        program = cam.program,
        post = esc(crate::cam::POSTS.iter().find(|p| p.0 == cam.post).map(|p| p.1).unwrap_or(&cam.post)),
    );
    let n_ops = paths.iter().filter(|p| !p.moves.is_empty()).count();
    let mut kpis = vec![
        ("Cycle time", time(total), false),
        ("Stock", format!("{} × {} × {}", fmt_mm(hi.x - lo.x), fmt_mm(hi.y - lo.y), fmt_mm(hi.z - lo.z)), false),
        ("Material", cam.stock.material.clone(), false),
        ("Tools", used.len().to_string(), false),
        ("Operations", n_ops.to_string(), false),
    ];
    if vis.sim.is_some() {
        let n = vis.events.len();
        kpis.push(("Simulation", if n == 0 { "no problems".into() } else { format!("{n} problem{}", if n == 1 { "" } else { "s" }) }, n > 0));
    }
    s.push_str("<div class=\"kpis\">");
    for (k, v, bad) in kpis {
        let _ = write!(s, "<div class=\"kpi{}\"><span>{k}</span><b>{}</b></div>", if bad { " bad" } else { "" }, esc(&v));
    }
    s.push_str("</div>");
    // setup summary and pictures
    let w = |v: DVec3| v - o;
    let mut kv: Vec<(&str, String)> = vec![
        ("Up direction", if cam.tilt == [0.0; 2] {
            format!("part {} faces the spindle", cam.orient)
        } else {
            format!("part {} faces the spindle, tilted {}° about X then {}° about Y", cam.orient, fmt_mm(cam.tilt[0]), fmt_mm(cam.tilt[1]))
        }),
        ("Work zero (G54)", cam_paths::work_zero_note(cam, part)),
        ("Stock", format!("{} × {} × {} mm, {}", fmt_mm(hi.x - lo.x), fmt_mm(hi.y - lo.y), fmt_mm(hi.z - lo.z), cam.stock.material)),
        ("Stock from / to (work)", format!("{} / {}", xyz(w(lo)), xyz(w(hi)))),
        ("Part from / to (work)", format!("{} / {}", xyz(w(part.lo)), xyz(w(part.hi)))),
        ("Clearance / retract", format!("{} / {} mm", fmt_mm(cam.clearance), fmt_mm(cam.retract))),
        ("Machine", if cam.machine.name.is_empty() { "custom".into() } else { cam.machine.name.clone() }),
        ("Tool changes", if cam.machine.tool_changer { "automatic".into() } else { "by hand (the program stops)".into() }),
        ("Spindle", format!("{} rpm, {} s to spin up", if cam.min_rpm > 0.0 { format!("{}–{}", fmt_mm(cam.min_rpm), fmt_mm(cam.max_rpm)) } else { format!("up to {}", fmt_mm(cam.max_rpm)) }, fmt_mm(cam.machine.spin_up))),
        ("Cycle time", time(total)),
        ("Tools / operations", format!("{} / {}", used.len(), paths.iter().filter(|p| !p.moves.is_empty()).count())),
    ];
    if let (Some(sj), Some(j)) = (&cam.soft_jaws, crate::cam_jaws::jaws(cam, part)) {
        kv.push((
            "Soft jaws",
            format!(
                "{} × {} × {} mm blanks, {} apart; part sits {} mm deep, jaw tops at Z{}",
                fmt_mm(sj.width),
                fmt_mm(sj.thickness),
                fmt_mm(sj.height),
                fmt_mm(sj.gap),
                fmt_mm(sj.grip),
                fmt_mm(j.top - o.z)
            ),
        ));
        if let Some(m) = &sj.mount {
            kv.push(("Jaw mounting", mount_text(m)));
        }
    }
    if !cam.jaws_of.is_empty() {
        kv.push(("Machines", format!("the soft jaws of {}: clamp both blanks in the vise on a spacer", cam.jaws_of)));
    }
    for (k, n) in cam.setup_notes().into_iter().enumerate() {
        kv.insert(k, (if k == 0 { "Before running" } else { "" }, n));
    }
    let taps: Vec<String> = used.iter().filter_map(|id| cam.tool(*id)).filter(|t| t.kind == ToolKind::Tap && !t.tapping_head).map(|t| format!("T{}", t.number)).collect();
    if !taps.is_empty() && cam.post != "linuxcnc" {
        let ctl = crate::cam::POSTS.iter().find(|p| p.0 == cam.post).map(|p| p.1).unwrap_or(&cam.post);
        kv.push((
            "Tapping",
            format!(
                "{ctl} has no rigid tapping: the spindle reverses at the bottom of each hole, so hold {} in a floating tap holder, or in a self-reversing tapping head (set it on the tool)",
                taps.join(", ")
            ),
        ));
    }
    match cam.probe.mode.as_str() {
        "plate" => kv.push((
            "Tool probing",
            format!(
                "touch plate {} mm thick at X{} Y{} (on Z{})",
                fmt_mm(cam.probe.plate_thickness),
                fmt_mm(cam.probe.plate_at[0]),
                fmt_mm(cam.probe.plate_at[1]),
                fmt_mm(cam.probe.plate_at[2])
            ),
        )),
        "setter" => kv.push(("Tool probing", "tool setter: zero the work Z with the first tool".into())),
        _ => {}
    }
    s.push_str("<h2>Setup</h2><div class=\"grid\"><div>");
    s.push_str(&figure(&vis.iso, "The part on the machine", "As on the machine, spindle above: the part, what holds it, the stock outline, and the X, Y and Z axes from the work zero."));
    s.push_str("</div><table class=\"kv\">");
    for (k, v) in &kv {
        let _ = write!(s, "<tr><td>{}</td><td>{}</td></tr>", k, esc(v));
    }
    s.push_str("</table></div><div class=\"views\">");
    s.push_str(&figure(&vis.top, "Top view", "Top: the stock's X and Y sizes."));
    s.push_str(&figure(&vis.front, "Front view", "Front, from the operator's side: X and Z sizes."));
    s.push_str("</div>");
    if !cam.fixtures.is_empty() {
        s.push_str("<h2>Fixtures</h2><table><tr><th>Name</th><th>From (work)</th><th>To (work)</th></tr>");
        for f in &cam.fixtures {
            let _ = write!(s, "<tr><td>{}</td><td>{}</td><td>{}</td></tr>", esc(&f.name), xyz(DVec3::from(f.min)), xyz(DVec3::from(f.max)));
        }
        s.push_str("</table>");
    }
    // tools
    s.push_str("<h2>Tools</h2><table><tr><th></th><th>T</th><th>Tool</th><th>Ø</th><th>Flutes</th><th>Flute length</th><th>Stick-out</th><th>Holder</th><th>Speed</th><th>Feed / plunge</th><th>Used by</th></tr>");
    for id in &used {
        let Some(t) = cam.tool(*id) else { continue };
        let ops: Vec<String> = paths.iter().filter(|p| p.tool == *id && !p.moves.is_empty()).filter_map(|p| cam.op(p.op)).map(|o| esc(&o.name)).collect();
        let desc = t.describe();
        let name = if t.name.is_empty() || t.name == desc { esc(&desc) } else { format!("{}<div class=\"muted\">{}</div>", esc(&t.name), esc(&desc)) };
        let holder = if t.holder_name.is_empty() { format!("Ø{} mm", fmt_mm(t.holder_diameter)) } else { esc(&t.holder_name) };
        let _ = write!(
            s,
            "<tr><td class=\"draw\">{}</td><td>T{}</td><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td>{}</td><td class=\"n\">{} rpm</td><td class=\"n\">{} / {}</td><td>{}</td></tr>",
            tool_svg(t, 110.0),
            t.number,
            name,
            fmt_mm(t.diameter),
            t.flutes,
            fmt_mm(t.flute_length),
            fmt_mm(t.stickout),
            if t.kind == ToolKind::Tap && t.tapping_head { format!("{holder}, self-reversing tapping head") } else { holder },
            cam.spindle_rpm(t).round(),
            t.feed.round(),
            t.plunge_feed.round(),
            ops.join(", ")
        );
    }
    s.push_str("</table>");
    // operations
    s.push_str("<h2>Operations</h2><table><tr><th>#</th><th>Operation</th><th>T</th><th>Z top / bottom (work)</th><th>Stepdown / stepover</th><th>Leave</th><th>Cut length</th><th>Time</th><th>Notes</th></tr>");
    let mut n = 0;
    let mut cards = String::new();
    for (pi, tp) in paths.iter().enumerate().filter(|(_, p)| !p.moves.is_empty()) {
        let (Some(op), Some(t)) = (cam.op(tp.op), cam.tool(tp.tool)) else { continue };
        n += 1;
        let mut prev = DVec3::new(0.0, 0.0, hi.z);
        let (mut zlo, mut zhi) = (f64::INFINITY, f64::NEG_INFINITY);
        for m in &tp.moves {
            let p = m.end(prev);
            if matches!(m, Move::Feed(..) | Move::Tap { .. }) {
                zlo = zlo.min(p.z.min(prev.z));
                // moves above the stock are lifts over fixtures and lead-ins, not cuts
                if p.z <= hi.z + 1e-6 {
                    zhi = zhi.max(p.z);
                }
            }
            if let Move::Tap { z, .. } = m {
                zlo = zlo.min(*z);
            }
            prev = p;
        }
        let z = if zlo.is_finite() && zhi.is_finite() { format!("{} / {}", fmt_mm(zhi - o.z), fmt_mm(zlo - o.z)) } else { "-".into() };
        let step = if matches!(op.kind, OpKind::Drill | OpKind::Tap | OpKind::Thread | OpKind::Chamfer | OpKind::Deburr) {
            "-".to_string()
        } else {
            format!("{} mm / {}% Ø", fmt_mm(cam_paths::stepdown(op, t)), (cam_paths::stepover(op, t) / t.diameter.max(1e-9) * 100.0).round())
        };
        let mut notes: Vec<String> = vec![];
        match cam.coolant_for(t, Some(op)) {
            crate::cam::Coolant::Flood => notes.push("flood coolant".into()),
            crate::cam::Coolant::Mist => notes.push("mist coolant".into()),
            _ => {}
        }
        if op.kind == OpKind::Adaptive && op.chip_thinning {
            notes.push(format!("chip thinning: feed ×{:.2}", crate::cam::chip_thinning(cam_paths::stepover(op, t) / t.diameter.max(1e-9))));
        }
        if op.rest {
            notes.push("rest machining".into());
        }
        notes.extend(tp.warnings.iter().map(|w| format!("<span class=\"warn\">{}</span>", esc(w))));
        let problems = vis.events.iter().filter(|e| e.op == op.id).count();
        if problems > 0 {
            notes.push(format!("<span class=\"warn\">{problems} problem(s) in the simulation</span>"));
        }
        let sw = format!("<span class=\"sw\" style=\"background:{}\"></span>", rgb(class_rgb(CLASS_PATH.saturating_add(pi as u8))));
        let leave = if op.leave > 0.0 || op.leave_floor > 0.0 { format!("{} / {} mm", fmt_mm(op.leave), fmt_mm(op.leave_floor)) } else { "-".into() };
        let _ = write!(cards, "<div class=\"op\"><h3>{sw}{n}. {} <span class=\"muted\">{} · T{}</span></h3>", esc(&op.name), op.kind.label(), t.number);
        cards.push_str(&figure(vis.ops.get(pi).unwrap_or(&None), &format!("{} toolpath", op.name), ""));
        let _ = write!(cards, "<div class=\"body\"><table class=\"kv\">");
        for (k, v) in [
            ("Tool", esc(&t.describe())),
            ("Speed / feed", format!("{} rpm / {} mm/min", cam.spindle_rpm(t).round(), t.feed.round())),
            ("Z top / bottom", z.clone()),
            ("Stepdown / over", step.clone()),
            ("Leave side / floor", leave.clone()),
            ("Cut length", format!("{} mm", tp.cut_length.round())),
            ("Time", time(tp.time)),
        ] {
            let _ = write!(cards, "<tr><td>{k}</td><td>{v}</td></tr>");
        }
        if !notes.is_empty() {
            let _ = write!(cards, "<tr><td>Notes</td><td>{}</td></tr>", notes.join("; "));
        }
        let _ = write!(cards, "</table>{}</div></div>", tool_svg(t, 120.0));
        let _ = write!(
            s,
            "<tr><td>{sw}{n}</td><td>{}<div class=\"muted\">{}</div></td><td>T{}</td><td class=\"n\">{z}</td><td class=\"n\">{step}</td><td class=\"n\">{}</td><td class=\"n\">{} mm</td><td class=\"n\">{}</td><td>{}</td></tr>",
            esc(&op.name),
            op.kind.label(),
            t.number,
            if op.leave > 0.0 || op.leave_floor > 0.0 { format!("{} / {}", fmt_mm(op.leave), fmt_mm(op.leave_floor)) } else { "-".into() },
            tp.cut_length.round(),
            time(tp.time),
            notes.join("; ")
        );
    }
    let _ = write!(s, "<tr><th></th><th>Total</th><th></th><th></th><th></th><th></th><th></th><th class=\"n\">{}</th><th></th></tr></table>", time(total));
    if !cards.is_empty() {
        s.push_str("<h2>Operation by operation</h2><p class=\"muted\">The stock after each operation, as simulated: its cut in its colour, the rest grey, raw stock beige. Cutting moves in the operation's colour, rapids in red.</p>");
        let _ = write!(s, "<div class=\"ops\">{cards}</div>");
    }
    if let Some((iso, top)) = &vis.sim {
        s.push_str("<h2>Simulation</h2><div class=\"views\">");
        s.push_str(&figure(iso, "The simulated result", ""));
        s.push_str(&figure(top, "The simulated result from the top", ""));
        s.push_str("</div><div class=\"legend\">");
        let chip = |c: [u8; 3], name: &str| format!("<span><i class=\"sw\" style=\"background:{}\"></i>{}</span>", rgb(c), esc(name));
        s.push_str(&chip(class_rgb(CLASS_STOCK), "raw stock"));
        for (pi, tp) in paths.iter().enumerate().filter(|(_, p)| !p.moves.is_empty()) {
            if let Some(op) = cam.op(tp.op) {
                s.push_str(&chip(class_rgb(CLASS_PATH.saturating_add(pi as u8)), &op.name));
            }
        }
        if !cam.follows.is_empty() {
            s.push_str(&chip(class_rgb(crate::cam_sim::CLASS_EARLIER), &format!("machined in {}", cam.follows)));
        }
        s.push_str(&chip(class_rgb(CLASS_GOUGE), "cut into the part"));
        s.push_str("</div>");
        if vis.events.is_empty() {
            s.push_str("<p class=\"ok\">The simulation found no crashes, gouges or rapids into the stock.</p>");
        } else {
            s.push_str("<table><tr><th>Problem</th><th>Operation</th><th>At (work)</th><th>Depth</th><th>What</th></tr>");
            for e in vis.events.iter().take(40) {
                let _ = write!(
                    s,
                    "<tr><td class=\"warn\">{}</td><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{} mm</td><td>{}</td></tr>",
                    e.kind.label(),
                    esc(cam.op(e.op).map(|o| o.name.as_str()).unwrap_or("?")),
                    xyz(DVec3::from(e.at) - o),
                    fmt_mm(e.depth),
                    esc(&e.what)
                );
            }
            s.push_str("</table>");
            if vis.events.len() > 40 {
                let _ = write!(s, "<p class=\"muted\">and {} more.</p>", vis.events.len() - 40);
            }
        }
    }
    s.push_str("<p class=\"muted\">Heights are in work coordinates, relative to the work zero. Made by rCAD.</p></body></html>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_lists_tools_and_operations() {
        let mut tris = vec![];
        let q = |x: f64, y: f64, z: f64| DVec3::new(x, y, z);
        for z in [0.0, 10.0] {
            tris.push([q(0.0, 0.0, z), q(40.0, 0.0, z), q(40.0, 30.0, z)]);
            tris.push([q(0.0, 0.0, z), q(40.0, 30.0, z), q(0.0, 30.0, z)]);
        }
        let part = Part::new(&tris, &[], "top");
        let mut c = Cam { name: "Plate <A>".into(), ..Default::default() };
        let t = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && t.diameter == 6.0).unwrap();
        let t = c.add_tool(t);
        c.add_op(OpKind::Face, t);
        c.add_op(OpKind::Contour, t);
        c.fixtures.push(Default::default());
        let paths = cam_paths::generate(&c, &part);
        let vis = visuals(&c, &part, &paths, Some(new_sim(&c, &part, &paths, None)));
        assert!(vis.iso.as_ref().unwrap().png.starts_with(b"\x89PNG"));
        assert!(vis.ops.iter().filter(|o| o.is_some()).count() == 2 && vis.sim.is_some());
        let h = html(&c, &part, &paths, &vis);
        assert!(h.contains("Plate &lt;A&gt;") && h.contains("data:image/png;base64,"));
        assert!(h.contains("<svg class=\"tool\"") && h.contains("Operation by operation") && h.contains("class=\"l ax x\""));
        assert!(h.contains("Face 1") && h.contains("Contour 1") && h.contains(">T1<") && h.contains("Clamp"));
        assert!(h.contains("Ø6 flat end mill"));
    }
}

/// The screw holes the jaw blanks need, for the sheet.
pub fn mount_text(m: &crate::cam::JawMount) -> String {
    let xs: Vec<String> = m.holes.iter().map(|x| fmt_mm(*x)).collect();
    format!(
        "{}: {} × Ø{} through, Ø{} counterbore leaving {} mm under the head, {} mm from the grip face at X {} from the jaw middle",
        m.name,
        m.holes.len(),
        fmt_mm(m.drill),
        fmt_mm(m.cbore),
        fmt_mm(m.seat),
        fmt_mm(m.inset),
        xs.join(", ")
    )
}
