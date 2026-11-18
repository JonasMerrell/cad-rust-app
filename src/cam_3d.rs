//! 3D toolpaths. Finishing passes come from a drop cutter: the tool, held
//! vertically over a point, is lowered onto the part's triangles until it
//! touches (exactly, for flat, ball, bull nose, tapered ball and conical
//! cutters). Parallel passes sample it along lines, waterline and scallop
//! passes take iso-lines of it (or of a distance measured over it) on a grid.
//! Deburring runs a conical cutter along the part's sharp convex edges.
//! Adaptive clearing roughs level by level with offset-following passes that
//! grow the cleared ground a stepover at a time, so the tool never takes more
//! than the stepover sideways and hardly ever feeds through air.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::f64::consts::{FRAC_PI_2, TAU};

use glam::{DVec2, DVec3};

use crate::cam::{OpKind, Operation, Tool, ToolKind};
use crate::cam_paths::{difference, flat, inside, levels, offset, rect, shapes_area, stepdown, stepover, union_all, Contour, Ctx, Part, Pb, Shapes, Toolpath, P2};
use crate::cam_sim::{tool_lut, LUT_STEP};
use crate::cam_stock::StockMap;

// ---------------------------------------------------------------- cutter shapes

#[derive(Clone, Copy, Debug)]
enum Shape {
    /// Flat (cr = 0), bull nose or ball (cr = r).
    Bull { cr: f64 },
    /// Cone with a flat tip of radius `tip`; `tanb` = tan of the half angle.
    Cone { tip: f64, tanb: f64 },
    /// Ball tip of radius `tr` blending into a cone of half angle `half`.
    Taper { tr: f64, half: f64 },
}

#[derive(Clone, Copy, Debug)]
pub struct Cutter3 {
    pub r: f64,
    shape: Shape,
}

impl Cutter3 {
    pub fn new(t: &Tool) -> Cutter3 {
        let r = t.radius().max(1e-3);
        let shape = match t.kind {
            ToolKind::Ball => Shape::Bull { cr: r },
            ToolKind::Bull => Shape::Bull { cr: t.corner_radius.clamp(0.0, r) },
            ToolKind::TaperedBall => Shape::Taper { tr: t.corner_radius.clamp(0.01, r), half: t.angle.to_radians().clamp(0.01, 1.4) },
            ToolKind::Chamfer | ToolKind::Engrave | ToolKind::SpotDrill | ToolKind::CenterDrill | ToolKind::Drill => Shape::Cone {
                tip: if t.kind == ToolKind::Chamfer { (t.tip_diameter / 2.0).clamp(0.0, r) } else { 0.0 },
                tanb: (t.angle.clamp(1.0, 179.0) / 2.0).to_radians().tan(),
            },
            _ => Shape::Bull { cr: 0.0 },
        };
        Cutter3 { r, shape }
    }

    /// The cutter grown by `l` all round (stock to leave); its tip is then `l` lower. Cones only move up.
    fn grown(self, l: f64) -> Cutter3 {
        if l <= 0.0 {
            return self;
        }
        match self.shape {
            Shape::Bull { cr } => Cutter3 { r: self.r + l, shape: Shape::Bull { cr: cr + l } },
            Shape::Taper { tr, half } => Cutter3 { r: self.r + l, shape: Shape::Taper { tr: tr + l, half } },
            Shape::Cone { .. } => self,
        }
    }

    /// Height of the cutting surface above the tip at distance `rho` from the axis.
    pub fn h(&self, rho: f64) -> f64 {
        let rho = rho.max(0.0);
        match self.shape {
            Shape::Bull { cr } => {
                let a = self.r - cr;
                if rho <= a {
                    0.0
                } else {
                    let d = (rho - a).min(cr);
                    cr - (cr * cr - d * d).max(0.0).sqrt()
                }
            }
            Shape::Cone { tip, tanb } => (rho - tip).max(0.0) / tanb.max(1e-6),
            Shape::Taper { tr, half } => {
                let rt = tr * half.cos();
                if rho <= rt {
                    tr - (tr * tr - rho * rho).max(0.0).sqrt()
                } else {
                    tr * (1.0 - half.sin()) + (rho - rt) / half.tan().max(1e-6)
                }
            }
        }
    }

    /// Slope (radians from horizontal) of the cutting surface at `rho`.
    fn slope(&self, rho: f64) -> f64 {
        match self.shape {
            Shape::Bull { cr } => {
                let a = self.r - cr;
                if rho >= self.r - 1e-9 {
                    FRAC_PI_2
                } else if rho <= a || cr <= 1e-9 {
                    0.0
                } else {
                    ((rho - a) / cr).clamp(0.0, 1.0).asin()
                }
            }
            Shape::Cone { tip, tanb } => {
                if rho <= tip + 1e-9 {
                    0.0
                } else {
                    (1.0 / tanb.max(1e-6)).atan()
                }
            }
            Shape::Taper { tr, half } => {
                if rho <= tr * half.cos() {
                    (rho / tr).clamp(0.0, 1.0).asin()
                } else {
                    FRAC_PI_2 - half
                }
            }
        }
    }

    /// Where (distance from the axis) a plane sloping at `a` radians touches the cutter.
    fn facet_rho(&self, a: f64) -> f64 {
        match self.shape {
            Shape::Bull { cr } => {
                if a < 1e-9 {
                    0.0
                } else if cr <= 1e-9 {
                    self.r
                } else {
                    self.r - cr + cr * a.sin()
                }
            }
            Shape::Cone { tip, tanb } => {
                if a < (1.0 / tanb.max(1e-6)).atan() - 1e-9 {
                    tip
                } else {
                    self.r
                }
            }
            Shape::Taper { tr, half } => {
                if a <= FRAC_PI_2 - half {
                    tr * a.sin()
                } else {
                    self.r
                }
            }
        }
    }
}

/// Height of a tool's cutting surface above its tip (exact, for the simulator).
pub fn tool_height(t: &Tool, rho: f64) -> Option<f64> {
    match t.kind {
        ToolKind::Flat
        | ToolKind::Ball
        | ToolKind::Bull
        | ToolKind::TaperedBall
        | ToolKind::Chamfer
        | ToolKind::Engrave
        | ToolKind::SpotDrill
        | ToolKind::CenterDrill
        | ToolKind::Drill => Some(Cutter3::new(t).h(rho)),
        _ => None,
    }
}

// ---------------------------------------------------------------- drop cutter

pub struct Dropper {
    c: Cutter3,
    lift: f64,
    tris: Vec<[DVec3; 3]>,
    /// Per triangle: top height and XY bounding box.
    zmax: Vec<f64>,
    bb: Vec<[f64; 4]>,
    x0: f64,
    y0: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    bins: Vec<Vec<u32>>,
    bin_top: Vec<f64>,
}

/// Per-thread working space for `Dropper::drop`.
#[derive(Default)]
pub struct Scratch {
    cand: Vec<(f64, u32)>,
    mark: Vec<u32>,
    stamp: u32,
}

fn in_tri(t: &[DVec3; 3], q: DVec2) -> bool {
    let d = (t[1].x - t[0].x) * (t[2].y - t[0].y) - (t[2].x - t[0].x) * (t[1].y - t[0].y);
    if d.abs() < 1e-14 {
        return false;
    }
    let a = ((t[1].x - q.x) * (t[2].y - q.y) - (t[2].x - q.x) * (t[1].y - q.y)) / d;
    let b = ((t[2].x - q.x) * (t[0].y - q.y) - (t[0].x - q.x) * (t[2].y - q.y)) / d;
    a >= -1e-9 && b >= -1e-9 && 1.0 - a - b >= -1e-9
}

