//! The part being printed, bead by bead: each extruding move leaves a box along its path
//! (line width wide, layer height tall, its top a little under the nozzle), kept in a grid of
//! columns that know their highest bead. A moving group is only checked against beads that
//! stand higher than its lowest point, so ordinary planar layers (all below the nozzle) cost
//! nothing and the beads a non-planar move could hit are found exactly.

use std::collections::HashMap;

use glam::DVec3;

use super::collide::{gjk, Aabb, Bvh};

pub struct Bead {
    pub pts: [DVec3; 8],
    pub bb: Aabb,
    /// The move that laid it.
    pub mv: u32,
}

#[derive(Default)]
struct Cell {
    top: f64,
    beads: Vec<u32>,
}

pub struct Print {
    pub beads: Vec<Bead>,
    cell: f64,
    grid: HashMap<(i32, i32), Cell>,
    pub volume: f64,
}

/// A bead the moving group came within `reach` of: which body, which bead, how close, where.
#[derive(Clone, Copy, Debug)]
pub struct PrintNear {
    pub dist: f64,
    pub body: u32,
    pub bead: u32,
    pub at: DVec3,
}

impl Print {
    pub fn new(cell: f64) -> Print {
        Print { beads: vec![], cell, grid: HashMap::new(), volume: 0.0 }
    }

    /// A bead from p0 to p1 (nozzle tip positions, in the bed's frame) `w` wide and `h` tall,
    /// its top `allow` under the nozzle.
    pub fn add(&mut self, p0: DVec3, p1: DVec3, w: f64, h: f64, allow: f64, up: DVec3, mv: u32) {
        let d = p1 - p0;
        let mut side = d.cross(up);
        if side.length_squared() < 1e-12 {
            side = up.any_orthonormal_vector();
        }
        let side = side.normalize() * (w / 2.0);
        let top = -up * allow;
        let bot = -up * h.max(allow + 0.01);
        let mut pts = [DVec3::ZERO; 8];
        let mut bb = Aabb::EMPTY;
        let mut k = 0;
        for p in [p0, p1] {
            for s in [side, -side] {
                for v in [top, bot] {
                    pts[k] = p + s + v;
                    bb.add(pts[k]);
                    k += 1;
                }
            }
        }
        self.volume += d.length() * w * h;
        let id = self.beads.len() as u32;
        let topz = bb.hi.dot(up.abs());
        let (i0, j0, i1, j1) =
            ((bb.lo.x / self.cell).floor() as i32, (bb.lo.y / self.cell).floor() as i32, (bb.hi.x / self.cell).floor() as i32, (bb.hi.y / self.cell).floor() as i32);
        for i in i0..=i1 {
            for j in j0..=j1 {
                let c = self.grid.entry((i, j)).or_insert(Cell { top: f64::NEG_INFINITY, beads: vec![] });
                c.top = c.top.max(topz);
                c.beads.push(id);
            }
        }
        self.beads.push(Bead { pts, bb, mv });
    }

    pub fn is_empty(&self) -> bool {
        self.beads.is_empty()
    }

    /// The highest bead under a box (in plan), if any.
    fn top_under(&self, b: &Aabb) -> f64 {
        let (i0, j0, i1, j1) = ((b.lo.x / self.cell).floor() as i32, (b.lo.y / self.cell).floor() as i32, (b.hi.x / self.cell).floor() as i32, (b.hi.y / self.cell).floor() as i32);
        let mut top = f64::NEG_INFINITY;
        if ((i1 - i0 + 1) as i64) * ((j1 - j0 + 1) as i64) > self.grid.len() as i64 {
            for ((i, j), c) in &self.grid {
                if *i >= i0 && *i <= i1 && *j >= j0 && *j <= j1 {
                    top = top.max(c.top);
                }
            }
            return top;
        }
        for i in i0..=i1 {
            for j in j0..=j1 {
                if let Some(c) = self.grid.get(&(i, j)) {
                    top = top.max(c.top);
                }
            }
        }
        top
    }

    /// Group `a` (triangles in its own reference frame) moving from r0 to r1 relative to the bed's
    /// frame. Z is up (the bed frame's +Z at the reference pose).
    pub fn sweep(&self, a: &Bvh, r0: DVec3, r1: DVec3, reach: f64, out: &mut Vec<PrintNear>) {
        if self.beads.is_empty() || a.is_empty() {
            return;
        }
        let root = a.bounds().swept(r0, r1, reach);
        if self.top_under(&root) < root.lo.z {
            return;
        }
        // per triangle (the BVH's leaves are in order, so this walks it coarsely first)
        let n = a.tris.len();
        let chunk = 64;
        let mut pts = [DVec3::ZERO; 6];
        let mut seen: Vec<u32> = vec![];
        for c0 in (0..n).step_by(chunk) {
            let mut cb = Aabb::EMPTY;
            for t in &a.tris[c0..(c0 + chunk).min(n)] {
                for p in t {
                    cb.add(*p);
                }
            }
            let cs = cb.swept(r0, r1, reach);
            if self.top_under(&cs) < cs.lo.z {
                continue;
            }
            for i in c0..(c0 + chunk).min(n) {
                let t = &a.tris[i];
                let mut tb = Aabb::EMPTY;
                for p in t {
                    tb.add(*p);
                }
                let ts = tb.swept(r0, r1, reach);
                if self.top_under(&ts) < ts.lo.z {
                    continue;
                }
                for k in 0..3 {
                    pts[k] = t[k] + r0;
                    pts[k + 3] = t[k] + r1;
                }
                let (i0, j0, i1, j1) =
                    ((ts.lo.x / self.cell).floor() as i32, (ts.lo.y / self.cell).floor() as i32, (ts.hi.x / self.cell).floor() as i32, (ts.hi.y / self.cell).floor() as i32);
                seen.clear();
                for ci in i0..=i1 {
                    for cj in j0..=j1 {
                        let Some(cell) = self.grid.get(&(ci, cj)) else { continue };
                        if cell.top < ts.lo.z {
                            continue;
                        }
                        for bi in &cell.beads {
                            let bd = &self.beads[*bi as usize];
                            if !bd.bb.overlaps(&ts) || seen.contains(bi) {
                                continue;
                            }
                            seen.push(*bi);
                            let (d, _, pb) = gjk(&pts, &bd.pts);
                            if d <= reach {
                                let body = a.body[i];
                                match out.iter_mut().find(|x| x.body == body) {
                                    Some(x) if x.dist <= d => {}
                                    Some(x) => *x = PrintNear { dist: d, body, bead: *bi, at: pb },
                                    None => out.push(PrintNear { dist: d, body, bead: *bi, at: pb }),
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
