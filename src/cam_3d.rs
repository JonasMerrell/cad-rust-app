//! 3D toolpaths. Finishing passes come from a drop cutter: the tool, held
//! vertically over a point, is lowered onto the part's triangles until it
//! touches (exactly, for flat, ball, bull nose, tapered ball and conical
//! cutters). Parallel passes sample it along lines, waterline and scallop
//! passes take iso-lines of it (or of a distance measured over it) on a grid.
//! Deburring runs a conical cutter along the part's sharp convex edges.
//! Adaptive clearing roughs level by level with cycloidal (trochoidal) loops,
//! so the tool never takes more than the stepover sideways.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::f64::consts::{FRAC_PI_2, PI, TAU};

use glam::{DVec2, DVec3};

use crate::cam::{OpKind, Operation, Tool, ToolKind};
use crate::cam_paths::{difference, flat, levels, offset, rect, stepdown, stepover, union_all, Contour, Ctx, Part, Pb, Shapes, Toolpath, P2};
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
    r: f64,
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
                    pb.approach(p0.x, p0.y, (p0.z + ENTRY_FEED).min(self.top).max(p0.z));
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
    let e = 0.2;
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
        pb.approach(run[0].x, run[0].y, (run[0].z + ENTRY_FEED).min(ctx.stock.1.z).max(run[0].z));
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

/// Material left in the current slab, on a grid (true = still there).
struct Mat {
    x0: f64,
    y0: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    m: Vec<bool>,
}

impl Mat {
    fn new(lo: DVec2, hi: DVec2, cell: f64) -> Mat {
        let nx = ((hi.x - lo.x) / cell).ceil() as usize + 1;
        let ny = ((hi.y - lo.y) / cell).ceil() as usize + 1;
        Mat { x0: lo.x, y0: lo.y, cell, nx, ny, m: vec![false; nx * ny] }
    }

    /// Material at height z: the stock box, or what is left of the stock.
    fn fill(&mut self, lo: DVec2, hi: DVec2, stock: Option<&StockMap>, z: f64) {
        for j in 0..self.ny {
            let y = self.y0 + j as f64 * self.cell;
            for i in 0..self.nx {
                let x = self.x0 + i as f64 * self.cell;
                self.m[j * self.nx + i] = x >= lo.x && x <= hi.x && y >= lo.y && y <= hi.y && stock.map(|m| m.solid(x, y, z, REST_TOL)).unwrap_or(true);
            }
        }
    }

    fn range(&self, lo: DVec2, hi: DVec2) -> (usize, usize, usize, usize) {
        let i0 = ((lo.x - self.x0) / self.cell).floor().max(0.0) as usize;
        let j0 = ((lo.y - self.y0) / self.cell).floor().max(0.0) as usize;
        let i1 = (((hi.x - self.x0) / self.cell).ceil().max(0.0) as usize).min(self.nx - 1);
        let j1 = (((hi.y - self.y0) / self.cell).ceil().max(0.0) as usize).min(self.ny - 1);
        (i0, i1, j0, j1)
    }

    /// No material within `rad` of `c`.
    fn air(&self, c: DVec2, rad: f64) -> bool {
        let (i0, i1, j0, j1) = self.range(c - DVec2::splat(rad), c + DVec2::splat(rad));
        let r2 = rad * rad;
        for j in j0..=j1 {
            let dy = self.y0 + j as f64 * self.cell - c.y;
            for i in i0..=i1 {
                let dx = self.x0 + i as f64 * self.cell - c.x;
                if dx * dx + dy * dy <= r2 && self.m[j * self.nx + i] {
                    return false;
                }
            }
        }
        true
    }

    fn seg_air(&self, a: DVec2, b: DVec2, rad: f64) -> bool {
        let n = (a.distance(b) / self.cell).ceil().max(1.0) as usize;
        (0..=n).all(|k| self.air(a.lerp(b, k as f64 / n as f64), rad))
    }

