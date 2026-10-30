//! Raw stock for a part: the smallest standard plate, bar or round bar it can be machined from.
//!
//! The part's points are boxed in a few orientations (the model axes and its principal axes, each
//! turned about to the smallest rectangle around the part), a machining allowance is added and the
//! sizes are rounded up to what suppliers sell.

use glam::{DMat3, DVec2, DVec3};
use serde::{Deserialize, Serialize};

/// How a part's stock is worked out (kept on the part; `None` there means all defaults).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StockPref {
    /// "auto" (least material), "plate" (plate or flat bar) or "round".
    pub shape: String,
    /// Standard sizes: "auto" (from the document units), "metric" or "imperial"; or "box": just the
    /// part's bounding box plus the allowances, not rounded to a stock size.
    pub standard: String,
    /// Machining allowance on each side of the width and length, mm.
    pub side: f64,
    /// Allowance on each face of the thickness (each end of a round bar's diameter), mm.
    pub face: f64,
    /// A size typed in by hand (shown instead of the suggestion), like "Flat bar 10 × 50 × 120".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub custom: String,
}

impl Default for StockPref {
    fn default() -> Self {
        StockPref { shape: "auto".into(), standard: "auto".into(), side: 1.0, face: 0.5, custom: String::new() }
    }
}

pub const SHAPES: [(&str, &str); 3] = [("auto", "Least material"), ("plate", "Plate / bar"), ("round", "Round bar")];
pub const STANDARDS: [(&str, &str); 4] = [("auto", "Standard (from units)"), ("metric", "Standard metric"), ("imperial", "Standard imperial"), ("box", "Bounding box + allowance")];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Plate,
    FlatBar,
    SquareBar,
    Round,
    /// Not a stock size: the bounding box plus allowances.
    Block,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Plate => "Plate",
            Kind::FlatBar => "Flat bar",
            Kind::SquareBar => "Square bar",
            Kind::Round => "Round bar",
            Kind::Block => "Block",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StockSize {
    pub kind: Kind,
    /// Stock thickness, width and length in mm (a round bar: diameter, diameter, length).
    pub dims: [f64; 3],
    /// The part's own size along the same directions.
    pub part: [f64; 3],
    /// Model directions of the thickness, width and length (length = a round bar's axis).
    pub axes: [DVec3; 3],
    pub imperial: bool,
}

impl StockSize {
    /// Stock volume, mm³.
    pub fn volume(&self) -> f64 {
        match self.kind {
            Kind::Round => std::f64::consts::PI / 4.0 * self.dims[0] * self.dims[0] * self.dims[2],
            _ => self.dims[0] * self.dims[1] * self.dims[2],
        }
    }

    /// Like "Flat bar 10 × 40 × 85 mm", "Round bar Ø25 × 60 mm" or "Plate 1/2 × 8 × 12 in".
    pub fn label(&self) -> String {
        format!("{} {}", self.kind.label(), self.size_text())
    }

    pub fn size_text(&self) -> String {
        let f = |v: f64| if self.imperial { inch(v) } else { crate::expr::trim(v, 1) };
        let u = if self.imperial { "in" } else { "mm" };
        match self.kind {
            Kind::Round => format!("Ø{} × {} {u}", f(self.dims[0]), f(self.dims[2])),
            Kind::Block if !self.imperial => format!("{} × {} × {} {u}", crate::expr::trim(self.dims[0], 1), crate::expr::trim(self.dims[1], 1), crate::expr::trim(self.dims[2], 1)),
            _ => format!("{} × {} × {} {u}", f(self.dims[0]), f(self.dims[1]), f(self.dims[2])),
        }
    }
}

