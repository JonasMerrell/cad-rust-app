//! The measure tool's numbers: what each picked face, edge, vertex or part is (radius, length,
//! area, volume, position) and, between two picks, the minimum and centre distances with their
//! X/Y/Z parts and the angle between them. Shared by the Part Studio and the Assembly; the
//! callers turn their picks into [`Item`]s in world coordinates.

use glam::DVec3;

use crate::expr::{trim, Units};
use crate::kernel::BodyMesh;

/// A picked thing's geometry, in world coordinates (mm).
#[derive(Clone, Debug)]
pub enum Geo {
    Point(DVec3),
    Line {
        a: DVec3,
        b: DVec3,
    },
    /// Circular edge (arc or full circle): centre, axis, radius, length and its polyline.
    Circle {
        c: DVec3,
        n: DVec3,
        r: f64,
        len: f64,
        pts: Vec<DVec3>,
    },
    Curve {
        pts: Vec<DVec3>,
        len: f64,
    },
    /// A flat face, or with no triangles an unbounded datum plane through `p`.
    Plane {
        p: DVec3,
        n: DVec3,
        area: f64,
        tris: Vec<[DVec3; 3]>,
    },
    /// A curved face: GeomAbs surface type and, for cylinders/cones/spheres/tori, its axis.
    Surface {
        kind: i32,
        axis: Option<(DVec3, DVec3, f64)>,
        center: DVec3,
        area: f64,
        tris: Vec<[DVec3; 3]>,
    },
    /// A whole part.
    Part {
        volume: f64,
        area: f64,
        lo: DVec3,
        hi: DVec3,
        centroid: DVec3,
        tris: Vec<[DVec3; 3]>,
    },
}

/// One pick: a short name ("Part 1", "Bracket") and its geometry.
#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub geo: Geo,
    /// Where the pick's number badge goes (the clicked point).
    pub at: DVec3,
}

/// A line of the measure window.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: String,
    pub value: String,
    /// 0/1/2: an X/Y/Z component, drawn in that axis's colour.
    pub axis: Option<usize>,
    /// The main number of its section (drawn bolder).
    pub main: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub title: String,
    pub rows: Vec<Row>,
    /// A pick's own section: its number (from 1) and a one-line summary for when several are picked.
    pub item: Option<(usize, String)>,
}

/// Everything the window and the viewport show for the picks.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub sections: Vec<Section>,
    /// Closest points between two picks (on the first, on the second) and that distance's label.
    pub min: Option<(DVec3, DVec3, String)>,
    /// Centre to centre, when it says something the minimum doesn't.
    pub center: Option<(DVec3, DVec3)>,
    /// Labels drawn at single picks (radius, length…), by item.
    pub tags: Vec<Option<String>>,
}

/// The numbers made for a set of picks, kept until `key` (what they depend on) changes.
pub struct Cache<K> {
    pub key: K,
    pub items: Vec<Item>,
    /// The item (window number - 1) each pick became, None when it can't be measured.
    pub of_pick: Vec<Option<usize>>,
    pub report: Report,
}

impl<K> Cache<K> {
    /// Badge colour of a pick, by its index in the picks.
    pub fn color(&self, pick: usize) -> Option<egui::Color32> {
        self.of_pick.get(pick).copied().flatten().map(|i| badge_color(i + 1))
    }
}

/// Number formatting in the document's units.
#[derive(Clone, Copy)]
pub struct Fmt(pub Units);

impl Fmt {
    pub fn len(self, mm: f64) -> String {
        self.0.fmt_with(mm)
    }
    pub fn area(self, mm2: f64) -> String {
        let s = self.0.scale();
        format!("{} {}²", sig(mm2 / (s * s)), self.0.label())
    }
    pub fn vol(self, mm3: f64) -> String {
        let s = self.0.scale();
        format!("{} {}³", sig(mm3 / (s * s * s)), self.0.label())
    }
    pub fn signed(self, mm: f64) -> String {
        format!("{} {}", trim(mm / self.0.scale(), 3), self.0.label())
    }
    pub fn xyz(self, p: DVec3) -> String {
        format!("{}, {}, {}", self.0.fmt(p.x), self.0.fmt(p.y), self.0.fmt(p.z))
    }
}

/// Three decimals, or more for small values so they don't show as 0.
fn sig(v: f64) -> String {
    let a = v.abs();
    let d = if a == 0.0 || a >= 1.0 { 3 } else { (3 - a.log10().floor() as i64).clamp(3, 9) as usize };
    trim(v, d)
}

pub fn deg(a: f64) -> String {
    format!("{}°", trim(a.to_degrees(), 3))
}

fn row(label: &str, value: String) -> Row {
    Row { label: label.into(), value, axis: None, main: false }
}

fn main_row(label: &str, value: String) -> Row {
    Row { label: label.into(), value, axis: None, main: true }
}

fn axis_rows(f: Fmt, d: DVec3, out: &mut Vec<Row>) {
    for (k, l) in ["ΔX", "ΔY", "ΔZ"].into_iter().enumerate() {
        out.push(Row { label: l.into(), value: f.len(d[k].abs()), axis: Some(k), main: false });
    }
}

fn coord_rows(f: Fmt, p: DVec3, out: &mut Vec<Row>) {
    for (k, l) in ["X", "Y", "Z"].into_iter().enumerate() {
        out.push(Row { label: l.into(), value: f.signed(p[k]), axis: Some(k), main: false });
    }
}

fn dir_text(d: DVec3) -> String {
    let d = d.normalize_or_zero();
    format!("{}, {}, {}", trim(d.x, 4), trim(d.y, 4), trim(d.z, 4))
}

