//! Closed regions of a sketch: curves are split at their intersections into a
//! planar graph whose minimal faces are the regions you can click and extrude.

use std::f64::consts::TAU;

use glam::DVec2;

use crate::sketch::{Geom, Sketch, arc_angles};

const TOL: f64 = 1e-6;

/// A boundary piece in sketch coordinates, oriented along the loop.
#[derive(Clone, Debug)]
pub enum Seg {
    Line {
        a: DVec2,
        b: DVec2,
    },
    /// From angle `t0` to `t1` (t1 < t0 means clockwise).
    Arc {
        c: DVec2,
        r: f64,
        t0: f64,
        t1: f64,
    },
    /// A free-form piece (spline, ellipse) through these points.
    Poly {
        pts: Vec<DVec2>,
        closed: bool,
    },
}

impl Seg {
    pub fn start(&self) -> DVec2 {
        match *self {
            Seg::Line { a, .. } => a,
            Seg::Arc { c, r, t0, .. } => c + DVec2::from_angle(t0) * r,
            Seg::Poly { ref pts, .. } => pts[0],
        }
    }
    pub fn end(&self) -> DVec2 {
        match *self {
            Seg::Line { b, .. } => b,
            Seg::Arc { c, r, t1, .. } => c + DVec2::from_angle(t1) * r,
            Seg::Poly { ref pts, closed } => {
                if closed {
                    pts[0]
                } else {
                    pts[pts.len() - 1]
                }
            }
        }
    }
    pub fn mid(&self) -> DVec2 {
        self.at(0.5)
    }
    pub fn at(&self, u: f64) -> DVec2 {
        match *self {
            Seg::Line { a, b } => a.lerp(b, u),
            Seg::Arc { c, r, t0, t1 } => c + DVec2::from_angle(t0 + (t1 - t0) * u) * r,
            Seg::Poly { ref pts, closed } => {
                let mut p = pts.clone();
                if closed {
                    p.push(pts[0]);
                }
                let f = u.clamp(0.0, 1.0) * (p.len() - 1) as f64;
                let i = (f.floor() as usize).min(p.len() - 2);
                p[i].lerp(p[i + 1], f - i as f64)
            }
        }
    }
    pub fn reversed(&self) -> Seg {
        match *self {
            Seg::Line { a, b } => Seg::Line { a: b, b: a },
            Seg::Arc { c, r, t0, t1 } => Seg::Arc { c, r, t0: t1, t1: t0 },
            Seg::Poly { ref pts, closed } => {
                let mut p = pts.clone();
                p.reverse();
                if closed {
                    p.rotate_right(1);
                }
                Seg::Poly { pts: p, closed }
            }
        }
    }
    /// This piece's share of the area its loop encloses: ½∮(x dy − y dx) along it.
    pub fn area_term(&self) -> f64 {
        match *self {
            Seg::Line { a, b } => a.perp_dot(b) * 0.5,
            Seg::Arc { c, r, t0, t1 } => 0.5 * (r * r * (t1 - t0) + r * c.x * (t1.sin() - t0.sin()) - r * c.y * (t1.cos() - t0.cos())),
            Seg::Poly { ref pts, closed } => {
                let n = if closed { pts.len() } else { pts.len() - 1 };
                (0..n).map(|i| pts[i].perp_dot(pts[(i + 1) % pts.len()])).sum::<f64>() * 0.5
            }
        }
    }
    pub fn polyline(&self, out: &mut Vec<DVec2>) {
        match *self {
            Seg::Poly { ref pts, closed } => {
                let n = if closed { pts.len() } else { pts.len() - 1 };
                out.extend_from_slice(&pts[..n]);
            }
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
    /// Signed area (counter-clockwise positive), exact for lines and arcs rather than from the
    /// polygon drawn through them.
    pub fn area(&self) -> f64 {
        self.segs.iter().map(Seg::area_term).sum()
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
    Line {
        a: DVec2,
        b: DVec2,
    },
    /// Arc from angle t0 to t1 (t1 > t0), full circle when t1 - t0 = TAU.
    Arc {
        c: DVec2,
        r: f64,
        t0: f64,
        t1: f64,
    },
    /// Polyline (spline, ellipse, text outline); closed ones end where they start.
    Poly {
        pts: Vec<DVec2>,
        closed: bool,
    },
}

impl Prim {
    fn at(&self, u: f64) -> DVec2 {
        match *self {
            Prim::Line { a, b } => a.lerp(b, u),
            Prim::Arc { c, r, t0, t1 } => c + DVec2::from_angle(t0 + (t1 - t0) * u) * r,
            Prim::Poly { ref pts, .. } => {
                let f = u.clamp(0.0, 1.0) * (pts.len() - 1) as f64;
                let i = (f.floor() as usize).min(pts.len() - 2);
                pts[i].lerp(pts[i + 1], f - i as f64)
            }
        }
    }
    fn segments(&self) -> Vec<(DVec2, DVec2)> {
        match self {
            Prim::Poly { pts, .. } => pts.windows(2).map(|w| (w[0], w[1])).collect(),
            Prim::Line { a, b } => vec![(*a, *b)],
            Prim::Arc { .. } => {
                let n = 96;
                (0..n).map(|i| (self.at(i as f64 / n as f64), self.at((i + 1) as f64 / n as f64))).collect()
            }
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
            Prim::Poly { ref pts, .. } => {
                let n = (pts.len() - 1) as f64;
                let mut best: Option<(f64, f64)> = None;
                for (i, w) in pts.windows(2).enumerate() {
                    let d = w[1] - w[0];
                    let t = if d.length_squared() < 1e-18 { 0.0 } else { ((p - w[0]).dot(d) / d.length_squared()).clamp(0.0, 1.0) };
                    let dist = (w[0] + d * t - p).length();
                    if best.map(|b| dist < b.0).unwrap_or(true) {
                        best = Some((dist, (i as f64 + t) / n));
                    }
                }
                best.filter(|b| b.0 < TOL * 100.0).map(|b| b.1)
            }
        }
    }
    fn sub(&self, u0: f64, u1: f64) -> Seg {
        match *self {
            Prim::Line { a, b } => Seg::Line { a: a.lerp(b, u0), b: a.lerp(b, u1) },
            Prim::Arc { c, r, t0, t1 } => Seg::Arc { c, r, t0: t0 + (t1 - t0) * u0, t1: t0 + (t1 - t0) * u1 },
            Prim::Poly { ref pts, .. } => {
                let n = (pts.len() - 1) as f64;
                let mut v = vec![self.at(u0)];
                for (i, p) in pts.iter().enumerate() {
                    let u = i as f64 / n;
                    if u > u0 + 1e-12 && u < u1 - 1e-12 {
                        v.push(*p);
                    }
                }
                v.push(self.at(u1));
                Seg::Poly { pts: v, closed: false }
            }
        }
    }
    fn is_full_circle(&self) -> bool {
        matches!(*self, Prim::Arc { t0, t1, .. } if (t1 - t0 - TAU).abs() < 1e-12) || matches!(*self, Prim::Poly { closed: true, .. })
    }
}

fn seg_seg(a: DVec2, b: DVec2, c: DVec2, d: DVec2) -> Option<DVec2> {
    let r = b - a;
    let s = d - c;
    let den = r.perp_dot(s);
    if den.abs() < 1e-14 {
        return None;
    }
    let t = (c - a).perp_dot(s) / den;
    let u = (c - a).perp_dot(r) / den;
    let eps = 1e-9;
    (t >= -eps && t <= 1.0 + eps && u >= -eps && u <= 1.0 + eps).then(|| a + r * t)
}

fn intersections(p: &Prim, q: &Prim) -> Vec<DVec2> {
    if matches!(p, Prim::Poly { .. }) || matches!(q, Prim::Poly { .. }) {
        let (sp, sq) = (p.segments(), q.segments());
        let mut out: Vec<DVec2> = vec![];
        for (a, b) in &sp {
            for (c, d) in &sq {
                if let Some(x) = seg_seg(*a, *b, *c, *d) {
                    if !out.iter().any(|o| (*o - x).length() < TOL * 10.0) {
                        out.push(x);
                    }
                }
            }
        }
        return out.into_iter().filter(|x| p.param(*x).is_some() && q.param(*x).is_some()).collect();
    }
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
            _ => {
                let closed = matches!(c.geom, Geom::Ellipse { .. } | Geom::Spline { closed: true, .. } | Geom::Poly { closed: true, .. });
                let pts = s.tessellate(c, 0.5);
                (pts.len() >= 2).then_some(Prim::Poly { pts, closed })
            }
        })
        .collect();

    // split parameters per primitive
    let mut splits: Vec<Vec<f64>> = prims.iter().map(|p| if p.is_full_circle() { vec![0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0] } else { vec![0.0, 1.0] }).collect();
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
            // a sliver between two splits that merged into one vertex (e.g. where a line meets an arc
            // tangentially) is no edge; only a whole circle may start and end at the same vertex
            let sliver = match seg {
                Seg::Arc { r, t0, t1, .. } => r * (t1 - t0).abs() < TOL * 100.0,
                Seg::Poly { ref pts, .. } => pts.windows(2).map(|w| (w[1] - w[0]).length()).sum::<f64>() < TOL * 100.0,
                Seg::Line { .. } => true,
            };
            if a == b && sliver {
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
    let mut regions: Vec<Region> = faces.into_iter().map(|outer| Region { area: outer.area(), outer, holes: vec![], tris: vec![], sample: DVec2::ZERO }).collect();
    // Each clockwise component boundary is a hole of the smallest face that contains it
    // (and that is not bounded by the same component).
    for o in outers {
        let probe = o.poly[0];
        let inside_pt = o.segs[0].mid();
        let best = regions
            .iter()
            .enumerate()
            .filter(|(_, r)| point_in_poly(inside_pt, &r.outer.poly) && point_in_poly(probe, &r.outer.poly) && !r.outer.poly.iter().any(|q| (*q - probe).length() < TOL * 10.0))
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
    let best = r.tris.iter().max_by(|a, b| tri_area(a).total_cmp(&tri_area(b))).copied();
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
        // exact, not the area of the polygon drawn through the circle
        assert!((big.area - (800.0 - std::f64::consts::PI * 25.0)).abs() < 1e-9, "{}", big.area);
        let small = r.iter().find(|r| r.holes.is_empty()).unwrap();
        assert!((small.area - std::f64::consts::PI * 25.0).abs() < 1e-9, "{}", small.area);
    }

    #[test]
    fn half_disc_area_is_exact() {
        let mut s = Sketch::default();
        let c = s.add_point(DVec2::new(3.0, -2.0));
        s.add_curve(Geom::Circle { c, r: 10.0 }, false);
        let a = s.add_point(DVec2::new(-20.0, -2.0));
        let b = s.add_point(DVec2::new(20.0, -2.0));
        s.add_curve(Geom::Line { a, b }, false);
        for r in find_regions(&s) {
            assert!((r.area - std::f64::consts::PI * 50.0).abs() < 1e-9, "{}", r.area);
        }
    }

    #[test]
    fn overlapping_rects() {
        let mut s = Sketch::default();
        rect(&mut s, 0.0, 0.0, 20.0, 20.0);
        rect(&mut s, 10.0, 10.0, 30.0, 30.0);
        assert_eq!(find_regions(&s).len(), 3);
    }

    /// A slot as the sketcher's slot tool draws it (two lines tangent to two end arcs), optionally
    /// solved with a radius and a centre-to-centre dimension so the points carry solver round-off.
    fn slot(c1: DVec2, c2: DVec2, r: f64, dims: Option<(f64, f64)>) -> Sketch {
        use crate::expr::{Env, Units};
        use crate::sketch::{Constraint, Dim};
        let mut s = Sketch::default();
        let n = (c2 - c1).perp().normalize();
        let a = s.add_point(c1);
        let b = s.add_point(c2);
        let p1 = s.add_point(c1 + n * r);
        let p2 = s.add_point(c2 + n * r);
        let p3 = s.add_point(c2 - n * r);
        let p4 = s.add_point(c1 - n * r);
        let l1 = s.add_curve(Geom::Line { a: p1, b: p2 }, false);
        let l2 = s.add_curve(Geom::Line { a: p3, b: p4 }, false);
        let a1 = s.add_curve(Geom::Arc { c: b, a: p3, b: p2 }, false);
        let a2 = s.add_curve(Geom::Arc { c: a, a: p1, b: p4 }, false);
        let axis = s.add_curve(Geom::Line { a, b }, true);
        for (l, ar) in [(l1, a1), (l1, a2), (l2, a1), (l2, a2)] {
            s.constraints.push(Constraint::Tangent { a: l, b: ar, inner: false });
        }
        s.constraints.push(Constraint::Equal { a: a1, b: a2 });
        s.constraints.push(Constraint::Parallel { a: axis, b: l1 });
        if let Some((rad, len)) = dims {
            s.constraints.push(Constraint::Radius { c: a1, dim: Dim::new(format!("{rad}"), [0.0; 2]) });
            s.constraints.push(Constraint::Distance { a, b, dim: Dim::new(format!("{len}"), [0.0; 2]) });
            let vars = std::collections::HashMap::new();
            assert!(s.solve(&Env { vars: &vars, units: Units::Mm }, &[a]).ok);
        }
        s
    }

    /// Dragging a slot's points or sides about (in small steps, as the mouse does) must leave a
    /// slot: once the tangent joints could fold into cusps, about one drag in nine turned it into
    /// a bow tie, a half-round with a stray line, or an arc swept the long way round.
    #[test]
    fn dragged_slot_stays_a_slot() {
        use crate::expr::{Env, Units};
        use crate::sketch::arc_angles;
        let vars = std::collections::HashMap::new();
        let env = Env { vars: &vars, units: Units::Mm };
        let mut seed = 12345u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 100000) as f64 / 100000.0
        };
        let mut bad = vec![];
        for it in 0..600 {
            let c1 = DVec2::new(rnd() * 40.0 - 20.0, rnd() * 40.0 - 20.0);
            let len = 0.5 + rnd() * 30.0;
            let c2 = c1 + DVec2::from_angle(rnd() * 6.28) * len;
            let r = 0.2 + rnd() * 15.0;
            let mut s = slot(c1, c2, r, None);
            // centres, then p1 p2 (one side) and p3 p4 (the other); 6 and 7 drag a whole side
            let pts: Vec<_> = s.points.iter().map(|p| p.id).collect();
            let which = (rnd() * 8.0) as usize;
            let held: Vec<_> = match which {
                6 => vec![pts[2], pts[3]],
                7 => vec![pts[4], pts[5]],
                i => vec![pts[i]],
            };
            let d = DVec2::new(rnd() * 40.0 - 20.0, rnd() * 40.0 - 20.0);
            let p0: Vec<DVec2> = held.iter().map(|h| s.pv(*h)).collect();
            let mut ok = true;
            for k in 1..=20 {
                for (h, p) in held.iter().zip(&p0) {
                    let q = *p + d * (k as f64 / 20.0);
                    let pm = s.point_mut(*h).unwrap();
                    (pm.x, pm.y) = (q.x, q.y);
                }
                ok = s.solve(&env, &held).ok;
            }
            // (dragged onto the other centre, a slot is a circle: the pieces left are too small to keep apart)
            if !ok || (s.pv(pts[1]) - s.pv(pts[0])).length() < 0.01 * r {
                continue;
            }
            let sweeps: Vec<f64> = s
                .curves
                .iter()
                .filter_map(|c| match c.geom {
                    Geom::Arc { c, a, b } => {
                        let (t0, t1) = arc_angles(s.pv(c), s.pv(a), s.pv(b));
                        Some((t1 - t0).to_degrees())
                    }
                    _ => None,
                })
                .collect();
            let n = find_regions(&s).len();
            if n != 1 || sweeps.iter().any(|w| (w - 180.0).abs() > 0.5) {
                bad.push(format!("drag {it} (point {which} by {d}): {n} regions, arcs sweep {sweeps:?}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn slot_is_one_region() {
        let mut bad = vec![];
        for (c1, c2, r) in [
            (DVec2::ZERO, DVec2::new(20.0, 0.0), 4.0),
            (DVec2::new(1.0, 2.0), DVec2::new(13.0, 9.0), 3.0),
            (DVec2::new(-5.0, -5.0), DVec2::new(-5.0, 15.0), 2.5),
            (DVec2::new(0.0, 0.0), DVec2::new(10.0, 6.0), 1.0),
            (DVec2::new(0.0, 0.0), DVec2::new(30.0, 17.0), 4.0),
            (DVec2::new(0.0, 0.0), DVec2::new(0.4, 0.3), 0.1),
            (DVec2::new(100.0, 50.0), DVec2::new(160.0, 85.0), 12.0),
        ] {
            for dims in [None, Some((4.0, 8.0)), Some((2.0, 15.0)), Some((0.5, 3.0))] {
                let s = slot(c1, c2, r, dims);
                let regs = find_regions(&s);
                let (rr, len) = dims.unwrap_or((r, (c2 - c1).length()));
                let want = std::f64::consts::PI * rr * rr + 2.0 * rr * len;
                if regs.len() != 1 || (regs[0].area - want).abs() > 1e-4 * want {
                    bad.push(format!("{c1} {c2} r{r} {dims:?}: {} regions, areas {:?} want {want}", regs.len(), regs.iter().map(|r| r.area).collect::<Vec<_>>()));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
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
        _ => {
            let closed = matches!(c.geom, Geom::Ellipse { .. } | Geom::Spline { closed: true, .. } | Geom::Poly { closed: true, .. });
            Some(Prim::Poly { pts: s.tessellate(c, 0.5), closed })
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
        pr @ Prim::Poly { .. } => {
            // nearest point on the polyline
            let segs = pr.segments();
            let n = segs.len() as f64;
            let mut best = (f64::INFINITY, 0.0);
            for (i, (a, b)) in segs.iter().enumerate() {
                let d = *b - *a;
                let t = if d.length_squared() < 1e-18 { 0.0 } else { ((p - *a).dot(d) / d.length_squared()).clamp(0.0, 1.0) };
                let dist = (*a + d * t - p).length();
                if dist < best.0 {
                    best = (dist, (i as f64 + t) / n);
                }
            }
            Some(best.1)
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