/// Inches as a fraction to 1/64 when it is one ("1-1/2"), otherwise decimals.
pub fn inch(mm: f64) -> String {
    let v = mm / 25.4;
    let n = (v * 64.0).round();
    if (v * 64.0 - n).abs() > 1e-3 {
        return crate::expr::trim(v, 3);
    }
    let (whole, mut num, mut den) = ((n as i64) / 64, (n as i64) % 64, 64);
    while num > 0 && num % 2 == 0 {
        num /= 2;
        den /= 2;
    }
    match (whole, num) {
        (w, 0) => w.to_string(),
        (0, n) => format!("{n}/{den}"),
        (w, n) => format!("{w}-{n}/{den}"),
    }
}

// Common stock sizes. Metric (mm) follows EN 485/10058/10060 plate and bar; imperial (inches) the
// usual US aluminium/steel bar and plate lists.
const METRIC_THICK: &[f64] = &[
    1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 15.0, 16.0, 20.0, 25.0, 30.0, 35.0, 40.0, 45.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0, 110.0, 120.0, 130.0, 140.0,
    150.0, 160.0, 180.0, 200.0,
];
const METRIC_BAR_WIDTH: &[f64] = &[10.0, 12.0, 15.0, 16.0, 20.0, 25.0, 30.0, 35.0, 40.0, 45.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0, 120.0, 140.0, 150.0, 160.0, 180.0, 200.0];
const METRIC_ROUND: &[f64] = &[
    3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 14.0, 15.0, 16.0, 18.0, 20.0, 22.0, 25.0, 28.0, 30.0, 32.0, 35.0, 40.0, 45.0, 50.0, 55.0, 60.0, 65.0, 70.0, 75.0, 80.0, 90.0, 100.0,
    110.0, 120.0, 130.0, 140.0, 150.0, 160.0, 180.0, 200.0, 220.0, 250.0, 300.0,
];
const IMPERIAL_THICK: &[f64] = &[
    0.0625, 0.125, 0.1875, 0.25, 0.3125, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 3.0, 3.5, 4.0, 4.5, 5.0, 6.0, 7.0, 8.0,
];
const IMPERIAL_BAR_WIDTH: &[f64] = &[0.5, 0.625, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 3.5, 4.0, 4.5, 5.0, 6.0, 8.0];
const IMPERIAL_ROUND: &[f64] = &[
    0.125, 0.1875, 0.25, 0.3125, 0.375, 0.4375, 0.5, 0.5625, 0.625, 0.75, 0.875, 1.0, 1.125, 1.25, 1.375, 1.5, 1.625, 1.75, 2.0, 2.25, 2.5, 2.75, 3.0, 3.25, 3.5, 4.0, 4.5,
    5.0, 5.5, 6.0, 7.0, 8.0, 10.0, 12.0,
];

struct Table {
    thick: Vec<f64>,
    bar_width: Vec<f64>,
    round: Vec<f64>,
    /// What plate and lengths are sawn to.
    step: f64,
}

fn table(imperial: bool) -> Table {
    if imperial {
        let mm = |v: &[f64]| v.iter().map(|x| x * 25.4).collect();
        Table { thick: mm(IMPERIAL_THICK), bar_width: mm(IMPERIAL_BAR_WIDTH), round: mm(IMPERIAL_ROUND), step: 0.25 * 25.4 }
    } else {
        Table { thick: METRIC_THICK.to_vec(), bar_width: METRIC_BAR_WIDTH.to_vec(), round: METRIC_ROUND.to_vec(), step: 5.0 }
    }
}

/// The first size in `list` that `v` fits in (a hair of tolerance), or `None` when it is bigger than all.
fn next_size(list: &[f64], v: f64) -> Option<f64> {
    list.iter().copied().find(|s| *s >= v - 1e-6)
}

fn up(v: f64, step: f64) -> f64 {
    ((v - 1e-6) / step).ceil().max(1.0) * step
}

/// A bounding-box size rounded up a hair, to 0.1 mm or 0.001 in, so it reads cleanly.
fn exact(v: f64, imperial: bool) -> f64 {
    // a micron over (mesh and snapping noise) doesn't round up a whole step
    up(v - 1e-3, if imperial { 0.0254 } else { 0.1 })
}