impl Geo {
    /// What it is, for the window's titles.
    pub fn kind(&self) -> &'static str {
        match self {
            Geo::Point(_) => "Vertex",
            Geo::Line { .. } => "Line",
            Geo::Circle { r, len, pts, .. } => {
                let closed = pts.len() > 2 && (pts[0] - pts[pts.len() - 1]).length() < 1e-4 * (1.0 + r);
                if closed || (len - std::f64::consts::TAU * r).abs() < 1e-4 * r.max(1e-9) {
                    "Circle"
                } else {
                    "Arc"
                }
            }
            Geo::Curve { .. } => "Curve",
            Geo::Plane { tris, .. } => {
                if tris.is_empty() {
                    "Plane"
                } else {
                    "Planar face"
                }
            }
            Geo::Surface { kind, .. } => match kind {
                1 => "Cylinder",
                2 => "Cone",
                3 => "Sphere",
                4 => "Torus",
                5 | 6 => "Spline face",
                7 => "Revolved face",
                8 => "Extruded face",
                _ => "Curved face",
            },
            Geo::Part { .. } => "Part",
        }
    }

    /// The middle: circle and cylinder axes, a face's centre, a part's centre of mass.
    pub fn center(&self) -> DVec3 {
        match self {
            Geo::Point(p) => *p,
            Geo::Line { a, b } => (*a + *b) / 2.0,
            Geo::Circle { c, .. } => *c,
            Geo::Curve { pts, .. } => mid_of(pts),
            Geo::Plane { p, .. } => *p,
            Geo::Surface { kind, axis, center, .. } => match axis {
                Some((o, d, _)) if *kind == 3 => {
                    let _ = d;
                    *o
                }
                Some((o, d, _)) => {
                    // the face's centre moved onto its axis
                    let d = d.normalize_or_zero();
                    *o + d * (*center - *o).dot(d)
                }
                None => *center,
            },
            Geo::Part { centroid, .. } => *centroid,
        }
    }

    /// Direction for angles: (direction, true for a plane normal / false for a line direction).
    fn dir(&self) -> Option<(DVec3, bool)> {
        match self {
            Geo::Line { a, b } => Some(((*b - *a).normalize_or_zero(), false)).filter(|(d, _)| *d != DVec3::ZERO),
            Geo::Circle { n, .. } => Some((n.normalize_or_zero(), true)),
            Geo::Plane { n, .. } => Some((n.normalize_or_zero(), true)),
            Geo::Surface { kind: 1 | 2, axis: Some((_, d, _)), .. } => Some((d.normalize_or_zero(), false)),
            _ => None,
        }
    }

    /// An axis line (point, direction): lines, circles, cylinders, cones, tori.
    fn axis_line(&self) -> Option<(DVec3, DVec3)> {
        match self {
            Geo::Line { a, b } => Some((*a, (*b - *a).normalize_or_zero())),
            Geo::Circle { c, n, .. } => Some((*c, n.normalize_or_zero())),
            Geo::Surface { kind: 1 | 2 | 4, axis: Some((o, d, _)), .. } => Some((*o, d.normalize_or_zero())),
            _ => None,
        }
    }

    fn radius(&self) -> Option<f64> {
        match self {
            Geo::Circle { r, .. } => Some(*r),
            Geo::Surface { kind: 1 | 3, axis: Some((_, _, r)), .. } => Some(*r),
            _ => None,
        }
    }

    fn length(&self) -> Option<f64> {
        match self {
            Geo::Line { a, b } => Some((*b - *a).length()),
            Geo::Circle { len, .. } | Geo::Curve { len, .. } => Some(*len),
            _ => None,
        }
    }

    fn area(&self) -> Option<f64> {
        match self {
            Geo::Plane { area, tris, .. } if !tris.is_empty() => Some(*area),
            Geo::Surface { area, .. } => Some(*area),
            _ => None,
        }
    }

    /// The headline number drawn next to a single pick in the viewport.
    fn tag(&self, f: Fmt) -> Option<String> {
        match self {
            Geo::Circle { r, .. } => Some(format!("Ø {}", f.len(2.0 * r))),
            Geo::Surface { kind: 1 | 3, axis: Some((_, _, r)), .. } => Some(format!("Ø {}", f.len(2.0 * r))),
            Geo::Line { a, b } => Some(f.len((*b - *a).length())),
            Geo::Curve { len, .. } => Some(f.len(*len)),
            Geo::Plane { area, tris, .. } if !tris.is_empty() => Some(f.area(*area)),
            Geo::Surface { area, .. } => Some(f.area(*area)),
            _ => None,
        }
    }

    /// Rows describing it on its own.
    fn rows(&self, f: Fmt) -> Vec<Row> {
        let mut out = vec![];
        match self {
            Geo::Point(p) => coord_rows(f, *p, &mut out),
            Geo::Line { a, b } => {
                out.push(main_row("Length", f.len((*b - *a).length())));
                axis_rows(f, *b - *a, &mut out);
            }
            Geo::Circle { c, n, r, len, .. } => {
                out.push(main_row("Diameter", f.len(2.0 * r)));
                out.push(row("Radius", f.len(*r)));
                let full = self.kind() == "Circle";
                out.push(row(if full { "Circumference" } else { "Length" }, f.len(if full { std::f64::consts::TAU * r } else { *len })));
                if !full && *r > 0.0 {
                    out.push(row("Arc angle", deg(len / r)));
                }
                out.push(row("Centre", f.xyz(*c)));
                out.push(row("Axis", dir_text(*n)));
            }
            Geo::Curve { len, .. } => out.push(main_row("Length", f.len(*len))),
            Geo::Plane { p, n, area, tris } => {
                if !tris.is_empty() {
                    out.push(main_row("Area", f.area(*area)));
                } else {
                    out.push(row("Origin", f.xyz(*p)));
                }
                out.push(row("Normal", dir_text(*n)));
            }
            Geo::Surface { kind, axis, area, .. } => {
                if let Some((o, d, r)) = axis {
                    match kind {
                        1 | 3 => {
                            out.push(main_row("Diameter", f.len(2.0 * r)));
                            out.push(row("Radius", f.len(*r)));
                        }
                        2 => out.push(row("Ref. radius", f.len(*r))),
                        4 => out.push(row("Major radius", f.len(*r))),
                        _ => {}
                    }
                    out.push(row("Area", f.area(*area)));
                    if *kind == 3 {
                        out.push(row("Centre", f.xyz(*o)));
                    } else {
                        out.push(row("Axis point", f.xyz(self.center())));
                        out.push(row("Axis", dir_text(*d)));
                    }
                } else {
                    out.push(main_row("Area", f.area(*area)));
                }
            }
            Geo::Part { volume, area, lo, hi, centroid, .. } => {
                out.push(main_row("Volume", f.vol(*volume)));
                out.push(row("Surface area", f.area(*area)));
                let s = *hi - *lo;
                out.push(row("Size", format!("{} × {} × {} {}", f.0.fmt(s.x), f.0.fmt(s.y), f.0.fmt(s.z), f.0.label())));
                out.push(row("Centre of mass", f.xyz(*centroid)));
            }
        }
        out
    }

    /// Points, segments and triangles for the minimum distance.
    fn prims(&self) -> Prims {
        let mut p = Prims::default();
        match self {
            Geo::Point(x) => p.segs.push((*x, *x)),
            Geo::Line { a, b } => p.segs.push((*a, *b)),
            Geo::Circle { pts, .. } | Geo::Curve { pts, .. } => {
                p.segs.extend(pts.windows(2).map(|w| (w[0], w[1])));
                if pts.len() == 1 {
                    p.segs.push((pts[0], pts[0]));
                }
            }
            Geo::Plane { p: o, n, tris, .. } if tris.is_empty() => p.plane = Some((*o, n.normalize_or_zero())),
            Geo::Plane { tris, .. } | Geo::Surface { tris, .. } => {
                p.tris = tris.clone();
                p.segs = tri_edges(tris);
            }
            Geo::Part { tris, .. } => {
                p.tris = tris.clone();
                p.pts = verts(tris);
            }
        }
        if p.pts.is_empty() {
            p.pts = p.segs.iter().flat_map(|s| [s.0, s.1]).collect();
            p.pts.extend(verts(&p.tris));
            dedup(&mut p.pts);
        }
        p
    }
}

