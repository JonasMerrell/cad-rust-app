//! Closed regions of a sketch: curves are split at their intersections into a
//! planar graph whose minimal faces are the regions you can click and extrude.

use std::f64::consts::TAU;

use glam::DVec2;

use crate::sketch::{arc_angles, Geom, Sketch};

const TOL: f64 = 1e-6;

/// A boundary piece in sketch coordinates, oriented along the loop.
#[derive(Clone, Debug)]
pub enum Seg {
    Line { a: DVec2, b: DVec2 },
    /// From angle `t0` to `t1` (t1 < t0 means clockwise).
    Arc { c: DVec2, r: f64, t0: f64, t1: f64 },
}

impl Seg {
    pub fn start(&self) -> DVec2 {
        match *self {
            Seg::Line { a, .. } => a,
            Seg::Arc { c, r, t0, .. } => c + DVec2::from_angle(t0) * r,
        }
    }
    pub fn end(&self) -> DVec2 {
        match *self {
            Seg::Line { b, .. } => b,
            Seg::Arc { c, r, t1, .. } => c + DVec2::from_angle(t1) * r,
        }
    }
    pub fn mid(&self) -> DVec2 {
        self.at(0.5)
    }
    pub fn at(&self, u: f64) -> DVec2 {
        match *self {
            Seg::Line { a, b } => a.lerp(b, u),
            Seg::Arc { c, r, t0, t1 } => c + DVec2::from_angle(t0 + (t1 - t0) * u) * r,
        }
    }
    pub fn reversed(&self) -> Seg {
        match *self {
            Seg::Line { a, b } => Seg::Line { a: b, b: a },
            Seg::Arc { c, r, t0, t1 } => Seg::Arc { c, r, t0: t1, t1: t0 },
        }
    }
    pub fn polyline(&self, out: &mut Vec<DVec2>) {
        match *self {
            Seg::Line { a, .. } => out.push(a),
            Seg::Arc { r, t0, t1, .. } => {
                let n = (((t1 - t0).abs() * r.max(1.0)).sqrt() * 3.0).ceil().clamp(4.0, 128.0) as usize;
                for i in 0..n {
                    out.push(self.at(i as f64 / n as f64));
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Loop {
    pub segs: Vec<Seg>,
    pub poly: Vec<DVec2>,
}

impl Loop {
    fn new(segs: Vec<Seg>) -> Loop {
        let mut poly = vec![];
        for s in &segs {
            s.polyline(&mut poly);
        }
        Loop { segs, poly }
    }
    pub fn area(&self) -> f64 {
        signed_area(&self.poly)
    }
    pub fn reversed(&self) -> Loop {
        Loop::new(self.segs.iter().rev().map(|s| s.reversed()).collect())
    }
}

#[derive(Clone, Debug)]
pub struct Region {
    /// Counter-clockwise outer boundary.
    pub outer: Loop,
    /// Clockwise holes.
    pub holes: Vec<Loop>,
    pub area: f64,
    /// Triangles (sketch coordinates) for shading and picking.
    pub tris: Vec<[DVec2; 3]>,
    /// A point inside the region, used to refer to it across rebuilds.
    pub sample: DVec2,
}

impl Region {
    pub fn contains(&self, p: DVec2) -> bool {
        point_in_poly(p, &self.outer.poly) && !self.holes.iter().any(|h| point_in_poly(p, &h.poly))
    }
}

pub fn signed_area(p: &[DVec2]) -> f64 {
    let n = p.len();
    (0..n).map(|i| p[i].perp_dot(p[(i + 1) % n])).sum::<f64>() * 0.5
}

pub fn point_in_poly(p: DVec2, poly: &[DVec2]) -> bool {
    let mut inside = false;
    let n = poly.len();
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let (a, b) = (poly[i], poly[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// A sketch curve in a form convenient for splitting.
#[derive(Clone, Debug)]
enum Prim {
    Line { a: DVec2, b: DVec2 },
    /// Arc from angle t0 to t1 (t1 > t0), full circle when t1 - t0 = TAU.
    Arc { c: DVec2, r: f64, t0: f64, t1: f64 },
}

impl Prim {
    fn at(&self, u: f64) -> DVec2 {
        match *self {
            Prim::Line { a, b } => a.lerp(b, u),
            Prim::Arc { c, r, t0, t1 } => c + DVec2::from_angle(t0 + (t1 - t0) * u) * r,
        }
    }
    /// Parameter (0..1) of a point known to lie on the curve, or None if outside.
    fn param(&self, p: DVec2) -> Option<f64> {
        match *self {
            Prim::Line { a, b } => {
                let d = b - a;
                let u = (p - a).dot(d) / d.length_squared().max(1e-18);
                let tol = TOL / d.length().max(1e-9);
                (u >= -tol && u <= 1.0 + tol).then_some(u.clamp(0.0, 1.0))
            }
            Prim::Arc { c, r, t0, t1 } => {
                let mut t = (p - c).to_angle();
                while t < t0 - 1e-9 {
                    t += TAU;
                }
                while t > t0 + TAU {
                    t -= TAU;
                }
                let tol = TOL / r.max(1e-9);
                if t <= t1 + tol {
                    Some(((t - t0) / (t1 - t0)).clamp(0.0, 1.0))
                } else if (t - TAU - t0).abs() < tol {
                    Some(0.0)
                } else {
                    None
                }
            }
        }
    }
    fn sub(&self, u0: f64, u1: f64) -> Seg {
        match *self {
            Prim::Line { a, b } => Seg::Line { a: a.lerp(b, u0), b: a.lerp(b, u1) },
            Prim::Arc { c, r, t0, t1 } => Seg::Arc { c, r, t0: t0 + (t1 - t0) * u0, t1: t0 + (t1 - t0) * u1 },
        }
    }
    fn is_full_circle(&self) -> bool {
        matches!(*self, Prim::Arc { t0, t1, .. } if (t1 - t0 - TAU).abs() < 1e-12)
    }
}

fn intersections(p: &Prim, q: &Prim) -> Vec<DVec2> {
    let circ = |pr: &Prim| match *pr {
        Prim::Arc { c, r, .. } => Some((c, r)),
        _ => None,
    };
    let cand: Vec<DVec2> = match (p, q) {
        (Prim::Line { a, b }, Prim::Line { a: c, b: d }) => {
            let r = *b - *a;
            let s = *d - *c;
            let den = r.perp_dot(s);
            if den.abs() < 1e-12 {
                // collinear overlaps: endpoints on the other segment
                vec![*a, *b, *c, *d]
            } else {
                let t = (*c - *a).perp_dot(s) / den;
                vec![*a + r * t]
            }
        }
        (Prim::Line { a, b }, _) | (_, Prim::Line { a, b }) => {
            let (c, r) = circ(if matches!(p, Prim::Line { .. }) { q } else { p }).unwrap();
            let d = *b - *a;
            let f = *a - c;
            let aa = d.dot(d);
            let bb = 2.0 * f.dot(d);
            let cc = f.dot(f) - r * r;
            let disc = bb * bb - 4.0 * aa * cc;
            if disc < -1e-9 * aa.max(1.0) {
                vec![]
            } else {
                let sq = disc.max(0.0).sqrt();
                vec![*a + d * ((-bb - sq) / (2.0 * aa)), *a + d * ((-bb + sq) / (2.0 * aa))]
            }
        }
        _ => {
            let (c0, r0) = circ(p).unwrap();
            let (c1, r1) = circ(q).unwrap();
            let d = (c1 - c0).length();
            if d < 1e-12 || d > r0 + r1 + TOL || d < (r0 - r1).abs() - TOL {
                vec![]
            } else {
                let a = (r0 * r0 - r1 * r1 + d * d) / (2.0 * d);
                let h = (r0 * r0 - a * a).max(0.0).sqrt();
                let m = c0 + (c1 - c0) * (a / d);
                let perp = (c1 - c0).perp() / d;
                vec![m + perp * h, m - perp * h]
            }
        }
    };
    cand.into_iter().filter(|x| p.param(*x).is_some() && q.param(*x).is_some()).collect()
}

/// Compute the minimal closed regions formed by the sketch's non-construction curves.
pub fn find_regions(s: &Sketch) -> Vec<Region> {
    let prims: Vec<Prim> = s
        .curves
        .iter()
        .filter(|c| !c.construction)
        .filter_map(|c| match c.geom {
            Geom::Line { a, b } => {
                let (a, b) = (s.pv(a), s.pv(b));
                ((b - a).length() > TOL).then_some(Prim::Line { a, b })
            }
            Geom::Circle { c, r } => (r > TOL).then(|| Prim::Arc { c: s.pv(c), r, t0: 0.0, t1: TAU }),
            Geom::Arc { c, a, b } => {
                let (cc, pa, pb) = (s.pv(c), s.pv(a), s.pv(b));
                let r = (pa - cc).length();
                let (t0, t1) = arc_angles(cc, pa, pb);
                (r > TOL).then_some(Prim::Arc { c: cc, r, t0, t1 })
            }
        })
        .collect();

    // split parameters per primitive
    let mut splits: Vec<Vec<f64>> = prims.iter().map(|p| if p.is_full_circle() { vec![0.0, 0.5, 1.0] } else { vec![0.0, 1.0] }).collect();
    for i in 0..prims.len() {
        for j in i + 1..prims.len() {
            for x in intersections(&prims[i], &prims[j]) {
                if let Some(u) = prims[i].param(x) {
                    splits[i].push(u);
                }
                if let Some(u) = prims[j].param(x) {
                    splits[j].push(u);
                }
            }
        }
    }

    // vertices (merged by tolerance) and edges
    let mut verts: Vec<DVec2> = vec![];
    let mut vid = |p: DVec2| -> usize {
        if let Some(i) = verts.iter().position(|v| (*v - p).length() < TOL * 10.0) {
            i
        } else {
            verts.push(p);
            verts.len() - 1
        }
    };
    struct Edge {
        a: usize,
        b: usize,
        seg: Seg,
    }
    let mut edges: Vec<Edge> = vec![];
    for (i, p) in prims.iter().enumerate() {
        let sp = &mut splits[i];
        sp.sort_by(f64::total_cmp);
        sp.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
        for w in sp.windows(2) {
            let seg = p.sub(w[0], w[1]);
            if (seg.start() - seg.end()).length() < TOL && matches!(seg, Seg::Line { .. }) {
                continue;
            }
            let (a, b) = (vid(p.at(w[0])), vid(p.at(w[1])));
            if a == b && matches!(seg, Seg::Line { .. }) {
                continue;
            }
            // skip duplicate overlapping edges
            if edges.iter().any(|e| ((e.a == a && e.b == b) || (e.a == b && e.b == a)) && (e.seg.mid() - seg.mid()).length() < TOL * 10.0) {
                continue;
            }
            edges.push(Edge { a, b, seg });
        }
    }

    // prune dangling edges
    let mut alive = vec![true; edges.len()];
    loop {
        let mut deg = vec![0usize; verts.len()];
        for (e, _) in edges.iter().zip(&alive).filter(|(_, a)| **a) {
            deg[e.a] += 1;
            deg[e.b] += 1;
        }
        let mut changed = false;
        for (i, e) in edges.iter().enumerate() {
            if alive[i] && (deg[e.a] < 2 || deg[e.b] < 2) {
                alive[i] = false;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // half-edges: 2*i is a->b, 2*i+1 is b->a
    let he_from = |h: usize| if h % 2 == 0 { edges[h / 2].a } else { edges[h / 2].b };
    let he_seg = |h: usize| if h % 2 == 0 { edges[h / 2].seg.clone() } else { edges[h / 2].seg.reversed() };
    let he_angle = |h: usize| {
        let s = he_seg(h);
        let d = s.at(1e-4) - s.start();
        let d2 = s.at(2e-2) - s.start();
        // primary: tangent; tie-break with chord direction a little further along
        (d.to_angle(), d2.to_angle())
    };
    let mut out: Vec<Vec<usize>> = vec![vec![]; verts.len()];
    for (i, _) in edges.iter().enumerate().filter(|(i, _)| alive[*i]) {
        out[he_from(2 * i)].push(2 * i);
        out[he_from(2 * i + 1)].push(2 * i + 1);
    }
    for o in &mut out {
        o.sort_by(|&x, &y| {
            let (a1, a2) = he_angle(x);
            let (b1, b2) = he_angle(y);
            if (a1 - b1).abs() > 1e-6 { a1.total_cmp(&b1) } else { a2.total_cmp(&b2) }
        });
    }
    let mut used = vec![false; edges.len() * 2];
    let mut loops: Vec<Loop> = vec![];
    for start in 0..edges.len() * 2 {
        if !alive[start / 2] || used[start] {
            continue;
        }
        let mut segs = vec![];
        let mut h = start;
        let mut guard = 0;
        loop {
            used[h] = true;
            segs.push(he_seg(h));
            let twin = h ^ 1;
            let v = he_from(twin);
            let ring = &out[v];
            let k = ring.iter().position(|&x| x == twin).unwrap();
            // next edge clockwise from the twin keeps the face on the left
            h = ring[(k + ring.len() - 1) % ring.len()];
            guard += 1;
            if h == start || used[h] || guard > 10000 {
                break;
            }
        }
        if h == start {
            loops.push(Loop::new(segs));
        }
    }

    let (faces, outers): (Vec<Loop>, Vec<Loop>) = loops.into_iter().partition(|l| l.area() > 1e-9);
    let mut regions: Vec<Region> = faces
        .into_iter()
        .map(|outer| Region { area: outer.area(), outer, holes: vec![], tris: vec![], sample: DVec2::ZERO })
        .collect();
    // Each clockwise component boundary is a hole of the smallest face that contains it
    // (and that is not bounded by the same component).
    for o in outers {
        let probe = o.poly[0];
        let inside_pt = o.segs[0].mid();
        let best = regions
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                point_in_poly(inside_pt, &r.outer.poly)
                    && point_in_poly(probe, &r.outer.poly)
                    && !r.outer.poly.iter().any(|q| (*q - probe).length() < TOL * 10.0)
            })
            .min_by(|a, b| a.1.area.total_cmp(&b.1.area))
            .map(|(i, _)| i);
        if let Some(i) = best {
            regions[i].area -= o.area().abs();
            regions[i].holes.push(o);
        }
    }
    for r in &mut regions {
        triangulate(r);
    }
    regions.retain(|r| !r.tris.is_empty());
    regions
}

fn triangulate(r: &mut Region) {
    let mut coords: Vec<f64> = vec![];
    let mut holes = vec![];
    for p in &r.outer.poly {
        coords.extend([p.x, p.y]);
    }
    for h in &r.holes {
        holes.push(coords.len() / 2);
        for p in &h.poly {
            coords.extend([p.x, p.y]);
        }
    }
    let Ok(idx) = earcutr::earcut(&coords, &holes, 2) else { return };
    let pt = |i: usize| DVec2::new(coords[2 * i], coords[2 * i + 1]);
    r.tris = idx.chunks(3).map(|t| [pt(t[0]), pt(t[1]), pt(t[2])]).collect();
    let best = r
        .tris
        .iter()
        .max_by(|a, b| tri_area(a).total_cmp(&tri_area(b)))
        .copied();
    if let Some(t) = best {
        r.sample = (t[0] + t[1] + t[2]) / 3.0;
    }
}

fn tri_area(t: &[DVec2; 3]) -> f64 {
    ((t[1] - t[0]).perp_dot(t[2] - t[0]) * 0.5).abs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sketch::Geom;

    fn rect(s: &mut Sketch, x0: f64, y0: f64, x1: f64, y1: f64) {
        let p: Vec<_> = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        for i in 0..4 {
            s.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false);
        }
    }

    #[test]
    fn rect_with_hole() {
        let mut s = Sketch::default();
        rect(&mut s, 0.0, 0.0, 40.0, 20.0);
        let c = s.add_point(DVec2::new(20.0, 10.0));
        s.add_curve(Geom::Circle { c, r: 5.0 }, false);
        let r = find_regions(&s);
        assert_eq!(r.len(), 2, "{r:?}");
        let big = r.iter().find(|r| r.holes.len() == 1).unwrap();
        assert!((big.area - (800.0 - std::f64::consts::PI * 25.0)).abs() < 2.0, "{}", big.area);
    }

    #[test]
    fn overlapping_rects() {
        let mut s = Sketch::default();
        rect(&mut s, 0.0, 0.0, 20.0, 20.0);
        rect(&mut s, 10.0, 10.0, 30.0, 30.0);
        assert_eq!(find_regions(&s).len(), 3);
    }

    #[test]
    fn circle_crossing_line() {
        let mut s = Sketch::default();
        let c = s.add_point(DVec2::ZERO);
        s.add_curve(Geom::Circle { c, r: 10.0 }, false);
        let a = s.add_point(DVec2::new(-20.0, 0.0));
        let b = s.add_point(DVec2::new(20.0, 0.0));
        s.add_curve(Geom::Line { a, b }, false);
        let r = find_regions(&s);
        assert_eq!(r.len(), 2);
    }
}

// ------------------------------------------------------------- trimming helpers

fn prim_of(s: &Sketch, c: &crate::sketch::Curve) -> Option<Prim> {
    match c.geom {
        Geom::Line { a, b } => Some(Prim::Line { a: s.pv(a), b: s.pv(b) }),
        Geom::Circle { c, r } => Some(Prim::Arc { c: s.pv(c), r, t0: 0.0, t1: TAU }),
        Geom::Arc { c, a, b } => {
            let (cc, pa, pb) = (s.pv(c), s.pv(a), s.pv(b));
            let (t0, t1) = arc_angles(cc, pa, pb);
            Some(Prim::Arc { c: cc, r: (pa - cc).length(), t0, t1 })
        }
    }
}

/// Sorted parameters (0..1, including the ends) where other curves cross curve `id`.
pub fn split_params(s: &Sketch, id: crate::sketch::Id) -> Vec<f64> {
    let Some(c) = s.curve(id) else { return vec![] };
    let Some(p) = prim_of(s, c) else { return vec![] };
    let mut out = vec![0.0, 1.0];
    for o in &s.curves {
        if o.id == id {
            continue;
        }
        if let Some(q) = prim_of(s, o) {
            for x in intersections(&p, &q) {
                if let Some(u) = p.param(x) {
                    out.push(u);
                }
            }
        }
    }
    out.sort_by(f64::total_cmp);
    out.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    out
}

/// Parameter of the point on curve `id` closest to `p`.
pub fn param_at(s: &Sketch, id: crate::sketch::Id, p: DVec2) -> Option<f64> {
    let c = s.curve(id)?;
    match prim_of(s, c)? {
        Prim::Line { a, b } => Some(((p - a).dot(b - a) / (b - a).length_squared().max(1e-18)).clamp(0.0, 1.0)),
        pr @ Prim::Arc { c, r, .. } => {
            let q = c + (p - c).normalize_or_zero() * r;
            pr.param(q).or(Some(0.0))
        }
    }
}

pub fn point_at(s: &Sketch, id: crate::sketch::Id, u: f64) -> DVec2 {
    s.curve(id).and_then(|c| prim_of(s, c)).map(|p| p.at(u)).unwrap_or_default()
}

/// Intersections of the infinite line through `a` with direction `d` and curve `id`,
/// as distances along `d`.
pub fn ray_hits(s: &Sketch, a: DVec2, d: DVec2, skip: crate::sketch::Id) -> Vec<f64> {
    let far = Prim::Line { a: a - d * 1e6, b: a + d * 1e6 };
    let mut out = vec![];
    for c in &s.curves {
        if c.id == skip {
            continue;
        }
        if let Some(q) = prim_of(s, c) {
            for x in intersections(&far, &q) {
                out.push((x - a).dot(d));
            }
        }
    }
    out
}