/// Rectangular stock for a part of size `t` ≤ `w` ≤ `l` (mm): (kind, [thickness, width, length]).
pub fn rect(part: [f64; 3], pref: &StockPref, imperial: bool) -> (Kind, [f64; 3]) {
    let tb = table(imperial);
    let t = part[0] + 2.0 * pref.face;
    let w = part[1] + 2.0 * pref.side;
    let l = part[2] + 2.0 * pref.side;
    if pref.standard == "box" {
        return (Kind::Block, [t, w, l].map(|v| exact(v, imperial)));
    }
    // thicker than any plate: a sawn block
    let ts = next_size(&tb.thick, t).unwrap_or_else(|| up(t, tb.step));
    // narrow enough to cut from bar, else sawn from plate
    let (kind, ws) = match next_size(&tb.bar_width, w.max(ts)) {
        Some(ws) if ts <= ws => (if (ws - ts).abs() < 1e-6 { Kind::SquareBar } else { Kind::FlatBar }, ws),
        _ => (Kind::Plate, up(w, tb.step)),
    };
    // a square bar is just as thick as wide: make the thickness match
    let ts = if kind == Kind::SquareBar { ws } else { ts };
    (kind, [ts, ws, up(l.max(ws), tb.step)])
}

/// Round bar for a part of diameter `d` and length `l` (mm): [diameter, diameter, length].
pub fn round(d: f64, l: f64, pref: &StockPref, imperial: bool) -> [f64; 3] {
    let tb = table(imperial);
    let d = d + 2.0 * pref.side;
    if pref.standard == "box" {
        let ds = exact(d, imperial);
        return [ds, ds, exact(l + 2.0 * pref.face, imperial)];
    }
    let ds = next_size(&tb.round, d).unwrap_or_else(|| up(d, tb.step));
    [ds, ds, up(l + 2.0 * pref.face, tb.step)]
}

/// Standard rectangular stock for a CAM setup whose part spans `extent` (X, Y, Z on the machine), kept
/// square to the machine: (stock size X, Y, Z; how far it reaches below the part, so it is faced evenly top and bottom).
pub fn for_setup(extent: [f64; 3], pref: &StockPref, imperial: bool) -> ([f64; 3], f64) {
    let mut order = [0, 1, 2];
    order.sort_by(|a, b| extent[*a].total_cmp(&extent[*b]));
    let (_, dims) = rect(order.map(|i| extent[i]), pref, imperial);
    let mut size = [0.0; 3];
    for (k, i) in order.iter().enumerate() {
        size[*i] = dims[k];
    }
    (size, (size[2] - extent[2]) / 2.0)
}

/// The suggested stock for a set of points (a part's mesh vertices, mm), or `None` for no points.
pub fn suggest(pts: &[DVec3], pref: &StockPref, imperial: bool) -> Option<StockSize> {
    if pts.is_empty() {
        return None;
    }
    let mut best: Option<(f64, StockSize)> = None;
    let consider = |s: StockSize, best: &mut Option<(f64, StockSize)>| {
        let v = s.volume();
        // earlier candidates (model axes, flat stock) win ties so the answer is steady
        if best.as_ref().map(|b| v < b.0 * 0.995).unwrap_or(true) {
            *best = Some((v, s));
        }
    };
    for axis in candidate_axes(pts) {
        let (u, v) = perp(axis);
        let along: Vec<f64> = pts.iter().map(|p| p.dot(axis)).collect();
        let len = along.iter().cloned().fold(f64::NEG_INFINITY, f64::max) - along.iter().cloned().fold(f64::INFINITY, f64::min);
        let flat: Vec<DVec2> = pts.iter().map(|p| DVec2::new(p.dot(u), p.dot(v))).collect();
        let hull = hull(flat);
        if pref.shape != "round" {
            // smallest rectangle across the axis, then the three sizes sorted thin to long
            let (a, b, dir) = min_rect(&hull);
            let d2 = u * dir.x + v * dir.y;
            let mut sides = [(a, d2), (b, axis.cross(d2).normalize()), (len, axis)];
            sides.sort_by(|x, y| x.0.total_cmp(&y.0));
            let part = sides.map(|s| s.0);
            let (kind, dims) = rect(part, pref, imperial);
            consider(StockSize { kind, dims, part, axes: sides.map(|s| s.1), imperial }, &mut best);
        }
        if pref.shape != "plate" {
            let d = 2.0 * min_circle(&hull).1;
            let dims = round(d, len, pref, imperial);
            consider(StockSize { kind: Kind::Round, dims, part: [d, d, len], axes: [u, v, axis], imperial }, &mut best);
        }
    }
    best.map(|b| b.1)
}