fn mid_of(pts: &[DVec3]) -> DVec3 {
    if pts.is_empty() {
        return DVec3::ZERO;
    }
    // halfway along the polyline
    let total: f64 = pts.windows(2).map(|w| (w[1] - w[0]).length()).sum();
    let mut left = total / 2.0;
    for w in pts.windows(2) {
        let l = (w[1] - w[0]).length();
        if l >= left && l > 0.0 {
            return w[0] + (w[1] - w[0]) * (left / l);
        }
        left -= l;
    }
    pts[pts.len() / 2]
}

#[derive(Default)]
struct Prims {
    pts: Vec<DVec3>,
    segs: Vec<(DVec3, DVec3)>,
    tris: Vec<[DVec3; 3]>,
    plane: Option<(DVec3, DVec3)>,
}

fn verts(tris: &[[DVec3; 3]]) -> Vec<DVec3> {
    let mut v: Vec<DVec3> = tris.iter().flatten().copied().collect();
    dedup(&mut v);
    v
}

fn dedup(v: &mut Vec<DVec3>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|p| seen.insert(((p.x * 1e5).round() as i64, (p.y * 1e5).round() as i64, (p.z * 1e5).round() as i64)));
}

/// A face's triangle edges, each once.
fn tri_edges(tris: &[[DVec3; 3]]) -> Vec<(DVec3, DVec3)> {
    let key = |p: DVec3| ((p.x * 1e5).round() as i64, (p.y * 1e5).round() as i64, (p.z * 1e5).round() as i64);
    let mut seen = std::collections::HashSet::new();
    let mut out = vec![];
    for t in tris {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let (ka, kb) = (key(a), key(b));
            if seen.insert(if ka < kb { (ka, kb) } else { (kb, ka) }) {
                out.push((a, b));
            }
        }
    }
    out
}

/// Thin a list to about `n` entries (for very large parts).
fn thin<T: Copy>(v: &[T], n: usize) -> Vec<T> {
    if v.len() <= n {
        return v.to_vec();
    }
    let step = v.len() as f64 / n as f64;
    (0..n).map(|i| v[(i as f64 * step) as usize]).collect()
}

/// Closest point to `p` on triangle abc (Ericson, Real-Time Collision Detection 5.1.5).
pub fn closest_on_tri(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> DVec3 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let den = 1.0 / (va + vb + vc);
    a + ab * (vb * den) + ac * (vc * den)
}

/// Closest points between segments p1q1 and p2q2 (Ericson 5.1.9).
pub fn closest_segs(p1: DVec3, q1: DVec3, p2: DVec3, q2: DVec3) -> (DVec3, DVec3) {
    let (d1, d2, r) = (q1 - p1, q2 - p2, p1 - p2);
    let (a, e, f) = (d1.length_squared(), d2.length_squared(), d2.dot(r));
    let eps = 1e-18;
    let (s, t);
    if a <= eps && e <= eps {
        return (p1, p2);
    }
    if a <= eps {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = d1.dot(r);
        if e <= eps {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = d1.dot(d2);
            let den = a * e - b * b;
            let mut s0 = if den > eps { ((b * f - c * e) / den).clamp(0.0, 1.0) } else { 0.0 };
            let mut t0 = (b * s0 + f) / e;
            if t0 < 0.0 {
                t0 = 0.0;
                s0 = (-c / a).clamp(0.0, 1.0);
            } else if t0 > 1.0 {
                t0 = 1.0;
                s0 = ((b - c) / a).clamp(0.0, 1.0);
            }
            s = s0;
            t = t0;
        }
    }
    (p1 + d1 * s, p2 + d2 * t)
}

