//! The CAM simulation as a web page: one self-contained .html (three.js from a CDN) that plays the toolpaths
//! back on the stock, with the tool, the fixtures, the finished part and the machine moving with it.
//!
//! The simulation runs here, once: the page gets the stock as blocks of cells at the start and the blocks
//! that changed at each of a few hundred frames spread over the run time, so what it shows is exactly what
//! the app's simulation found (problems included). The tool moves smoothly along the moves between frames.

use std::collections::HashMap;

use base64::Engine;
use glam::{DVec3, Vec3};
use serde_json::{json, Value};

use crate::cam::Cam;
use crate::cam_machine::CARRIERS;
use crate::cam_paths::{self, Move, Part, Toolpath};
use crate::cam_sim::{class_rgb, Sim, CLASS_EARLIER, CLASS_GOUGE, CLASS_PATH, CLASS_STOCK};

const TEMPLATE: &str = include_str!("cam_web.html");

/// Blocks along the stock's longer side in the page.
const BLOCKS: usize = 240;
/// Most frames the stock is stored at.
const FRAMES: usize = 900;

#[derive(Default)]
struct Blob {
    bytes: Vec<u8>,
}

impl Blob {
    fn align(&mut self) {
        while self.bytes.len() % 4 != 0 {
            self.bytes.push(0);
        }
    }

    fn f32s(&mut self, v: &[f32]) -> Value {
        self.align();
        let o = self.bytes.len();
        for x in v {
            self.bytes.extend_from_slice(&x.to_le_bytes());
        }
        json!([o, v.len()])
    }

    fn u32s(&mut self, v: &[u32]) -> Value {
        self.align();
        let o = self.bytes.len();
        for x in v {
            self.bytes.extend_from_slice(&x.to_le_bytes());
        }
        json!([o, v.len()])
    }

    fn u16s(&mut self, v: &[u16]) -> Value {
        self.align();
        let o = self.bytes.len();
        for x in v {
            self.bytes.extend_from_slice(&x.to_le_bytes());
        }
        json!([o, v.len()])
    }

    fn u8s(&mut self, v: &[u8]) -> Value {
        self.align();
        let o = self.bytes.len();
        self.bytes.extend_from_slice(v);
        json!([o, v.len()])
    }