/// Stock for the vertices of a part mesh (positions as xyz triples).
pub fn for_mesh(positions: &[f32], pref: &StockPref, imperial: bool) -> Option<StockSize> {
    let pts: Vec<DVec3> = positions.chunks_exact(3).map(|p| DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)).collect();
    suggest(&pts, pref, imperial)
}

/// The model axes, then the principal axes of the points (when they differ from the model axes).
fn candidate_axes(pts: &[DVec3]) -> Vec<DVec3> {
    let mut out = vec![DVec3::Z, DVec3::Y, DVec3::X];
    let c = pts.iter().copied().sum::<DVec3>() / pts.len() as f64;
    let mut cov = DMat3::ZERO;
    for p in pts {
        let d = *p - c;
        cov += DMat3::from_cols(d * d.x, d * d.y, d * d.z);
    }
    for e in eigenvectors(cov) {
        if out.iter().all(|a| a.dot(e).abs() < 0.9999) {
            out.push(e);
        }
    }
    out
}

/// Eigenvectors of a symmetric matrix (Jacobi rotations).
fn eigenvectors(m: DMat3) -> [DVec3; 3] {
    let mut a = m.to_cols_array_2d();
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..50 {
        let (mut p, mut q, mut big) = (0, 1, 0.0);
        for (i, j) in [(0, 1), (0, 2), (1, 2)] {
            if a[i][j].abs() > big {
                (p, q, big) = (i, j, a[i][j].abs());
            }
        }
        let scale = a[0][0].abs() + a[1][1].abs() + a[2][2].abs();
        if big <= 1e-12 * scale.max(1e-300) {
            break;
        }
        let th = 0.5 * (2.0 * a[p][q]).atan2(a[q][q] - a[p][p]);
        let (s, c) = th.sin_cos();
        for k in 0..3 {
            let (akp, akq) = (a[k][p], a[k][q]);
            a[k][p] = c * akp - s * akq;
            a[k][q] = s * akp + c * akq;
        }
        for k in 0..3 {
            let (apk, aqk) = (a[p][k], a[q][k]);
            a[p][k] = c * apk - s * aqk;
            a[q][k] = s * apk + c * aqk;
        }
        for row in v.iter_mut() {
            let (vp, vq) = (row[p], row[q]);
            row[p] = c * vp - s * vq;
            row[q] = s * vp + c * vq;
        }
    }
    [0, 1, 2].map(|k| DVec3::new(v[0][k], v[1][k], v[2][k]).normalize_or(DVec3::X))
}

fn perp(n: DVec3) -> (DVec3, DVec3) {
    let u = if n.x.abs() < 0.9 { DVec3::X.cross(n) } else { DVec3::Y.cross(n) }.normalize();
    // for a model axis keep the other two model axes, so the box lines up with the model
    let u = [DVec3::X, DVec3::Y, DVec3::Z].into_iter().find(|a| a.dot(n).abs() < 1e-9).unwrap_or(u);
    (u, n.cross(u))
}