/// Pairs of primitives tested at most (big parts are thinned to stay under it).
const BUDGET: usize = 3_000_000;

/// Minimum distance between two picks: (distance, point on a, point on b, approximate).
pub fn min_distance(a: &Geo, b: &Geo) -> Option<(f64, DVec3, DVec3, bool)> {
    let (pa, pb) = (a.prims(), b.prims());
    // an unbounded plane: the nearest of the other's points (no points: two planes)
    match (pa.plane, pb.plane) {
        (Some((o, n)), None) => return plane_min(o, n, &pb.pts).map(|(d, x, y)| (d, x, y, false)),
        (None, Some((o, n))) => return plane_min(o, n, &pa.pts).map(|(d, y, x)| (d, x, y, false)),
        (Some((oa, na)), Some((ob, nb))) => {
            if na.cross(nb).length() < 1e-9 {
                let d = (ob - oa).dot(na);
                return Some((d.abs(), oa, oa + na * d, false));
            }
            return None;
        }
        _ => {}
    }
    let mut best = (f64::INFINITY, DVec3::ZERO, DVec3::ZERO);
    let mut approx = false;
    let mut take = |x: DVec3, y: DVec3| {
        let d = (y - x).length_squared();
        if d < best.0 {
            best = (d, x, y);
        }
    };
    // segments against segments
    let (sa, sb) = fit(&pa.segs, &pb.segs, &mut approx);
    for s in &sa {
        for t in &sb {
            let (x, y) = closest_segs(s.0, s.1, t.0, t.1);
            take(x, y);
        }
    }
    // points against the other's triangles (and against its segments when it has no triangles but the points have no segments)
    let (qa, tb) = fit(&pa.pts, &pb.tris, &mut approx);
    for p in &qa {
        for t in &tb {
            take(*p, closest_on_tri(*p, t[0], t[1], t[2]));
        }
    }
    let (qb, ta) = fit(&pb.pts, &pa.tris, &mut approx);
    for p in &qb {
        for t in &ta {
            take(closest_on_tri(*p, t[0], t[1], t[2]), *p);
        }
    }
    if pa.segs.is_empty() || pb.segs.is_empty() {
        let (qa, sb) = fit(&pa.pts, &pb.segs, &mut approx);
        for p in &qa {
            for s in &sb {
                let (y, _) = closest_segs(s.0, s.1, *p, *p);
                take(*p, y);
            }
        }
        let (qb, sa) = fit(&pb.pts, &pa.segs, &mut approx);
        for p in &qb {
            for s in &sa {
                let (x, _) = closest_segs(s.0, s.1, *p, *p);
                take(x, *p);
            }
        }
    }
    best.0.is_finite().then(|| (best.0.sqrt(), best.1, best.2, approx))
}

fn fit<A: Copy, B: Copy>(a: &[A], b: &[B], approx: &mut bool) -> (Vec<A>, Vec<B>) {
    if a.len() * b.len() <= BUDGET || a.is_empty() || b.is_empty() {
        return (a.to_vec(), b.to_vec());
    }
    *approx = true;
    let k = (BUDGET as f64 / (a.len() * b.len()) as f64).sqrt();
    let n = |l: usize| ((l as f64 * k).ceil() as usize).max(1);
    (thin(a, n(a.len())), thin(b, n(b.len())))
}

fn plane_min(o: DVec3, n: DVec3, pts: &[DVec3]) -> Option<(f64, DVec3, DVec3)> {
    // the side the points are on: crossing the plane gives 0 where it crosses
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut best: Option<(f64, DVec3)> = None;
    for p in pts {
        let d = (*p - o).dot(n);
        lo = lo.min(d);
        hi = hi.max(d);
        if best.map(|b| d.abs() < b.0.abs()).unwrap_or(true) {
            best = Some((d, *p));
        }
    }
    let (d, p) = best?;
    if lo < 0.0 && hi > 0.0 {
        return Some((0.0, p - n * d, p - n * d));
    }
    Some((d.abs(), p - n * d, p))
}

/// Everything shown for these picks.
pub fn report(items: &[Item], f: Fmt) -> Report {
    let mut r = Report { tags: vec![None; items.len()], ..Default::default() };
    if items.len() == 1 {
        r.tags[0] = items[0].geo.tag(f);
    }
    if let [a, b] = items {
        pair(&a.geo, &b.geo, f, &mut r);
    }
    if items.len() > 2 {
        let lens: Vec<f64> = items.iter().filter_map(|i| i.geo.length()).collect();
        let areas: Vec<f64> = items.iter().filter_map(|i| i.geo.area()).collect();
        let vols: Vec<f64> = items.iter().filter_map(|i| if let Geo::Part { volume, .. } = i.geo { Some(volume) } else { None }).collect();
        let mut rows = vec![];
        if lens.len() > 1 {
            rows.push(main_row(&format!("Length of {} edges", lens.len()), f.len(lens.iter().sum())));
        }
        if areas.len() > 1 {
            rows.push(main_row(&format!("Area of {} faces", areas.len()), f.area(areas.iter().sum())));
        }
        if vols.len() > 1 {
            rows.push(main_row(&format!("Volume of {} parts", vols.len()), f.vol(vols.iter().sum())));
        }
        // the box around all the picks' centres
        let cs: Vec<DVec3> = items.iter().map(|i| i.geo.center()).collect();
        let lo = cs.iter().fold(DVec3::splat(f64::INFINITY), |a, b| a.min(*b));
        let hi = cs.iter().fold(DVec3::splat(f64::NEG_INFINITY), |a, b| a.max(*b));
        rows.push(row("Centres span", String::new()));
        axis_rows(f, hi - lo, &mut rows);
        r.sections.push(Section { title: format!("{} selected", items.len()), rows, item: None });
    }
    for (i, it) in items.iter().enumerate() {
        let title = if it.name.is_empty() { it.geo.kind().to_string() } else { format!("{} · {}", it.geo.kind(), it.name) };
        let brief = match &it.geo {
            Geo::Point(p) => f.xyz(*p),
            g => g.tag(f).unwrap_or_default(),
        };
        r.sections.push(Section { title, rows: it.geo.rows(f), item: Some((i + 1, brief)) });
    }
    r
}