    /// Remove the material within `rad` of the segment.
    fn stamp(&mut self, a: DVec2, b: DVec2, rad: f64) {
        if rad <= 0.0 {
            return;
        }
        let (i0, i1, j0, j1) = self.range(a.min(b) - DVec2::splat(rad), a.max(b) + DVec2::splat(rad));
        let ab = b - a;
        let l2 = ab.length_squared();
        for j in j0..=j1 {
            for i in i0..=i1 {
                let k = j * self.nx + i;
                if !self.m[k] {
                    continue;
                }
                let p = DVec2::new(self.x0 + i as f64 * self.cell, self.y0 + j as f64 * self.cell);
                let t = if l2 > 1e-18 { ((p - a).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
                if (a + ab * t).distance_squared(p) <= rad * rad {
                    self.m[k] = false;
                }
            }
        }
    }
}

struct Slab<'a, 'b> {
    pb: &'a mut Pb<'b>,
    mat: &'a mut Mat,
    z: f64,
    z_from: f64,
    r: f64,
    adv: f64,
    dir: f64,
    /// Feed along the loops and walls (with chip thinning, above the tool's).
    feed: f64,
}

impl Slab<'_, '_> {
    fn here(&self) -> Option<DVec2> {
        self.pb.pos.map(|p| p.truncate())
    }

    fn feed(&mut self, p: DVec2) {
        let from = self.pb.pos;
        self.pb.cut(DVec3::new(p.x, p.y, self.z), self.feed);
        if let Some(f) = from {
            let rad = self.r - self.mat.cell * 0.75;
            self.mat.stamp(f.truncate(), p, rad);
        }
    }

    /// Get to `p` at depth: feed over through cleared space, drop in where it's air, else spiral
    /// down around `centre` (a circle of radius `rh` through `p`, starting at angle `phi`).
    fn enter(&mut self, p: DVec2, centre: DVec2, rh: f64, phi: f64) {
        let rad = self.r + self.mat.cell;
        if let Some(q) = self.pb.pos {
            if (q.z - self.z).abs() < 1e-6 && self.mat.seg_air(q.truncate(), p, rad) {
                self.feed(p);
                return;
            }
        }
        self.pb.approach(p.x, p.y, self.z_from);
        if self.mat.air(p, rad) || rh < 0.05 {
            self.pb.cut(DVec3::new(p.x, p.y, self.z), self.pb.plunge);
            return;
        }
        // helical ramp along the loop circle
        self.pb.cut(DVec3::new(p.x, p.y, self.z_from), self.pb.plunge);
        let dz = self.z_from - self.z;
        let pitch = (TAU * rh * self.pb.ramp.tan()).max(0.05);
        let turns = (dz / pitch).ceil().max(1.0);
        let seg = (2.0 * (1.0 - (0.01 / rh).min(1.0)).acos()).clamp(0.08, 0.5);
        let n = ((turns * TAU / seg).ceil() as usize).max(8);
        for i in 1..=n {
            let f = i as f64 / n as f64;
            let a = phi + self.dir * f * turns * TAU;
            let q = centre + DVec2::new(a.cos(), a.sin()) * rh;
            self.pb.cut(DVec3::new(q.x, q.y, self.z_from - dz * f), self.pb.feed);
        }
        let rad = self.r - self.mat.cell * 0.75;
        self.mat.stamp(centre, centre, rad + rh);
    }

    /// Cut along the closed centre path `c` with cycloidal loops of radius `rt`.
    fn ring(&mut self, c: &[P2], rt: f64) {
        let pts: Vec<DVec2> = c.iter().map(|p| DVec2::from(*p)).collect();
        let n = pts.len();
        if n < 2 {
            return;
        }
        let mut cum = vec![0.0];
        for i in 0..n {
            let l = cum[i] + pts[i].distance(pts[(i + 1) % n]);
            cum.push(l);
        }
        let total = cum[n];
        if total < 1e-6 {
            return;
        }
        let at = |s: f64| -> DVec2 {
            let s = s.rem_euclid(total);
            let i = match cum.binary_search_by(|v| v.total_cmp(&s)) {
                Ok(i) => i.min(n - 1),
                Err(i) => i.saturating_sub(1).min(n - 1),
            };
            let seg = cum[i + 1] - cum[i];
            let f = if seg > 1e-12 { (s - cum[i]) / seg } else { 0.0 };
            pts[i].lerp(pts[(i + 1) % n], f)
        };
        let nl = (total / self.adv).ceil().max(1.0) as usize;
        let step = total / nl as f64;
        let delta = (rt * 0.5).max(step).min(total / 4.0).max(1e-4);
        let frame = |s: f64| -> (DVec2, DVec2, DVec2) {
            let c = at(s);
            let t = (at(s + delta) - at(s - delta)).normalize_or(DVec2::X);
            (c, t, DVec2::new(-t.y, t.x))
        };
        let phi0 = PI;
        let point = |s: f64, phi: f64| {
            let (c, t, nn) = frame(s);
            c + (t * phi.cos() + nn * phi.sin()) * rt
        };
        let reach = rt + self.r + step / 2.0 + self.mat.cell;
        let air: Vec<bool> = (0..nl).map(|k| self.mat.air(at((k as f64 + 0.5) * step), reach)).collect();
        if air.iter().all(|a| *a) {
            return;
        }
        // start on the last loop of the longest run of air loops, so the tool drops in clear of material
        let start = if air.iter().any(|a| *a) {
            let mut best = (0usize, 0usize);
            for k in 0..nl {
                if air[k] && !air[(k + nl - 1) % nl] {
                    let mut len = 0;
                    while len < nl && air[(k + len) % nl] {
                        len += 1;
                    }
                    if len > best.1 {
                        best = (k, len);
                    }
                }
            }
            (best.0 + best.1 + nl - 1) % nl
        } else {
            let here = self.here().unwrap_or(DVec2::ZERO);
            (0..nl).min_by(|a, b| at(*a as f64 * step).distance_squared(here).total_cmp(&at(*b as f64 * step).distance_squared(here))).unwrap()
        };
        let m = {
            let seg = (2.0 * (1.0 - (0.02 / rt.max(0.02)).min(1.0)).acos()).clamp(0.15, 0.6);
            ((TAU / seg).ceil() as usize).clamp(8, 64)
        };
        for kk in 0..nl {
            let k = (start + kk) % nl;
            let s0 = k as f64 * step;
            let p0 = point(s0, phi0);
            if kk == 0 {
                self.enter(p0, at(s0), rt, phi0 + frame(s0).1.to_angle());
            } else {
                self.feed(p0);
            }
            if air[k] {
                continue;
            }
            for j in 1..=m {
                let f = j as f64 / m as f64;
                self.feed(point(s0 + step * f, phi0 + self.dir * TAU * f));
            }
        }
        let end = point((start + nl) as f64 * step, phi0);
        self.feed(end);
    }

    /// Finish along the region's boundary where material is left.
    fn wall(&mut self, c: &[P2]) {
        let pts = densify(&c.iter().map(|p| DVec2::from(*p)).collect::<Vec<_>>(), self.mat.cell.max(0.2), true);
        let rad = self.r + self.mat.cell * 1.5;
        if pts.iter().all(|p| self.mat.air(*p, rad)) {
            return;
        }
        let here = self.here().unwrap_or(DVec2::ZERO);
        let n = pts.len();
        let i = (0..n).min_by(|a, b| pts[*a].distance_squared(here).total_cmp(&pts[*b].distance_squared(here))).unwrap();
        // prefer a start in the air
        let i = (0..n).map(|k| (i + k) % n).find(|k| self.mat.air(pts[*k], self.r + self.mat.cell)).unwrap_or(i);
        let p0 = pts[i];
        let linked = self.pb.pos.map(|q| (q.z - self.z).abs() < 1e-6 && self.mat.seg_air(q.truncate(), p0, self.r + self.mat.cell)).unwrap_or(false);
        if linked || self.mat.air(p0, self.r + self.mat.cell) {
            self.enter(p0, p0, 0.0, 0.0);
            for k in 1..=n {
                self.feed(pts[(i + k) % n]);
            }
        } else {
            let rot: Contour = (0..n).map(|k| pts[(i + k) % n].to_array()).collect();
            self.pb.cut_loop(&rot, self.z, self.z_from, 0.0, None, None);
            for k in 0..n {
                let (a, b) = (pts[(i + k) % n], pts[(i + k + 1) % n]);
                self.mat.stamp(a, b, self.r - self.mat.cell * 0.75);
            }
        }
    }

    /// Cut a set of rings, nearest first.
    fn rings(&mut self, mut rings: Vec<Contour>, rt: f64) {
        while !rings.is_empty() {
            let here = self.here().unwrap_or(DVec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY));
            let k = if here.x.is_finite() {
                (0..rings.len())
                    .min_by(|a, b| {
                        let da = rings[*a].iter().map(|q| DVec2::from(*q).distance_squared(here)).fold(f64::INFINITY, f64::min);
                        let db = rings[*b].iter().map(|q| DVec2::from(*q).distance_squared(here)).fold(f64::INFINITY, f64::min);
                        da.total_cmp(&db)
                    })
                    .unwrap()
            } else {
                0
            };
            let c = rings.remove(k);
            self.ring(&c, rt);
        }
    }
}

/// Centre paths for loops of radius `rt` filling the region: offsets of its outline, the first `rt` in.
/// Also returns the band they sweep (centre space).
fn ring_set(region: &Shapes, rt: f64) -> (Vec<Contour>, Shapes) {
    let w = 1.8 * rt;
    let mut rings = vec![];
    let mut bands: Vec<Contour> = vec![];
    let mut d = rt;
    let mut last: Option<Shapes> = None;
    for _ in 0..10_000 {
        let x = offset(region, -d);
        if x.is_empty() {
            break;
        }
        rings.extend(flat(&x));
        bands.extend(flat(&difference(&offset(&x, rt), &offset(&x, -rt))));
        last = Some(x);
        d += w;
    }
    if let Some(x) = last {
        let tcore = offset(&x, -rt);
        if !tcore.is_empty() {
            rings.extend(flat(&tcore));
            bands.extend(flat(&offset(&tcore, rt)));
        }
    }
    (rings, union_all(&bands))
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
    let rt0 = if op.trochoid > 0.0 { op.trochoid } else { 0.4 * t.diameter };
    let adv = stepover(op, t);
    // the loops take a cut about `adv` wide: with chip thinning the feed rises to keep the chip as thick as the feed per tooth
    let thin = if op.chip_thinning { crate::cam::chip_thinning(adv / t.diameter.max(1e-9)) } else { 1.0 };
    let bound = offset(&rect(lo, hi), r * 0.5);
    let floors: Vec<f64> = ctx.geo.floors().iter().map(|f| f + op.leave_floor).collect();
    let margin = r + rt0 + 2.0;
    let (mlo, mhi) = (lo.truncate() - DVec2::splat(margin), hi.truncate() + DVec2::splat(margin));
    let cell = (r / 5.0).clamp(0.05, 1.0).max(((mhi.x - mlo.x) * (mhi.y - mlo.y) / 4.0e6).sqrt());
    let mut mat = Mat::new(mlo, mhi, cell);
    let mut z_prev = top;
    let mut any = false;
    for z in levels(top, bottom, stepdown(op, t), &floors) {
        let keep = offset(&ctx.geo.shadow(z - op.leave_floor), r + op.leave);
        let mut area = difference(&bound, &keep);
        if let Some(m) = stock {
            // only where the tool would reach material that is left
            area = intersect(&area, &offset(&leftover(ctx, m, z, op.leave), r + rt0));
            area = offset(&offset(&area, -0.05 * r), 0.05 * r);
        }
        if area.is_empty() {
            z_prev = z;
            continue;
        }
        any = true;
        mat.fill(lo.truncate(), hi.truncate(), stock, z);
        let feed = pb.feed * thin;
        let mut slab = Slab { pb: &mut *pb, mat: &mut mat, z, z_from: z_prev, r, adv, dir: if op.climb { 1.0 } else { -1.0 }, feed };
        let mut covered: Shapes = vec![];
        let mut rt = rt0;
        for stage in 0..4 {
            let target = if stage == 0 {
                area.clone()
            } else {
                let rest = difference(&area, &covered);
                let rest = offset(&offset(&rest, -0.05 * r), 0.05 * r);
                if rest.is_empty() {
                    break;
                }
                intersect(&offset(&rest, rt), &area)
            };
            if target.is_empty() {
                break;
            }
            let (rings, band) = ring_set(&target, rt);
            slab.rings(rings, rt);
            covered = union_all(&[flat(&covered), flat(&band)].concat());
            rt /= 2.0;
            if rt < 0.05 * r {
                break;
            }
        }
        for c in flat(&area) {
            let c: Contour = if op.climb { c } else { c.into_iter().rev().collect() };
            slab.wall(&c);
        }
        z_prev = z;
    }
    if !any {
        tp.warnings.push("nothing to clear".into());
    }
}

pub(crate) fn intersect(a: &Shapes, b: &Shapes) -> Shapes {
    difference(a, &difference(a, b))
}

/// Stock left at height z that isn't part (nor the `leave` on it), without slivers a couple of cells thin.
pub(crate) fn leftover(ctx: &Ctx, m: &StockMap, z: f64, leave: f64) -> Shapes {
    let c = m.cell * 1.5;
    let x = difference(&material(m, z), &offset(&ctx.geo.shadow(z), leave.max(0.0) + c));
    offset(&offset(&x, -c), c)
}

/// Outline of the stock that is left at height z (more than REST_TOL above it).
pub(crate) fn material(m: &StockMap, z: f64) -> Shapes {
    // a grid one cell bigger all round, so every outline closes
    let (nx, ny) = (m.nx + 2, m.ny + 2);
    let mut v = vec![-1.0; nx * ny];
    for j in 0..m.ny {
        for i in 0..m.nx {
            let k = j * m.nx + i;
            if m.hi[k] > m.lo[k] && (m.lo[k] as f64) < z {
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

    #[test]
    fn adaptive_clears_with_loops_and_no_crashes() {
        let part = dome();
        let mut c = Cam::default();
        c.stock.side = 3.0;
        c.stock.top = 1.0;
        let t = tool(&mut c, ToolKind::Flat, 6.0);
        let id = c.add_op(OpKind::Adaptive, t);
        assert_eq!(c.op(id).unwrap().stepover, Some(0.1));
        let (s, paths) = sim(&c, &part);
        let tp = &paths[0];
        assert!(tp.warnings.is_empty(), "{:?}", tp.warnings);
        assert!(s.events.is_empty(), "{:?}", s.events.iter().take(5).collect::<Vec<_>>());
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
        let feeds: Vec<DVec2> = tp.moves.iter().filter_map(|m| if let Move::Feed(p, _) = m { Some(DVec2::new(p[0], p[1])) } else { None }).collect();
        let mut turn = 0.0;
        let mut full = 0;
        for w in feeds.windows(3) {
            let (a, b) = (w[1] - w[0], w[2] - w[1]);
            if a.length() > 1e-6 && b.length() > 1e-6 {
                turn += a.angle_to(b);
                if turn.abs() >= TAU {
                    full += 1;
                    turn = 0.0;
                }
            }
        }
        assert!(full > 50, "only {full} loops");
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
