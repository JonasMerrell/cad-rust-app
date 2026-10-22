//! Toolpath simulation on a height map of the stock (seen from the spindle),
//! with crash checks: rapids into material, the shank or holder touching the
//! stock, and cuts into the part.

use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::cam::{Cam, OpKind, Tool, ToolKind};
use crate::cam_paths::{Move, Part, Toolpath};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A rapid move cut material.
    Rapid,
    /// The shank (above the flutes) touched material.
    Shank,
    /// The holder touched material.
    Holder,
    /// The tool cut into the finished part.
    Gouge,
}

impl EventKind {
    pub fn label(self) -> &'static str {
        match self {
            EventKind::Rapid => "rapid move into stock",
            EventKind::Shank => "shank hits the stock",
            EventKind::Holder => "holder hits the stock",
            EventKind::Gouge => "cuts into the part",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SimEvent {
    pub kind: EventKind,
    pub op: u64,
    /// Index into all moves of all toolpaths, in order.
    pub step: usize,
    /// Where (setup coordinates).
    pub at: [f64; 3],
    /// How deep, mm.
    pub depth: f64,
}

struct Cutter {
    radius: f64,
    /// Height of the cutting surface above the tip by distance from the axis, every LUT_STEP mm.
    lut: Vec<f32>,
    /// Shank radius and bottom height above the tip.
    shank_r: f64,
    shank_z: f32,
    holder: Vec<(i32, i32)>,
    holder_z: f32,
    /// Cuts under an overhang (slot mills, thread mills): not modelled in the height map.
    undercut: bool,
    /// How deep it may cut into the part without it counting (chamfers, taps, threads).
    allowance: f32,
}

const LUT_STEP: f64 = 0.002;

fn disk(r: f64, cell: f64) -> Vec<(i32, i32)> {
    let n = (r / cell).ceil() as i32;
    let mut v = vec![];
    for j in -n..=n {
        for i in -n..=n {
            let (x, y) = (i as f64 * cell, j as f64 * cell);
            if x * x + y * y <= r * r {
                v.push((i, j));
            }
        }
    }
    v
}

impl Cutter {
    fn new(t: &Tool, cell: f64, allowance: f32) -> Cutter {
        let prof = t.profile();
        let r = t.radius();
        let undercut = matches!(t.kind, ToolKind::Slot | ToolKind::ThreadMill);
        let height_at = |rho: f64| -> f64 {
            for w in prof.windows(2) {
                let (r0, z0) = w[0];
                let (r1, z1) = w[1];
                if rho >= r0 - 1e-9 && rho <= r1 + 1e-9 {
                    if (r1 - r0).abs() < 1e-9 {
                        return z0.min(z1);
                    }
                    return z0 + (z1 - z0) * (rho - r0) / (r1 - r0);
                }
            }
            prof.last().map(|p| p.1).unwrap_or(0.0)
        };
        let exact = |rho: f64| crate::cam_3d::tool_height(t, rho).unwrap_or_else(|| height_at(rho));
        let lut = if undercut { vec![] } else { (0..=((r / LUT_STEP).ceil() as usize)).map(|i| exact((i as f64 * LUT_STEP).min(r)) as f32).collect() };
        let flutes = if undercut { t.cutting_width.max(0.5) } else { t.flute_length.max(r) };
        let neck = if undercut { if t.neck_diameter > 0.0 { t.neck_diameter / 2.0 } else { r * 0.6 } } else { (t.shank_diameter / 2.0).max(0.1) };
        let stick = t.stickout.max(flutes + 1.0);
        Cutter {
            radius: r,
            lut,
            shank_r: neck,
            shank_z: flutes as f32,
            holder: disk((t.holder_diameter / 2.0).max(neck), cell * 2.0).into_iter().map(|(i, j)| (i * 2, j * 2)).collect(),
            holder_z: stick as f32,
            undercut,
            allowance,
        }
    }
}

pub struct Sim {
    pub x0: f64,
    pub y0: f64,
    pub cell: f64,
    pub nx: usize,
    pub ny: usize,
    pub stock: Vec<f32>,
    pub part: Vec<f32>,
    pub gouged: Vec<bool>,
    pub top: f32,
    pub bottom: f32,
    moves: Vec<(usize, u64, Move)>,
    cutters: Vec<Cutter>,
    /// Next move to run.
    pub done: usize,
    pos: DVec3,
    pub events: Vec<SimEvent>,
    pub removed: f64,
    max_h: f32,
}

impl Sim {
    /// A fresh simulation of `paths` on the stock box (setup coordinates). `resolution` cells along the longer side.
    pub fn new(cam: &Cam, part: &Part, stock: (DVec3, DVec3), paths: &[Toolpath], resolution: usize) -> Sim {
        let (lo, hi) = stock;
        let size = (hi - lo).truncate();
        let cell = (size.x.max(size.y) / resolution.max(50) as f64).max(0.02);
        let pad = 0.0;
        let nx = ((size.x + 2.0 * pad) / cell).ceil() as usize + 1;
        let ny = ((size.y + 2.0 * pad) / cell).ceil() as usize + 1;
        let x0 = lo.x - pad;
        let y0 = lo.y - pad;
        let mut s = Sim {
            x0,
            y0,
            cell,
            nx,
            ny,
            stock: vec![hi.z as f32; nx * ny],
            part: vec![f32::NEG_INFINITY; nx * ny],
            gouged: vec![false; nx * ny],
            top: hi.z as f32,
            bottom: lo.z as f32,
            moves: vec![],
            cutters: vec![],
            done: 0,
            pos: DVec3::new(0.0, 0.0, hi.z + cam.clearance),
            events: vec![],
            removed: 0.0,
            max_h: hi.z as f32,
        };
        // part height map: highest upward surface over each cell centre
        for t in &part.tris {
            let n = (t[1] - t[0]).cross(t[2] - t[0]);
            if n.z <= 1e-12 {
                continue;
            }
            let (mnx, mxx) = (t[0].x.min(t[1].x).min(t[2].x), t[0].x.max(t[1].x).max(t[2].x));
            let (mny, mxy) = (t[0].y.min(t[1].y).min(t[2].y), t[0].y.max(t[1].y).max(t[2].y));
            let i0 = (((mnx - x0) / cell).floor().max(0.0)) as usize;
            let i1 = (((mxx - x0) / cell).ceil().max(0.0) as usize).min(nx - 1);
            let j0 = (((mny - y0) / cell).floor().max(0.0)) as usize;
            let j1 = (((mxy - y0) / cell).ceil().max(0.0) as usize).min(ny - 1);
            let d = (t[1].x - t[0].x) * (t[2].y - t[0].y) - (t[2].x - t[0].x) * (t[1].y - t[0].y);
            if d.abs() < 1e-14 {
                continue;
            }
            for j in j0..=j1 {
                for i in i0..=i1 {
                    let (px, py) = (x0 + i as f64 * cell, y0 + j as f64 * cell);
                    let a = ((t[1].x - px) * (t[2].y - py) - (t[2].x - px) * (t[1].y - py)) / d;
                    let b = ((t[2].x - px) * (t[0].y - py) - (t[0].x - px) * (t[2].y - py)) / d;
                    let c = 1.0 - a - b;
                    if a >= -1e-9 && b >= -1e-9 && c >= -1e-9 {
                        let z = (a * t[0].z + b * t[1].z + c * t[2].z) as f32;
                        let k = j * nx + i;
                        if z > s.part[k] {
                            s.part[k] = z;
                        }
                    }
                }
            }
        }
        for tp in paths {
            let Some(tool) = cam.tool(tp.tool) else { continue };
            let op = cam.op(tp.op);
            let allowance = match op.map(|o| o.kind) {
                Some(OpKind::Chamfer) | Some(OpKind::Deburr) => op.map(|o| o.chamfer * 2.0 + 0.3).unwrap_or(1.0) as f32,
                Some(OpKind::Tap) | Some(OpKind::Thread) => f32::INFINITY,
                _ => 0.02,
            };
            s.cutters.push(Cutter::new(tool, cell, allowance));
            let ci = s.cutters.len() - 1;
            for m in &tp.moves {
                s.moves.push((ci, tp.op, *m));
            }
        }
        s
    }

    pub fn total(&self) -> usize {
        self.moves.len()
    }

    pub fn tool_pos(&self) -> DVec3 {
        self.pos
    }

    /// Index of the toolpath cutter in use at the next move.
    pub fn current_cutter(&self) -> Option<usize> {
        self.moves.get(self.done.min(self.moves.len().saturating_sub(1))).map(|m| m.0)
    }

    /// Run moves until `target` (exclusive) or until about `budget` tool positions have been stamped.
    /// Returns true when it reached the target.
    pub fn run_to(&mut self, target: usize, budget: usize) -> bool {
        let target = target.min(self.moves.len());
        let mut spent = 0usize;
        while self.done < target {
            let (ci, op, m) = self.moves[self.done];
            let step = self.done;
            match m {
                Move::Rapid(p) => spent += self.travel(ci, op, step, DVec3::from(p), true),
                Move::Feed(p, _) => spent += self.travel(ci, op, step, DVec3::from(p), false),
                Move::Tap { z, r, .. } => {
                    let a = self.pos;
                    spent += self.travel(ci, op, step, DVec3::new(a.x, a.y, z), false);
                    spent += self.travel(ci, op, step, DVec3::new(a.x, a.y, r), false);
                }
                Move::Dwell(_) => {}
            }
            self.done += 1;
            if spent >= budget {
                break;
            }
        }
        self.done >= target
    }

    fn travel(&mut self, ci: usize, op: u64, step: usize, to: DVec3, rapid: bool) -> usize {
        let from = self.pos;
        let len = from.distance(to);
        let n = ((len / self.cell).ceil() as usize).max(1);
        let check_every = ((1.0 / self.cell).ceil() as usize).max(1);
        let mut work = 0;
        for k in 1..=n {
            let p = from.lerp(to, k as f64 / n as f64);
            // nothing to touch above the stock
            if p.z as f32 > self.max_h + 1e-3 {
                continue;
            }
            work += 1;
            self.stamp(ci, op, step, p, rapid, k % check_every == 0 || k == n);
        }
        self.pos = to;
        work
    }

    /// sqrt(1 + slope²) of the part's height map at a cell (gentler of the two sides along each axis).
    fn slope_factor(&self, i: i32, j: i32) -> f32 {
        let (nx, ny) = (self.nx as i32, self.ny as i32);
        let h = |i: i32, j: i32| if i < 0 || j < 0 || i >= nx || j >= ny { f32::NEG_INFINITY } else { self.part[(j * nx + i) as usize] };
        let c = h(i, j);
        let side = |a: f32, b: f32| {
            let d = |q: f32| if q.is_finite() { (q - c).abs() } else { f32::INFINITY };
            let m = d(a).min(d(b));
            if m.is_finite() { m } else { 0.0 }
        };
        let cell = self.cell as f32;
        let gx = side(h(i - 1, j), h(i + 1, j)) / cell;
        let gy = side(h(i, j - 1), h(i, j + 1)) / cell;
        (1.0 + gx * gx + gy * gy).sqrt()
    }

    fn cell_of(&self, p: DVec3) -> (i32, i32) {
        (((p.x - self.x0) / self.cell).round() as i32, ((p.y - self.y0) / self.cell).round() as i32)
    }

    fn event(&mut self, kind: EventKind, op: u64, step: usize, at: DVec3, depth: f64) {
        if let Some(e) = self.events.iter_mut().rev().take(8).find(|e| e.kind == kind && e.op == op && (e.step == step || e.step + 1 == step)) {
            e.depth = e.depth.max(depth);
            e.step = step;
            return;
        }
        if self.events.len() < 500 {
            self.events.push(SimEvent { kind, op, step, at: at.to_array(), depth });
        }
    }

    fn stamp(&mut self, ci: usize, op: u64, step: usize, p: DVec3, rapid: bool, check_holder: bool) {
        let (ci0, cj0) = self.cell_of(p);
        let (nx, ny) = (self.nx as i32, self.ny as i32);
        let tip = p.z as f32;
        let area = (self.cell * self.cell) as f32;
        let mut removed = 0.0f32;
        let mut deepest = 0.0f32;
        let mut gouge = 0.0f32;
        let c = &self.cutters[ci];
        if !c.lut.is_empty() {
            let n = (c.radius / self.cell).ceil() as i32 + 1;
            let r2 = c.radius * c.radius;
            let inner = (c.radius - 0.02).max(0.0);
            for j in (cj0 - n).max(0)..=(cj0 + n).min(ny - 1) {
                let dy = self.y0 + j as f64 * self.cell - p.y;
                for i in (ci0 - n).max(0)..=(ci0 + n).min(nx - 1) {
                    let dx = self.x0 + i as f64 * self.cell - p.x;
                    let d2 = dx * dx + dy * dy;
                    if d2 > r2 {
                        continue;
                    }
                    let rho = d2.sqrt();
                    let z = tip + c.lut[((rho / LUT_STEP) as usize).min(c.lut.len() - 1)];
                    let k = (j * nx + i) as usize;
                    let s = self.stock[k];
                    if s > z {
                        removed += (s - z) * area;
                        deepest = deepest.max(s - z);
                        self.stock[k] = z;
                        if rho <= inner && self.part[k] > z + c.allowance {
                            // depth across the surface, not straight down: on a steep wall a hair's
                            // breadth sideways reads as a deep cut from above
                            let d = (self.part[k] - z) / self.slope_factor(i, j);
                            if d > c.allowance {
                                gouge = gouge.max(d);
                                self.gouged[k] = true;
                            }
                        }
                    }
                }
            }
        }
        let mut shank_hit = 0.0f32;
        let mut holder_hit = 0.0f32;
        if check_holder {
            let sz = tip + c.shank_z;
            let hz = tip + c.holder_z;
            // cells clearly inside the shank (it may rub a wall its own flutes cut)
            let rs = c.shank_r - 0.02;
            let n = (rs / self.cell).ceil() as i32;
            for j in (cj0 - n).max(0)..=(cj0 + n).min(ny - 1) {
                let dy = self.y0 + j as f64 * self.cell - p.y;
                for i in (ci0 - n).max(0)..=(ci0 + n).min(nx - 1) {
                    let dx = self.x0 + i as f64 * self.cell - p.x;
                    if dx * dx + dy * dy > rs * rs {
                        continue;
                    }
                    let s = self.stock[(j * nx + i) as usize];
                    if s > sz + 0.01 {
                        shank_hit = shank_hit.max(s - sz);
                    }
                }
            }
            for &(di, dj) in &c.holder {
                let (i, j) = (ci0 + di, cj0 + dj);
                if i < 0 || j < 0 || i >= nx || j >= ny {
                    continue;
                }
                let s = self.stock[(j * nx + i) as usize];
                if s > hz + 0.01 {
                    holder_hit = holder_hit.max(s - hz);
                }
            }
        }
        let undercut = c.undercut;
        self.removed += removed as f64;
        if rapid && deepest > 0.02 {
            self.event(EventKind::Rapid, op, step, p, deepest as f64);
        }
        if gouge > 0.0 {
            self.event(EventKind::Gouge, op, step, p, gouge as f64);
        }
        // an undercut cutter's neck may just graze the wall it is cutting under
        if shank_hit > 0.0 && !(undercut && shank_hit < 0.05) {
            self.event(EventKind::Shank, op, step, p, shank_hit as f64);
        }
        if holder_hit > 0.0 {
            self.event(EventKind::Holder, op, step, p, holder_hit as f64);
        }
    }

    /// Triangles of the simulated stock (setup coordinates), at most `max_cells` along each side:
    /// positions and per-triangle colour class (0 = untouched stock, 1 = machined, 2 = gouge).
    pub fn mesh(&self, max_cells: usize) -> (Vec<[DVec3; 3]>, Vec<u8>) {
        let k = ((self.nx.max(self.ny) as f64 / max_cells.max(10) as f64).ceil() as usize).max(1);
        let (mx, my) = (self.nx.div_ceil(k), self.ny.div_ceil(k));
        let mut h = vec![0.0f32; mx * my];
        let mut cls = vec![0u8; mx * my];
        for bj in 0..my {
            for bi in 0..mx {
                let mut lo = f32::INFINITY;
                let mut g = false;
                for j in bj * k..((bj + 1) * k).min(self.ny) {
                    for i in bi * k..((bi + 1) * k).min(self.nx) {
                        lo = lo.min(self.stock[j * self.nx + i]);
                        g |= self.gouged[j * self.nx + i];
                    }
                }
                h[bj * mx + bi] = lo.max(self.bottom);
                cls[bj * mx + bi] = if g {
                    2
                } else if lo < self.top - 0.005 {
                    1
                } else {
                    0
                };
            }
        }
        let c = self.cell * k as f64;
        let mut tris = vec![];
        let mut col = vec![];
        let bz = self.bottom as f64;
        let x_of = |i: usize| (self.x0 + i as f64 * c - self.cell / 2.0).min(self.x0 + (self.nx as f64 - 0.5) * self.cell);
        let y_of = |j: usize| (self.y0 + j as f64 * c - self.cell / 2.0).min(self.y0 + (self.ny as f64 - 0.5) * self.cell);
        let quad = |tris: &mut Vec<[DVec3; 3]>, col: &mut Vec<u8>, a: DVec3, b: DVec3, cc: DVec3, d: DVec3, cl: u8| {
            tris.push([a, b, cc]);
            tris.push([a, cc, d]);
            col.push(cl);
            col.push(cl);
        };
        for j in 0..my {
            for i in 0..mx {
                let z = h[j * mx + i] as f64;
                if z <= bz + 1e-4 {
                    continue;
                }
                let (xa, xb, ya, yb) = (x_of(i), x_of(i + 1), y_of(j), y_of(j + 1));
                let cl = cls[j * mx + i];
                quad(&mut tris, &mut col, DVec3::new(xa, ya, z), DVec3::new(xb, ya, z), DVec3::new(xb, yb, z), DVec3::new(xa, yb, z), cl);
                // walls down to the neighbour (or the stock bottom at the edge)
                let nb = |ii: i64, jj: i64| -> f64 { if ii < 0 || jj < 0 || ii >= mx as i64 || jj >= my as i64 { bz } else { (h[jj as usize * mx + ii as usize] as f64).max(bz) } };
                // walls exposed by cutting count as machined; the stock's outer sides keep its colour
                let wall = |edge: bool| if edge { cl } else { cl.max(1) };
                let e = nb(i as i64 + 1, j as i64);
                if e < z {
                    quad(&mut tris, &mut col, DVec3::new(xb, ya, e), DVec3::new(xb, yb, e), DVec3::new(xb, yb, z), DVec3::new(xb, ya, z), wall(i + 1 >= mx));
                }
                let w = nb(i as i64 - 1, j as i64);
                if w < z {
                    quad(&mut tris, &mut col, DVec3::new(xa, yb, w), DVec3::new(xa, ya, w), DVec3::new(xa, ya, z), DVec3::new(xa, yb, z), wall(i == 0));
                }
                let nn = nb(i as i64, j as i64 + 1);
                if nn < z {
                    quad(&mut tris, &mut col, DVec3::new(xb, yb, nn), DVec3::new(xa, yb, nn), DVec3::new(xa, yb, z), DVec3::new(xb, yb, z), wall(j + 1 >= my));
                }
                let s = nb(i as i64, j as i64 - 1);
                if s < z {
                    quad(&mut tris, &mut col, DVec3::new(xa, ya, s), DVec3::new(xb, ya, s), DVec3::new(xb, ya, z), DVec3::new(xa, ya, z), wall(j == 0));
                }
            }
        }
        (tris, col)
    }
}