fn acute(a: f64) -> f64 {
    if a > std::f64::consts::FRAC_PI_2 {
        std::f64::consts::PI - a
    } else {
        a
    }
}

/// Distances and angle between two picks.
fn pair(a: &Geo, b: &Geo, f: Fmt, r: &mut Report) {
    let mut rows = vec![];
    let tiny = 1e-6;
    // parallel flat faces/planes: their distance along the normal
    let (da, db) = (a.dir(), b.dir());
    let mut normal_dist = None;
    if let (Geo::Plane { p: pa, n: na, .. }, Geo::Plane { p: pb, n: nb, .. }) = (a, b) {
        if na.normalize().cross(nb.normalize()).length() < 1e-6 {
            normal_dist = Some((*pb - *pa).dot(na.normalize()).abs());
        }
    }
    let min = min_distance(a, b);
    if let Some((d, x, y, approx)) = min {
        let label = if approx { "Min. distance ≈" } else { "Min. distance" };
        rows.push(main_row(label, f.len(d)));
        if d > tiny {
            axis_rows(f, y - x, &mut rows);
        }
        r.min = Some((x, y, f.len(d)));
    }
    if let Some(nd) = normal_dist {
        if min.map(|m| (m.0 - nd).abs() > 1e-6).unwrap_or(true) {
            rows.push(main_row("Normal distance", f.len(nd)));
        }
    }
    // centre to centre, unless both are points (that is the minimum already)
    let (ca, cb) = (a.center(), b.center());
    let points = matches!(a, Geo::Point(_)) && matches!(b, Geo::Point(_));
    let unbounded = |g: &Geo| matches!(g, Geo::Plane { tris, .. } if tris.is_empty());
    if !points && !unbounded(a) && !unbounded(b) {
        let same_as_min = min.map(|(_, x, y, _)| (x - ca).length() < tiny && (y - cb).length() < tiny).unwrap_or(false);
        if !same_as_min {
            rows.push(row("Centre distance", f.len((cb - ca).length())));
            axis_rows(f, cb - ca, &mut rows);
            r.center = Some((ca, cb));
        }
    }
    // angle
    if let (Some((u, nu)), Some((v, nv))) = (da, db) {
        let raw = u.angle_between(v);
        if acute(raw) < 1e-6 && nu == nv {
            rows.push(main_row("Angle", "0° (parallel)".into()));
        } else {
            let (label, ang) = match (nu, nv) {
                // two faces: between their normals (90° for a box corner, 45° for a chamfer)
                (true, true) => ("Angle", if matches!(a, Geo::Circle { .. }) || matches!(b, Geo::Circle { .. }) { acute(raw) } else { raw }),
                (false, false) => ("Angle", acute(raw)),
                // a line and a plane: from the plane
                _ => ("Angle to plane", std::f64::consts::FRAC_PI_2 - acute(raw)),
            };
            rows.push(main_row(label, deg(ang.max(0.0))));
        }
    }
    // axes: parallel ones' distance, or the same axis
    if let (Some((oa, ua)), Some((ob, ub))) = (a.axis_line(), b.axis_line()) {
        let line_pair = matches!(a, Geo::Line { .. }) && matches!(b, Geo::Line { .. });
        if ua.cross(ub).length() < 1e-6 && !line_pair {
            let off = ob - oa;
            let d = (off - ua * off.dot(ua)).length();
            if d < 1e-6 {
                rows.push(row("Axes", "Concentric".into()));
            } else {
                rows.push(row("Axis distance", f.len(d)));
            }
        } else if line_pair && ua.cross(ub).length() < 1e-6 {
            let off = ob - oa;
            rows.push(row("Line distance", f.len((off - ua * off.dot(ua)).length())));
        }
    }
    // a point and an axis: off the axis
    for (p, o) in [(a, b), (b, a)] {
        if let (Geo::Point(x), Some((c, d))) = (p, o.axis_line()) {
            if o.radius().is_some() {
                let off = *x - c;
                rows.push(row("Distance to axis", f.len((off - d * off.dot(d)).length())));
            }
        }
    }
    // two circles or cylinders: radius difference
    if let (Some(ra), Some(rb)) = (a.radius(), b.radius()) {
        if (ra - rb).abs() > 1e-9 {
            rows.push(row("Radius difference", f.len((ra - rb).abs())));
        }
    }
    r.sections.push(Section { title: "Between 1 and 2".into(), rows, item: None });
}

// ---------------------------------------------------------------- window

/// What the user did in the measure window.
pub enum Action {
    None,
    Clear,
    Close,
    Remove(usize),
}