/// Convex hull (monotone chain), counter-clockwise.
fn hull(p: Vec<DVec2>) -> Vec<DVec2> {
    // snap off rounding noise, or points that should share an x sort out of order and a near-collinear
    // test drops a corner
    let mut p: Vec<DVec2> = p.into_iter().map(|q| (q * 1e6).round() / 1e6).collect();
    p.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    p.dedup_by(|a, b| (*a - *b).length_squared() < 1e-12);
    if p.len() < 3 {
        return p;
    }
    let cross = |o: DVec2, a: DVec2, b: DVec2| (a - o).perp_dot(b - o);
    let mut h: Vec<DVec2> = Vec::with_capacity(p.len() * 2);
    for pass in 0..2 {
        let start = h.len();
        let it: Box<dyn Iterator<Item = &DVec2>> = if pass == 0 { Box::new(p.iter()) } else { Box::new(p.iter().rev()) };
        for &q in it {
            while h.len() >= start + 2 && cross(h[h.len() - 2], h[h.len() - 1], q) <= 1e-12 {
                h.pop();
            }
            h.push(q);
        }
        h.pop();
    }
    h
}

/// Smallest-area rectangle around a convex hull (counter-clockwise): (size along `dir`, size across, unit `dir`).
/// Rotating calipers: one pass over the edges with three pointers that only move forward.
fn min_rect(h: &[DVec2]) -> (f64, f64, DVec2) {
    let n = h.len();
    let extent = |dir: DVec2| {
        let nn = dir.perp();
        let (mut a0, mut a1, mut b0, mut b1) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        for p in h {
            let (a, b) = (p.dot(dir), p.dot(nn));
            (a0, a1, b0, b1) = (a0.min(a), a1.max(a), b0.min(b), b1.max(b));
        }
        (a1 - a0, b1 - b0)
    };
    // the model axes first, so a part square to them keeps that box
    let (a, b) = extent(DVec2::X);
    let mut best = (a * b, a, b, DVec2::X);
    if n < 3 {
        return (best.1, best.2, best.3);
    }
    let (mut far, mut hi, mut lo) = (1, 1, 0);
    for i in 0..n {
        let e = h[(i + 1) % n] - h[i];
        if e.length_squared() < 1e-18 {
            continue;
        }
        let dir = e.normalize();
        let nn = dir.perp();
        // farthest from the edge (inwards is +perp for a counter-clockwise hull)
        while (h[(far + 1) % n] - h[i]).dot(nn) >= (h[far % n] - h[i]).dot(nn) - 1e-12 && (far + 1) % n != i {
            far += 1;
        }
        while (h[(hi + 1) % n] - h[i]).dot(dir) >= (h[hi % n] - h[i]).dot(dir) - 1e-12 && (hi + 1) % n != i {
            hi += 1;
        }
        if i == 0 {
            lo = far;
        }
        // (the lowest along the edge can be the edge's own start)
        while lo % n != i && (h[(lo + 1) % n] - h[i]).dot(dir) <= (h[lo % n] - h[i]).dot(dir) + 1e-12 {
            lo += 1;
        }
        let a = (h[hi % n] - h[lo % n]).dot(dir);
        let b = (h[far % n] - h[i]).dot(nn);
        if a * b < best.0 * (1.0 - 1e-6) {
            best = (a * b, a, b, dir);
        }
    }
    (best.1, best.2, best.3)
}