    /// Triangles with shared corners merged, as 16-bit positions within their box and 32-bit indices.
    fn mesh(&mut self, tris: impl Iterator<Item = [Vec3; 3]>) -> Option<Value> {
        let tris: Vec<[Vec3; 3]> = tris.collect();
        if tris.is_empty() {
            return None;
        }
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for t in &tris {
            for p in t {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        let span = (hi - lo).max(Vec3::splat(1e-6));
        let mut ids: HashMap<[u16; 3], u32> = HashMap::new();
        let mut pos: Vec<u16> = vec![];
        let mut idx: Vec<u32> = Vec::with_capacity(tris.len() * 3);
        for t in &tris {
            let q: [[u16; 3]; 3] = t.map(|p| {
                let n = (p - lo) / span * 65535.0;
                [n.x.round().clamp(0.0, 65535.0) as u16, n.y.round().clamp(0.0, 65535.0) as u16, n.z.round().clamp(0.0, 65535.0) as u16]
            });
            if q[0] == q[1] || q[1] == q[2] || q[0] == q[2] {
                continue;
            }
            for k in q {
                let next = (pos.len() / 3) as u32;
                let id = *ids.entry(k).or_insert_with(|| {
                    pos.extend_from_slice(&k);
                    next
                });
                idx.push(id);
            }
        }
        Some(json!({"lo": lo.to_array(), "hi": hi.to_array(), "pos": self.u16s(&pos), "idx": self.u32s(&idx)}))
    }
}

fn rgb(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// One straight piece of the run: where it ends, when (s from the start), what it is.
struct Seg {
    to: DVec3,
    t: f64,
    /// 0 rapid, 1 feed, 2 tap, 3 dwell
    kind: u8,
    path: usize,
    mv: usize,
    feed: f64,
}

/// A fresh simulation of `paths` on setup `c` as the app runs it: from the stock an earlier setup left
/// (`stock_in`), with the setup's machine when it has one.
pub fn new_sim(c: &Cam, part: &Part, paths: &[Toolpath], stock_in: Option<&crate::cam_stock::StockMap>, resolution: usize) -> Result<Sim, String> {
    let stock = cam_paths::stock_box(c, part);
    let mut sim = Sim::new(c, part, stock, paths, resolution);
    if let Some(m) = stock_in {
        sim.start_from(m);
    }
    if !c.machine.model.is_empty() {
        let l = crate::cam_machine::load(&c.machine.model)?;
        let placed = std::sync::Arc::new(Sim::place_machine(c, part, stock, l));
        sim.set_machine(c, part, placed);
    }
    Ok(sim)
}

/// The page for a fresh (not yet run) simulation `sim` of `paths` on setup `c`: the html.
pub fn page(c: &Cam, part: &Part, paths: &[Toolpath], mut sim: Sim) -> Result<String, String> {
    if sim.total() == 0 {
        return Err("there are no toolpaths to simulate".into());
    }
    let rapid_rate = c.rapid_rate.max(1.0);
    let rapid_z = if c.machine.rapid_z > 0.0 { c.machine.rapid_z } else { rapid_rate };
    let start = sim.tool_pos();

    // the run as straight pieces in time; each move ends at `ends[k]` s
    let mut segs: Vec<Seg> = vec![];
    let mut ends: Vec<f64> = Vec::with_capacity(sim.total());
    let (mut p, mut t) = (start, 0.0f64);
    for k in 0..sim.total() {
        let (_, m) = sim.move_at(k).unwrap();
        let ci = sim.cutter_at(k).unwrap_or(0);
        let path = ci.min(paths.len().saturating_sub(1));
        match m {
            Move::Rapid(q) => {
                let q = DVec3::from(q);
                let d = q - p;
                // the very first move comes from a made-up start: show it, but quickly
                t += if k == 0 { 0.5 } else { (d.truncate().length() / rapid_rate).max(d.z.abs() / rapid_z) * 60.0 };
                segs.push(Seg { to: q, t, kind: 0, path, mv: k, feed: 0.0 });
                p = q;
            }
            Move::Feed(q, f) => {
                let q = DVec3::from(q);
                t += p.distance(q) / f.max(1.0) * 60.0;
                segs.push(Seg { to: q, t, kind: 1, path, mv: k, feed: f });
                p = q;
            }
            Move::Tap { z, r, pitch } => {
                let rpm = paths.get(path).and_then(|tp| c.tool(tp.tool)).map(|tl| c.spindle_rpm(tl)).unwrap_or(500.0).max(1.0);
                let f = (pitch.abs() * rpm).max(1.0);
                let down = DVec3::new(p.x, p.y, z);
                t += (p.z - z).abs() / f * 60.0;
                segs.push(Seg { to: down, t, kind: 2, path, mv: k, feed: f });
                let up = DVec3::new(p.x, p.y, r);
                t += (r - z).abs() / f * 60.0;
                segs.push(Seg { to: up, t, kind: 2, path, mv: k, feed: f });
                p = up;
            }
            Move::Dwell(s) => {
                t += s.max(0.0);
                segs.push(Seg { to: p, t, kind: 3, path, mv: k, feed: 0.0 });
            }
        }
        ends.push(t);
    }
    let total_t = t.max(1e-3);

    // the stock at the start, then the blocks that changed at each frame
    let b0 = sim.blocks(BLOCKS);
    let (mx, my, k) = (b0.mx, b0.my, b0.k);
    let zlo = b0.l.iter().copied().fold(f64::INFINITY, f64::min).min(sim.bottom as f64);
    let zhi = b0.h.iter().copied().fold(f64::NEG_INFINITY, f64::max).max(sim.top as f64);
    let zspan = (zhi - zlo).max(1e-6);
    let qz = |z: f64| (((z - zlo) / zspan) * 65535.0).round().clamp(0.0, 65535.0) as u16;
    let mut cur_h: Vec<u16> = b0.h.iter().map(|z| qz(*z)).collect();
    let cur_l: Vec<u16> = b0.l.iter().map(|z| qz(*z)).collect();
    let mut cur_c: Vec<u8> = b0.cls.clone();
    let h0 = cur_h.clone();
    let c0 = cur_c.clone();
    let nf = FRAMES.min(sim.total()).max(1);
    let (mut f_time, mut f_move, mut f_count, mut f_removed) = (vec![], vec![], vec![], vec![]);
    let (mut d_idx, mut d_h, mut d_c): (Vec<u32>, Vec<u16>, Vec<u8>) = (vec![], vec![], vec![]);
    let mut done = 0usize;
    for f in 1..=nf {
        let tf = if f == nf { f64::INFINITY } else { total_t * f as f64 / nf as f64 };
        let mut target = done;
        while target < ends.len() && ends[target] <= tf + 1e-9 {
            target += 1;
        }
        if target == done && f != nf {
            continue;
        }
        sim.run_to(target, usize::MAX);
        done = target;
        let b = sim.blocks(BLOCKS);
        let before = d_idx.len();
        for i in 0..mx * my {
            let h = qz(b.h[i]);
            if h != cur_h[i] || b.cls[i] != cur_c[i] {
                cur_h[i] = h;
                cur_c[i] = b.cls[i];
                d_idx.push(i as u32);
                d_h.push(h);
                d_c.push(b.cls[i]);
            }
        }
        f_time.push(if f == nf { total_t } else { tf } as f32);
        f_move.push(done as u32);
        f_count.push((d_idx.len() - before) as u32);
        f_removed.push(sim.removed as f32);
    }

    let mut blob = Blob::default();
    let o = cam_paths::work_origin(c, part);
    // block edges: the last one stops at the stock's edge
    let cb = sim.cell * k as f64;
    let xs: Vec<f32> = (0..=mx).map(|i| (sim.x0 + i as f64 * cb - sim.cell / 2.0).min(sim.x0 + (sim.nx as f64 - 0.5) * sim.cell) as f32).collect();
    let ys: Vec<f32> = (0..=my).map(|j| (sim.y0 + j as f64 * cb - sim.cell / 2.0).min(sim.y0 + (sim.ny as f64 - 0.5) * sim.cell) as f32).collect();
    let stock = json!({
        "mx": mx, "my": my, "zlo": zlo, "zhi": zhi,
        "xs": blob.f32s(&xs), "ys": blob.f32s(&ys),
        "h": blob.u16s(&h0), "l": blob.u16s(&cur_l), "c": blob.u8s(&c0),
        "frames": {"t": blob.f32s(&f_time), "move": blob.u32s(&f_move), "n": blob.u32s(&f_count), "removed": blob.f32s(&f_removed)},
        "diff": {"i": blob.u32s(&d_idx), "h": blob.u16s(&d_h), "c": blob.u8s(&d_c)},
    });

    let mut sp: Vec<f32> = vec![start.x as f32, start.y as f32, start.z as f32];
    let mut st: Vec<f32> = vec![0.0];
    let (mut sk, mut spath, mut smove, mut sfeed) = (vec![], vec![], vec![], vec![]);
    for s in &segs {
        sp.extend_from_slice(&[s.to.x as f32, s.to.y as f32, s.to.z as f32]);
        st.push(s.t as f32);
        sk.push(s.kind);
        spath.push(s.path as u16);
        smove.push(s.mv as u32);
        sfeed.push(s.feed as f32);
    }
    let run = json!({"p": blob.f32s(&sp), "t": blob.f32s(&st), "kind": blob.u8s(&sk), "path": blob.u16s(&spath), "move": blob.u32s(&smove), "feed": blob.f32s(&sfeed), "total": total_t, "moves": sim.total()});

    // each toolpath: its operation, tool and colour, where it starts
    let mut first_seg = vec![usize::MAX; paths.len()];
    for (i, s) in segs.iter().enumerate() {
        if s.path < first_seg.len() && first_seg[s.path] == usize::MAX {
            first_seg[s.path] = i;
        }
    }
    let ops: Vec<Value> = paths
        .iter()
        .enumerate()
        .map(|(i, tp)| {
            let op = c.op(tp.op);
            let tool = c.tool(tp.tool);
            let (body, stickout) = sim.cutter_body(i).map(|(b, s)| (b.to_vec(), s)).unwrap_or_default();
            let t0 = if first_seg[i] == usize::MAX {
                total_t
            } else if first_seg[i] == 0 {
                0.0
            } else {
                segs[first_seg[i] - 1].t
            };
            json!({
                "name": op.map(|o| o.name.clone()).unwrap_or_else(|| format!("Toolpath {}", i + 1)),
                "kind": op.map(|o| o.kind.label()).unwrap_or(""),
                "tool": tool.map(|t| format!("T{} {}", t.number, t.name)).unwrap_or_default(),
                "rpm": tool.map(|t| c.spindle_rpm(t).round()).unwrap_or(0.0),
                "color": rgb(class_rgb(CLASS_PATH.saturating_add(i.min(250) as u8))),
                "body": body, "stickout": stickout, "gauge": sim.gauge(i),
                "start": t0, "time": tp.time,
            })
        })
        .collect();

    let names = |id: u64| c.op(id).map(|o| o.name.clone()).unwrap_or_default();
    let events: Vec<Value> = sim
        .events
        .iter()
        .map(|e| {
            let t = if e.step == 0 { 0.0 } else { ends.get(e.step - 1).copied().unwrap_or(total_t) };
            let t1 = ends.get(e.step).copied().unwrap_or(total_t);
            json!({"kind": format!("{:?}", e.kind).to_lowercase(), "what": if e.what.is_empty() { e.kind.label().to_string() } else { e.what.clone() }, "op": names(e.op), "at": e.at, "depth": e.depth, "t": t, "t1": t1})
        })
        .collect();

    let fixtures: Vec<Value> = cam_paths::fixture_boxes(c, part, o).into_iter().map(|(n, lo, hi)| json!({"name": n, "lo": lo.to_array(), "hi": hi.to_array()})).collect();
    let part_mesh = blob.mesh(part.tris.iter().map(|t| t.map(|p| p.as_vec3())));

    let machine = match &sim.machine {
        None => Value::Null,
        Some(m) => {
            let pl = &m.placed;
            let model = &pl.machine.model;
            let shift = pl.shift.as_vec3();
            // parts by carrier and colour
            let mut groups: Vec<((usize, [u8; 3]), Vec<usize>)> = vec![];
            for (i, mp) in model.parts.iter().enumerate() {
                let key = (CARRIERS.iter().position(|c| *c == mp.carrier).unwrap_or(0), mp.color);
                match groups.iter_mut().find(|g| g.0 == key) {
                    Some(g) => g.1.push(i),
                    None => groups.push((key, vec![i])),
                }
            }
            let meshes: Vec<Value> = groups
                .iter()
                .filter_map(|((ci, col), parts)| {
                    let tris = parts.iter().flat_map(|&i| pl.machine.tris[i].iter().map(|t| t.map(|p| p + shift)));
                    blob.mesh(tris).map(|v| json!({"carrier": ci, "color": rgb(*col), "mesh": v}))
                })
                .collect();
            // each carrier's displacement (relative to the work) per unit of each axis
            let unit = [DVec3::X, DVec3::Y, DVec3::Z].map(|e| model.relative(e));
            let coef: Vec<[f64; 3]> = (0..4).map(|c| [unit[0][c].x, unit[1][c].y, unit[2][c].z]).collect();
            json!({
                "name": model.name, "meshes": meshes, "coef": coef,
                "home": pl.tip_home(0.0).to_array(), "travel": model.travel,
            })
        }
    };

    let classes = json!({
        "stock": rgb(class_rgb(CLASS_STOCK)), "gouge": rgb(class_rgb(CLASS_GOUGE)), "earlier": rgb(class_rgb(CLASS_EARLIER)), "path0": CLASS_PATH,
        "paths": crate::cam_sim::PATH_COLOURS.iter().map(|c| rgb(*c)).collect::<Vec<_>>(),
    });
    let title = if part.tris.is_empty() { c.name.clone() } else { format!("{} · simulation", c.name) };
    let spec = json!({
        "format": 1, "title": title, "setup": c.name, "origin": o.to_array(), "cell": sim.cell * k as f64,
        "stock": stock, "run": run, "ops": ops, "events": events, "fixtures": fixtures, "part": part_mesh, "machine": machine, "classes": classes,
    });
    let spec = serde_json::to_string(&spec).map_err(|e| e.to_string())?.replace('<', "\\u003c");
    let b64 = base64::engine::general_purpose::STANDARD.encode(&blob.bytes);
    let data = format!("window.SIM = {spec};\nwindow.BLOB64 = \"{b64}\";");
    Ok(TEMPLATE.replacen("__TITLE__", &html_escape(&title), 1).replacen("/*__DATA__*/", &data, 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::ToolKind;

    /// A slot milled across a block: the page carries the run and the stock going down over the frames, and
    /// the last frame is the stock the simulation ends with.
    #[test]
    fn page_plays_the_whole_run() {
        let part = Part::new(&[], &[], "top");
        let mut cam = Cam::default();
        let mill = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat).unwrap();
        let t = cam.add_tool(mill);
        let stock = (DVec3::new(0.0, 0.0, 0.0), DVec3::new(60.0, 40.0, 10.0));
        let moves = vec![Move::Rapid([-10.0, 20.0, 15.0]), Move::Rapid([-10.0, 20.0, 8.0]), Move::Feed([70.0, 20.0, 8.0], 600.0), Move::Rapid([70.0, 20.0, 15.0])];
        let tp = Toolpath { op: 1, tool: t, moves, ..Default::default() };
        let paths = vec![tp];
        let sim = Sim::new(&cam, &part, stock, &paths, 200);
        let html = page(&cam, &part, &paths, sim).unwrap();
        let spec = html.split("window.SIM = ").nth(1).unwrap().split(";\nwindow.BLOB64").next().unwrap();
        let v: Value = serde_json::from_str(spec).unwrap();
        assert_eq!(v["run"]["moves"], 4);
        // 80 mm at 600 mm/min is 8 s of the run
        assert!(v["run"]["total"].as_f64().unwrap() > 8.0);
        let frames = v["stock"]["frames"]["n"][1].as_u64().unwrap();
        assert!(frames >= 2 && frames <= 4, "{frames}");
        let changed = v["stock"]["diff"]["i"][1].as_u64().unwrap();
        assert!(changed > 100, "{changed}");
        assert!(v["machine"].is_null());
        assert!(!html.contains("/*__DATA__*/"));
    }
}