/// The floating measure window in the viewport's bottom right corner.
pub fn window(ctx: &egui::Context, id: &str, rect: egui::Rect, report: &Report, n: usize, hint: &str) -> Action {
    use crate::theme::{self, vp};
    use egui::{RichText, Stroke};
    let pal = theme::pal();
    let mut act = Action::None;
    let shown = egui::Area::new(egui::Id::new(id))
        .order(egui::Order::Middle)
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .fixed_pos(egui::pos2(rect.right() - 58.0, rect.bottom() - 14.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(pal.panel)
                .stroke(Stroke::new(1.0, pal.border))
                .corner_radius(10)
                .shadow(theme::card_shadow())
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(268.0);
                    ui.spacing_mut().interact_size.y = 18.0;
                    ui.horizontal(|ui| {
                        ui.add(crate::icons::image("measure", 15.0).tint(pal.accent));
                        ui.label(RichText::new("Measure").strong().size(13.0).color(pal.text));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::image(crate::icons::image("close", 14.0).tint(pal.muted)).frame(false)).on_hover_text("Stop measuring (M or Esc)").clicked() {
                                act = Action::Close;
                            }
                            if n > 0 && ui.add(egui::Button::new(RichText::new("Clear").size(11.5)).small()).on_hover_text("Unselect everything (or click empty space)").clicked() {
                                act = Action::Clear;
                            }
                        });
                    });
                    if n == 0 {
                        ui.add_space(4.0);
                        ui.label(RichText::new(hint).size(12.0).color(pal.muted));
                        return;
                    }
                    ui.add_space(2.0);
                    // sized by its content up to most of the viewport (not by the room below the window's top)
                    let max_h = (rect.height() * 0.62).max(160.0);
                    egui::ScrollArea::vertical().max_height(max_h).min_scrolled_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                        for (si, sec) in report.sections.iter().enumerate() {
                            ui.add_space(4.0);
                            ui.separator();
                            // with several picks each pick's own section is one line until clicked open
                            let open_id = egui::Id::new((id, "open", si));
                            let mut open = n < 2 || ui.ctx().data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
                            ui.horizontal(|ui| {
                                if let Some((k, brief)) = &sec.item {
                                    badge(ui, *k);
                                    let head = ui.add(egui::Label::new(RichText::new(&sec.title).size(12.0).strong().color(pal.text)).sense(egui::Sense::click()).truncate());
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui
                                            .add(egui::Button::image(crate::icons::image("close", 12.0).tint(pal.muted)).frame(false))
                                            .on_hover_text("Remove from the measure")
                                            .clicked()
                                        {
                                            act = Action::Remove(k - 1);
                                        }
                                        if n >= 2 {
                                            let more = ui
                                                .add(egui::Button::image(crate::icons::image(if open { "chevronDown" } else { "chevronRight" }, 12.0).tint(pal.muted)).frame(false))
                                                .on_hover_text(if open { "Less" } else { "More about it" });
                                            if !open && !brief.is_empty() {
                                                ui.label(RichText::new(brief).size(12.0).color(pal.text));
                                            }
                                            if more.clicked() || head.clicked() {
                                                open = !open;
                                                ui.ctx().data_mut(|d| d.insert_temp(open_id, open));
                                            }
                                        }
                                    });
                                } else {
                                    ui.label(RichText::new(&sec.title).size(12.0).strong().color(pal.accent));
                                }
                            });
                            if sec.item.is_some() && !open {
                                continue;
                            }
                            egui::Grid::new((id, "grid", si)).num_columns(2).spacing([10.0, 2.0]).min_col_width(96.0).show(ui, |ui| {
                                for r in &sec.rows {
                                    let lc = match r.axis {
                                        Some(0) => vp::axis_x(),
                                        Some(1) => vp::axis_y(),
                                        Some(2) => vp::axis_z(),
                                        _ => pal.muted,
                                    };
                                    let indent = if r.axis.is_some() && r.label.starts_with('Δ') { "   " } else { "" };
                                    ui.label(RichText::new(format!("{indent}{}", r.label)).size(12.0).color(lc));
                                    if r.value.is_empty() {
                                        ui.label("");
                                    } else {
                                        let mut t = RichText::new(&r.value).size(if r.main { 13.0 } else { 12.0 }).color(pal.text);
                                        if r.main {
                                            t = t.strong();
                                        }
                                        let resp = ui.add(egui::Label::new(t).sense(egui::Sense::click())).on_hover_text("Click to copy");
                                        if resp.clicked() {
                                            ui.ctx().copy_text(r.value.clone());
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                        }
                    });
                });
        });
    // placed from its bottom: when its height changes, draw again so it sits right
    let h = shown.response.rect.height();
    let key = egui::Id::new((id, "height"));
    if ctx.data(|d| d.get_temp::<f32>(key)) != Some(h) {
        ctx.data_mut(|d| d.insert_temp(key, h));
        ctx.request_repaint();
    }
    act
}

/// Colour of pick `k` (from 1): its badge in the window and on the model.
pub fn badge_color(k: usize) -> egui::Color32 {
    const C: [egui::Color32; 6] = [
        egui::Color32::from_rgb(0x2f, 0x86, 0xe0),
        egui::Color32::from_rgb(0xd6, 0x40, 0x7a),
        egui::Color32::from_rgb(0x2f, 0xae, 0x6a),
        egui::Color32::from_rgb(0x9b, 0x59, 0xd0),
        egui::Color32::from_rgb(0xd9, 0xa0, 0x20),
        egui::Color32::from_rgb(0x1f, 0xb3, 0xb3),
    ];
    C[(k.max(1) - 1) % C.len()]
}

fn badge(ui: &mut egui::Ui, k: usize) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
    paint_badge(ui.painter(), r.center(), k);
}

/// A round numbered marker.
pub fn paint_badge(p: &egui::Painter, at: egui::Pos2, k: usize) {
    p.circle(at, 9.0, badge_color(k), egui::Stroke::new(1.5, egui::Color32::WHITE));
    p.text(at, egui::Align2::CENTER_CENTER, k.to_string(), egui::FontId::proportional(11.0), egui::Color32::WHITE);
}