/// Smallest circle around the points (Welzl, iterative): (centre, radius).
fn min_circle(p: &[DVec2]) -> (DVec2, f64) {
    // in hull order every point would grow the circle: shuffle (repeatably) for the expected linear time
    let mut p = p.to_vec();
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    for i in (1..p.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        p.swap(i, (seed % (i as u64 + 1)) as usize);
    }
    let inside = |c: (DVec2, f64), q: DVec2| (q - c.0).length() <= c.1 + 1e-7;
    let two = |a: DVec2, b: DVec2| ((a + b) / 2.0, (a - b).length() / 2.0);
    let three = |a: DVec2, b: DVec2, c: DVec2| {
        let (b, c2) = (b - a, c - a);
        let d = 2.0 * b.perp_dot(c2);
        if d.abs() < 1e-12 {
            // in a line: the two far ones
            let pairs = [two(a, b + a), two(a, c2 + a), two(b + a, c2 + a)];
            return pairs.into_iter().max_by(|x, y| x.1.total_cmp(&y.1)).unwrap();
        }
        let ux = (c2.y * b.length_squared() - b.y * c2.length_squared()) / d;
        let uy = (b.x * c2.length_squared() - c2.x * b.length_squared()) / d;
        let u = DVec2::new(ux, uy);
        (u + a, u.length())
    };
    let Some(&first) = p.first() else { return (DVec2::ZERO, 0.0) };
    let mut c = (first, 0.0);
    for i in 1..p.len() {
        if inside(c, p[i]) {
            continue;
        }
        c = (p[i], 0.0);
        for j in 0..i {
            if inside(c, p[j]) {
                continue;
            }
            c = two(p[i], p[j]);
            for k in 0..j {
                if !inside(c, p[k]) {
                    c = three(p[i], p[j], p[k]);
                }
            }
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(x: f64, y: f64, z: f64) -> Vec<DVec3> {
        (0..8).map(|k| DVec3::new([0.0, x][k & 1], [0.0, y][(k >> 1) & 1], [0.0, z][(k >> 2) & 1])).collect()
    }

    fn cylinder(r: f64, h: f64, axis: DVec3) -> Vec<DVec3> {
        let (u, v) = perp(axis);
        (0..64).flat_map(|i| {
            let a = i as f64 / 64.0 * std::f64::consts::TAU;
            let p = (u * a.cos() + v * a.sin()) * r;
            [p, p + axis * h]
        }).collect()
    }

    #[test]
    fn flat_bar_for_a_small_plate() {
        let s = suggest(&block(80.0, 36.0, 8.0), &StockPref::default(), false).unwrap();
        assert_eq!(s.kind, Kind::FlatBar);
        // 8 + 2×0.5 → 10 thick, 36 + 2 → 40 wide, 82 → 85 long
        assert_eq!(s.dims, [10.0, 40.0, 85.0]);
        assert!(s.axes[0].dot(DVec3::Z).abs() > 0.999);
        assert_eq!(s.label(), "Flat bar 10 × 40 × 85 mm");
    }

    #[test]
    fn plate_when_too_wide_for_bar() {
        let s = suggest(&block(300.0, 240.0, 11.0), &StockPref::default(), false).unwrap();
        assert_eq!(s.kind, Kind::Plate);
        assert_eq!(s.dims, [12.0, 245.0, 305.0]);
    }

    #[test]
    fn round_bar_for_a_turned_part() {
        let s = suggest(&cylinder(11.0, 50.0, DVec3::X), &StockPref::default(), false).unwrap();
        assert_eq!(s.kind, Kind::Round);
        // Ø22 + 2 → Ø25, 50 + 1 → 55
        assert_eq!(s.dims, [25.0, 25.0, 55.0]);
        assert!(s.axes[2].dot(DVec3::X).abs() > 0.999);
    }

    #[test]
    fn forced_shape() {
        let pref = StockPref { shape: "plate".into(), ..Default::default() };
        let s = suggest(&cylinder(11.0, 50.0, DVec3::Z), &pref, false).unwrap();
        assert_eq!(s.kind, Kind::SquareBar);
        assert_eq!(s.dims, [25.0, 25.0, 55.0]);
    }

    #[test]
    fn turned_block_finds_its_own_axes() {
        // a 100 × 30 × 6 plate turned 30° about Z: boxed square to its edges, not the model axes
        let r = DMat3::from_rotation_z(30f64.to_radians());
        let pts: Vec<DVec3> = block(100.0, 30.0, 6.0).into_iter().map(|p| r * p).collect();
        let s = suggest(&pts, &StockPref::default(), false).unwrap();
        assert!((s.part[1] - 30.0).abs() < 1e-6 && (s.part[2] - 100.0).abs() < 1e-6, "{:?}", s.part);
        assert_eq!(s.dims, [8.0, 35.0, 105.0]);
    }

    #[test]
    fn imperial_sizes() {
        let mm = |v: f64| v * 25.4;
        let s = suggest(&block(mm(3.0), mm(1.3), mm(0.4)), &StockPref { side: 0.5, face: 0.25, ..Default::default() }, true).unwrap();
        assert_eq!(s.kind, Kind::FlatBar);
        assert_eq!(s.label(), "Flat bar 1/2 × 1-1/2 × 3-1/4 in");
    }

    #[test]
    fn setup_keeps_machine_axes() {
        // a part lying flat, longest along Y
        let (size, below) = for_setup([36.0, 80.0, 8.0], &StockPref::default(), false);
        assert_eq!(size, [40.0, 85.0, 10.0]);
        assert_eq!(below, 1.0);
    }

    #[test]
    fn big_mesh_is_quick() {
        // a finely meshed turned part: 200k points
        let pts: Vec<DVec3> = (0..100_000)
            .flat_map(|i| {
                let a = i as f64 * 0.001;
                let p = DVec3::new(a.cos() * 20.0, a.sin() * 20.0, 0.0);
                [p, p + DVec3::Z * 70.0]
            })
            .collect();
        let t = std::time::Instant::now();
        let s = suggest(&pts, &StockPref::default(), false).unwrap();
        assert_eq!(s.kind, Kind::Round);
        assert_eq!(s.dims, [45.0, 45.0, 75.0]);
        assert!(t.elapsed().as_secs_f64() < 2.0, "{:?}", t.elapsed());
    }

    #[test]
    fn bounding_box_with_padding() {
        let pref = StockPref { standard: "box".into(), ..Default::default() };
        let s = suggest(&block(80.0, 36.0, 8.0), &pref, false).unwrap();
        assert_eq!(s.kind, Kind::Block);
        assert_eq!(s.dims, [9.0, 38.0, 82.0]);
        assert_eq!(s.label(), "Block 9 × 38 × 82 mm");
        let r = suggest(&cylinder(11.0, 50.0, DVec3::X), &pref, false).unwrap();
        assert_eq!(r.kind, Kind::Round);
        assert_eq!(r.dims, [24.0, 24.0, 51.0]);
    }

    #[test]
    fn calipers_match_brute_force() {
        let mut seed = 7u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        for _ in 0..50 {
            let pts: Vec<DVec2> = (0..40).map(|_| DVec2::new(rnd() * 100.0, rnd() * 30.0)).collect();
            let h = hull(pts);
            let (a, b, _) = min_rect(&h);
            let mut brute = f64::INFINITY;
            for i in 0..h.len() {
                let d = (h[(i + 1) % h.len()] - h[i]).normalize();
                let ex = |v: DVec2| h.iter().map(|p| p.dot(v)).fold(f64::NEG_INFINITY, f64::max) - h.iter().map(|p| p.dot(v)).fold(f64::INFINITY, f64::min);
                brute = brute.min(ex(d) * ex(d.perp()));
            }
            assert!((a * b - brute).abs() <= brute * 1e-9, "{} vs {brute}", a * b);
        }
    }

    #[test]
    fn inch_text() {
        assert_eq!(inch(25.4 * 1.5), "1-1/2");
        assert_eq!(inch(25.4 * 0.375), "3/8");
        assert_eq!(inch(25.4 * 2.0), "2");
        assert_eq!(inch(25.4 * 0.3), "0.3");
    }

    #[test]
    fn thick_block_is_sawn_plate() {
        let s = suggest(&block(400.0, 300.0, 230.0), &StockPref::default(), false).unwrap();
        assert_eq!(s.kind, Kind::Plate);
        assert_eq!(s.dims[0], 235.0);
    }
}