/// Distance in XY from `p` to the triangle (0 inside).
fn tri_dist(t: &[DVec3; 3], p: DVec2) -> f64 {
    if in_tri(t, p) {
        return 0.0;
    }
    (0..3)
        .map(|i| {
            let (a, b) = (t[i].truncate(), t[(i + 1) % 3].truncate());
            let ab = b - a;
            let l2 = ab.length_squared();
            let u = if l2 > 1e-18 { ((p - a).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
            (a + ab * u).distance(p)
        })
        .fold(f64::INFINITY, f64::min)
}

impl Dropper {
    /// A drop cutter for `tool` on the part, kept `leave` off its surface.
    pub fn new(part: &Part, tool: &Tool, leave: f64) -> Dropper {
        let c = Cutter3::new(tool).grown(leave);
        let lift = leave.max(0.0);
        let tris: Vec<[DVec3; 3]> = part
            .tris
            .iter()
            .filter(|t| {
                let n = (t[1] - t[0]).cross(t[2] - t[0]);
                let l = n.length();
                l > 1e-14 && n.z / l > -1e-6
            })
            .copied()
            .collect();
        let zmax: Vec<f64> = tris.iter().map(|t| t[0].z.max(t[1].z).max(t[2].z)).collect();
        let bb: Vec<[f64; 4]> =
            tris.iter().map(|t| [t[0].x.min(t[1].x).min(t[2].x), t[0].y.min(t[1].y).min(t[2].y), t[0].x.max(t[1].x).max(t[2].x), t[0].y.max(t[1].y).max(t[2].y)]).collect();
        let (lo, hi) = (part.lo, part.hi);
        let cell = (c.r * 0.5).max(0.25).max(((hi.x - lo.x) * (hi.y - lo.y) / 1_000_000.0).sqrt());
        let nx = ((hi.x - lo.x) / cell).floor().max(0.0) as usize + 1;
        let ny = ((hi.y - lo.y) / cell).floor().max(0.0) as usize + 1;
        let mut bins = vec![vec![]; nx * ny];
        for (k, b) in bb.iter().enumerate() {
            let i0 = (((b[0] - lo.x) / cell).floor().max(0.0) as usize).min(nx - 1);
            let i1 = (((b[2] - lo.x) / cell).floor().max(0.0) as usize).min(nx - 1);
            let j0 = (((b[1] - lo.y) / cell).floor().max(0.0) as usize).min(ny - 1);
            let j1 = (((b[3] - lo.y) / cell).floor().max(0.0) as usize).min(ny - 1);
            for j in j0..=j1 {
                for i in i0..=i1 {
                    bins[j * nx + i].push(k as u32);
                }
            }
        }
        let bin_top = bins.iter().map(|b| b.iter().map(|k| zmax[*k as usize]).fold(f64::NEG_INFINITY, f64::max)).collect();
        Dropper { c, lift, tris, zmax, bb, x0: lo.x, y0: lo.y, cell, nx, ny, bins, bin_top }
    }

    pub fn radius(&self) -> f64 {
        self.c.r
    }

    /// Tip height of the cutter over `p` resting on the part, and the slope of what it touches
    /// (radians); NEG_INFINITY when there is nothing under it.
    pub fn drop(&self, p: DVec2, s: &mut Scratch) -> (f64, f64) {
        let r = self.c.r;
        let i0 = ((p.x - r - self.x0) / self.cell).floor() as i64;
        let i1 = ((p.x + r - self.x0) / self.cell).floor() as i64;
        let j0 = ((p.y - r - self.y0) / self.cell).floor() as i64;
        let j1 = ((p.y + r - self.y0) / self.cell).floor() as i64;
        let mut best = (f64::NEG_INFINITY, 0.0);
        if i1 < 0 || j1 < 0 || i0 >= self.nx as i64 || j0 >= self.ny as i64 {
            return best;
        }
        if s.mark.len() != self.tris.len() || s.stamp == u32::MAX {
            s.mark = vec![0; self.tris.len()];
            s.stamp = 0;
        }
        s.stamp += 1;
        let stamp = s.stamp;
        s.cand.clear();
        // the bin under the centre first: its triangles usually decide the height early
        let ci = ((p.x - self.x0) / self.cell).floor() as i64;
        let cj = ((p.y - self.y0) / self.cell).floor() as i64;
        let centre = (ci >= 0 && cj >= 0 && ci < self.nx as i64 && cj < self.ny as i64).then_some((ci, cj));
        let rest = (j0.max(0)..=j1.min(self.ny as i64 - 1)).flat_map(|j| (i0.max(0)..=i1.min(self.nx as i64 - 1)).map(move |i| (i, j))).filter(|b| Some(*b) != centre);
        for (i, j) in centre.into_iter().chain(rest) {
            let dx = ((self.x0 + i as f64 * self.cell - p.x).max(p.x - self.x0 - (i + 1) as f64 * self.cell)).max(0.0);
            let dy = ((self.y0 + j as f64 * self.cell - p.y).max(p.y - self.y0 - (j + 1) as f64 * self.cell)).max(0.0);
            if dx * dx + dy * dy > r * r || self.bin_top[j as usize * self.nx + i as usize] - self.c.h(dx.hypot(dy)) <= best.0 {
                continue;
            }
            {
                for &k in &self.bins[j as usize * self.nx + i as usize] {
                    let ku = k as usize;
                    if s.mark[ku] == stamp {
                        continue;
                    }
                    s.mark[ku] = stamp;
                    let b = &self.bb[ku];
                    let dx = (b[0] - p.x).max(p.x - b[2]).max(0.0);
                    let dy = (b[1] - p.y).max(p.y - b[3]).max(0.0);
                    let dist = dx.hypot(dy);
                    if dist > r {
                        continue;
                    }
                    if dist == 0.0 {
                        // under the centre: usually decides it
                        self.tri(&self.tris[ku], p, &mut best);
                    } else {
                        let ub = self.zmax[ku] - self.c.h(dist);
                        if ub > best.0 {
                            s.cand.push((ub, k));
                        }
                    }
                }
            }
        }
        let floor = best.0;
        s.cand.retain(|c| c.0 > floor);
        s.cand.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
        for &(ub, k) in s.cand.iter() {
            if ub <= best.0 {
                break;
            }
            let t = &self.tris[k as usize];
            if self.zmax[k as usize] - self.c.h(tri_dist(t, p)) <= best.0 {
                continue;
            }
            self.tri(t, p, &mut best);
        }
        if best.0.is_finite() {
            best.0 += self.lift;
        }
        best
    }

    fn tri(&self, t: &[DVec3; 3], c: DVec2, best: &mut (f64, f64)) {
        let r = self.c.r;
        let n = (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
        if n.z > 1e-6 {
            let nxy = DVec2::new(n.x, n.y);
            let s = nxy.length();
            let a = s.atan2(n.z);
            let rho = self.c.facet_rho(a);
            let q = if s > 1e-12 { c - nxy / s * rho } else { c };
            if in_tri(t, q) {
                // the plane minus the cutter is concave: touching inside, nothing on the rim can be higher
                let z = t[0].z - (n.x * (q.x - t[0].x) + n.y * (q.y - t[0].y)) / n.z - self.c.h(rho);
                if z > best.0 {
                    *best = (z, a);
                }
                return;
            }
        }
        for v in t {
            let rho = v.truncate().distance(c);
            if rho <= r {
                let z = v.z - self.c.h(rho);
                if z > best.0 {
                    *best = (z, self.c.slope(rho));
                }
            }
        }
        for i in 0..3 {
            self.edge(t[i], t[(i + 1) % 3], c, best);
        }
    }

    fn edge(&self, a: DVec3, b: DVec3, c: DVec2, best: &mut (f64, f64)) {
        let r = self.c.r;
        let d = (b - a).truncate();
        let w = a.truncate() - c;
        let qa = d.length_squared();
        if qa < 1e-14 {
            return;
        }
        let qb = 2.0 * d.dot(w);
        let qc = w.length_squared() - r * r;
        let disc = qb * qb - 4.0 * qa * qc;
        if disc <= 0.0 {
            return;
        }
        let sq = disc.sqrt();
        let t0 = ((-qb - sq) / (2.0 * qa)).max(0.0);
        let t1 = ((-qb + sq) / (2.0 * qa)).min(1.0);
        if t1 <= t0 {
            return;
        }
        let zt = |t: f64| a.z + (b.z - a.z) * t;
        let tc = (-w.dot(d) / qa).clamp(t0, t1);
        if zt(t0).max(zt(t1)) - self.c.h((w + d * tc).length()) <= best.0 {
            return;
        }
        match self.c.shape {
            // flat end: the highest point of the edge inside the disk
            Shape::Bull { cr } if cr <= 1e-9 => {
                for t in [t0, t1] {
                    if zt(t) > best.0 {
                        *best = (zt(t), self.c.slope((w + d * t).length()));
                    }
                }
                return;
            }
            // ball: the sphere's centre sits r from the line
            Shape::Bull { cr } if (cr - r).abs() < 1e-9 => {
                let dz = b.z - a.z;
                let (wx, wy) = (-w.x, -w.y);
                let qa2 = qa;
                let qb2 = -2.0 * dz * (wx * d.x + wy * d.y);
                let d3 = qa + dz * dz;
                let qc2 = dz * dz * (wx * wx + wy * wy) + (wx * d.y - wy * d.x).powi(2) - r * r * d3;
                let disc = qb2 * qb2 - 4.0 * qa2 * qc2;
                if disc < 0.0 {
                    return;
                }
                let h = (-qb2 + disc.sqrt()) / (2.0 * qa2);
                let t = (wx * d.x + wy * d.y + h * dz) / d3;
                if (0.0..=1.0).contains(&t) {
                    let z = a.z + h - r;
                    if z > best.0 {
                        *best = (z, self.c.slope((w + d * t).length()));
                    }
                }
                return;
            }
            _ => {}
        }
        // z(t) - h(rho(t)) is concave: golden section search
        let f = |t: f64| zt(t) - self.c.h((w + d * t).length());
        let g = 0.618_033_988_749_895;
        let len = qa.sqrt();
        let (mut lo, mut hi) = (t0, t1);
        let mut x1 = hi - g * (hi - lo);
        let mut x2 = lo + g * (hi - lo);
        let (mut f1, mut f2) = (f(x1), f(x2));
        for _ in 0..60 {
            if (hi - lo) * len < 1e-3 {
                break;
            }
            if f1 < f2 {
                lo = x1;
                x1 = x2;
                f1 = f2;
                x2 = lo + g * (hi - lo);
                f2 = f(x2);
            } else {
                hi = x2;
                x2 = x1;
                f2 = f1;
                x1 = hi - g * (hi - lo);
                f1 = f(x1);
            }
        }
        for t in [(lo + hi) / 2.0, t0, t1] {
            let z = f(t);
            if z > best.0 {
                *best = (z, self.c.slope((w + d * t).length()));
            }
        }
    }
}

/// Map over 0..n on all cores; each worker gets its own scratch buffer for `Dropper::drop`.
fn par_map<T: Send, F: Fn(usize, &mut Scratch) -> T + Sync>(n: usize, f: F) -> Vec<T> {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 16);
    if n < 16 || threads == 1 {
        let mut s = Scratch::default();
        return (0..n).map(|i| f(i, &mut s)).collect();
    }
    let chunk = n.div_ceil(threads);
    std::thread::scope(|sc| {
        let hs: Vec<_> = (0..threads)
            .map(|k| {
                let f = &f;
                sc.spawn(move || {
                    let mut s = Scratch::default();
                    (k * chunk..((k + 1) * chunk).min(n)).map(|i| f(i, &mut s)).collect::<Vec<T>>()
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    })
}

// ---------------------------------------------------------------- grids and iso-lines

struct Grid {
    x0: f64,
    y0: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    v: Vec<f64>,
}

impl Grid {
    fn pos(&self, i: usize, j: usize) -> DVec2 {
        DVec2::new(self.x0 + i as f64 * self.cell, self.y0 + j as f64 * self.cell)
    }
}

/// A grid spacing near `want` that keeps the node count reasonable.
fn grid_cell(lo: DVec2, hi: DVec2, want: f64) -> f64 {
    let a = (hi.x - lo.x).max(0.0) * (hi.y - lo.y).max(0.0);
    want.max((a / 1_500_000.0).sqrt())
}

/// Drop-cutter heights on a grid, clamped to `floor`.
fn drop_grid(d: &Dropper, lo: DVec2, hi: DVec2, cell: f64, floor: f64) -> Grid {
    let nx = ((hi.x - lo.x) / cell).ceil().max(1.0) as usize + 1;
    let ny = ((hi.y - lo.y) / cell).ceil().max(1.0) as usize + 1;
    let rows = par_map(ny, |j, s| (0..nx).map(|i| d.drop(DVec2::new(lo.x + i as f64 * cell, lo.y + j as f64 * cell), s).0.max(floor)).collect::<Vec<f64>>());
    Grid { x0: lo.x, y0: lo.y, cell, nx, ny, v: rows.concat() }
}

type Refine<'a> = &'a (dyn Fn(DVec2, DVec2, &mut Scratch) -> DVec2 + Sync);

/// Closed iso-lines at `level` around the nodes where the grid is above it, with that region on the
/// left (outer loops counter-clockwise). `refine(inside, outside)` places each crossing exactly.
fn iso(g: &Grid, level: f64, refine: Option<Refine>) -> Vec<Vec<DVec2>> {
    let nx = g.nx;
    let inside = |i: usize, j: usize| g.v[j * nx + i] > level;
    let kh = |i: usize, j: usize| (j * nx + i) * 2;
    let kv = |i: usize, j: usize| (j * nx + i) * 2 + 1;
    let mut next: HashMap<usize, usize> = HashMap::new();
    for j in 0..g.ny.saturating_sub(1) {
        for i in 0..nx.saturating_sub(1) {
            let cs = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
            let ins = cs.map(|(a, b)| inside(a, b));
            if ins.iter().all(|x| *x) || ins.iter().all(|x| !*x) {
                continue;
            }
            let keys = [kh(i, j), kv(i + 1, j), kh(i, j + 1), kv(i, j)];
            // crossings in counter-clockwise order: (edge key, entering the region)
            let mut cr: Vec<(usize, bool)> = vec![];
            for e in 0..4 {
                let (a, b) = (ins[e], ins[(e + 1) % 4]);
                if a != b {
                    cr.push((keys[e], b));
                }
            }
            if !cr[0].1 {
                cr.rotate_left(1);
            }
            if cr.len() == 2 {
                next.insert(cr[1].0, cr[0].0);
            } else {
                let centre = cs.iter().map(|(a, b)| g.v[b * nx + a]).sum::<f64>() / 4.0 > level;
                if centre {
                    next.insert(cr[1].0, cr[2].0);
                    next.insert(cr[3].0, cr[0].0);
                } else {
                    next.insert(cr[1].0, cr[0].0);
                    next.insert(cr[3].0, cr[2].0);
                }
            }
        }
    }
    let keys: Vec<usize> = next.keys().copied().collect();
    let pts = par_map(keys.len(), |n, s| {
        let k = keys[n];
        let idx = k / 2;
        let (i, j) = (idx % nx, idx / nx);
        let (i2, j2) = if k % 2 == 0 { (i + 1, j) } else { (i, j + 1) };
        let (va, vb) = (g.v[j * nx + i], g.v[j2 * nx + i2]);
        let (pa, pb) = (g.pos(i, j), g.pos(i2, j2));
        match refine {
            Some(f) => {
                if va > level {
                    f(pa, pb, s)
                } else {
                    f(pb, pa, s)
                }
            }
            None => {
                let t = ((level - va) / (vb - va)).clamp(0.0, 1.0);
                pa.lerp(pb, t)
            }
        }
    });
    let at: HashMap<usize, DVec2> = keys.into_iter().zip(pts).collect();
    let mut out = vec![];
    // smallest key first, so the loops come out the same every run
    while let Some(&start) = next.keys().min() {
        let mut k = start;
        let mut lp = vec![];
        loop {
            lp.push(at[&k]);
            let Some(n) = next.remove(&k) else { break };
            k = n;
            if k == start {
                break;
            }
        }
        if lp.len() >= 3 {
            out.push(lp);
        }
    }
    out
}

/// Points along a closed loop at most `step` apart.
fn densify(c: &[DVec2], step: f64, closed: bool) -> Vec<DVec2> {
    let mut out = vec![];
    let n = c.len();
    let m = if closed { n } else { n.saturating_sub(1) };
    for i in 0..m {
        let (a, b) = (c[i], c[(i + 1) % n]);
        let k = (a.distance(b) / step).ceil().max(1.0) as usize;
        for s in 0..k {
            out.push(a.lerp(b, s as f64 / k as f64));
        }
    }
    if !closed {
        if let Some(l) = c.last() {
            out.push(*l);
        }
    }
    out
}

/// Drop points on a line that collapse onto straight runs, keeping the shape within `tol`.
fn simplify3(p: &[DVec3], tol: f64) -> Vec<DVec3> {
    if p.len() < 3 {
        return p.to_vec();
    }
    let mut out = vec![p[0]];
    let mut a = 0;
    let mut i = 2;
    while i < p.len() {
        let (pa, pb) = (p[a], p[i]);
        let ab = pb - pa;
        let ok = (a + 1..i).all(|k| {
            let q = p[k] - pa;
            let l2 = ab.length_squared();
            let t = if l2 > 1e-18 { (q.dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
            (q - ab * t).length() <= tol
        }) && i - a < 200;
        if !ok {
            out.push(p[i - 1]);
            a = i - 1;
        }
        i += 1;
    }
    out.push(*p.last().unwrap());
    out
}

// ---------------------------------------------------------------- shared finishing helpers

/// How far above a finishing entry the tool switches from rapid to plunge feed.
const ENTRY_FEED: f64 = 2.0;

struct Fin {
    d: Dropper,
    floor: f64,
    /// Top of the stock: entries feed the last `ENTRY_FEED` mm down (roughing leaves material above the surface), never from above it.
    top: f64,
    lo_slope: f64,
    hi_slope: f64,
    /// Rest machining: the stock left by earlier operations and the tool's profile; only positions
    /// that cut into it are kept.
    stock: Option<(StockMap, Vec<f32>)>,
}

/// Material thinner than this counts as already cut.
const REST_TOL: f64 = 0.02;

impl Fin {
    fn new(ctx: &Ctx, op: &Operation, t: &Tool) -> Fin {
        let d = Dropper::new(ctx.geo.part, t, op.leave);
        let floor = ctx.wz(op.bottom).unwrap_or(ctx.geo.part.lo.z);
        let (a, b) = (op.slope_min.clamp(0.0, 90.0), op.slope_max.clamp(0.0, 90.0));
        let stock = ctx.stock_in.clone().map(|m| (m, tool_lut(t))).filter(|(_, l)| !l.is_empty());
        let top = ctx.stock_in.as_ref().map(|m| m.max_top()).unwrap_or(ctx.stock.1.z).min(ctx.stock.1.z);
        Fin { d, floor, top, lo_slope: a.min(b).to_radians() - 1e-6, hi_slope: a.max(b).to_radians() + 1e-6, stock }
    }

    /// Whether a tool position is worth cutting: its slope is in range and (for rest machining) it touches material.
    fn keep(&self, p: DVec3, slope: f64) -> bool {
        self.wanted(slope) && self.stock.as_ref().map(|(m, lut)| m.engaged(p, self.d.radius(), lut, LUT_STEP) > REST_TOL).unwrap_or(true)
    }

    fn at(&self, p: DVec2, s: &mut Scratch) -> (DVec3, f64) {
        let (z, a) = self.d.drop(p, s);
        if z < self.floor {
            (DVec3::new(p.x, p.y, self.floor), 0.0)
        } else {
            (DVec3::new(p.x, p.y, z), a)
        }
    }

    /// Height to come down to (plus the retract) before feeding in to `p`. Roughing can leave far more than
    /// ENTRY_FEED above the surface (steps up to its stepdown on slopes), so without the stock that is left this
    /// is the stock's top; with it, the highest material under the tool there.
    fn entry_top(&self, p: DVec3) -> f64 {
        let over = match &self.stock {
            Some((m, _)) => m.top_near(p.x, p.y, self.d.radius()).min(self.top),
            None => self.top,
        };
        (p.z + ENTRY_FEED).max(over).min(self.top).max(p.z)
    }

    fn wanted(&self, slope: f64) -> bool {
        slope >= self.lo_slope && slope <= self.hi_slope
    }

    /// Sample a segment, adding points where the surface bends away from a straight line.
    fn sample(&self, a: DVec2, b: DVec2, ds: f64, s: &mut Scratch) -> Vec<(DVec3, f64)> {
        let n = (a.distance(b) / ds).ceil().max(1.0) as usize;
        let base: Vec<(DVec3, f64)> = (0..=n).map(|i| self.at(a.lerp(b, i as f64 / n as f64), s)).collect();
        self.refined(&base, s)
    }

    /// Add points between dropped points wherever a straight move would leave the surface by more than the tolerance.
    fn refined(&self, pts: &[(DVec3, f64)], s: &mut Scratch) -> Vec<(DVec3, f64)> {
        let mut out = pts[..1.min(pts.len())].to_vec();
        for w in pts.windows(2) {
            self.refine(w[0], w[1], 0, s, &mut out);
            out.push(w[1]);
        }
        out
    }

    fn refine(&self, p: (DVec3, f64), q: (DVec3, f64), depth: u32, s: &mut Scratch, out: &mut Vec<(DVec3, f64)>) {
        let m = self.at((p.0.truncate() + q.0.truncate()) / 2.0, s);
        let dev = m.0.z - (p.0.z + q.0.z) / 2.0;
        if dev.abs() <= 0.003 {
            return;
        }
        if depth >= 7 || p.0.truncate().distance(q.0.truncate()) < 0.01 {
            // a step too sharp to follow (over an edge): go over it, never through it
            if dev > 0.0 {
                let c = if p.0.z > q.0.z { DVec3::new(q.0.x, q.0.y, p.0.z.max(m.0.z)) } else { DVec3::new(p.0.x, p.0.y, q.0.z.max(m.0.z)) };
                if p.0.z <= q.0.z {
                    out.push((c, p.1));
                } else {
                    out.push((c, q.1));
                }
            }
            return;
        }
        self.refine(p, m, depth + 1, s, out);
        out.push(m);
        self.refine(m, q, depth + 1, s, out);
    }

    /// Split sampled points into the runs whose slope is wanted.
    fn runs(&self, pts: &[(DVec3, f64)]) -> Vec<Vec<DVec3>> {
        let mut out = vec![];
        let mut cur: Vec<DVec3> = vec![];
        for (p, a) in pts {
            if self.keep(*p, *a) {
                cur.push(*p);
            } else if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out.retain(|r| r.len() >= 2);
        out
    }

    /// Cut 3D runs in order. A hop shorter than `link` follows the surface; longer ones retract.
    fn emit(&self, pb: &mut Pb, runs: Vec<Vec<DVec3>>, link: f64) {
        let mut s = Scratch::default();
        for run in runs {
            let run = simplify3(&run, 0.002);
            let p0 = run[0];
            match pb.pos {
                Some(q) if q.truncate().distance(p0.truncate()) <= link && q.z < pb.safe - 1e-6 => {
                    let mut hop: Vec<DVec3> = self.sample(q.truncate(), p0.truncate(), 0.25, &mut s).into_iter().map(|p| p.0).collect();
                    // keep the hop at or above where it starts and ends
                    if let Some(f) = hop.first_mut() {
                        f.z = f.z.max(q.z);
                    }
                    if let Some(l) = hop.last_mut() {
                        l.z = l.z.max(p0.z);
                    }
                    for p in simplify3(&hop, 0.002) {
                        pb.cut(p, pb.feed);
                    }
                    pb.cut(p0, pb.feed);
                }
                _ => {
                    pb.approach(p0.x, p0.y, self.entry_top(p0));
                    pb.cut(p0, pb.plunge);
                }
            }
            for p in run.into_iter().skip(1) {
                pb.cut(p, pb.feed);
            }
        }
    }
}

/// Where the tool centre may go for finishing: the part's outline seen from above.
fn silhouette(ctx: &Ctx) -> Shapes {
    ctx.geo.shadow(ctx.geo.part.lo.z - 1.0)
}

/// Order closed loops nearest-first, starting each at the point nearest the tool.
fn order_loops(mut loops: Vec<Vec<DVec3>>, mut here: DVec2) -> Vec<Vec<DVec3>> {
    let mut out = vec![];
    while !loops.is_empty() {
        let (k, i) = loops
            .iter()
            .enumerate()
            .map(|(k, l)| {
                let (i, d) = l.iter().enumerate().map(|(i, p)| (i, p.truncate().distance_squared(here))).min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
                (k, i, d)
            })
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(k, i, _)| (k, i))
            .unwrap();
        let mut l = loops.remove(k);
        l.rotate_left(i);
        l.push(l[0]);
        here = l[0].truncate();
        out.push(l);
    }
    out
}

/// Order open runs nearest-first, reversing them when the far end is closer.
fn order_runs(mut runs: Vec<Vec<DVec3>>, mut here: DVec2) -> Vec<Vec<DVec3>> {
    let mut out = vec![];
    while !runs.is_empty() {
        let (k, rev) = runs
            .iter()
            .enumerate()
            .map(|(k, r)| {
                let a = r[0].truncate().distance_squared(here);
                let b = r.last().unwrap().truncate().distance_squared(here);
                (k, b < a, a.min(b))
            })
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(k, rev, _)| (k, rev))
            .unwrap();
        let mut r = runs.remove(k);
        if rev {
            r.reverse();
        }
        here = r.last().unwrap().truncate();
        out.push(r);
    }
    out
}

/// Turn a closed loop with a slope flag per point into runs: the whole loop when every point is wanted.
fn loop_runs(fin: &Fin, pts: Vec<(DVec3, f64)>) -> (Option<Vec<DVec3>>, Vec<Vec<DVec3>>) {
    let keep: Vec<bool> = pts.iter().map(|(p, a)| fin.keep(*p, *a)).collect();
    if keep.iter().all(|k| *k) {
        return (Some(pts.into_iter().map(|p| p.0).collect()), vec![]);
    }
    // start just after an unwanted point so runs don't wrap
    let n = pts.len();
    let k = keep.iter().position(|k| !*k).unwrap_or(0);
    let rot: Vec<(DVec3, f64)> = (0..n).map(|i| pts[(k + i) % n]).collect();
    (None, fin.runs(&rot))
}

// ---------------------------------------------------------------- parallel

pub(crate) fn parallel(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let t = pb.tool;
    let fin = Fin::new(ctx, op, t);
    let region = silhouette(ctx);
    if region.is_empty() {
        tp.warnings.push("nothing to finish".into());
        return;
    }
    let step = stepover(op, t);
    let ang = op.angle.to_radians();
    let dir = DVec2::new(ang.cos(), ang.sin());
    let nrm = DVec2::new(-ang.sin(), ang.cos());
    let pts: Vec<DVec2> = flat(&region).iter().flat_map(|c| c.iter().map(|p| DVec2::from(*p))).collect();
    let smin = pts.iter().map(|p| p.dot(nrm)).fold(f64::INFINITY, f64::min);
    let smax = pts.iter().map(|p| p.dot(nrm)).fold(f64::NEG_INFINITY, f64::max);
    let n = ((smax - smin) / step).ceil().max(0.0) as usize;
    let lines: Vec<f64> = (0..=n).map(|k| (smin + k as f64 * step).clamp(smin + 1e-4, smax - 1e-4)).collect();
    let ds = (t.radius() / 4.0).clamp(0.05, 0.5);
    let per_line = par_map(lines.len(), |k, s| {
        let iv = scan(&region, nrm, dir, lines[k]);
        let mut runs = vec![];
        for (a, b) in iv {
            let pa = nrm * lines[k] + dir * a;
            let pb2 = nrm * lines[k] + dir * b;
            let samples = fin.sample(pa, pb2, ds, s);
            runs.extend(fin.runs(&samples));
        }
        runs
    });
    let mut all = vec![];
    for (k, mut runs) in per_line.into_iter().enumerate() {
        if k % 2 == 1 {
            runs.reverse();
            for r in runs.iter_mut() {
                r.reverse();
            }
        }
        all.extend(runs);
    }
    if all.is_empty() {
        tp.warnings.push("no surfaces in the slope range".into());
        return;
    }
    fin.emit(pb, all, step * 3.0 + t.radius());
}

/// Where the line {p · nrm = s} runs inside the shapes, as intervals of p · dir.
fn scan(region: &Shapes, nrm: DVec2, dir: DVec2, s: f64) -> Vec<(f64, f64)> {
    let mut xs = vec![];
    for c in flat(region) {
        let n = c.len();
        for i in 0..n {
            let (p, q) = (DVec2::from(c[i]), DVec2::from(c[(i + 1) % n]));
            let (sp, sq) = (p.dot(nrm) - s, q.dot(nrm) - s);
            if (sp > 0.0) != (sq > 0.0) {
                let t = sp / (sp - sq);
                xs.push(p.lerp(q, t).dot(dir));
            }
        }
    }
    xs.sort_by(|a, b| a.total_cmp(b));
    xs.chunks(2).filter(|c| c.len() == 2 && c[1] - c[0] > 1e-6).map(|c| (c[0], c[1])).collect()
}

// ---------------------------------------------------------------- waterline

pub(crate) fn waterline(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let t = pb.tool;
    let fin = Fin::new(ctx, op, t);
    let part = ctx.geo.part;
    let r = fin.d.radius();
    let top = ctx.wz(op.top).unwrap_or(part.hi.z);
    let sd = stepdown(op, t);
    let want = (t.diameter / 12.0).clamp(0.05, 0.4);
    let pad = r + 2.0 * want + 0.5;
    let (lo, hi) = (part.lo.truncate() - DVec2::splat(pad), part.hi.truncate() + DVec2::splat(pad));
    let cell = grid_cell(lo, hi, want);
    let below = fin.floor - 1.0;
    let g = drop_grid(&fin.d, lo, hi, cell, below);
    let mut all = vec![];
    let mut here = DVec2::new(lo.x, lo.y);
    for z in levels(top, fin.floor, sd, &[]) {
        let refine = |pin: DVec2, pout: DVec2, s: &mut Scratch| {
            let (mut a, mut b) = (pin, pout);
            for _ in 0..11 {
                let m = (a + b) / 2.0;
                if fin.d.drop(m, s).0.max(below) > z {
                    a = m;
                } else {
                    b = m;
                }
            }
            (a + b) / 2.0
        };
        let loops = iso(&g, z, Some(&refine));
        let mut closed = vec![];
        let mut open = vec![];
        let loops: Vec<Vec<DVec2>> = loops.into_iter().filter(|l| l.len() >= 4).collect();
        let done = par_map(loops.len(), |k, s| {
            let mut l = tighten(&fin.d, &loops[k], z, below, cell, s);
            if op.climb {
                l.reverse();
            }
            l.iter()
                .map(|p| {
                    let (_, a) = fin.d.drop(*p, s);
                    (DVec3::new(p.x, p.y, z), a)
                })
                .collect::<Vec<(DVec3, f64)>>()
        });
        for pts in done {
            match loop_runs(&fin, pts) {
                (Some(c), _) => closed.push(c),
                (None, runs) => open.extend(runs),
            }
        }
        let mut level = order_loops(closed, here);
        // open pieces keep their direction (climb)
        let mut rest = open;
        rest.sort_by(|a, b| a[0].truncate().distance_squared(here).total_cmp(&b[0].truncate().distance_squared(here)));
        level.extend(rest);
        if let Some(l) = level.last() {
            here = l.last().unwrap().truncate();
        }
        all.extend(level);
    }
    if all.is_empty() {
        tp.warnings.push("no walls in the slope range".into());
        return;
    }
    fin.emit(pb, all, (t.diameter * 2.0).max(sd * 3.0));
}

/// Add points where a waterline loop's straight segments would cut into the part
/// (the region above `z` is on the loop's left).
fn tighten(d: &Dropper, l: &[DVec2], z: f64, below: f64, cell: f64, s: &mut Scratch) -> Vec<DVec2> {
    fn split(d: &Dropper, a: DVec2, b: DVec2, z: f64, below: f64, cell: f64, depth: u32, s: &mut Scratch, out: &mut Vec<DVec2>) {
        let m = (a + b) / 2.0;
        if depth >= 6 || a.distance(b) < 0.01 || d.drop(m, s).0.max(below) <= z + 1e-4 {
            return;
        }
        let t = b - a;
        let out_dir = DVec2::new(t.y, -t.x).normalize_or_zero();
        let (mut lo, mut hi) = (m, m + out_dir * cell);
        for _ in 0..16 {
            let q = (lo + hi) / 2.0;
            if d.drop(q, s).0.max(below) > z {
                lo = q;
            } else {
                hi = q;
            }
        }
        if hi.distance(m) < 0.0005 {
            return;
        }
        split(d, a, hi, z, below, cell, depth + 1, s, out);
        out.push(hi);
        split(d, hi, b, z, below, cell, depth + 1, s, out);
    }
    let n = l.len();
    let mut out = vec![];
    for i in 0..n {
        out.push(l[i]);
        split(d, l[i], l[(i + 1) % n], z, below, cell, 0, s, &mut out);
    }
    out
}

// ---------------------------------------------------------------- scallop

#[derive(PartialEq)]
struct Node(f64, usize);
impl Eq for Node {}
impl PartialOrd for Node {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Node {
    fn cmp(&self, o: &Self) -> Ordering {
        o.0.total_cmp(&self.0)
    }
}

/// Cut passes along iso-lines of a distance grid at the given levels, each level nearest-first.
fn offset_passes(fin: &Fin, op: &Operation, dg: &Grid, levels: &[f64], start: DVec2) -> Vec<Vec<DVec3>> {
    let mut all = vec![];
    let mut here = start;
    for &level in levels {
        let mut closed = vec![];
        let mut open = vec![];
        let loops = iso(dg, level, None);
        let done = par_map(loops.len(), |k, s| {
            let mut l = loops[k].clone();
            if op.climb {
                l.reverse();
            }
            let mut pts: Vec<(DVec3, f64)> = densify(&l, dg.cell * 0.5, true).into_iter().map(|p| fin.at(p, s)).collect();
            if let Some(f) = pts.first().copied() {
                pts.push(f);
                pts = fin.refined(&pts, s);
                pts.pop();
            }
            pts
        });
        for pts in done {
            if pts.len() < 4 {
                continue;
            }
            match loop_runs(fin, pts) {
                (Some(c), _) => closed.push(c),
                (None, runs) => open.extend(runs),
            }
        }
        let mut lv = order_loops(closed, here);
        lv.extend(order_runs(open, lv.last().map(|l| l.last().unwrap().truncate()).unwrap_or(here)));
        if let Some(l) = lv.last() {
            here = l.last().unwrap().truncate();
        }
        all.extend(lv);
    }
    all
}

/// Distance over the surface (the grid's heights) from the edge of the masked nodes, by Dijkstra on a
/// 16-neighbour grid; infinite outside the mask.
fn surface_distance(g: &Grid, mask: &[bool]) -> Vec<f64> {
    let (nx, ny, cell) = (g.nx, g.ny, g.cell);
    let mut dist = vec![f64::INFINITY; nx * ny];
    let mut heap = BinaryHeap::new();
    let nb: [(i64, i64); 16] = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1), (1, 2), (2, 1), (-1, 2), (-2, 1), (1, -2), (2, -1), (-1, -2), (-2, -1)];
    for j in 0..ny {
        for i in 0..nx {
            let k = j * nx + i;
            if !mask[k] {
                continue;
            }
            let edge = nb[..4].iter().any(|(di, dj)| {
                let (a, b) = (i as i64 + di, j as i64 + dj);
                a < 0 || b < 0 || a >= nx as i64 || b >= ny as i64 || !mask[b as usize * nx + a as usize]
            });
            if edge {
                dist[k] = 0.0;
                heap.push(Node(0.0, k));
            }
        }
    }
    while let Some(Node(d, k)) = heap.pop() {
        if d > dist[k] {
            continue;
        }
        let (i, j) = ((k % nx) as i64, (k / nx) as i64);
        for (di, dj) in nb {
            let (a, b) = (i + di, j + dj);
            if a < 0 || b < 0 || a >= nx as i64 || b >= ny as i64 {
                continue;
            }
            let q = b as usize * nx + a as usize;
            if !mask[q] {
                continue;
            }
            let dz = g.v[q] - g.v[k];
            let w = ((di * di + dj * dj) as f64 * cell * cell + dz * dz).sqrt();
            if d + w < dist[q] {
                dist[q] = d + w;
                heap.push(Node(d + w, q));
            }
        }
    }
    dist
}

pub(crate) fn scallop(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let t = pb.tool;
    let fin = Fin::new(ctx, op, t);
    let region = silhouette(ctx);
    if region.is_empty() {
        tp.warnings.push("nothing to finish".into());
        return;
    }
    let step = stepover(op, t);
    let pts: Vec<DVec2> = flat(&region).iter().flat_map(|c| c.iter().map(|p| DVec2::from(*p))).collect();
    let lo = pts.iter().fold(DVec2::splat(f64::INFINITY), |a, p| a.min(*p));
    let hi = pts.iter().fold(DVec2::splat(f64::NEG_INFINITY), |a, p| a.max(*p));
    let want = (step / 3.0).clamp(0.05, 0.5);
    let cell = grid_cell(lo, hi, want);
    let (lo, hi) = (lo - DVec2::splat(2.0 * cell), hi + DVec2::splat(2.0 * cell));
    let g = drop_grid(&fin.d, lo, hi, cell, fin.floor);
    let (nx, ny) = (g.nx, g.ny);
    // nodes inside the boundary, row by row
    let mut mask = vec![false; nx * ny];
    for j in 0..ny {
        let y = g.y0 + j as f64 * cell;
        for (a, b) in scan(&region, DVec2::Y, DVec2::X, y) {
            let i0 = (((a - g.x0) / cell).ceil() as i64).max(0);
            let i1 = (((b - g.x0) / cell).floor() as i64).min(nx as i64 - 1);
            for i in i0..=i1 {
                mask[j * nx + i as usize] = true;
            }
        }
    }
    let dist = surface_distance(&g, &mask);
    let dmax = dist.iter().filter(|d| d.is_finite()).fold(0.0f64, |a, b| a.max(*b));
    let dg = Grid { x0: g.x0, y0: g.y0, cell, nx, ny, v: dist.iter().zip(&mask).map(|(d, m)| if *m && d.is_finite() { *d } else { -1.0 }).collect() };
    let levels: Vec<f64> = (0..).map(|k| step * (k as f64 + 0.5)).take_while(|l| *l <= dmax).collect();
    let all = offset_passes(&fin, op, &dg, &levels, lo);
    if all.is_empty() {
        tp.warnings.push("no surfaces in the slope range".into());
        return;
    }
    fin.emit(pb, all, step * 3.0 + t.radius());
}

// ---------------------------------------------------------------- pencil / corner clean-up

/// The bigger tool whose leftovers a pencil pass cleans up: a ball of `ref_diameter`, or the smallest of
/// the bigger tools that finished (or else cut) before it, or a ball twice its size.
fn reference_tool(ctx: &Ctx, op: &Operation, t: &Tool) -> Tool {
    let ball = |d: f64| Tool { kind: ToolKind::Ball, diameter: d, corner_radius: d / 2.0, flute_length: t.flute_length.max(d * 3.0), ..t.clone() };
    if op.ref_diameter > 0.0 {
        return ball(op.ref_diameter);
    }
    let before = ctx.cam.ops.iter().take_while(|o| o.id != op.id).filter(|o| !o.suppressed);
    let bigger = |finishing: bool| {
        before
            .clone()
            .filter(|o| !finishing || matches!(o.kind, OpKind::Parallel | OpKind::Waterline | OpKind::Scallop | OpKind::Pencil))
            .filter_map(|o| ctx.cam.tool(o.tool))
            .filter(|x| x.diameter > t.diameter + 1e-6 && !x.kind.is_hole_tool())
            .min_by(|a, b| a.diameter.total_cmp(&b.diameter))
            .cloned()
    };
    bigger(true).or_else(|| bigger(false)).unwrap_or_else(|| ball(t.diameter * 2.0))
}

pub(crate) fn pencil(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let t = pb.tool;
    let fin = Fin::new(ctx, op, t);
    let region = silhouette(ctx);
    if region.is_empty() {
        tp.warnings.push("nothing to finish".into());
        return;
    }
    let reft = reference_tool(ctx, op, t);
    if reft.diameter <= t.diameter + 1e-6 {
        tp.warnings.push("the reference tool must be bigger than this one".into());
        return;
    }
    let step = stepover(op, t);
    let rr = reft.radius();
    let pts: Vec<DVec2> = flat(&region).iter().flat_map(|c| c.iter().map(|p| DVec2::from(*p))).collect();
    let lo = pts.iter().fold(DVec2::splat(f64::INFINITY), |a, p| a.min(*p)) - DVec2::splat(rr);
    let hi = pts.iter().fold(DVec2::splat(f64::NEG_INFINITY), |a, p| a.max(*p)) + DVec2::splat(rr);
    let cell = grid_cell(lo, hi, (step / 2.0).max(rr / 12.0).clamp(0.05, 0.3));
    let g = drop_grid(&fin.d, lo, hi, cell, fin.floor);
    let (nx, ny) = (g.nx, g.ny);
    // the lowest the reference tool's cutting surface reaches over each node
    let gr = drop_grid(&Dropper::new(ctx.geo.part, &reft, 0.0), lo, hi, cell, fin.floor);
    let lref = tool_lut(&reft);
    let n = (rr / cell).ceil() as i64;
    let disk: Vec<(i64, i64, f64)> = (-n..=n)
        .flat_map(|j| (-n..=n).map(move |i| (i, j)))
        .filter_map(|(i, j)| {
            let d = ((i * i + j * j) as f64).sqrt() * cell;
            (d <= rr).then(|| (i, j, lref[((d / LUT_STEP) as usize).min(lref.len() - 1)] as f64))
        })
        .collect();
    let env = par_map(ny, |j, _| {
        (0..nx)
            .map(|i| {
                let mut best = f64::INFINITY;
                for &(di, dj, h) in &disk {
                    let (a, b) = (i as i64 + di, j as i64 + dj);
                    if a < 0 || b < 0 || a >= nx as i64 || b >= ny as i64 {
                        continue;
                    }
                    best = best.min(gr.v[b as usize * nx + a as usize] + h);
                }
                best as f32
            })
            .collect::<Vec<f32>>()
    })
    .concat();
    // what the reference tool leaves, as stock (and no more than what is really left, for rest machining)
    let mut left = StockMap { x0: g.x0, y0: g.y0, cell, nx, ny, hi: env, lo: vec![(fin.floor - 10.0) as f32; nx * ny] };
    if op.rest {
        if let Some(m) = &ctx.stock_in {
            for j in 0..ny {
                for i in 0..nx {
                    let k = j * nx + i;
                    left.hi[k] = left.hi[k].min(m.top(g.x0 + i as f64 * cell, g.y0 + j as f64 * cell));
                }
            }
        }
    }
    // tool positions inside the outline that would still cut something
    let lut = tool_lut(t);
    let mut inside = vec![false; nx * ny];
    for j in 0..ny {
        let y = g.y0 + j as f64 * cell;
        for (a, b) in scan(&region, DVec2::Y, DVec2::X, y) {
            let i0 = (((a - g.x0) / cell).ceil() as i64).max(0);
            let i1 = (((b - g.x0) / cell).floor() as i64).min(nx as i64 - 1);
            for i in i0..=i1 {
                inside[j * nx + i as usize] = true;
            }
        }
    }
    let rows = par_map(ny, |j, _| {
        (0..nx)
            .map(|i| {
                let k = j * nx + i;
                inside[k] && g.v[k] > fin.floor + 1e-6 && left.engaged(DVec3::new(g.x0 + i as f64 * cell, g.y0 + j as f64 * cell, g.v[k]), t.radius(), &lut, LUT_STEP) > REST_TOL
            })
            .collect::<Vec<bool>>()
    });
    let mask: Vec<bool> = rows.concat();
    if !mask.iter().any(|m| *m) {
        tp.warnings.push(format!("nothing left by a Ø{} {} to clean up", crate::cam::fmt_mm(reft.diameter), reft.kind.label().to_lowercase()));
        return;
    }
    let dist = surface_distance(&g, &mask);
    // passes in from the edge of each leftover strip; strips narrower than a stepover get one down the middle
    let mut comp = vec![usize::MAX; nx * ny];
    let mut cmax: Vec<f64> = vec![];
    for k0 in 0..nx * ny {
        if !mask[k0] || comp[k0] != usize::MAX {
            continue;
        }
        let c = cmax.len();
        let mut best = 0.0f64;
        let mut stack = vec![k0];
        comp[k0] = c;
        while let Some(k) = stack.pop() {
            best = best.max(dist[k]);
            let (i, j) = (k % nx, k / nx);
            for (a, b) in [(i.wrapping_sub(1), j), (i + 1, j), (i, j.wrapping_sub(1)), (i, j + 1)] {
                if a < nx && b < ny && mask[b * nx + a] && comp[b * nx + a] == usize::MAX {
                    comp[b * nx + a] = c;
                    stack.push(b * nx + a);
                }
            }
        }
        cmax.push(best);
    }
    let dmax = cmax.iter().fold(0.0f64, |a, b| a.max(*b));
    let wide = Grid { x0: g.x0, y0: g.y0, cell, nx, ny, v: (0..nx * ny).map(|k| if mask[k] && cmax[comp[k]] >= step * 0.5 { dist[k] } else { -1.0 }).collect() };
    let narrow = Grid {
        x0: g.x0,
        y0: g.y0,
        cell,
        nx,
        ny,
        v: (0..nx * ny).map(|k| if mask[k] && cmax[comp[k]] < step * 0.5 && cmax[comp[k]] > 0.0 { dist[k] / cmax[comp[k]] } else { -1.0 }).collect(),
    };
    let levels: Vec<f64> = (0..).map(|k| step * (k as f64 + 0.5)).take_while(|l| *l <= dmax).collect();
    let mut all = offset_passes(&fin, op, &narrow, &[0.5], lo);
    let here = all.last().and_then(|r| r.last()).map(|p| p.truncate()).unwrap_or(lo);
    all.extend(offset_passes(&fin, op, &wide, &levels, here));
    if all.is_empty() {
        tp.warnings.push("no corners in the slope range".into());
        return;
    }
    fin.emit(pb, all, step * 3.0 + t.radius());
}

// ---------------------------------------------------------------- deburr / 3D chamfer

/// A deburr's picked points in setup coordinates: those given in work coordinates (`edges`) and those
/// picked in the view (`edge_picks`, part coordinates).
pub fn deburr_picks(op: &Operation, part: &Part, origin: DVec3) -> Vec<DVec3> {
    op.edges.iter().map(|p| DVec3::from(*p) + origin).chain(op.edge_picks.iter().map(|p| part.rot * DVec3::from(*p))).collect()
}

/// Sharp convex edges that a vertical tool can reach (not the vertical ones), as chains of points with the
/// outward horizontal direction of each edge, and whether the chain closes.
pub fn sharp_edges(part: &Part, min_angle: f64) -> Vec<(Vec<DVec3>, Vec<DVec2>, bool)> {
    let q = |p: DVec3| ((p.x * 1e4).round() as i64, (p.y * 1e4).round() as i64, (p.z * 1e4).round() as i64);
    let mut ids: HashMap<(i64, i64, i64), usize> = HashMap::new();
    let mut verts: Vec<DVec3> = vec![];
    let mut tri_ids: Vec<[usize; 3]> = vec![];
    for t in &part.tris {
        let mut v = [0usize; 3];
        for (k, p) in t.iter().enumerate() {
            let n = ids.len();
            let id = *ids.entry(q(*p)).or_insert(n);
            if id == verts.len() {
                verts.push(*p);
            }
            v[k] = id;
        }
        tri_ids.push(v);
    }
    let mut edges: HashMap<(usize, usize), Vec<(usize, usize)>> = HashMap::new();
    for (ti, v) in tri_ids.iter().enumerate() {
        for e in 0..3 {
            let (a, b) = (v[e], v[(e + 1) % 3]);
            if a == b {
                continue;
            }
            edges.entry((a.min(b), a.max(b))).or_default().push((ti, v[(e + 2) % 3]));
        }
    }
    let normal = |ti: usize| {
        let t = &part.tris[ti];
        (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero()
    };
    let cos_min = min_angle.to_radians().cos();
    // sharp edge → outward horizontal direction
    let mut sharp: HashMap<(usize, usize), DVec2> = HashMap::new();
    for (&(a, b), fs) in &edges {
        if fs.len() != 2 {
            continue;
        }
        let (n1, n2) = (normal(fs[0].0), normal(fs[1].0));
        if n1 == DVec3::ZERO || n2 == DVec3::ZERO || n1.dot(n2) > cos_min {
            continue;
        }
        // convex: the other face's far corner lies behind the first face
        if (verts[fs[1].1] - verts[a]).dot(n1) > -1e-7 {
            continue;
        }
        if n1.z < -0.05 || n2.z < -0.05 {
            continue;
        }
        let bis = (n1 + n2).normalize_or_zero();
        let h = bis.truncate();
        if h.length() < 0.2 || bis.z < -0.3 || (verts[b] - verts[a]).normalize_or_zero().z.abs() > 0.5 {
            continue;
        }
        sharp.insert((a, b), h.normalize());
    }
    // chain the edges
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut keys: Vec<(usize, usize)> = sharp.keys().copied().collect();
    keys.sort();
    for (a, b) in keys {
        adj.entry(a).or_default().push(b);
        adj.entry(b).or_default().push(a);
    }
    let mut used: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut chains = vec![];
    let mut starts: Vec<usize> = adj.iter().filter(|(_, n)| n.len() != 2).map(|(v, _)| *v).collect();
    starts.sort();
    let mut rest: Vec<usize> = adj.keys().copied().collect();
    rest.sort();
    starts.extend(rest);
    for s0 in starts {
        for &n0 in adj[&s0].clone().iter() {
            let key = (s0.min(n0), s0.max(n0));
            if used.contains(&key) {
                continue;
            }
            let mut ch = vec![s0];
            let (mut prev, mut cur) = (s0, n0);
            used.insert(key);
            let mut closed = false;
            loop {
                ch.push(cur);
                if cur == s0 {
                    closed = true;
                    break;
                }
                let nexts = &adj[&cur];
                if nexts.len() != 2 {
                    break;
                }
                let nx = if nexts[0] == prev { nexts[1] } else { nexts[0] };
                let k = (cur.min(nx), cur.max(nx));
                if used.contains(&k) {
                    break;
                }
                used.insert(k);
                prev = cur;
                cur = nx;
            }
            let pts: Vec<DVec3> = ch.iter().map(|v| verts[*v]).collect();
            let dirs = ch.windows(2).map(|w| sharp[&(w[0].min(w[1]), w[0].max(w[1]))]).collect();
            chains.push((pts, dirs, closed));
        }
    }
    chains
}

pub(crate) fn deburr(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let t = pb.tool;
    let c = Cutter3::new(t);
    let Shape::Cone { tip, tanb } = c.shape else {
        tp.warnings.push("deburring needs a chamfer mill, V-bit or spot drill".into());
        return;
    };
    let d = Dropper::new(ctx.geo.part, t, 0.0);
    let w = op.chamfer.max(0.0);
    let dz = w / tanb.max(1e-6);
    let e = if op.tip_offset > 0.0 { op.tip_offset } else { 0.2 };
    let rho = tip + (dz + e) * tanb;
    if rho > c.r + 1e-6 {
        tp.warnings.push(format!("a {} mm chamfer is too wide for this tool", crate::cam::fmt_mm(w)));
        return;
    }
    let picks = deburr_picks(op, ctx.geo.part, ctx.origin);
    let mut runs = vec![];
    let mut s = Scratch::default();
    let mut chains = sharp_edges(ctx.geo.part, op.edge_angle.clamp(5.0, 170.0));
    if !picks.is_empty() {
        // picked edges: break the chains at corners and keep the pieces picked
        let mut pieces = vec![];
        for (pts, dirs, _) in chains {
            let mut start = 0;
            for i in 1..=dirs.len() {
                if i == dirs.len() || dirs[i - 1].angle_to(dirs[i]).abs() > 0.5 {
                    pieces.push((pts[start..=i].to_vec(), dirs[start..i].to_vec(), false));
                    start = i;
                }
            }
        }
        pieces.retain(|(pts, _, _)| pts.windows(2).any(|w| picks.iter().any(|p| seg_dist(*p, w[0], w[1]) < 1.0)));
        chains = pieces;
    }
    // only the edges whose cut lies between the operation's top and bottom
    let (zlo, zhi) = (ctx.wz(op.bottom).unwrap_or(f64::NEG_INFINITY), ctx.wz(op.top).unwrap_or(f64::INFINITY));
    for (pts, dirs, closed) in chains {
        let centres = deburr_centres(&pts, &dirs, closed, rho);
        let mut cur: Vec<DVec3> = vec![];
        let mut pieces = vec![];
        for ctr in centres {
            let (z, _) = d.drop(ctr, &mut s);
            if !z.is_finite() || z - dz < zlo - 1e-6 || z - dz > zhi + 1e-6 {
                if cur.len() >= 2 {
                    pieces.push(std::mem::take(&mut cur));
                }
                cur.clear();
                continue;
            }
            cur.push(DVec3::new(ctr.x, ctr.y, z - dz));
        }
        if cur.len() >= 2 {
            pieces.push(cur);
        }
        for mut pc in pieces {
            // climb: part on the right of travel
            let a = pc[0].truncate();
            let b = pc[pc.len().min(4) - 1].truncate();
            let tdir = (b - a).normalize_or_zero();
            let mid = pc.len() / 2;
            let probe = d.drop(pc[mid].truncate() + DVec2::new(tdir.y, -tdir.x) * 0.5, &mut s).0;
            let right_is_part = probe > d.drop(pc[mid].truncate() - DVec2::new(tdir.y, -tdir.x) * 0.5, &mut s).0;
            if right_is_part != op.climb {
                pc.reverse();
            }
            runs.push(pc);
        }
    }
    if runs.is_empty() {
        tp.warnings.push(if picks.is_empty() { "no sharp edges the tool can reach".into() } else { "no sharp edges near the picked points".into() });
        return;
    }
    let mut here = DVec2::new(ctx.stock.0.x, ctx.stock.0.y);
    let mut ordered = vec![];
    while !runs.is_empty() {
        let k = (0..runs.len()).min_by(|a, b| runs[*a][0].truncate().distance_squared(here).total_cmp(&runs[*b][0].truncate().distance_squared(here))).unwrap();
        let r = runs.remove(k);
        here = r.last().unwrap().truncate();
        ordered.push(r);
    }
    for run in ordered {
        let run = simplify3(&run, 0.002);
        // the cone first touches the edge `dz` above the run: come down to above that
        pb.approach(run[0].x, run[0].y, (run[0].z + dz + ENTRY_FEED).min(ctx.stock.1.z).max(run[0].z + dz));
        pb.cut(run[0], pb.plunge);
        for p in run.into_iter().skip(1) {
            pb.cut(p, pb.feed);
        }
    }
}

/// Tool centres along a chain of edges `pts` (each edge `i` from `pts[i]` to `pts[i + 1]`, offset by `rho` along its
/// outward direction `dirs[i]`): arcs round the convex corners; at the concave ones (inside a hole, an inside
/// corner) the neighbouring offsets meet where they cross, and edges too short to show between their
/// neighbours' offsets drop out, so the tool never runs back and forth over the same stretch.
fn deburr_centres(pts: &[DVec3], dirs: &[DVec2], closed: bool, rho: f64) -> Vec<DVec2> {
    let ne = dirs.len();
    if ne == 0 {
        return vec![];
    }
    let closed = closed && ne > 2;
    // each edge as an offset line: start, unit direction, and the stretch of it [s0, s1] that is used
    struct Line {
        a: DVec2,
        t: DVec2,
        len: f64,
        k: usize,
    }
    let lines: Vec<Line> = (0..ne)
        .map(|i| {
            let (a, b) = (pts[i].truncate(), pts[i + 1].truncate());
            Line { a: a + dirs[i] * rho, t: (b - a).normalize_or_zero(), len: a.distance(b), k: i }
        })
        .collect();
    let convex = |j: &Line, i: &Line| {
        let turn = dirs[j.k].angle_to(dirs[i.k]);
        turn.abs() > 0.01 && (dirs[i.k] - dirs[j.k]).dot(j.t) > 0.0
    };
    // where line j's offset meets line i's at a concave corner: (end on j, start on i)
    let meet = |j: &Line, i: &Line| -> Option<(f64, f64)> {
        let den = j.t.perp_dot(i.t);
        if den.abs() < 1e-9 {
            return None;
        }
        let d = i.a - j.a;
        Some((d.perp_dot(i.t) / den, d.perp_dot(j.t) / den))
    };
    let mut keep: Vec<usize> = (0..ne).collect();
    let mut span: Vec<(f64, f64)> = lines.iter().map(|l| (0.0, l.len)).collect();
    // trim the concave corners, dropping the edges that vanish, until none do
    for _ in 0..ne {
        let m = keep.len();
        for v in span.iter_mut() {
            *v = (0.0, f64::NAN);
        }
        for &k in &keep {
            span[k] = (0.0, lines[k].len);
        }
        let pairs = if closed { m } else { m.saturating_sub(1) };
        for q in 0..pairs {
            let (j, i) = (keep[q], keep[(q + 1) % m]);
            if i == j || convex(&lines[j], &lines[i]) {
                continue;
            }
            if let Some((sj, si)) = meet(&lines[j], &lines[i]) {
                // a near-parallel crossing far off: leave both ends as they are
                if sj.is_finite() && si.is_finite() && (sj - lines[j].len).abs() < 4.0 * rho + lines[j].len && si.abs() < 4.0 * rho + lines[i].len {
                    span[j].1 = sj;
                    span[i].0 = si;
                }
            }
        }
        let gone: Vec<usize> = keep.iter().copied().filter(|&k| span[k].1 < span[k].0 - 1e-9).collect();
        if gone.is_empty() || gone.len() >= keep.len().saturating_sub(1) {
            break;
        }
        // the ends of an open chain stay
        keep.retain(|k| !gone.contains(k) || (!closed && (*k == 0 || *k == ne - 1)));
        if keep.len() == m {
            break;
        }
    }
    let m = keep.len();
    let mut centres: Vec<DVec2> = vec![];
    let arc = |centres: &mut Vec<DVec2>, j: usize, i: usize| {
        // round a convex corner at the start of edge i
        let a = pts[i].truncate();
        let turn = dirs[j].angle_to(dirs[i]);
        let k = ((turn.abs() * rho / 0.2).ceil() as usize).max(2);
        for s in 1..k {
            centres.push(a + DVec2::from_angle(turn * s as f64 / k as f64).rotate(dirs[j]) * rho);
        }
    };
    for q in 0..m {
        let k = keep[q];
        let l = &lines[k];
        if q > 0 && keep[q - 1] + 1 == k && convex(&lines[keep[q - 1]], l) {
            arc(&mut centres, keep[q - 1], k);
        }
        let (s0, s1) = span[k];
        let s1 = s1.max(s0);
        let n = ((s1 - s0) / 0.3).ceil().max(1.0) as usize;
        for c in 0..=n {
            centres.push(l.a + l.t * (s0 + (s1 - s0) * c as f64 / n as f64));
        }
    }
    if closed && m > 1 {
        let (j, i) = (keep[m - 1], keep[0]);
        if (j + 1) % ne == i && convex(&lines[j], &lines[i]) {
            arc(&mut centres, j, i);
        }
        if let Some(&p) = centres.first() {
            centres.push(p);
        }
    }
    centres.dedup_by(|a, b| a.distance(*b) < 1e-6);
    centres
}

fn seg_dist(p: DVec3, a: DVec3, b: DVec3) -> f64 {
    let ab = b - a;
    let l2 = ab.length_squared();
    let t = if l2 > 1e-18 { ((p - a).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (a + ab * t).distance(p)
}

// ---------------------------------------------------------------- adaptive clearing

/// Offset-following adaptive clearing of one level. Everything is in tool-centre space: `a` is where the
/// centre may go (outside the part and its allowance), `material` the stock at this level.
///
/// The cleared region grows from where the tool is already free (beside the stock, or what earlier
/// operations cleared) by one stepover at a time, `free_{k+1} = (free_k + stepover) ∩ a`, and the tool runs
/// along the new boundary only where it still touches material. Every point of that boundary lies within a
/// stepover of centres that are already clear, so the tool never takes more than the stepover sideways, also
/// along the walls, round corners and in the last pass at each level. The pieces start and end in air, links
/// stay at depth through cleared ground at the fast feed, and material enclosed all round gets a helix in.
pub(crate) struct Level<'a, 'b> {
    pub pb: &'a mut Pb<'b>,
    pub z: f64,
    pub z_from: f64,
    pub r: f64,
    pub step: f64,
    /// Cutting feed (with chip thinning) and the feed for links through cleared ground.
    pub feed: f64,
    pub fast: f64,
    pub climb: bool,
    /// Radius of the helix into enclosed material.
    pub helix: f64,
    /// How much further out than `material` says the stock may really stand (rest machining takes the outline
    /// from the stock map, less slivers a cell or two thin): the tool comes down from above that much clear of it.
    pub slack: f64,
    /// Passes not cut yet that a link mustn't pass near (the passes run out of ring order, see `emit`).
    pub blocking: Blocking,
    /// The highest the part (with its allowance) or the stock stands under the tool along a straight move,
    /// for hops between passes no higher than they need be; None: up to the safe height.
    pub hop: Option<&'a dyn Fn(DVec2, DVec2) -> f64>,
}

/// Where passes not cut yet run: their points on a grid, so a link can tell it would pass through material
/// they haven't taken yet.
#[derive(Default)]
pub(crate) struct Blocking {
    /// How near a link may come to such a pass.
    d: f64,
    grid: std::collections::HashMap<(i64, i64), Vec<usize>>,
    pts: Vec<Vec<DVec2>>,
    active: Vec<bool>,
}

impl Blocking {
    fn new(d: f64, pts: Vec<Vec<DVec2>>) -> Blocking {
        let mut grid: std::collections::HashMap<(i64, i64), Vec<usize>> = Default::default();
        for (i, ps) in pts.iter().enumerate() {
            for p in ps {
                let k = ((p.x / d).floor() as i64, (p.y / d).floor() as i64);
                let v = grid.entry(k).or_default();
                if v.last() != Some(&i) {
                    v.push(i);
                }
            }
        }
        let n = pts.len();
        Blocking { d, grid, pts, active: vec![false; n] }
    }

    /// The passes (other than those `skip` says) with a point within `d` of `p`.
    fn near(&self, p: DVec2, mut keep: impl FnMut(usize) -> bool) -> Vec<usize> {
        let (cx, cy) = ((p.x / self.d).floor() as i64, (p.y / self.d).floor() as i64);
        let mut out = vec![];
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(v) = self.grid.get(&(cx + dx, cy + dy)) {
                    for &i in v {
                        if !out.contains(&i) && keep(i) && self.pts[i].iter().any(|q| q.distance(p) <= self.d) {
                            out.push(i);
                        }
                    }
                }
            }
        }
        out
    }

    /// Whether `p` is within `d` of a pass that is still to be cut and must come first.
    fn blocks(&self, p: DVec2) -> bool {
        self.d > 0.0 && !self.near(p, |i| self.active[i]).is_empty()
    }
}

/// A pass of one ring of the clearing: a piece of the ring, or a helix down into enclosed material.
enum Piece {
    Seg(crate::cam_contour::Seg),
    Helix(DVec2, f64),
}

/// What a ring was made from: the centres cleared before it, the material left, and the centres touching it.
struct Ring {
    free: Shapes,
    remaining: Shapes,
    near: Shapes,
}

/// A centre counts as clear of material when its tool misses it by this much.
const GAP: f64 = 0.05;

fn circle(c: DVec2, rad: f64) -> Contour {
    let n = ((TAU * rad / 0.3).ceil() as usize).clamp(16, 180);
    (0..n).map(|i| (c + DVec2::from_angle(i as f64 * TAU / n as f64) * rad).to_array()).collect()
}

fn opened(s: &Shapes, e: f64) -> Shapes {
    offset(&offset(s, -e), e)
}

impl Level<'_, '_> {
    fn at_depth(&self) -> bool {
        self.pb.pos.map(|p| (p.z - self.z).abs() < 1e-6).unwrap_or(false)
    }

    /// Can the tool feed straight from `a` to `b` at depth without touching material (`near`: centres that do)?
    fn link_clear(&self, a: DVec2, b: DVec2, area: &Shapes, near: &Shapes, free: &Shapes) -> bool {
        let len = a.distance(b);
        if len > 25.0 * self.r.max(1.0) {
            return false;
        }
        let n = (len / (0.25 * self.r).max(0.05)).ceil().max(1.0) as usize;
        // the end too, and only over centres already cleared at this level (the phone stand: a 12 mm link along
        // the groove's backrest, over a band the passes hadn't reached yet, met a strip 4 mm wide)
        let cleared = offset(free, 0.1 * self.r);
        (1..=n).all(|k| {
            let p = a.lerp(b, k as f64 / n as f64).to_array();
            inside(area, p) && !inside(near, p) && inside(&cleared, p) && !self.blocking.blocks(DVec2::from(p))
        })
    }

    /// Get to `p` (a clear centre) at depth: straight over through cleared ground, else from above.
    fn go_to(&mut self, p: DVec2, area: &Shapes, near: &Shapes, free: &Shapes) {
        let here = self.pb.pos;
        if let Some(q) = here {
            if q.truncate().distance(p) < 1e-6 && (q.z - self.z).abs() < 1e-6 {
                return;
            }
        }
        if self.at_depth() && self.link_clear(here.unwrap().truncate(), p, area, near, free) {
            self.pb.cut(p.extend(self.z), self.fast);
            return;
        }
        // retract, and come down beside the material (the column over a clear centre is clear); a start whose
        // tool would touch material (a hole the stock map shows open lower down, say) gets a helix, not a plunge
        if inside(near, p.to_array()) {
            // (no wider than the room round the start: the helix must not leave where the centre may go)
            let room = nearest_on(area, p).map(|q| q.distance(p)).unwrap_or(0.0);
            self.helix_in(p, self.helix.min(0.9 * room));
            return;
        }
        // (right beside material the stock may stand a little further out than its outline here, and higher
        // than the level above: come down that much further off, and feed over)
        let down = match nearest_on(near, p) {
            Some(q) if self.slack > 0.0 && q.distance(p) < self.slack && q.distance(p) > 1e-9 => {
                let off = q + (p - q) / q.distance(p) * (self.slack + 0.05);
                if inside(area, off.to_array()) && !inside(near, off.to_array()) { off } else { p }
            }
            _ => p,
        };
        self.hop_over(down);
        self.pb.cut(down.extend(self.z), self.fast);
        self.pb.cut(p.extend(self.z), self.feed);
    }

    /// Up, over to `p` and down to just above the level before: only as high as what stands under the way
    /// needs (plus the retract), not all the way to the safe height.
    fn hop_over(&mut self, p: DVec2) {
        let h = match (self.hop, self.pb.pos) {
            (Some(f), Some(q)) => Some(f(q.truncate(), p).max(self.z_from) + self.pb.retract.max(1.0)),
            _ => None,
        };
        match (h, self.pb.pos) {
            (Some(h), Some(q)) if h < self.pb.safe - 1e-6 => {
                let over = h.max(q.z);
                self.pb.rapid(DVec3::new(q.x, q.y, over));
                self.pb.rapid(DVec3::new(p.x, p.y, over));
                self.pb.rapid(DVec3::new(p.x, p.y, (self.z_from + self.pb.retract).min(over)));
            }
            _ => self.pb.approach(p.x, p.y, self.z_from),
        }
    }

    /// Cut along `pts` at depth; `near`: centres whose tool reaches material (tight arcs there are slowed).
    fn run(&mut self, pts: &[P2], near: &Shapes) {
        let start = self.pb.pos.map(|p| p.truncate());
        for (i, q) in pts.iter().enumerate() {
            let prev = if i > 0 { Some(DVec2::from(pts[i - 1])) } else { start };
            let next = pts.get(i + 1).map(|n| DVec2::from(*n));
            let f = match (prev, next) {
                (Some(a), Some(c)) if inside(near, *q) => arc_feed(self.feed, a, DVec2::from(*q), c, self.r),
                _ => self.feed,
            };
            self.pb.cut(DVec3::new(q[0], q[1], self.z), f);
        }
    }

    /// Spiral down into material at `c` on a circle of radius `rh`, then clear that circle at depth.
    fn helix_in(&mut self, c: DVec2, rh: f64) {
        let start = c + DVec2::new(rh, 0.0);
        self.pb.approach(start.x, start.y, self.z_from);
        self.pb.cut(start.extend(self.z_from), self.pb.plunge);
        if rh < 0.05 {
            self.pb.cut(start.extend(self.z), self.pb.plunge);
            return;
        }
        let dz = self.z_from - self.z;
        let pitch = (TAU * rh * self.pb.ramp.tan()).max(0.05);
        let turns = (dz / pitch).ceil().max(1.0);
        let seg = (2.0 * (1.0 - (0.01 / rh).min(1.0)).acos()).clamp(0.08, 0.5);
        let n = ((turns * TAU / seg).ceil() as usize).max(8);
        // climbing the material round the helix: counter-clockwise, with it on the right
        let dir = if self.climb { 1.0 } else { -1.0 };
        // down, then one more turn at depth
        let m = (TAU / seg).ceil() as usize;
        for i in 1..=n + m {
            let f = (i as f64 / n as f64).min(1.0);
            let a = dir * i as f64 / n as f64 * turns * TAU;
            let q = c + DVec2::from_angle(a) * rh;
            self.pb.cut(DVec3::new(q.x, q.y, self.z_from - dz * f), self.pb.feed);
        }
    }

    /// The loop oriented so the material outside the free region is on the cutting side: climb milling (a
    /// spindle turning clockwise, M3) has it on the right of travel, as an outside contour going clockwise
    /// round the part.
    fn orient(&self, c: &Contour, outer: bool) -> Contour {
        let ccw = crate::cam_paths::area(c) > 0.0;
        // outer loops have the free region inside: counter-clockwise puts what's outside on the right
        let want_ccw = if outer { self.climb } else { !self.climb };
        if ccw == want_ccw {
            c.clone()
        } else {
            c.iter().rev().copied().collect()
        }
    }

    /// Clear the level: `a` = where the centre may go, `material` = the stock there, `cleared` = centres the
    /// tool has already cleared round (besides those whose tool misses the material).
    pub(crate) fn clear(&mut self, a: &Shapes, material: &Shapes, cleared: &Shapes) {
        let r = self.r;
        let mut free = difference(a, &offset(material, r - GAP));
        if !cleared.is_empty() {
            free = union_all(&[flat(&free), flat(&intersect(cleared, a))].concat());
        }
        // a hole the tool only just fits (a bore from an earlier setup, say) is no place to start from: rings
        // growing out of it are arcs tighter than the tool, where the edge runs several times the stepover into
        // the material for every one the centre moves. Leave such holes to the passes that come from open ground,
        // which break through into them; they are a start only for material nothing else reaches.
        let room = std::f64::consts::PI * r * r;
        let (holes, open): (Shapes, Shapes) = free.iter().cloned().partition(|sh| shapes_area(&vec![sh.clone()]) < room);
        let mut holes = if open.is_empty() { vec![] } else { holes };
        if !holes.is_empty() {
            free = open;
        }
        // first everything but a stepover-wide band along the walls, with the passes turning away before they
        // reach it (a pass that runs into the wall takes the corner the passes before it left there, head-on);
        // then the walls, in passes along them that take that band
        let inner = opened(&offset(a, -self.step), 0.05);
        if !inner.is_empty() {
            free = self.grow(&inner, a, material, free, false, &mut vec![]);
        }
        self.grow(a, a, material, free, true, &mut holes);
    }

    /// Grow the cleared centres `free` within `lim` (part of `a`) until no material is left that it reaches;
    /// `seed`: start from `holes` (small clear spots set aside), then with a helix into the material, for what
    /// can't be reached otherwise.
    fn grow(&mut self, lim: &Shapes, a: &Shapes, material: &Shapes, mut free: Shapes, seed: bool, holes: &mut Shapes) -> Shapes {
        let r = self.r;
        let h = (0.2 * r).clamp(0.05, 1.0);
        let bridge = (1.5 * r).max(1.0);
        // the rings first, then their passes in an order that keeps the tool in one place (see `emit`)
        let mut rings: Vec<Ring> = vec![];
        let mut pieces: Vec<(usize, Piece)> = vec![];
        for _ in 0..5000 {
            let remaining = opened(&difference(material, &offset(&free, r)), 0.02);
            if shapes_area(&remaining) < 0.05 {
                break;
            }
            // centres whose tool reaches the material that is left
            let near = offset(&remaining, r - GAP);
            // the new ring with its corners rounded to the tool's radius: a pass that runs into a sharp corner of
            // it (where it meets a wall) takes the corner the passes before it left there head-on. The corners
            // come off in later rings, which round them a stepover further in; the last bits without rounding.
            // Where the ground is narrower than the tool (a channel between two walls less than 2 r apart in
            // centres) a rounding of r takes the whole ring away: round tighter there rather than not at all, or
            // each pass across the channel starts with a stepover straight along the wall into the face, the
            // front half of the tool in the cut. Rounded, the ring leaves the walls tangentially and the cut
            // grows from nothing, as on a trochoid. Round a curve of radius ρ the edge takes (ρ + r) / ρ times
            // what the centre moves, so a tighter rounding grows by less: as much at its heaviest as a ring
            // rounded to r.
            let had = shapes_area(&free);
            let plain = union_all(&[flat(&free), flat(&intersect(&offset(&free, self.step), lim))].concat());
            let mut grown = union_all(&[flat(&free), flat(&opened(&intersect(&offset(&free, self.step), lim), r))].concat());
            if shapes_area(&grown) - had < 0.05 * self.step * r {
                // (a tighter rounding only where it takes a good part of what a plain ring as wide would: in a
                // channel, not for the last slivers in a corner, which would take many rings of next to nothing)
                let worth = 0.5 * (shapes_area(&plain) - had);
                grown = [0.8 * r, 0.6 * r, 0.4 * r, 0.25 * r]
                    .iter()
                    .map(|round| {
                        let share = 2.0 * round / (round + r);
                        let ring = intersect(&offset(&free, self.step * share), lim);
                        (share, union_all(&[flat(&free), flat(&opened(&ring, *round))].concat()))
                    })
                    .find(|(share, g)| shapes_area(g) - had >= (share * worth).max(0.05 * self.step * r))
                    .map(|(_, g)| g)
                    .unwrap_or(plain);
            }
            if shapes_area(&grown) - shapes_area(&free) < 1e-3 {
                if !seed {
                    break;
                }
                // what is left can't be reached from open ground: from the holes set aside, if there are any
                if !holes.is_empty() {
                    free = union_all(&[flat(&free), flat(holes)].concat());
                    holes.clear();
                    continue;
                }
                // material enclosed all round (or out of reach): a helix in where it is roomiest
                match roomiest(&intersect(a, &near), r) {
                    Some((c, room)) => {
                        let rh = self.helix.min(room * 0.9).max(0.0);
                        rings.push(Ring { free: free.clone(), remaining: remaining.clone(), near: near.clone() });
                        pieces.push((rings.len() - 1, Piece::Helix(c, rh)));
                        free = union_all(&[flat(&free), flat(&intersect(&vec![vec![circle(c, rh.max(0.02))]], a))].concat());
                        continue;
                    }
                    None => break,
                }
            }
            let mut segs: Vec<crate::cam_contour::Seg> = vec![];
            for sh in &grown {
                for (i, c) in sh.iter().enumerate() {
                    segs.extend(crate::cam_contour::clip(&self.orient(c, i == 0), &near, h, bridge));
                }
            }
            rings.push(Ring { free: free.clone(), remaining: remaining.clone(), near: near.clone() });
            pieces.extend(segs.into_iter().map(|sg| (rings.len() - 1, Piece::Seg(sg))));
            free = grown;
        }
        self.emit(pieces, &rings, a, h);
        free
    }

    /// Cut the rings' passes. A pass waits only for the passes of earlier rings near it (they clear what it
    /// leans on), so the tool finishes one area ring after ring before it goes elsewhere, instead of going round
    /// every area once per ring; of the passes it may cut, it takes the nearest. Links at depth also keep clear
    /// of passes still to be cut whose material they would otherwise run into.
    fn emit(&mut self, pieces: Vec<(usize, Piece)>, rings: &[Ring], a: &Shapes, h: f64) {
        let r = self.r;
        let d = 2.0 * r + self.step + 0.5;
        let sample = |p: &Piece| -> Vec<DVec2> {
            match p {
                Piece::Helix(c, rh) => vec![*c + DVec2::new(*rh, 0.0), *c, *c - DVec2::new(*rh, 0.0)],
                Piece::Seg(sg) => {
                    // every point, and more along long straight stretches
                    let mut v: Vec<DVec2> = vec![];
                    for w in sg.pts.windows(2) {
                        let (p, q) = (DVec2::from(w[0]), DVec2::from(w[1]));
                        let n = (p.distance(q) / (0.5 * d)).ceil().max(1.0) as usize;
                        v.extend((0..n).map(|k| p.lerp(q, k as f64 / n as f64)));
                    }
                    v.extend(sg.pts.last().map(|p| DVec2::from(*p)));
                    v
                }
            }
        };
        let pts: Vec<Vec<DVec2>> = pieces.iter().map(|(_, p)| sample(p)).collect();
        let n = pieces.len();
        let mut blocking = Blocking::new(d, pts);
        // what each pass waits for: passes of earlier rings within reach
        let deps: Vec<Vec<usize>> = (0..n)
            .map(|q| {
                let mut v: Vec<usize> = vec![];
                for p in blocking.pts[q].clone() {
                    for i in blocking.near(p, |i| pieces[i].0 < pieces[q].0) {
                        if !v.contains(&i) {
                            v.push(i);
                        }
                    }
                }
                v
            })
            .collect();
        let mut done = vec![false; n];
        let mut left = n;
        while left > 0 {
            let here = self.pb.pos.map(|p| p.truncate()).unwrap_or(DVec2::ZERO);
            let dist = |i: usize| match &pieces[i].1 {
                Piece::Helix(c, rh) => (*c + DVec2::new(*rh, 0.0)).distance_squared(here),
                Piece::Seg(sg) if sg.closed => sg.pts.iter().map(|q| DVec2::from(*q).distance_squared(here)).fold(f64::INFINITY, f64::min),
                Piece::Seg(sg) => DVec2::from(sg.pts[0]).distance_squared(here),
            };
            // (ties broken by ring, then the order the rings made them in)
            let j = (0..n)
                .filter(|i| !done[*i] && deps[*i].iter().all(|k| done[*k]))
                .min_by(|x, y| dist(*x).total_cmp(&dist(*y)).then(pieces[*x].0.cmp(&pieces[*y].0)).then(x.cmp(y)))
                .unwrap_or_else(|| (0..n).find(|i| !done[*i]).unwrap());
            let ring = pieces[j].0;
            for i in 0..n {
                // (only earlier rings: the link stays within the centres cleared before this ring, and what this
                // ring's other passes take lies beyond those centres' reach)
                blocking.active[i] = !done[i] && pieces[i].0 < ring;
            }
            self.blocking = std::mem::take(&mut blocking);
            let rg = &rings[ring];
            match &pieces[j].1 {
                Piece::Helix(c, rh) => self.helix_in(*c, *rh),
                Piece::Seg(sg) => self.emit_seg(sg.clone(), rg, a, h),
            }
            blocking = std::mem::take(&mut self.blocking);
            done[j] = true;
            left -= 1;
        }
    }

    /// One pass of a ring, cut from where the tool is.
    fn emit_seg(&mut self, s: crate::cam_contour::Seg, ring: &Ring, a: &Shapes, h: f64) {
        let r = self.r;
        let (free, remaining, near) = (&ring.free, &ring.remaining, &ring.near);
        let here = self.pb.pos.map(|p| p.truncate()).unwrap_or(DVec2::ZERO);
        {
            {

                if s.closed {
                    // the whole loop is in material: ease onto it from the cleared ground beside it over a stretch
                    // behind its start (a blend from the nearest clear centres out to the loop), so the cut deepens
                    // gradually instead of the tool pushing straight out into the wall
                    let n = s.pts.len();
                    let pt = |i: usize| DVec2::from(s.pts[i % n]);
                    let k = (0..n).min_by(|x, y| pt(*x).distance_squared(here).total_cmp(&pt(*y).distance_squared(here))).unwrap();
                    let per: f64 = (0..n).map(|i| pt(i).distance(pt(i + 1))).sum();
                    let ease = (4.0 * r).max(3.0 * self.step).min(0.4 * per);
                    let mut back = vec![];
                    let (mut acc, mut i) = (0.0, k);
                    while acc < ease && back.len() + 1 < n {
                        let j = (i + n - 1) % n;
                        acc += pt(j).distance(pt(i));
                        back.push((j, acc));
                        i = j;
                    }
                    // the cleared outline beside the stretch, walked alongside it at the same share of its length
                    // (nearest points would gather at the outline's corners and fan out from them, head-on)
                        let stretch: Vec<(f64, DVec2)> = back.iter().rev().map(|(j, d)| (acc - d, pt(*j))).chain(std::iter::once((acc, pt(k)))).collect();
                    let lead = beside(&free, &stretch, 1.5 * self.step + h);
                    let blend: Vec<DVec2> = back
                        .iter()
                        .rev()
                        .map(|(j, d)| {
                            let q = pt(*j);
                            let f = match &lead {
                                Some(l) => l(acc - d),
                                None => nearest_on(&free, q).filter(|f| f.distance(q) <= 1.5 * self.step + h).unwrap_or(q),
                            };
                            let t = (1.0 - d / acc.max(1e-9)).clamp(0.0, 1.0);
                            f + (q - f) * (t * t * (3.0 - 2.0 * t))
                        })
                        .collect();
                    let start = blend.first().copied().unwrap_or(pt(k));
                    self.go_to(start, a, &near, &free);
                    let mut pts: Vec<P2> = blend.iter().skip(1).map(|q| q.to_array()).collect();
                    pts.extend((0..=n).map(|i| s.pts[(k + i) % n]));
                    self.run(&pts, &near);
                } else {
                    // a pass that runs head-on into the material (along a wall into the face it is clearing: a slot
                    // for a moment) can be eased in at its start, but not at its end, where the leading edge takes
                    // the whole crescent the earlier passes left against the wall. So run it the way that ends in the
                    // open; when both ends meet walls, cut it from each wall in to its middle.
                    let rev: Vec<P2> = s.pts.iter().rev().copied().collect();
                    let per: f64 = s.pts.windows(2).map(|w| DVec2::from(w[0]).distance(DVec2::from(w[1]))).sum();
                    let w = 3.0 * r;
                    let (fs, bs) = (head_on(&s.pts, &remaining, r, w), head_on(&rev, &remaining, r, w));
                    let (fe, be) = (head_on(tail(&s.pts, w), &remaining, r, f64::INFINITY), head_on(tail(&rev, w), &remaining, r, f64::INFINITY));
                    let hit = |e: f64| e > 0.5 * h;
                    if hit(fe) && hit(be) && per > 8.0 * r {
                        let mut acc = 0.0;
                        let m = (1..s.pts.len())
                            .find(|k| {
                                acc += DVec2::from(s.pts[k - 1]).distance(DVec2::from(s.pts[*k]));
                                acc >= per / 2.0
                            })
                            .unwrap_or(s.pts.len() / 2);
                        let first = s.pts[..=m].to_vec();
                        let second: Vec<P2> = s.pts[m..].iter().rev().copied().collect();
                        self.open_pass(&first, &remaining, &free, a, &near, h);
                        self.open_pass(&second, &remaining, &free, a, &near, h);
                    } else {
                        let backwards = if hit(fe) != hit(be) { hit(fe) } else if hit(fe) { be < fe } else { fs > bs + 0.5 * h };
                        self.open_pass(if backwards { &rev } else { &s.pts }, &remaining, &free, a, &near, h);
                    }
                }
            }
        }
    }

    /// Cut an open pass, eased onto from the cleared ground over its first stretch when it starts head-on.
    fn open_pass(&mut self, pts: &[P2], remaining: &Shapes, free: &Shapes, a: &Shapes, near: &Shapes, h: f64) {
        let r = self.r;
        if pts.len() < 2 {
            return;
        }
        if head_on(pts, remaining, r, 3.0 * r) <= 0.5 * h {
            self.go_to(DVec2::from(pts[0]), a, near, free);
            self.run(&pts[1..], near);
            return;
        }
        // (over no more than the first part of a short pass: one eased all the way never gets out to the line it
        // is meant to cut, and the ring it leaves there counts as cleared)
        let per: f64 = pts.windows(2).map(|w| DVec2::from(w[0]).distance(DVec2::from(w[1]))).sum();
        let ease = (4.0 * r).max(3.0 * self.step).min(0.4 * per);
        let mut run: Vec<P2> = vec![];
        let mut acc = 0.0;
        for (i, q) in pts.iter().enumerate() {
            if i > 0 {
                acc += DVec2::from(pts[i - 1]).distance(DVec2::from(*q));
            }
            if acc >= ease {
                run.extend(pts[i..].iter().copied());
                break;
            }
            let q = DVec2::from(*q);
            let f = nearest_on(free, q).filter(|f| f.distance(q) <= 1.5 * self.step + h).unwrap_or(q);
            let t = acc / ease;
            run.push((f + (q - f) * (t * t * (3.0 - 2.0 * t))).to_array());
        }
        self.go_to(DVec2::from(run[0]), a, near, free);
        if run.len() > 1 {
            self.run(&run[1..], near);
        }
    }
}

/// The outline of `free` running beside a stretch of a loop from `a` to `b`, `len` long: a function from the
/// distance along the stretch to the point as far along the outline, scaled to its length there. None when no
/// outline is within `reach` of both ends.
fn beside(free: &Shapes, stretch: &[(f64, DVec2)], reach: f64) -> Option<impl Fn(f64) -> DVec2> {
    let (a, b) = (stretch.first()?.1, stretch.last()?.1);
    let len = stretch.last()?.0;
    // the contour nearest the stretch's start
    let mut best: Option<(f64, Vec<DVec2>)> = None;
    for c in free.iter().flat_map(|sh| sh.iter()) {
        let d = nearest_on(&vec![vec![c.clone()]], a).map(|q| q.distance(a)).unwrap_or(f64::INFINITY);
        if best.as_ref().map(|b| d < b.0).unwrap_or(true) {
            best = Some((d, c.iter().map(|p| DVec2::from(*p)).collect()));
        }
    }
    let (d0, c) = best?;
    let n = c.len();
    if d0 > reach || n < 3 || len < 1e-9 {
        return None;
    }
    let cum: Vec<f64> = std::iter::once(0.0).chain((0..n).scan(0.0, |s, i| { *s += c[i].distance(c[(i + 1) % n]); Some(*s) })).collect();
    let per = cum[n];
    // arc length of the foot of p on the contour, and how far p is from it
    let proj = |p: DVec2| {
        let mut best = (f64::INFINITY, 0.0);
        for i in 0..n {
            let (u, v) = (c[i], c[(i + 1) % n]);
            let uv = v - u;
            let t = if uv.length_squared() > 1e-18 { ((p - u).dot(uv) / uv.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
            let d = (u + uv * t).distance(p);
            if d < best.0 {
                best = (d, cum[i] + t * uv.length());
            }
        }
        best
    };
    let ((_, sa), (db, sb)) = (proj(a), proj(b));
    if db > reach {
        return None;
    }
    let at = move |s: f64| {
        let s = s.rem_euclid(per);
        let i = (0..n).find(|i| cum[i + 1] >= s).unwrap_or(n - 1);
        let seg = cum[i + 1] - cum[i];
        let t = if seg > 1e-12 { (s - cum[i]) / seg } else { 0.0 };
        c[i].lerp(c[(i + 1) % n], t)
    };
    // round the contour the way that keeps beside the stretch
    let fwd = (sb - sa).rem_euclid(per);
    let off = |span: f64| stretch.iter().map(|(d, q)| at(sa + span * d / len).distance(*q)).sum::<f64>();
    let span = if off(fwd) <= off(fwd - per) { fwd } else { fwd - per };
    Some(move |d: f64| at(sa + span * (d / len).clamp(0.0, 1.0)))
}

/// The last `len` mm of the path.
fn tail(pts: &[P2], len: f64) -> &[P2] {
    let mut acc = 0.0;
    for i in (1..pts.len()).rev() {
        acc += DVec2::from(pts[i - 1]).distance(DVec2::from(pts[i]));
        if acc >= len {
            return &pts[i - 1..];
        }
    }
    pts
}

/// How far along the path the tool meets material straight ahead of it (rather than beside it, as a side cut
/// a stepover wide does): pushing head-on into a face engages the whole cutter.
/// Only the first `within` mm of the path count.
fn head_on(pts: &[P2], material: &Shapes, r: f64, within: f64) -> f64 {
    let mut len = 0.0;
    let mut along = 0.0;
    for w in pts.windows(2) {
        if along > within {
            break;
        }
        along += DVec2::from(w[0]).distance(DVec2::from(w[1]));
        let (a, b) = (DVec2::from(w[0]), DVec2::from(w[1]));
        let d = b - a;
        let l = d.length();
        if l < 1e-9 {
            continue;
        }
        // straight ahead, or 30° either side of it: a side cut a stepover wide has nothing there
        let t = d / l;
        if [0.0f64, 0.5236, -0.5236].iter().any(|a| inside(material, (b + t.rotate(DVec2::from_angle(*a)) * (0.9 * r)).to_array())) {
            len += l;
        }
    }
    len
}

/// The nearest point of the shapes' outlines to `p`.
fn nearest_on(s: &Shapes, p: DVec2) -> Option<DVec2> {
    let mut best: Option<(f64, DVec2)> = None;
    for c in s.iter().flat_map(|sh| sh.iter()) {
        let n = c.len();
        for i in 0..n {
            let (a, b) = (DVec2::from(c[i]), DVec2::from(c[(i + 1) % n]));
            let ab = b - a;
            let t = if ab.length_squared() > 1e-18 { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
            let q = a + ab * t;
            let d = q.distance_squared(p);
            if best.map(|b| d < b.0).unwrap_or(true) {
                best = Some((d, q));
            }
        }
    }
    best.map(|b| b.1)
}

/// The point deepest inside the shapes (roughly), and how far it is from their outline.
fn roomiest(s: &Shapes, r: f64) -> Option<(DVec2, f64)> {
    if shapes_area(s) < 1e-3 {
        return None;
    }
    let d = (0.1 * r).max(0.02);
    let mut cur = s.clone();
    let mut depth = 0.0;
    loop {
        let next = offset(&cur, -d);
        if shapes_area(&next) < 1e-6 {
            break;
        }
        cur = next;
        depth += d;
    }
    let c = cur.first()?.first()?;
    let m = c.iter().fold(DVec2::ZERO, |acc, q| acc + DVec2::from(*q)) / c.len() as f64;
    let p = if inside(&cur, m.to_array()) { m } else { DVec2::from(c[0]) };
    Some((p, depth))
}

/// The feed for the centre at `b` on a path a → b → c: on a tight curve with the cut on its outside (rings growing
/// out of a hole or a helix) the cutting edge runs (R + r) / R times faster than the centre, so the chip and
/// the power go up as much: slow the centre down so the edge keeps to `feed` (arc feed compensation). Only
/// curves tighter than half a tool radius (the edge 3× and more: spirals out of a hole); the rings'
/// corners, rounded to the tool radius, are mostly in air, and slowing them costs time for nothing.
fn arc_feed(feed: f64, a: DVec2, b: DVec2, c: DVec2, r: f64) -> f64 {
    let (u, v) = (b - a, c - b);
    let (lu, lv) = (u.length(), v.length());
    if lu < 1e-9 || lv < 1e-9 {
        return feed;
    }
    // radius of the circle through a, b, c
    let cross = u.perp_dot(v);
    let chord = (c - a).length();
    if cross.abs() < 1e-12 || chord < 1e-9 {
        return feed;
    }
    let big_r = lu * lv * chord / (2.0 * cross.abs());
    if big_r > 0.5 * r {
        return feed;
    }
    feed * (big_r / (big_r + r)).max(0.15)
}

/// The share of the spindle's power (at the tool's speed) the cut may take at its heaviest (where a pass meets
/// a wall); `AREA_SHARE` of it along the passes' sides.
const POWER_SHARE: f64 = 0.7;

/// The share of the tool's cut area (`cam_sim::area_limit`) a pass takes along its side at most: where a pass
/// meets a wall it takes two to three times as much for a moment, so the side cut keeps well under the limit.
const AREA_SHARE: f64 = 0.4;

/// The share of the widest cut the tool takes at a level's depth (`cam_sim::allowed_depth`) a pass takes along
/// its side at most: a pass that starts or ends against a wall, or crosses a narrow channel, takes about three
/// times its stepover for a moment.
const WIDTH_SHARE: f64 = 0.25;

/// The widest radial engagement at which an end mill of diameter `d` may cut `depth` deep.
fn widest(d: f64, depth: f64) -> f64 {
    let (mut lo, mut hi) = (0.0, d);
    if crate::cam_sim::allowed_depth(d, hi) >= depth {
        return d;
    }
    for _ in 0..40 {
        let m = 0.5 * (lo + hi);
        if crate::cam_sim::allowed_depth(d, m) >= depth {
            lo = m;
        } else {
            hi = m;
        }
    }
    lo
}

/// The stepover at which a cut `depth` deep at `feed` takes `target` W at the shaft (the cutting model), at
/// most `max_area` / depth wide and `widest`, and within what the tool takes (force, bend, chip).
fn power_step(ctx: &Ctx, t: &Tool, depth: f64, feed: f64, target: f64, max_width: f64, widest: f64) -> f64 {
    let rpm = ctx.cam.spindle_rpm(t);
    let m = crate::cam_cutting::material(&ctx.cam.stock.material);
    let g = crate::cam_cutting::tool_geom(t);
    let (lim, _) = ctx.cam.machine.cut_limits();
    let hi = max_width.min(widest).min(t.diameter).max(0.02 * t.diameter);
    let ok = |w: f64| {
        let c = crate::cam_cutting::cut(&g, &m, w, depth, feed, rpm, w * depth);
        let u = crate::cam_cutting::use_of(&c, &g, &lim, rpm, false);
        c.power <= target && u.worst <= 1.0
    };
    let (mut lo, mut hi) = (0.02 * t.diameter, hi);
    if ok(hi) {
        return hi;
    }
    if !ok(lo) {
        return lo;
    }
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if ok(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

pub(crate) fn adaptive(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let t = pb.tool;
    let r = t.radius();
    let (lo, hi) = ctx.stock;
    let stock = ctx.stock_in.as_ref();
    let mut top = ctx.wz(op.top).unwrap_or(hi.z);
    if let Some(m) = stock {
        top = top.min(m.max_top());
    }
    let bottom = ctx.wz(op.bottom).unwrap_or(ctx.geo.part.lo.z + op.leave_floor);
    let step = stepover(op, t);
    // the passes take a cut `step` wide: with chip thinning the feed rises to keep the chip as thick as the feed per tooth
    let thin = if op.chip_thinning { crate::cam::chip_thinning(step / t.diameter.max(1e-9)) } else { 1.0 };
    let feed = pb.feed * thin;
    // links through cleared ground (`Level::link_clear`: only over centres already cleared at the level): as fast
    // as the machine feeds
    let fast = match ctx.cam.machine.max_feed {
        m if m > 0.0 => m.max(feed),
        _ => feed * 1.5,
    };
    let helix = if op.trochoid > 0.0 { op.trochoid } else { 0.4 * t.diameter };
    // centres reach a little beyond the stock, so the tool starts beside it
    let bound = offset(&rect(lo, hi), r + step + 1.0);
    // flat faces a hair apart (a faceted surface) make one level, at the lowest of them: each would otherwise
    // take a whole level of passes for a few hundredths of a millimetre
    let mut floors: Vec<f64> = ctx.geo.floors().iter().map(|f| f + op.leave_floor).collect();
    floors.sort_by(|a, b| a.total_cmp(b));
    let mut kept: Vec<f64> = vec![];
    for f in floors {
        if kept.last().map(|k| f - k > 0.5 * op.leave_floor.max(0.1)).unwrap_or(true) {
            kept.push(f);
        }
    }
    let area_max = crate::cam_sim::area_limit(t.diameter, &t.material);
    // the cut area (mm² per mm) at which the spindle gives POWER_SHARE of the power it has at this speed, at this
    // feed (the physical model, cam_cutting): on a small spindle that is what limits a cut, not the tool
    let area_power = {
        let (lim, _) = ctx.cam.machine.cut_limits();
        let rpm = ctx.cam.spindle_rpm(t);
        let m = crate::cam_cutting::material(&ctx.cam.stock.material);
        let avail = lim.available(rpm);
        // the chip at the full stepover sets the specific cutting force (a thinner chip costs more per mm³)
        let fz = feed / (rpm.max(1.0) * t.flutes.max(1) as f64);
        let phi = (1.0 - 2.0 * step.min(t.diameter) / t.diameter.max(1e-9)).clamp(-1.0, 1.0).acos();
        let hm = if phi > 1e-9 { fz * (1.0 - phi.cos()) / phi } else { fz };
        let kc = m.kc1 * hm.max(0.005).powf(-m.mc);
        if rpm > 0.0 && feed > 0.0 { POWER_SHARE * avail * 60_000.0 / (feed * kc) } else { f64::INFINITY }
    };
    let mut narrowed = f64::INFINITY;
    let mut z_prev = top;
    let mut any = false;
    // how high the part (with its allowance) and the stock left stand under the tool along a move; above the
    // level before, this op has taken everything else
    let part_drop = Dropper::new(ctx.geo.part, t, op.leave.max(0.0));
    let hop = |a: DVec2, b: DVec2| -> f64 {
        let n = ((a.distance(b) / (0.5 * r).max(0.2)).ceil() as usize).max(1);
        let mut sc = Scratch::default();
        let mut h = f64::NEG_INFINITY;
        for k in 0..=n {
            let p = a.lerp(b, k as f64 / n as f64);
            let z = part_drop.drop(p, &mut sc).0;
            if z.is_finite() {
                h = h.max(z);
            }
            if let Some(m) = stock {
                h = h.max(m.top_near(p.x, p.y, r) as f64);
            }
        }
        h
    };
    for z in levels(top, bottom, stepdown(op, t), &kept) {
        // the floors less than leave_floor below this level stay covered
        let shadow = ctx.geo.shadow(z - op.leave_floor);
        // (without hairline slits: the part's outline can have cracks between its triangles that the offset keeps)
        let a = opened(&difference(&bound, &offset(&shadow, r + op.leave)), 0.1);
        let material = match stock {
            // (with what hangs over the level: stock whose underside is above it but below the level before, which
            // the flutes take with their side as they pass under it. An earlier setup's cut from the other side
            // leaves such a rim: round the phone stand's plinth, 2.7 mm of it, 4 mm wide at the corners)
            Some(m) => leftover_under(ctx, m, z, z_prev, op.leave),
            None if z >= lo.z - 1e-6 && z < hi.z - 1e-6 => difference(&rect(lo, hi), &offset(&shadow, op.leave.max(0.0))),
            None => vec![],
        };
        if shapes_area(&material) < 1e-3 || shapes_area(&intersect(&a, &offset(&material, r - GAP))) < 1e-3 {
            z_prev = z;
            continue;
        }
        any = true;
        // a deep level at the full stepover can take more than the tool's cut area: narrow the stepover there
        let deep = z_prev - z;
        let level_step = match crate::cam_paths::power_budget(ctx.cam, op, t).map(|b| b.0) {
            // from the power budget: the stepover whose steady cut takes the target at this depth and feed, within
            // what the tool takes (its whole cut area and width: the corners are held by the feed, not here)
            Some(target) if deep > 1e-6 => power_step(ctx, t, deep, feed, target, area_max.min(area_power / AREA_SHARE) / deep, widest(t.diameter, deep)),
            _ if deep > 1e-6 => step.min(AREA_SHARE * area_max.min(area_power) / deep).min(WIDTH_SHARE * widest(t.diameter, deep)),
            _ => step,
        };
        if op.power_target.is_none() && level_step < step - 1e-3 {
            narrowed = narrowed.min(level_step);
        }
        let slack = stock.map(|m| 2.5 * m.cell).unwrap_or(0.0);
        let mut lv = Level { pb: &mut *pb, z, z_from: z_prev, r, step: level_step, feed, fast, climb: op.climb, helix, slack, blocking: Default::default(), hop: Some(&hop) };
        lv.clear(&a, &material, &vec![]);
        z_prev = z;
    }
    if !any {
        tp.warnings.push("nothing to clear".into());
    }
    if narrowed.is_finite() {
        tp.warnings.push(format!(
            "the stepover is cut to {} mm ({:.0} % of Ø) on the deepest levels, to keep the cut within what the tool takes; a smaller stepdown keeps the full stepover",
            crate::cam::fmt_mm(narrowed),
            narrowed / t.diameter * 100.0
        ));
    }
}

pub(crate) fn intersect(a: &Shapes, b: &Shapes) -> Shapes {
    crate::cam_paths::intersection(a, b)
}

/// Stock left at height z that isn't part (nor the `leave` on it), without slivers a couple of cells thin.
pub(crate) fn leftover(ctx: &Ctx, m: &StockMap, z: f64, leave: f64) -> Shapes {
    leftover_under(ctx, m, z, z, leave)
}

/// The same, with the stock that starts above z but below `z_top` (it hangs over the level: a tool at z whose
/// flutes reach up to `z_top` cuts it too).
pub(crate) fn leftover_under(ctx: &Ctx, m: &StockMap, z: f64, z_top: f64, leave: f64) -> Shapes {
    let c = m.cell * 1.5;
    let x = difference(&material_under(m, z, z_top), &offset(&ctx.geo.shadow(z), leave.max(0.0) + c));
    offset(&offset(&x, -c), c)
}

/// Outline of the stock that is left at height z (more than REST_TOL above it).
pub(crate) fn material(m: &StockMap, z: f64) -> Shapes {
    material_under(m, z, z)
}

/// Outline of the stock that is left above z and starts below `z_top`.
pub(crate) fn material_under(m: &StockMap, z: f64, z_top: f64) -> Shapes {
    // a grid one cell bigger all round, so every outline closes
    let (nx, ny) = (m.nx + 2, m.ny + 2);
    let mut v = vec![-1.0; nx * ny];
    for j in 0..m.ny {
        for i in 0..m.nx {
            let k = j * m.nx + i;
            if m.hi[k] > m.lo[k] && (m.lo[k] as f64) < z_top.max(z) {
                v[(j + 1) * nx + i + 1] = m.hi[k] as f64 - z;
            }
        }
    }
    let g = Grid { x0: m.x0 - m.cell, y0: m.y0 - m.cell, cell: m.cell, nx, ny, v };
    let loops: Vec<Contour> = iso(&g, REST_TOL, None)
        .into_iter()
        .map(|l| {
            let mut p: Vec<DVec3> = l.iter().map(|q| q.extend(0.0)).collect();
            p.push(p[0]);
            let mut s = simplify3(&p, m.cell * 0.25);
            s.pop();
            s.into_iter().map(|q| [q.x, q.y]).collect::<Contour>()
        })
        .filter(|c: &Contour| c.len() >= 3)
        .collect();
    union_all(&loops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::{Cam, OpKind};
    use crate::cam_paths::{generate, stock_box, Move};
    use crate::cam_sim::{EventKind, Sim};

    /// A 60 × 40 × 10 plate with a dome of radius 15 on top, as a height field on a 0.5 mm grid.
    fn dome() -> Part {
        let h = |x: f64, y: f64| {
            let d2 = (x - 30.0).powi(2) + (y - 20.0).powi(2);
            10.0 + (225.0 - d2).max(0.0).sqrt()
        };
        let mut tris = vec![];
        let s = 0.5;
        for j in 0..80 {
            for i in 0..120 {
                let (x, y) = (i as f64 * s, j as f64 * s);
                let p = |dx: f64, dy: f64| DVec3::new(x + dx, y + dy, h(x + dx, y + dy));
                tris.push([p(0.0, 0.0), p(s, 0.0), p(s, s)]);
                tris.push([p(0.0, 0.0), p(s, s), p(0.0, s)]);
            }
        }
        tris.push([DVec3::ZERO, DVec3::new(0.0, 40.0, 0.0), DVec3::new(60.0, 0.0, 0.0)]);
        Part::new(&tris, &[], "top")
    }

    /// A closed 40 × 30 × 10 box.
    fn block() -> Part {
        let v = |x: f64, y: f64, z: f64| DVec3::new(x * 40.0, y * 30.0, z * 10.0);
        let q = |a: DVec3, b: DVec3, c: DVec3, d: DVec3| [[a, b, c], [a, c, d]];
        let mut t = vec![];
        t.extend(q(v(0., 0., 1.), v(1., 0., 1.), v(1., 1., 1.), v(0., 1., 1.)));
        t.extend(q(v(0., 0., 0.), v(0., 1., 0.), v(1., 1., 0.), v(1., 0., 0.)));
        t.extend(q(v(0., 0., 0.), v(1., 0., 0.), v(1., 0., 1.), v(0., 0., 1.)));
        t.extend(q(v(1., 0., 0.), v(1., 1., 0.), v(1., 1., 1.), v(1., 0., 1.)));
        t.extend(q(v(1., 1., 0.), v(0., 1., 0.), v(0., 1., 1.), v(1., 1., 1.)));
        t.extend(q(v(0., 1., 0.), v(0., 0., 0.), v(0., 0., 1.), v(0., 1., 1.)));
        Part::new(&t, &[], "top")
    }

    /// A closed 40 × 30 × 10 block with a 20 × 10 × 5 pocket in the middle of its top.
    fn pocket_block() -> Part {
        let v = DVec3::new;
        let q = |a: DVec3, b: DVec3, c: DVec3, d: DVec3| [[a, b, c], [a, c, d]];
        let mut t = vec![];
        // top around the pocket (10..30 × 10..20)
        t.extend(q(v(0., 0., 10.), v(40., 0., 10.), v(40., 10., 10.), v(0., 10., 10.)));
        t.extend(q(v(0., 20., 10.), v(40., 20., 10.), v(40., 30., 10.), v(0., 30., 10.)));
        t.extend(q(v(0., 10., 10.), v(10., 10., 10.), v(10., 20., 10.), v(0., 20., 10.)));
        t.extend(q(v(30., 10., 10.), v(40., 10., 10.), v(40., 20., 10.), v(30., 20., 10.)));
        // pocket floor and walls (facing into the pocket)
        t.extend(q(v(10., 10., 5.), v(30., 10., 5.), v(30., 20., 5.), v(10., 20., 5.)));
        t.extend(q(v(10., 10., 5.), v(10., 10., 10.), v(30., 10., 10.), v(30., 10., 5.)));
        t.extend(q(v(30., 20., 5.), v(30., 20., 10.), v(10., 20., 10.), v(10., 20., 5.)));
        t.extend(q(v(10., 20., 5.), v(10., 20., 10.), v(10., 10., 10.), v(10., 10., 5.)));
        t.extend(q(v(30., 10., 5.), v(30., 10., 10.), v(30., 20., 10.), v(30., 20., 5.)));
        // outside and bottom
        t.extend(q(v(0., 0., 0.), v(0., 30., 0.), v(40., 30., 0.), v(40., 0., 0.)));
        t.extend(q(v(0., 0., 0.), v(40., 0., 0.), v(40., 0., 10.), v(0., 0., 10.)));
        t.extend(q(v(40., 0., 0.), v(40., 30., 0.), v(40., 30., 10.), v(40., 0., 10.)));
        t.extend(q(v(40., 30., 0.), v(0., 30., 0.), v(0., 30., 10.), v(40., 30., 10.)));
        t.extend(q(v(0., 30., 0.), v(0., 0., 0.), v(0., 0., 10.), v(0., 30., 10.)));
        Part::new(&t, &[], "top")
    }

    #[test]
    fn ball_stays_off_pocket_walls() {
        let part = pocket_block();
        let mut c = Cam::default();
        let t = tool(&mut c, ToolKind::Ball, 6.0);
        let mut s = Scratch::default();
        let d = Dropper::new(&part, c.tool(t).unwrap(), 0.0);
        // 2 mm in from the wall at y = 10: the ball sits on the wall's top edge
        let z = d.drop(DVec2::new(20.0, 12.0), &mut s).0;
        assert!((z - (10.0 - (3.0 - (9.0f64 - 4.0).sqrt()))).abs() < 1e-6, "{z}");
        let id = c.add_op(OpKind::Parallel, t);
        c.op_mut(id).unwrap().slope_max = 50.0;
        let (s, _) = sim(&c, &part);
        assert!(!s.events.iter().any(|e| e.kind == EventKind::Gouge), "{:?}", s.events.iter().take(3).collect::<Vec<_>>());
    }

    fn tool(c: &mut Cam, kind: ToolKind, d: f64) -> u64 {
        let mut t = crate::cam::presets().into_iter().find(|t| t.kind == kind && t.diameter == d).unwrap();
        t.flute_length = t.flute_length.max(30.0);
        t.stickout = t.stickout.max(40.0);
        c.add_tool(t)
    }

    fn sim(c: &Cam, part: &Part) -> (Sim, Vec<Toolpath>) {
        let paths = generate(c, part);
        let stock = stock_box(c, part);
        let mut s = Sim::new(c, part, stock, &paths, 400);
        s.run_to(usize::MAX, usize::MAX);
        (s, paths)
    }

    #[test]
    fn finishing_after_deep_roughing_never_rapids_into_its_steps() {
        let part = dome();
        let mut c = Cam::default();
        let flat = tool(&mut c, ToolKind::Flat, 6.0);
        let ball = tool(&mut c, ToolKind::Ball, 6.0);
        let a = c.add_op(OpKind::Adaptive, flat);
        c.op_mut(a).unwrap().stepdown = Some(9.0);
        c.add_op(OpKind::Parallel, ball);
        let (s, paths) = sim(&c, &part);
        let bad: Vec<_> = s.events.iter().filter(|e| e.kind == EventKind::Rapid).take(3).collect();
        assert!(bad.is_empty(), "{bad:?}");
        // without the stock that is left, finishing comes down over the stock's top
        let top = stock_box(&c, &part).1.z;
        let mv = &paths[1].moves;
        let first = mv.iter().position(|m| matches!(m, Move::Feed(..))).unwrap();
        assert!(matches!(mv[first - 1], Move::Rapid(p) if p[2] >= top - 1e-6), "{:?}", mv[first - 1]);
    }

    #[test]
    fn drop_cutter_shapes() {
        let part = dome();
        let mut c = Cam::default();
        let ball = tool(&mut c, ToolKind::Ball, 6.0);
        let flat = tool(&mut c, ToolKind::Flat, 6.0);
        let mut s = Scratch::default();
        let db = Dropper::new(&part, c.tool(ball).unwrap(), 0.0);
        let df = Dropper::new(&part, c.tool(flat).unwrap(), 0.0);
        // on the plate
        assert!((db.drop(DVec2::new(5.0, 5.0), &mut s).0 - 10.0).abs() < 1e-6);
        assert!((df.drop(DVec2::new(5.0, 5.0), &mut s).0 - 10.0).abs() < 1e-6);
        // over the top of the dome: tips at the apex
        assert!((db.drop(DVec2::new(30.0, 20.0), &mut s).0 - 25.0).abs() < 0.02);
        // a ball rolling over a sphere: its centre stays 18 from the sphere's centre
        for x in [32.0, 36.0, 40.0, 42.0] {
            let z = db.drop(DVec2::new(x, 20.0), &mut s).0;
            let centre = DVec3::new(x, 20.0, z + 3.0);
            let d = centre.distance(DVec3::new(30.0, 20.0, 10.0));
            assert!((d - 18.0).abs() < 0.05, "x {x}: {d}");
        }
        // a flat end mill touches the sphere with its rim
        let z = df.drop(DVec2::new(36.0, 20.0), &mut s).0;
        let rim = 10.0 + (225.0f64 - 9.0).sqrt();
        assert!((z - rim).abs() < 0.05, "{z} vs {rim}");
    }

    #[test]
    fn finishing_passes_do_not_gouge() {
        let part = dome();
        for kind in [OpKind::Parallel, OpKind::Waterline, OpKind::Scallop] {
            let mut c = Cam::default();
            c.stock.top = 0.0;
            let t = tool(&mut c, ToolKind::Ball, 6.0);
            let id = c.add_op(kind, t);
            if kind == OpKind::Parallel {
                c.op_mut(id).unwrap().angle = 30.0;
            }
            if kind == OpKind::Waterline {
                c.op_mut(id).unwrap().stepdown = Some(1.0);
            }
            let (s, paths) = sim(&c, &part);
            let tp = &paths[0];
            assert!(tp.warnings.is_empty(), "{kind:?}: {:?}", tp.warnings);
            assert!(tp.moves.len() > 50, "{kind:?}");
            assert!(!s.events.iter().any(|e| e.kind == EventKind::Gouge), "{kind:?}: {:?}", s.events.iter().take(3).collect::<Vec<_>>());
            // the passes reach down the dome's side
            let low_on_dome = tp.moves.iter().any(|m| matches!(m, Move::Feed(p, _) if p[2] > 11.0 && p[2] < 15.0));
            assert!(low_on_dome, "{kind:?}");
        }
    }

    #[test]
    fn slope_limits_split_the_work() {
        let part = dome();
        let mut c = Cam::default();
        let t = tool(&mut c, ToolKind::Ball, 6.0);
        let id = c.add_op(OpKind::Parallel, t);
        c.op_mut(id).unwrap().slope_max = 30.0;
        let tp = &generate(&c, &part)[0];
        // nothing steeper than 30° is cut: no feed point on the dome's steep flank
        for m in &tp.moves {
            if let Move::Feed(p, _) = m {
                let d = (p[0] - 30.0).hypot(p[1] - 20.0);
                assert!(!(d > 14.0 && d < 16.5 && p[2] > 12.0), "steep point {p:?}");
            }
        }
    }

    /// A 74 × 74 × 15 block with R10 corners: the phone stand's plinth.
    fn plinth() -> Part {
        let (h, rc) = (37.0, 10.0);
        let mut ring = vec![];
        for (cx, cy, a0) in [(h - rc, h - rc, 0.0), (rc - h, h - rc, 90.0), (rc - h, rc - h, 180.0), (h - rc, rc - h, 270.0)] {
            for k in 0..=12 {
                let a = (a0 + k as f64 * 7.5f64).to_radians();
                ring.push(DVec2::new(cx + rc * a.cos(), cy + rc * a.sin()));
            }
        }
        let z = 15.0;
        let mut t = vec![];
        let n = ring.len();
        for k in 0..n {
            let (a, b) = (ring[k], ring[(k + 1) % n]);
            t.push([DVec3::new(0.0, 0.0, z), a.extend(z), b.extend(z)]);
            t.push([DVec3::ZERO, b.extend(0.0), a.extend(0.0)]);
            t.push([a.extend(0.0), b.extend(0.0), b.extend(z)]);
            t.push([a.extend(0.0), b.extend(z), a.extend(z)]);
        }
        Part::new(&t, &[], "top")
    }

    /// What overloaded a 1/4" end mill on the DMC2: one contour 4 mm deep round the R10 corners of a plinth in
    /// square stock 1 mm bigger all round (about 5.6 mm of stock at each corner). The contour now takes the
    /// corners down in extra passes no wider than its engagement limit, so the load check passes it, as it
    /// passes adaptive clearing of the same.
    #[test]
    fn load_check_passes_a_contour_into_square_corners_and_adaptive() {
        let part = plinth();
        let run = |kind: OpKind, f: &dyn Fn(&mut crate::cam::Operation)| {
            let mut c = Cam::default();
            c.stock.side = 1.0;
            c.stock.top = 0.0;
            c.origin = crate::cam::Origin::StockBottomCenter;
            let t = tool(&mut c, ToolKind::Flat, 6.0);
            // as the DMC2 ran it: 3 flutes, 24000 rpm, 2400 mm/min, 24 out of the holder
            let tl = c.tools.iter_mut().find(|x| x.id == t).unwrap();
            (tl.flutes, tl.rpm, tl.feed, tl.stickout, tl.material) = (3, 24000.0, 2400.0, 24.0, "carbide".into());
            let id = c.add_op(kind, t);
            let op = c.ops.iter_mut().find(|o| o.id == id).unwrap();
            op.bottom = Some(5.0);
            op.leave = 0.2;
            f(op);
            let (s, paths) = sim(&c, &part);
            assert!(paths[0].moves.len() > 10, "{kind:?}: {:?} {:?}", paths[0].warnings, paths[0].moves.len());
            s
        };
        let s = run(OpKind::Contour, &|op| op.stepdown = Some(4.0));
        let over: Vec<_> = s.events.iter().filter(|e| e.kind == EventKind::Overload).collect();
        assert!(over.is_empty(), "{:?} {:?}", s.loads, over.iter().take(5).collect::<Vec<_>>());
        let l = s.loads[0];
        assert!(l.ratio < 1.0 && l.depth > 3.5, "{l:?}");
        // the plain single pass (no engagement limit) is what overloaded
        let s = run(OpKind::Contour, &|op| {
            op.stepdown = Some(4.0);
            op.max_engagement = Some(1.0);
            op.stepover = Some(1.0);
        });
        let over: Vec<_> = s.events.iter().filter(|e| e.kind == EventKind::Overload).collect();
        assert!(!over.is_empty(), "{:?}", s.loads);
        // at the corners, not along the straight sides (1 mm of stock)
        let (lo, hi) = stock_box(&Cam { origin: crate::cam::Origin::StockBottomCenter, ..Default::default() }, &part);
        let mid = (lo + hi) / 2.0;
        assert!(over.iter().all(|e| (e.at[0] - mid.x).abs() > 25.0 && (e.at[1] - mid.y).abs() > 25.0), "{:?}", over.iter().take(5).collect::<Vec<_>>());
        let l = s.loads[0];
        assert!(l.width > 4.0 && l.depth > 3.5 && l.ratio > 1.0 && l.uses.power > 1.0 && l.geometric > 1.0, "{l:?}");

        let s = run(OpKind::Adaptive, &|op| {
            op.stepdown = Some(5.0);
            op.stepover = Some(0.12);
        });
        assert!(!s.events.iter().any(|e| e.kind == EventKind::Overload), "{:?}", s.loads);
        assert!(s.loads[0].ratio < 1.0 && s.loads[0].geometric < 0.9, "{:?}", s.loads);
    }

    /// The phone stand's op 2 rough (rcnc jobs/phone-stand): face, then a rest adaptive with T24 from the
    /// raw 76 × 76 top band down to the groove floor. Needs the job's STL (RCAD_PHONE_STAND, the part in op 2
    /// coordinates); prints each op's load, air and time. RCAD_PS_STEP sets the adaptive's step-down.
    #[test]
    #[ignore]
    fn phone_stand_op2_rough() {
        let path = std::env::var("RCAD_PHONE_STAND").unwrap_or("../rcnc/jobs/phone-stand/phone-stand.stl".into());
        let Ok(d) = std::fs::read(&path) else {
            eprintln!("no {path}: set RCAD_PHONE_STAND to the phone stand's STL");
            return;
        };
        let n = u32::from_le_bytes(d[80..84].try_into().unwrap()) as usize;
        let f = |o: usize| f32::from_le_bytes(d[o..o + 4].try_into().unwrap()) as f64;
        let tris: Vec<[DVec3; 3]> = (0..n)
            .map(|i| {
                let b = 84 + 50 * i + 12;
                [0, 1, 2].map(|k| DVec3::new(f(b + 12 * k), f(b + 12 * k + 4), f(b + 12 * k + 8)))
            })
            .collect();
        let part = Part::new(&tris, &[], "top");
        let step: f64 = std::env::var("RCAD_PS_STEP").ok().and_then(|s| s.parse().ok()).unwrap_or(10.5);
        let mut c = Cam::default();
        c.stock.side = 1.0;
        c.stock.top = 4.5;
        c.stock.material = "6061-T6".into();
        c.origin = crate::cam::Origin::StockBottomCenter;
        let mut t = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && (t.diameter - 6.35).abs() < 0.01).unwrap_or_else(|| {
            let mut t = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat).unwrap();
            t.diameter = 6.35;
            t
        });
        t.number = 24;
        t.flutes = 3;
        t.flute_length = 19.05;
        t.stickout = 24.0;
        t.material = "carbide".into();
        t.rpm = 24000.0;
        t.feed = 2400.0;
        t.plunge_feed = 330.0;
        let t = c.add_tool(t);
        let face = c.add_op(OpKind::Face, t);
        {
            let o = c.op_mut(face).unwrap();
            o.stepdown = Some(2.0);
            o.stepover = Some(0.4);
        }
        let a = c.add_op(OpKind::Adaptive, t);
        {
            let o = c.op_mut(a).unwrap();
            o.stepdown = Some(step);
            o.stepover = Some(0.22);
            o.leave = 0.3;
            o.leave_floor = 0.2;
            o.bottom = Some(10.2);
            o.chip_thinning = true;
            o.rest = true;
        }
        let t0 = std::time::Instant::now();
        let (s, paths) = sim(&c, &part);
        eprintln!("generate + sim {:.1} s", t0.elapsed().as_secs_f64());
        for p in &paths {
            let l = s.loads.iter().find(|l| l.op == p.op);
            let air = s.air.iter().find(|x| x.op == p.op);
            eprintln!(
                "op {} time {:.1} min, cut {:.0} mm, load {:?}, air {:?}, warnings {:?}",
                p.op,
                p.time / 60.0,
                p.cut_length,
                l.map(|l| (format!("{:.0}%", l.ratio * 100.0), l.width, l.depth, l.at)),
                air.map(|a| (format!("{:.0}% air", 100.0 * a.air_s / a.feed_s), format!("{:.0}% light", 100.0 * a.light_s / a.feed_s))),
                p.warnings
            );
        }
        let events: Vec<_> = s.events.iter().filter(|e| matches!(e.kind, EventKind::Overload | EventKind::Gouge)).collect();
        for e in events.iter().take(8) {
            eprintln!("{:?} op {} ({:.2}) at {:?}", e.kind, e.op, e.depth, e.at);
        }
        assert!(events.is_empty(), "{} overloads or gouges", events.len());
        // the rough: within what the tool takes, under 30 % of its feed time in air, quicker than the 30 min the
        // cycloidal loops took at a third of the step-down
        let rough = paths.iter().find(|p| p.op == a).unwrap();
        let l = s.loads.iter().find(|l| l.op == a).unwrap();
        let air = s.air.iter().find(|x| x.op == a).unwrap();
        assert!(l.ratio < 1.0 && air.air_s < 0.3 * air.feed_s && rough.time < 30.0 * 60.0);
    }

    /// Clear and Pocket keep to their stepover sideways after their first ring, also at the outline's corners and
    /// along the outer edge: a square pocket's corners used to take nearly the whole tool width.
    #[test]
    fn clear_and_pocket_keep_to_the_stepover_at_corners() {
        let mut tris = vec![];
        for j in 0..40 {
            for i in 0..60 {
                let (x, y) = (i as f64, j as f64);
                let z = if (10..30).contains(&i) && (10..30).contains(&j) { 5.0 } else { 10.0 };
                let p = |dx: f64, dy: f64| DVec3::new(x + dx, y + dy, z);
                tris.push([p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0)]);
                tris.push([p(0.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)]);
            }
        }
        tris.push([DVec3::ZERO, DVec3::new(0.0, 40.0, 0.0), DVec3::new(60.0, 0.0, 0.0)]);
        let plate = Part::new(&tris, &[], "top");
        let pl = plinth();
        for kind in [OpKind::Pocket, OpKind::Clear] {
            let part = if kind == OpKind::Pocket { &plate } else { &pl };
            let mut c = Cam::default();
            c.stock.side = 6.0;
            c.stock.top = 0.0;
            let t = tool(&mut c, ToolKind::Flat, 6.0);
            let id = c.add_op(kind, t);
            let op = c.ops.iter_mut().find(|o| o.id == id).unwrap();
            op.leave = 0.2;
            op.stepdown = Some(if kind == OpKind::Pocket { 2.0 } else { 4.0 });
            let op = op.clone();
            let paths = generate(&c, part);
            let tl = c.tool(t).unwrap();
            let (r, step) = (tl.radius(), stepover(&op, tl));
            let (lo, hi) = stock_box(&c, part);
            let geo = crate::cam_paths::Geo::new(part);
            // the first level, its centre area, and the material less what the first ring (the core) takes
            let z = hi.z - op.stepdown.unwrap();
            let (region, material) = if kind == OpKind::Pocket {
                let raw: Shapes = vec![vec![vec![[10.0, 10.0], [30.0, 10.0], [30.0, 30.0], [10.0, 30.0]]]];
                (offset(&raw, -(r + op.leave)), raw)
            } else {
                let keep = offset(&geo.shadow(z), r + op.leave);
                (difference(&offset(&rect(lo, hi), r + 0.5), &keep), difference(&rect(lo, hi), &offset(&geo.shadow(z), op.leave)))
            };
            // a pocket starts from loops round its core; a clear grows in from outside the stock
            let mut core = if kind == OpKind::Pocket { region.clone() } else { vec![] };
            while !core.is_empty() {
                let next = offset(&core, -step);
                if next.is_empty() {
                    break;
                }
                core = next;
            }
            // a flat end mill can't run into a sharp inside corner without wrapping round it: leave the area
            // a tool's width from the region's corners out of the sideways check (the load check covers them)
            let mut corners = vec![];
            for c in region.iter().flat_map(|sh| sh.iter()) {
                let n = c.len();
                for i in 0..n {
                    let (a, b, d) = (DVec2::from(c[(i + n - 1) % n]), DVec2::from(c[i]), DVec2::from(c[(i + 1) % n]));
                    if (b - a).angle_to(d - b).abs() > 0.5 {
                        corners.push(vec![circle(b, 2.0 * r)]);
                    }
                }
            }
            let mut material = difference(&difference(&material, &offset(&core, r)), &corners);
            if kind == OpKind::Clear {
                // the stock's edge: the first two passes in from outside it
                material = difference(&material, &offset(&rect(lo, hi), -2.0 * step));
            }
            let moves = &paths[0].moves;
            let mut pos = DVec3::ZERO;
            let mut level = vec![];
            for m in moves {
                let q = m.end(pos);
                if q.z < z - 1e-6 {
                    break;
                }
                level.push(*m);
                pos = q;
            }
            let e = crate::cam_contour::check(&level, None, &material, hi.z, r, 0.05).unwrap();
            let (sm, _) = sim(&c, part);
            // round a part's corner close to the stock's edge, the passes that come in from outside veer in to
            // meet the band along the part a little steeply (up to about 25°): a pass takes half as much again there
            // (a hair over 1.6 × with the passes climbing, which meet that corner from the other side)
            let most = if kind == OpKind::Pocket { step + 0.15 } else { 1.65 * step };
            assert!(e.peak <= most, "{kind:?}: takes {} mm at {:?}, stepover {step}", e.peak, e.at);
            // (the engagement rule: the generic preset's feed is more than the default spindle's power takes)
            assert!(sm.loads.iter().all(|l| l.geometric < 1.0), "{kind:?}: {:?}", sm.loads);
            assert!(level.iter().any(|m| matches!(m, Move::Feed(p, _) if !inside(&offset(&core, 0.1), [p[0], p[1]]))));
        }
    }

    /// Machined surface left with roughing stock on it is reported; a finishing pass clears the report.
    #[test]
    fn unfinished_surface_is_reported() {
        let part = plinth();
        let run = |leave: f64| {
            let mut c = Cam::default();
            c.stock.side = 1.0;
            c.stock.top = 2.0;
            let t = tool(&mut c, ToolKind::Flat, 6.0);
            let id = c.add_op(OpKind::Face, t);
            c.ops.iter_mut().find(|o| o.id == id).unwrap().leave_floor = leave;
            sim(&c, &part).0.unfinished(0.1)
        };
        let u = run(0.5).expect("the face left 0.5 mm on the top");
        assert!(u.area_mm2 > 3000.0 && (u.max - 0.5).abs() < 0.05, "{u:?}");
        assert_eq!(run(0.0), None);
    }

    /// A ring growing out of a small hole: the edge runs (R + r) / R faster than the centre, so the centre is
    /// slowed to keep the edge at the feed; wide curves and straights keep the feed.
    #[test]
    fn arc_feed_keeps_the_edge_at_the_feed() {
        let on = |rad: f64, a: f64| DVec2::new(rad * a.cos(), rad * a.sin());
        let f = arc_feed(2400.0, on(1.0, 0.0), on(1.0, 0.3), on(1.0, 0.6), 3.175);
        assert!((f - 2400.0 * 1.0 / 4.175).abs() < 20.0, "{f}");
        assert_eq!(arc_feed(2400.0, on(20.0, 0.0), on(20.0, 0.05), on(20.0, 0.1), 3.175), 2400.0);
        assert_eq!(arc_feed(2400.0, DVec2::ZERO, DVec2::X, 2.0 * DVec2::X, 3.175), 2400.0);
    }

    /// One level cleared with a Ø6.35 tool, stepover 1.2: the moves, and the widest cut they take.
    fn level_engagement(a: &Shapes, material: &Shapes) -> (Vec<Move>, f64, DVec3) {
        let mut c = Cam::default();
        let t = tool(&mut c, ToolKind::Flat, 6.0);
        let r = 3.175;
        let mut pb = Pb { tool: c.tool(t).unwrap(), moves: vec![], pos: None, safe: 10.0, retract: 2.0, ramp: 0.05, feed: 2400.0, plunge: 300.0 };
        let mut lv = Level { pb: &mut pb, z: 0.0, z_from: 3.0, r, step: 1.2, feed: 2400.0, fast: 3000.0, climb: true, helix: 2.5, slack: 0.0, blocking: Default::default(), hop: None };
        lv.clear(a, material, &vec![]);
        let e = crate::cam_contour::check(&pb.moves, None, material, 3.0, r, 0.05).unwrap();
        (pb.moves, e.peak, e.at)
    }

    /// A groove the tool only just fits along (the phone stand's, 11.6 wide for a Ø6.35 tool: centres 5.3 apart):
    /// the passes across it leave the walls tangentially, like a trochoid's loops. They used to start with a
    /// stepover straight along the wall into the face (4.3 mm wide at the full depth), and, eased from the cleared
    /// ground over more than their whole length, never got out to their line, so a band counted as cleared was
    /// still standing when a link at the fast feed crossed it.
    #[test]
    fn adaptive_keeps_its_engagement_across_a_narrow_groove() {
        let slab: Shapes = vec![vec![vec![[-37.0, -7.0], [37.0, -7.0], [37.0, 7.0], [-37.0, 7.0]]]];
        let a: Shapes = vec![vec![vec![[-45.0, -2.65], [45.0, -2.65], [45.0, 2.65], [-45.0, 2.65]]]];
        let (moves, peak, at) = level_engagement(&a, &slab);
        assert!(peak <= 2.3 * 1.2, "takes {peak} mm at {at:?}");
        // (links at the fast feed included: one through a band still standing would show as a wide cut)
        // and the groove is cleared: the centre line is run from end to end
        let xs: Vec<f64> = moves.iter().filter_map(|m| if let Move::Feed(p, _) = m { (p[2] < 1e-6).then_some(p[0]) } else { None }).collect();
        let (lo, hi) = xs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| (a.min(*x), b.max(*x)));
        assert!(lo < -30.0 && hi > 30.0, "{lo} .. {hi}");
    }

    /// A bore the tool only just fits (Ø8 for a Ø6.35 tool, left by an earlier setup) is not where the clearing
    /// starts when there is open ground: rings growing out of it are arcs far tighter than the tool, each taking
    /// several stepovers. The passes from outside break through into it. Shut in on all sides, it is the start.
    #[test]
    fn adaptive_does_not_spiral_out_of_a_bore_it_can_reach_from_outside() {
        let hole: Contour = circle(DVec2::ZERO, 4.0).into_iter().rev().collect();
        let slab: Shapes = vec![vec![vec![[-15.0, -15.0], [15.0, -15.0], [15.0, 15.0], [-15.0, 15.0]], hole.clone()]];
        let open: Shapes = vec![vec![vec![[-22.0, -22.0], [22.0, -22.0], [22.0, 22.0], [-22.0, 22.0]]]];
        let (moves, _, _) = level_engagement(&open, &slab);
        // (round the bore: the stock's outer corners are other tests' business)
        let round_bore = intersect(&slab, &vec![vec![circle(DVec2::ZERO, 9.0)]]);
        let e = crate::cam_contour::check(&moves, None, &round_bore, 3.0, 3.175, 0.05).unwrap();
        assert!(e.peak <= 2.0 * 1.2, "takes {} mm at {:?}", e.peak, e.at);
        let first = moves.iter().find_map(|m| if let Move::Feed(p, _) = m { (p[2] < 1e-6).then_some(DVec2::new(p[0], p[1])) } else { None }).unwrap();
        assert!(first.length() > 15.0, "starts at {first:?}");
        // a pocket with nothing open but the bore: cleared from the bore
        let shut: Shapes = vec![vec![vec![[-11.0, -11.0], [11.0, -11.0], [11.0, 11.0], [-11.0, 11.0]]]];
        let (moves, _, _) = level_engagement(&shut, &slab);
        let first = moves.iter().find_map(|m| if let Move::Feed(p, _) = m { (p[2] < 1e-6).then_some(DVec2::new(p[0], p[1])) } else { None }).unwrap();
        assert!(first.length() < 2.0, "starts at {first:?}");
        let far = moves.iter().filter_map(|m| if let Move::Feed(p, _) = m { (p[2] < 1e-6).then_some(DVec2::new(p[0], p[1]).length()) } else { None }).fold(0.0, f64::max);
        assert!(far > 14.0, "reaches {far}");
    }

    /// Stock whose underside is above a level but below the level before hangs over it, and the flutes cut it as
    /// they pass under: it counts as the level's material (a rim an earlier setup left by cutting from the other
    /// side; the phone stand's was taken unplanned, 4 mm wide at the plinth's corners).
    #[test]
    fn stock_hanging_over_a_level_is_its_material() {
        let mut m = StockMap::block(DVec3::ZERO, DVec3::new(20.0, 20.0, 10.0), 0.5);
        for j in 0..m.ny {
            for i in 0..m.nx {
                if m.x0 + i as f64 * m.cell > 10.2 {
                    m.lo[j * m.nx + i] = 6.0;
                }
            }
        }
        let half = shapes_area(&material(&m, 5.0));
        assert!((half - 215.0).abs() < 20.0, "{half}");
        assert!((shapes_area(&material_under(&m, 5.0, 5.5)) - half).abs() < 1e-6, "the level before is below the rim");
        let all = shapes_area(&material_under(&m, 5.0, 8.0));
        assert!((all - 430.0).abs() < 30.0 && all > 1.8 * half, "{all}");
    }

    #[test]
    fn allowed_depth_and_area_by_engagement() {
        use crate::cam_sim::{allowed_depth, area_limit};
        let d = 6.35;
        assert!((allowed_depth(d, 0.5) - 2.5 * d).abs() < 1e-9, "a light step: 2.5 D");
        assert!((allowed_depth(d, d / 2.0) - 0.8 * d).abs() < 1e-9);
        assert!((allowed_depth(d, d) - 0.45 * d).abs() < 1e-9, "a full slot");
        // the test part's heaviest cut, 4.65 wide x 3 deep, cut fine on the DMC2; the plinth corner didn't
        assert!(3.0 < allowed_depth(d, 4.65) && 4.0 > allowed_depth(d, 5.0));
        assert!(area_limit(d, "carbide") > 13.9 && area_limit(d, "carbide") < 20.0);
        assert!(area_limit(6.0, "hss") < area_limit(6.0, "carbide"));
    }

    /// Two channels across a plate (as on a 2-up blank of bases): the clearing finishes one before it goes to
    /// the other, instead of going back and forth once per stepover ring, and its links never cut into material.
    #[test]
    fn adaptive_finishes_one_area_before_the_next() {
        let h = |x: f64, _y: f64| if (20.0..36.0).contains(&x) || (64.0..80.0).contains(&x) { 4.0 } else { 10.0 };
        let mut tris = vec![];
        let s = 0.5;
        for j in 0..80 {
            for i in 0..200 {
                let (x, y) = (i as f64 * s, j as f64 * s);
                let z = h(x + s / 2.0, y + s / 2.0);
                let p = |dx: f64, dy: f64| DVec3::new(x + dx, y + dy, z);
                tris.push([p(0.0, 0.0), p(s, 0.0), p(s, s)]);
                tris.push([p(0.0, 0.0), p(s, s), p(0.0, s)]);
            }
        }
        tris.push([DVec3::ZERO, DVec3::new(0.0, 40.0, 0.0), DVec3::new(100.0, 0.0, 0.0)]);
        let part = Part::new(&tris, &[], "top");
        let mut c = Cam::default();
        c.stock.side = 0.0;
        c.stock.top = 0.0;
        let t = tool(&mut c, ToolKind::Flat, 6.0);
        let id = c.add_op(OpKind::Adaptive, t);
        c.op_mut(id).unwrap().stepdown = Some(10.0);
        c.op_mut(id).unwrap().leave_floor = 0.0;
        let (s, paths) = sim(&c, &part);
        let tp = &paths[0];
        // switches between the halves while cutting (feed moves below the top)
        let mut prev = DVec3::new(0.0, 0.0, 30.0);
        let (mut side, mut switches, mut retracts) = (None, 0, 0);
        for m in &tp.moves {
            let p = m.end(prev);
            if let Move::Feed(..) = m {
                if p.z < 9.0 {
                    let now = p.x > 50.0;
                    if side.is_some_and(|s| s != now) {
                        switches += 1;
                    }
                    side = Some(now);
                }
            }
            if p.z > prev.z + 0.5 {
                retracts += 1;
            }
            prev = p;
        }
        assert!(switches <= 6, "{switches} switches between the parts, {retracts} retracts");
        // still clean: no gouges, no rapids or links into material
        assert!(s.events.iter().all(|e| e.kind == EventKind::Overload), "{:?}", s.events.iter().take(5).collect::<Vec<_>>());
    }

    #[test]
    fn adaptive_clears_within_the_load_and_little_air() {
        let part = dome();
        let mut c = Cam::default();
        c.stock.side = 3.0;
        c.stock.top = 1.0;
        let t = tool(&mut c, ToolKind::Flat, 6.0);
        let id = c.add_op(OpKind::Adaptive, t);
        assert_eq!(c.op(id).unwrap().stepover, Some(0.1));
        let (s, paths) = sim(&c, &part);
        let tp = &paths[0];
        // the preset's stepdown is deep enough that the stepover is narrowed on the deepest levels
        assert!(tp.warnings.iter().all(|w| w.starts_with("the stepover is cut to")), "{:?}", tp.warnings);
        // (the preset's feed is more than the default 1.5 kW spindle takes: that's the load check's business,
        // not this test's)
        assert!(s.events.iter().all(|e| e.kind == EventKind::Overload), "{:?}", s.events.iter().take(5).collect::<Vec<_>>());
        // the stock around the dome comes down to near the plate (leaving 0.2 on the floor)
        let stock = stock_box(&c, &part);
        let mut high = 0;
        let mut n = 0;
        for j in 0..s.ny {
            for i in 0..s.nx {
                let (x, y) = (s.x0 + i as f64 * s.cell, s.y0 + j as f64 * s.cell);
                let on_plate = (x - 30.0).hypot(y - 20.0) > 22.0 && x > 0.0 && x < 60.0 && y > 0.0 && y < 40.0;
                if on_plate {
                    n += 1;
                    if s.stock[j * s.nx + i] as f64 > 10.0 + 0.2 + 0.05 {
                        high += 1;
                    }
                }
            }
        }
        assert!(n > 100 && (high as f64) < n as f64 * 0.02, "{high} of {n} cells left high (stock top {})", stock.1.z);
        // cycloidal: the path turns all the way round, again and again
        // within what the tool takes everywhere, and little of the feed time in air
        let l = s.loads.iter().find(|l| l.op == tp.op).unwrap();
        assert!(l.geometric < 1.0, "{l:?}");
        let air = s.air.iter().find(|a| a.op == tp.op).unwrap();
        assert!(air.air_s < 0.3 * air.feed_s, "{:.0} % in air", 100.0 * air.air_s / air.feed_s);
    }

    #[test]
    fn deburr_follows_the_edges() {
        let part = block();
        let edges = sharp_edges(&part, 30.0);
        let total: f64 = edges.iter().map(|(p, _, _)| p.windows(2).map(|w| w[0].distance(w[1])).sum::<f64>()).sum();
        // the four top edges, in one loop (vertical edges can't be reached from above)
        assert!((total - 140.0).abs() < 1e-6, "{total}");
        assert_eq!(edges.len(), 1);
        let mut c = Cam::default();
        let t = tool(&mut c, ToolKind::Chamfer, 6.0);
        let id = c.add_op(OpKind::Deburr, t);
        let (s, paths) = sim(&c, &part);
        assert!(paths[0].warnings.is_empty(), "{:?}", paths[0].warnings);
        assert!(s.events.is_empty(), "{:?}", s.events.iter().take(3).collect::<Vec<_>>());
        // a 0.3 chamfer on the top edge: the tool's cone crosses the edge 0.3 below the top
        let half = (c.tool(t).unwrap().angle / 2.0).to_radians();
        let tip = c.tool(t).unwrap().tip_diameter / 2.0;
        let on_front = paths[0]
            .moves
            .iter()
            .filter_map(|m| if let Move::Feed(p, _) = m { Some(*p) } else { None })
            .find(|p| p[0] > -0.01 && p[0] < 40.01 && p[1] < -0.5 && p[1] > -5.0)
            .unwrap();
        let reach = tip + (10.0 - 0.3 - on_front[2]) * half.tan();
        assert!((reach - (-on_front[1])).abs() < 0.05, "{on_front:?}");
        // only the picked edge
        let o = crate::cam_paths::work_origin(&c, &part);
        c.op_mut(id).unwrap().edges = vec![[20.0 - o.x, -o.y, 10.0 - o.z]];
        let picked = &generate(&c, &part)[0];
        let ys: Vec<f64> = picked.moves.iter().filter_map(|m| if let Move::Feed(p, _) = m { Some(p[1]) } else { None }).collect();
        assert!(!ys.is_empty() && ys.iter().all(|y| *y < 1.0), "{ys:?}");
        // picked in the view (part coordinates): still on that edge after the stock and work zero move
        c.op_mut(id).unwrap().edges.clear();
        c.op_mut(id).unwrap().edge_picks = vec![part.to_part(DVec3::new(20.0, 0.0, 10.0)).to_array()];
        c.stock.side += 3.0;
        c.stock.top += 2.0;
        let picked = &generate(&c, &part)[0];
        assert!(picked.warnings.is_empty(), "{:?}", picked.warnings);
        let ys: Vec<f64> = picked.moves.iter().filter_map(|m| if let Move::Feed(p, _) = m { Some(p[1]) } else { None }).collect();
        assert!(!ys.is_empty() && ys.iter().all(|y| *y < 1.0), "{ys:?}");
    }

    /// A tube standing on end: 30 across, a Ø10 hole down its middle, 10 high, 64 sides.
    fn tube() -> Part {
        let n = 64;
        let at = |r: f64, k: usize, z: f64| {
            let a = std::f64::consts::TAU * (k % n) as f64 / n as f64;
            DVec3::new(15.0 + r * a.cos(), 15.0 + r * a.sin(), z)
        };
        let q = |a: DVec3, b: DVec3, c: DVec3, d: DVec3| [[a, b, c], [a, c, d]];
        let mut t = vec![];
        for k in 0..n {
            t.extend(q(at(5.0, k, 10.0), at(15.0, k, 10.0), at(15.0, k + 1, 10.0), at(5.0, k + 1, 10.0)));
            t.extend(q(at(5.0, k, 0.0), at(5.0, k + 1, 0.0), at(15.0, k + 1, 0.0), at(15.0, k, 0.0)));
            t.extend(q(at(5.0, k, 0.0), at(5.0, k, 10.0), at(5.0, k + 1, 10.0), at(5.0, k + 1, 0.0)));
            t.extend(q(at(15.0, k, 0.0), at(15.0, k + 1, 0.0), at(15.0, k + 1, 10.0), at(15.0, k, 10.0)));
        }
        Part::new(&t, &[], "top")
    }

    #[test]
    fn deburr_runs_smoothly_round_holes_and_keeps_to_its_heights() {
        let part = tube();
        let mut c = Cam::default();
        let t = tool(&mut c, ToolKind::Chamfer, 6.0);
        let id = c.add_op(OpKind::Deburr, t);
        let tp = generate(&c, &part).remove(0);
        assert!(tp.warnings.is_empty(), "{:?}", tp.warnings);
        // inside the hole: one smooth circle, never doubling back (the offsets of the facets used to overlap)
        let mut inner = vec![];
        let mut prev: Option<DVec3> = None;
        for m in &tp.moves {
            let p = m.end(prev.unwrap_or(DVec3::ZERO));
            if let (Move::Feed(..), Some(q)) = (m, prev) {
                if (p.truncate() - DVec2::splat(15.0)).length() < 5.0 && (q.truncate() - DVec2::splat(15.0)).length() < 5.0 {
                    inner.push((q, p));
                }
            }
            prev = Some(p);
        }
        assert!(inner.len() > 20, "{}", inner.len());
        let r: Vec<f64> = inner.iter().map(|(_, p)| (p.truncate() - DVec2::splat(15.0)).length()).collect();
        let (rmin, rmax) = r.iter().fold((f64::INFINITY, 0.0f64), |(a, b), x| (a.min(*x), b.max(*x)));
        assert!(rmax - rmin < 0.05, "{rmin} .. {rmax}");
        for w in inner.windows(2) {
            let (a, b) = ((w[0].1 - w[0].0).truncate(), (w[1].1 - w[1].0).truncate());
            if a.length() > 1e-6 && b.length() > 1e-6 && w[0].1.distance(w[1].0) < 1e-9 {
                assert!(a.normalize().dot(b.normalize()) > 0.5, "turns back at {:?}", w[1].0);
            }
        }
        // the rims are chamfered 0.3 below the top: a bottom above that leaves nothing to do
        let o = crate::cam_paths::work_origin(&c, &part);
        c.op_mut(id).unwrap().bottom = Some(10.0 - 0.2 - o.z);
        let none = generate(&c, &part).remove(0);
        assert!(!none.moves.iter().any(|m| matches!(m, Move::Feed(..))), "{:?}", none.warnings);
        c.op_mut(id).unwrap().bottom = Some(10.0 - 1.0 - o.z);
        let low = generate(&c, &part).remove(0);
        assert!(low.moves.iter().any(|m| matches!(m, Move::Feed(..))));
        assert!(low.moves.iter().all(|m| !matches!(m, Move::Feed(p, _) if p[2] < 9.0 - 1e-6)));
    }

    #[test]
    fn rest_roughing_cuts_only_whats_left() {
        let part = pocket_block();
        let mut c = Cam::default();
        let big = tool(&mut c, ToolKind::Flat, 8.0);
        let small = tool(&mut c, ToolKind::Flat, 3.0);
        c.add_op(OpKind::Adaptive, big);
        let id = c.add_op(OpKind::Adaptive, small);
        assert!(c.op(id).unwrap().rest, "roughing after roughing defaults to rest");
        let (s, paths) = sim(&c, &part);
        let bad: Vec<_> = s.events.iter().filter(|e| matches!(e.kind, EventKind::Gouge | EventKind::Rapid)).take(3).collect();
        assert!(bad.is_empty(), "{bad:?}");
        let mut full = c.clone();
        full.op_mut(id).unwrap().rest = false;
        let all = generate(&full, &part);
        assert!(paths[1].cut_length > 1.0 && paths[1].cut_length < all[1].cut_length * 0.5, "rest {} vs full {}", paths[1].cut_length, all[1].cut_length);
    }

    #[test]
    fn edits_regenerate_only_what_they_change() {
        let part = pocket_block();
        let mut c = Cam::default();
        let big = tool(&mut c, ToolKind::Flat, 8.0);
        let small = tool(&mut c, ToolKind::Flat, 3.0);
        let ball = tool(&mut c, ToolKind::Ball, 6.0);
        c.add_op(OpKind::Adaptive, big);
        c.add_op(OpKind::Adaptive, small);
        let f = c.add_op(OpKind::Parallel, ball);
        let json = |t: &[Toolpath]| t.iter().map(|t| serde_json::to_string(&t.moves).unwrap()).collect::<Vec<_>>();
        let first = json(&generate(&c, &part));
        // the same again, worked out from scratch and from what is remembered
        crate::cam_paths::forget_paths();
        assert_eq!(json(&generate(&c, &part)), first);
        assert_eq!(json(&generate(&c, &part)), first);
        // a finishing pass changes on its own
        c.op_mut(f).unwrap().stepover = Some(0.3);
        let second = json(&generate(&c, &part));
        assert_eq!(second[..2], first[..2]);
        assert_ne!(second[2], first[2]);
        // the rest roughing follows the roughing before it
        c.tools.iter_mut().find(|t| t.id == big).unwrap().diameter = 10.0;
        let third = json(&generate(&c, &part));
        assert_ne!(third[0], first[0]);
        assert_ne!(third[1], first[1]);
        assert_eq!(third[2], second[2]);
    }

    #[test]
    fn pencil_cleans_the_corners() {
        let part = pocket_block();
        let mut c = Cam::default();
        c.stock.top = 0.0;
        c.stock.side = 0.0;
        let ball6 = tool(&mut c, ToolKind::Ball, 6.0);
        let ball2 = tool(&mut c, ToolKind::Ball, 2.0);
        c.add_op(OpKind::Parallel, ball6);
        let id = c.add_op(OpKind::Pencil, ball2);
        let (s, paths) = sim(&c, &part);
        let tp = paths.iter().find(|p| p.op == id).unwrap();
        assert!(tp.warnings.is_empty(), "{:?}", tp.warnings);
        let pts: Vec<DVec3> = tp.moves.iter().filter_map(|m| if let Move::Feed(p, _) = m { Some(DVec3::from(*p)) } else { None }).collect();
        assert!(pts.len() > 20);
        // every cut lies in the band along the pocket's floor edges, where the Ø6 ball couldn't reach
        for p in pts.iter().filter(|p| p.z < 9.0) {
            let wall = (p.x - 10.0).min(30.0 - p.x).min(p.y - 10.0).min(20.0 - p.y);
            assert!(wall > -0.01 && wall < 3.2, "{p}");
        }
        assert!(pts.iter().any(|p| p.z < 5.5), "reaches the floor corner");
        let g: Vec<_> = s.events.iter().filter(|e| e.kind == EventKind::Gouge).take(3).collect();
        assert!(g.is_empty(), "{g:?}");
    }

    /// Op 1 keeps a carrier body under the part that op 2 (flipped, the part alone) machines off: op 2 starts from
    /// op 1's stock where it really is, carrier and all, not from a box around its own part.
    #[test]
    fn a_later_setup_can_drop_a_body_the_earlier_one_kept() {
        let part_tris = block().tris.clone();
        let (lo, hi) = part_tris.iter().flatten().fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)), |(a, b), p| (a.min(*p), b.max(*p)));
        // a 3 mm carrier slab under the part, the part's footprint
        let c = |k: usize| DVec3::new([lo.x, hi.x][k & 1], [lo.y, hi.y][(k >> 1) & 1], [lo.z - 3.0, lo.z][(k >> 2) & 1]);
        let carrier: Vec<[DVec3; 3]> = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]].iter().flat_map(|q| [[c(q[0]), c(q[1]), c(q[2])], [c(q[0]), c(q[2]), c(q[3])]]).collect();
        let with_carrier: Vec<[DVec3; 3]> = part_tris.iter().chain(&carrier).copied().collect();
        let mut a = Cam { name: "Op 1".into(), ..Default::default() };
        a.stock.side = 3.0;
        a.stock.top = 1.0;
        a.stock.bottom = 0.0;
        let t = tool(&mut a, ToolKind::Flat, 6.0);
        a.add_op(OpKind::Face, t);
        let b = Cam { name: "Op 2".into(), follows: "Op 1".into(), orient: "bottom".into(), tools: a.tools.clone(), next_id: a.next_id, ..Default::default() };
        let mut cams = vec![a, b];
        let input = |k: usize| crate::cam_paths::PartInput { tris: if k == 0 { with_carrier.clone() } else { part_tris.clone() }, ..Default::default() };
        let left = crate::cam_paths::link_setups_each(&mut cams, 1, &input).unwrap().expect("carried stock");
        let pb = Part::new(&part_tris, &[], "bottom");
        // op 1's box: the part + carrier (hi - lo + 3 tall) + 1 on top, carried over whole
        let (blo, bhi) = stock_box(&cams[1], &pb);
        assert!((bhi.z - blo.z - (hi.z - lo.z + 3.0 + 1.0)).abs() < 1e-6, "{blo} {bhi}");
        // upside down the carrier is on top of the part: 3 mm of it still there over the part's middle
        let mid = (pb.lo + pb.hi) / 2.0;
        let (_, h) = left.column(mid.x, mid.y).expect("stock over the part");
        assert!((h as f64 - (pb.hi.z + 3.0)).abs() < 0.3, "top of the stock {h}, part top {}", pb.hi.z);
        // with every setup taken to machine op 2's parts (the old behaviour) the carrier would be lost
        let mut cams2 = cams.clone();
        let old = crate::cam_paths::link_setups(&mut cams2, 1, &input(1)).unwrap().unwrap();
        assert!((old.column(mid.x, mid.y).unwrap().1 as f64 - pb.hi.z).abs() < 0.3);
    }

    #[test]
    fn second_setup_starts_from_the_flipped_stock() {
        let part_tris = {
            let p = block();
            p.tris.clone()
        };
        let mut a = Cam { name: "Top".into(), ..Default::default() };
        a.stock.side = 3.0;
        a.stock.top = 1.0;
        a.stock.bottom = 2.0;
        let t = tool(&mut a, ToolKind::Flat, 6.0);
        let id = a.add_op(OpKind::Adaptive, t);
        a.op_mut(id).unwrap().leave = 0.0;
        a.op_mut(id).unwrap().leave_floor = 0.0;
        let mut b = Cam { name: "Bottom".into(), follows: "Top".into(), orient: "bottom".into(), tools: a.tools.clone(), next_id: a.next_id, ..Default::default() };
        let bid = b.add_op(OpKind::Adaptive, t);
        b.op_mut(bid).unwrap().leave = 0.0;
        b.op_mut(bid).unwrap().leave_floor = 0.0;
        let mut cams = vec![a, b];
        let left = crate::cam_paths::link_setups(&mut cams, 1, &crate::cam_paths::PartInput { tris: part_tris.clone(), ..Default::default() }).unwrap().expect("carried stock");
        let pb = Part::new(&part_tris, &[], "bottom");
        // the same stock box seen upside down
        let (lo, hi) = stock_box(&cams[1], &pb);
        assert!((hi.z - lo.z - 13.0).abs() < 1e-6 && (hi.x - lo.x - 46.0).abs() < 1e-6, "{lo} {hi}");
        // the first setup cleared round the part down to its bottom: only the 2 mm skin is left there
        let beside = DVec3::new(pb.lo.x - 2.0, (pb.lo.y + pb.hi.y) / 2.0, 0.0);
        let (l, h) = left.column(beside.x, beside.y).expect("skin");
        assert!((h as f64 - (pb.hi.z + 2.0)).abs() < 0.3 && (l as f64 - pb.hi.z).abs() < 0.3, "{l} {h}");
        let paths = crate::cam_paths::generate_from(&cams[1], &pb, Some(&left));
        let deepest = paths[0].moves.iter().filter_map(|m| if let Move::Feed(p, _) = m { Some(p[2]) } else { None }).fold(f64::INFINITY, f64::min);
        assert!(deepest > pb.hi.z - 0.5, "goes down to {deepest} through air (part top {})", pb.hi.z);
        let mut s = Sim::new(&cams[1], &pb, (lo, hi), &paths, 300);
        s.start_from(&left);
        s.run_to(usize::MAX, usize::MAX);
        assert!(s.events.iter().all(|e| e.kind != EventKind::Gouge), "{:?}", s.events.first());
    }

    #[test]
    fn machine_limits_and_holders() {
        let part = block();
        let mut c = Cam::default();
        assert!(crate::cam::set_machine(&mut c, "Desktop router (3018 class)"));
        c.machine.travel = [30.0, 300.0, 100.0];
        let t = tool(&mut c, ToolKind::Flat, 6.0);
        c.tools[0].feed = 5000.0;
        c.add_op(OpKind::Contour, t);
        let tp = &generate(&c, &part)[0];
        assert!(tp.warnings.iter().any(|w| w.contains("X travel")), "{:?}", tp.warnings);
        assert!(tp.warnings.iter().any(|w| w.contains("slowed down")), "{:?}", tp.warnings);
        assert!(tp.moves.iter().all(|m| !matches!(m, Move::Feed(_, f) if *f > 1000.0 + 1e-9)));
        // an ER20 nut: 30 mm across 3 mm up, 34 mm across above that
        let mut h = c.tools[0].clone();
        assert!(crate::cam::set_holder(&mut h, "ER20 collet chuck"));
        let o = h.holder_outline();
        assert_eq!(crate::cam::holder_underside(&o, 10.0), Some(0.0));
        assert!((crate::cam::holder_underside(&o, 16.0).unwrap() - 1.5).abs() < 1e-9);
        assert!((crate::cam::holder_underside(&o, 18.0).unwrap() - 19.5).abs() < 1e-9);
        assert_eq!(crate::cam::holder_underside(&o, 70.0), None);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    #[test]
    #[ignore]
    fn drop_speed() {
        let h = |x: f64, y: f64| {
            let d2 = (x - 30.0).powi(2) + (y - 20.0).powi(2);
            10.0 + (225.0 - d2).max(0.0).sqrt()
        };
        let mut tris = vec![];
        let s = 0.5;
        for j in 0..80 {
            for i in 0..120 {
                let (x, y) = (i as f64 * s, j as f64 * s);
                let p = |dx: f64, dy: f64| DVec3::new(x + dx, y + dy, h(x + dx, y + dy));
                tris.push([p(0.0, 0.0), p(s, 0.0), p(s, s)]);
                tris.push([p(0.0, 0.0), p(s, s), p(0.0, s)]);
            }
        }
        let part = Part::new(&tris, &[], "top");
        let t = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Ball && t.diameter == 6.0).unwrap();
        let d = Dropper::new(&part, &t, 0.0);
        let mut sc = Scratch::default();
        let t0 = std::time::Instant::now();
        let mut acc = 0.0;
        for k in 0..20000 {
            let p = DVec2::new(10.0 + (k % 200) as f64 * 0.2, 5.0 + (k / 200) as f64 * 0.3);
            acc += d.drop(p, &mut sc).0;
        }
        eprintln!("{:?} per drop ({acc})", t0.elapsed() / 20000);
    }
}