/// Draw the minimum-distance line with its X/Y/Z legs, the centre line and badges (screen overlay).
pub fn paint_overlay(p: &egui::Painter, report: &Report, items: &[Item], to_screen: &dyn Fn(DVec3) -> egui::Pos2) {
    use crate::theme::vp;
    use egui::Stroke;
    if let Some((a, b)) = report.center {
        let (sa, sb) = (to_screen(a), to_screen(b));
        dashed(p, sa, sb, Stroke::new(1.2, vp::dim().gamma_multiply(0.7)));
        p.circle_filled(sa, 3.0, vp::dim());
        p.circle_filled(sb, 3.0, vp::dim());
    }
    if let Some((a, b, label)) = &report.min {
        let d = *b - *a;
        // the X, Y and Z legs from the first point to the second
        let mut at = *a;
        let comps = (0..3).filter(|k| d[*k].abs() > 1e-6).count();
        if comps > 1 {
            for (k, col) in [vp::axis_x(), vp::axis_y(), vp::axis_z()].into_iter().enumerate() {
                if d[k].abs() <= 1e-6 {
                    continue;
                }
                let mut next = at;
                next[k] = b[k];
                dashed(p, to_screen(at), to_screen(next), Stroke::new(1.6, col));
                at = next;
            }
        }
        let (sa, sb) = (to_screen(*a), to_screen(*b));
        p.line_segment([sa, sb], Stroke::new(2.2, vp::dim()));
        p.circle(sa, 4.0, vp::dim(), Stroke::new(1.2, egui::Color32::WHITE));
        p.circle(sb, 4.0, vp::dim(), Stroke::new(1.2, egui::Color32::WHITE));
        crate::viewport::label_box(p, sa + (sb - sa) / 2.0 + egui::vec2(0.0, -14.0), label, vp::dim());
    }
    for (i, it) in items.iter().enumerate() {
        let s = to_screen(it.at);
        if let Some(Some(t)) = report.tags.get(i) {
            crate::viewport::label_box(p, s + egui::vec2(0.0, -24.0), t, vp::dim());
        }
        paint_badge(p, s + egui::vec2(12.0, -12.0), i + 1);
    }
}

fn dashed(p: &egui::Painter, a: egui::Pos2, b: egui::Pos2, s: egui::Stroke) {
    p.add(egui::Shape::dashed_line(&[a, b], s, 5.0, 4.0));
}

// ---------------------------------------------------------------- from meshes

/// A face of a body, placed by `xf` (part to world).
pub fn face_geo(m: &BodyMesh, fi: usize, xf: &dyn Fn(DVec3) -> DVec3) -> Option<Geo> {
    let fa = m.faces.get(fi)?;
    let dir = |d: DVec3| (xf(d) - xf(DVec3::ZERO)).normalize_or_zero();
    let mut tris = vec![];
    for (ti, t) in m.indices.chunks(3).enumerate() {
        if m.tri_face.get(ti).copied() == Some(fi as u32) {
            let v = |i: u32| xf(DVec3::new(m.positions[3 * i as usize] as f64, m.positions[3 * i as usize + 1] as f64, m.positions[3 * i as usize + 2] as f64));
            tris.push([v(t[0]), v(t[1]), v(t[2])]);
        }
    }
    Some(if fa.planar {
        Geo::Plane { p: xf(fa.center.into()), n: dir(fa.normal.into()), area: fa.area, tris }
    } else {
        let (kind, axis) = fa.surf.unwrap_or((-1, None));
        Geo::Surface { kind, axis: axis.map(|(c, d, r)| (xf(c.into()), dir(d.into()), r)), center: xf(fa.center.into()), area: fa.area, tris }
    })
}

/// An edge of a body.
pub fn edge_geo(m: &BodyMesh, ei: usize, xf: &dyn Fn(DVec3) -> DVec3) -> Option<Geo> {
    let e = m.edges.get(ei)?;
    let pts: Vec<DVec3> = e.pts.iter().map(|p| xf(DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64))).collect();
    if pts.is_empty() {
        return None;
    }
    let dir = |d: DVec3| (xf(d) - xf(DVec3::ZERO)).normalize_or_zero();
    Some(if let Some((c, n, r)) = e.circle {
        Geo::Circle { c: xf(c.into()), n: dir(n.into()), r, len: e.length, pts }
    } else if e.linear {
        Geo::Line { a: pts[0], b: pts[pts.len() - 1] }
    } else {
        Geo::Curve { len: e.length, pts }
    })
}

/// A whole body.
pub fn body_geo(m: &BodyMesh, xf: &dyn Fn(DVec3) -> DVec3) -> Geo {
    let v = |i: u32| xf(DVec3::new(m.positions[3 * i as usize] as f64, m.positions[3 * i as usize + 1] as f64, m.positions[3 * i as usize + 2] as f64));
    let tris: Vec<[DVec3; 3]> = m.indices.chunks(3).map(|t| [v(t[0]), v(t[1]), v(t[2])]).collect();
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for t in &tris {
        for p in t {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
    }
    if tris.is_empty() {
        (lo, hi) = (xf(m.bbox[0].into()), xf(m.bbox[1].into()));
    }
    Geo::Part { volume: m.volume, area: m.area, lo, hi, centroid: xf(m.centroid.into()), tris }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(z: f64, s: f64) -> Vec<[DVec3; 3]> {
        let p = |x: f64, y: f64| DVec3::new(x, y, z);
        vec![[p(0.0, 0.0), p(s, 0.0), p(s, s)], [p(0.0, 0.0), p(s, s), p(0.0, s)]]
    }

    fn rows_of(r: &Report) -> Vec<(String, String)> {
        r.sections.iter().flat_map(|s| s.rows.iter().map(|x| (x.label.clone(), x.value.clone()))).collect()
    }

    fn val(r: &Report, l: &str) -> Option<String> {
        rows_of(r).into_iter().find(|x| x.0 == l).map(|x| x.1)
    }

    #[test]
    fn parallel_faces() {
        let a = Geo::Plane { p: DVec3::new(5.0, 5.0, 0.0), n: DVec3::Z, area: 100.0, tris: square(0.0, 10.0) };
        let b = Geo::Plane { p: DVec3::new(5.0, 5.0, 7.5), n: -DVec3::Z, area: 100.0, tris: square(7.5, 10.0) };
        let it = |g: Geo| Item { name: String::new(), at: g.center(), geo: g };
        let r = report(&[it(a), it(b)], Fmt(Units::Mm));
        assert_eq!(val(&r, "Min. distance").as_deref(), Some("7.5 mm"));
        assert_eq!(val(&r, "Angle").as_deref(), Some("0° (parallel)"));
        assert_eq!(val(&r, "ΔZ").as_deref(), Some("7.5 mm"));
    }

    #[test]
    fn perpendicular_faces_and_offsets() {
        // a face on z=0 and a wall face on x=20, 5 mm up
        let wall = vec![[DVec3::new(20.0, 0.0, 5.0), DVec3::new(20.0, 10.0, 5.0), DVec3::new(20.0, 10.0, 15.0)]];
        let a = Geo::Plane { p: DVec3::new(5.0, 5.0, 0.0), n: DVec3::Z, area: 100.0, tris: square(0.0, 10.0) };
        let b = Geo::Plane { p: DVec3::new(20.0, 7.0, 10.0), n: DVec3::X, area: 50.0, tris: wall };
        let (d, x, y, approx) = min_distance(&a, &b).unwrap();
        assert!(!approx);
        assert!((d - (100.0f64 + 25.0).sqrt()).abs() < 1e-9, "{d}");
        assert!((x - DVec3::new(10.0, 0.0, 0.0)).length() < 1e-9 || (x.x - 10.0).abs() < 1e-9);
        assert!((y.x - 20.0).abs() < 1e-9 && (y.z - 5.0).abs() < 1e-9);
        let it = |g: Geo| Item { name: String::new(), at: g.center(), geo: g };
        let r = report(&[it(a), it(b)], Fmt(Units::Mm));
        assert_eq!(val(&r, "Angle").as_deref(), Some("90°"));
    }

    #[test]
    fn circles_and_points() {
        let circle = |c: DVec3, r: f64| {
            let pts: Vec<DVec3> =
                (0..=64).map(|i| c + DVec3::new((i as f64 / 64.0 * std::f64::consts::TAU).cos() * r, (i as f64 / 64.0 * std::f64::consts::TAU).sin() * r, 0.0)).collect();
            Geo::Circle { c, n: DVec3::Z, r, len: std::f64::consts::TAU * r, pts }
        };
        let a = circle(DVec3::ZERO, 5.0);
        assert_eq!(a.kind(), "Circle");
        let b = circle(DVec3::new(30.0, 40.0, 0.0), 2.0);
        let it = |g: Geo| Item { name: String::new(), at: g.center(), geo: g };
        let r = report(&[it(a.clone()), it(b)], Fmt(Units::Mm));
        assert_eq!(val(&r, "Centre distance").as_deref(), Some("50 mm"));
        assert_eq!(val(&r, "Axis distance").as_deref(), Some("50 mm"));
        assert_eq!(val(&r, "Radius difference").as_deref(), Some("3 mm"));
        let single = report(&[it(a.clone())], Fmt(Units::Mm));
        assert_eq!(val(&single, "Diameter").as_deref(), Some("10 mm"));
        assert_eq!(single.tags[0].as_deref(), Some("Ø 10 mm"));
        // a point to the circle's axis
        let r = report(&[it(Geo::Point(DVec3::new(3.0, 4.0, 9.0))), it(a)], Fmt(Units::Mm));
        assert_eq!(val(&r, "Distance to axis").as_deref(), Some("5 mm"));
    }

    #[test]
    fn line_and_plane_angle() {
        let a = Geo::Line { a: DVec3::ZERO, b: DVec3::new(10.0, 0.0, 10.0) };
        let b = Geo::Plane { p: DVec3::ZERO, n: DVec3::Z, area: 0.0, tris: vec![] };
        let it = |g: Geo| Item { name: String::new(), at: g.center(), geo: g };
        let r = report(&[it(a), it(b)], Fmt(Units::Mm));
        assert_eq!(val(&r, "Angle to plane").as_deref(), Some("45°"));
        assert_eq!(val(&r, "Min. distance").as_deref(), Some("0 mm"));
    }

    #[test]
    fn units_and_totals() {
        let f = Fmt(Units::In);
        assert_eq!(f.len(25.4), "1 in");
        assert_eq!(f.area(645.16), "1 in²");
        let l = |x: f64| Item { name: String::new(), at: DVec3::ZERO, geo: Geo::Line { a: DVec3::ZERO, b: DVec3::new(x, 0.0, 0.0) } };
        let r = report(&[l(10.0), l(20.0), l(5.0)], Fmt(Units::Mm));
        assert_eq!(val(&r, "Length of 3 edges").as_deref(), Some("35 mm"));
    }

    #[test]
    fn segments() {
        let (x, y) = closest_segs(DVec3::new(0.0, 0.0, 0.0), DVec3::new(10.0, 0.0, 0.0), DVec3::new(5.0, -5.0, 3.0), DVec3::new(5.0, 5.0, 3.0));
        assert!((x - DVec3::new(5.0, 0.0, 0.0)).length() < 1e-12);
        assert!((y - DVec3::new(5.0, 0.0, 3.0)).length() < 1e-12);
    }
}
