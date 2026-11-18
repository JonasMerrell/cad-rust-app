//! Collision geometry for the motion simulation: triangle BVHs of rigid groups, and the exact
//! distance between two convex point sets (GJK). Each group only translates, so a straight
//! move sweeps a triangle into the convex hull of its two end positions and the distance over
//! the whole move is one GJK query: nothing between two samples can be missed.

use glam::DVec3;

// ---------------------------------------------------------------- GJK

#[derive(Clone, Copy)]
struct SV {
    w: DVec3,
    a: DVec3,
    b: DVec3,
}

fn support(pts: &[DVec3], d: DVec3) -> DVec3 {
    let mut best = pts[0];
    let mut bd = best.dot(d);
    for p in &pts[1..] {
        let v = p.dot(d);
        if v > bd {
            bd = v;
            best = *p;
        }
    }
    best
}

/// Closest point of the simplex to the origin, as barycentric weights; reduces the simplex to
/// the points that carry it.
fn closest(s: &mut Vec<SV>) -> (DVec3, Vec<f64>) {
    match s.len() {
        1 => (s[0].w, vec![1.0]),
        2 => {
            let (a, b) = (s[0].w, s[1].w);
            let ab = b - a;
            let t = (-a.dot(ab) / ab.length_squared().max(1e-300)).clamp(0.0, 1.0);
            if t <= 0.0 {
                s.truncate(1);
                (a, vec![1.0])
            } else if t >= 1.0 {
                s.remove(0);
                (b, vec![1.0])
            } else {
                (a + ab * t, vec![1.0 - t, t])
            }
        }
        3 => {
            let (p, l) = closest_tri(s[0].w, s[1].w, s[2].w);
            let keep: Vec<usize> = (0..3).filter(|i| l[*i] > 0.0).collect();
            let ws: Vec<f64> = keep.iter().map(|i| l[*i]).collect();
            *s = keep.iter().map(|i| s[*i]).collect();
            (p, ws)
        }
        _ => {
            // tetrahedron: inside → touching; else the closest face
            let (a, b, c, d) = (s[0].w, s[1].w, s[2].w, s[3].w);
            let faces = [[0, 1, 2, 3], [0, 1, 3, 2], [0, 2, 3, 1], [1, 2, 3, 0]];
            let mut best: Option<(f64, DVec3, [usize; 3], [f64; 3])> = None;
            let mut inside = true;
            let pts = [a, b, c, d];
            for f in faces {
                let (p0, p1, p2, opp) = (pts[f[0]], pts[f[1]], pts[f[2]], pts[f[3]]);
                let n = (p1 - p0).cross(p2 - p0);
                let so = n.dot(-p0);
                let sd = n.dot(opp - p0);
                if so * sd < 0.0 || sd == 0.0 {
                    // origin outside this face
                    inside = false;
                    let (q, l) = closest_tri(p0, p1, p2);
                    let dd = q.length_squared();
                    if best.map(|b| dd < b.0).unwrap_or(true) {
                        best = Some((dd, q, [f[0], f[1], f[2]], l));
                    }
                }
            }
            if inside {
                return (DVec3::ZERO, vec![]);
            }
            let (_, q, idx, l) = best.unwrap();
            let keep: Vec<usize> = (0..3).filter(|i| l[*i] > 0.0).collect();
            let ws: Vec<f64> = keep.iter().map(|i| l[*i]).collect();
            *s = keep.iter().map(|i| s[idx[*i]]).collect();
            (q, ws)
        }
    }
}

/// Closest point of triangle abc to the origin and its barycentric weights (Ericson 5.1.5).
fn closest_tri(a: DVec3, b: DVec3, c: DVec3) -> (DVec3, [f64; 3]) {
    let p = DVec3::ZERO;
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return (a, [1.0, 0.0, 0.0]);
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return (b, [0.0, 1.0, 0.0]);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return (a + ab * v, [1.0 - v, v, 0.0]);
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return (c, [0.0, 0.0, 1.0]);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return (a + ac * w, [1.0 - w, 0.0, w]);
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return (b + (c - b) * w, [0.0, 1.0 - w, w]);
    }
    let den = 1.0 / (va + vb + vc);
    let (v, w) = (vb * den, vc * den);
    (a + ab * v + ac * w, [1.0 - v - w, v, w])
}

fn centre(p: &[DVec3]) -> DVec3 {
    p.iter().copied().sum::<DVec3>() / p.len() as f64
}

/// Distance between the convex hulls of two point sets, with the closest points (on A, on B).
/// 0 when they touch or overlap.
pub fn gjk(a: &[DVec3], b: &[DVec3]) -> (f64, DVec3, DVec3) {
    let sup = |d: DVec3| -> SV {
        let pa = support(a, -d);
        let pb = support(b, d);
        SV { w: pa - pb, a: pa, b: pb }
    };
    let first = SV { w: a[0] - b[0], a: a[0], b: b[0] };
    let mut s: Vec<SV> = vec![first];
    let mut v = first.w;
    let mut lam = vec![1.0];
    for _ in 0..64 {
        let vv = v.length_squared();
        if vv < 1e-18 {
            return (0.0, centre(b), centre(b));
        }
        let w = sup(v);
        // no progress: v is the closest point
        if vv - v.dot(w.w) <= 1e-10 * vv.max(1e-12) || s.iter().any(|p| (p.w - w.w).length_squared() < 1e-20) {
            break;
        }
        s.push(w);
        let (nv, l) = closest(&mut s);
        if l.is_empty() {
            return (0.0, centre(b), centre(b));
        }
        if nv.length_squared() >= vv - 1e-15 {
            // numerically stuck; keep the better one
            if nv.length_squared() < vv {
                v = nv;
                lam = l;
            }
            break;
        }
        v = nv;
        lam = l;
    }
    let (mut pa, mut pb) = (DVec3::ZERO, DVec3::ZERO);
    let tot: f64 = lam.iter().sum::<f64>().max(1e-300);
    for (p, l) in s.iter().zip(&lam) {
        pa += p.a * (*l / tot);
        pb += p.b * (*l / tot);
    }
    (v.length(), pa, pb)
}

// ---------------------------------------------------------------- BVH

#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub lo: DVec3,
    pub hi: DVec3,
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb { lo: DVec3::splat(f64::INFINITY), hi: DVec3::splat(f64::NEG_INFINITY) };
    pub fn add(&mut self, p: DVec3) {
        self.lo = self.lo.min(p);
        self.hi = self.hi.max(p);
    }
    /// Swept by a translation from r0 to r1 and grown by `pad`.
    pub fn swept(&self, r0: DVec3, r1: DVec3, pad: f64) -> Aabb {
        Aabb { lo: self.lo + r0.min(r1) - DVec3::splat(pad), hi: self.hi + r0.max(r1) + DVec3::splat(pad) }
    }
    pub fn overlaps(&self, o: &Aabb) -> bool {
        self.lo.x <= o.hi.x && self.hi.x >= o.lo.x && self.lo.y <= o.hi.y && self.hi.y >= o.lo.y && self.lo.z <= o.hi.z && self.hi.z >= o.lo.z
    }
    fn volume(&self) -> f64 {
        let d = (self.hi - self.lo).max(DVec3::ZERO);
        d.x * d.y * d.z
    }
}

#[derive(Clone, Debug)]
struct Node {
    bb: Aabb,
    /// leaf: first triangle; inner: left child (right is left + 1 for the first child... see `right`)
    start: u32,
    count: u32,
    right: u32,
}

/// Triangles of one rigid group (world coordinates at the reference pose), with the body each
/// came from.
#[derive(Clone, Debug, Default)]
pub struct Bvh {
    pub tris: Vec<[DVec3; 3]>,
    pub body: Vec<u32>,
    nodes: Vec<Node>,
}

const LEAF: usize = 4;

impl Bvh {
    pub fn new(tris: Vec<[DVec3; 3]>, body: Vec<u32>) -> Bvh {
        let mut b = Bvh { tris, body, nodes: vec![] };
        if b.tris.is_empty() {
            return b;
        }
        let n = b.tris.len();
        let mut idx: Vec<u32> = (0..n as u32).collect();
        let cent: Vec<DVec3> = b.tris.iter().map(|t| (t[0] + t[1] + t[2]) / 3.0).collect();
        b.build(&mut idx, &cent, 0, n);
        // reorder triangles to leaf order
        let tris: Vec<[DVec3; 3]> = idx.iter().map(|i| b.tris[*i as usize]).collect();
        let body: Vec<u32> = idx.iter().map(|i| b.body[*i as usize]).collect();
        b.tris = tris;
        b.body = body;
        b
    }

    fn build(&mut self, idx: &mut [u32], cent: &[DVec3], start: usize, end: usize) -> u32 {
        let mut bb = Aabb::EMPTY;
        let mut cb = Aabb::EMPTY;
        for i in &idx[start..end] {
            for p in &self.tris[*i as usize] {
                bb.add(*p);
            }
            cb.add(cent[*i as usize]);
        }
        let me = self.nodes.len() as u32;
        self.nodes.push(Node { bb, start: start as u32, count: (end - start) as u32, right: 0 });
        if end - start <= LEAF {
            return me;
        }
        let ext = cb.hi - cb.lo;
        let ax = if ext.x >= ext.y && ext.x >= ext.z {
            0
        } else if ext.y >= ext.z {
            1
        } else {
            2
        };
        let mid = (start + end) / 2;
        idx[start..end].select_nth_unstable_by(mid - start, |a, b| cent[*a as usize][ax].partial_cmp(&cent[*b as usize][ax]).unwrap_or(std::cmp::Ordering::Equal));
        self.build(idx, cent, start, mid);
        let r = self.build(idx, cent, mid, end);
        let n = &mut self.nodes[me as usize];
        n.count = 0;
        n.right = r;
        me
    }

    pub fn bounds(&self) -> Aabb {
        self.nodes.first().map(|n| n.bb).unwrap_or(Aabb::EMPTY)
    }

    pub fn is_empty(&self) -> bool {
        self.tris.is_empty()
    }
}

/// The closest a moving triangle came to a still one during a move.
#[derive(Clone, Copy, Debug)]
pub struct Near {
    pub dist: f64,
    pub body_a: u32,
    pub body_b: u32,
    /// Where (world, on the still group's side), and how far along the move (0..1).
    pub at: DVec3,
    pub t: f64,
}

/// Group A moves from translation r0 to r1 (relative to B); every body pair that comes within
/// `reach` (and isn't `skip`ped) is reported with the closest it gets, at most one per pair.
pub fn sweep(a: &Bvh, b: &Bvh, r0: DVec3, r1: DVec3, reach: f64, skip: &dyn Fn(u32, u32) -> bool, out: &mut Vec<Near>) {
    if a.nodes.is_empty() || b.nodes.is_empty() {
        return;
    }
    let mut stack: Vec<(u32, u32)> = vec![(0, 0)];
    let mut pts_a = [DVec3::ZERO; 6];
    while let Some((na, nb)) = stack.pop() {
        let (x, y) = (&a.nodes[na as usize], &b.nodes[nb as usize]);
        if !x.bb.swept(r0, r1, reach).overlaps(&y.bb) {
            continue;
        }
        let (xl, yl) = (x.count > 0, y.count > 0);
        if xl && yl {
            for i in x.start..x.start + x.count {
                let ta = &a.tris[i as usize];
                let mut tb_box = Aabb::EMPTY;
                for p in ta {
                    tb_box.add(*p);
                }
                let swept = tb_box.swept(r0, r1, reach);
                for k in 0..3 {
                    pts_a[k] = ta[k] + r0;
                    pts_a[k + 3] = ta[k] + r1;
                }
                for j in y.start..y.start + y.count {
                    let (ba, bb) = (a.body[i as usize], b.body[j as usize]);
                    if skip(ba, bb) {
                        continue;
                    }
                    let tb = &b.tris[j as usize];
                    let mut bx = Aabb::EMPTY;
                    for p in tb {
                        bx.add(*p);
                    }
                    if !swept.overlaps(&bx) {
                        continue;
                    }
                    // GJK on the swept hull is the quick filter; the exact distance comes from the
                    // triangles themselves along the move (GJK can call flat, sliding hulls touching)
                    // the hull distance is a lower bound, so a pair already closer elsewhere needs no refine
                    let lower = gjk(&pts_a, tb).0;
                    if lower > reach {
                        continue;
                    }
                    if let Some(n) = out.iter().find(|n| n.body_a == ba && n.body_b == bb) {
                        if n.dist < lower || (n.dist <= 1e-9 && lower > 1e-9) {
                            continue;
                        }
                    }
                    let (d, t, pb) = refine(ta, tb, r0, r1);
                    if d > reach {
                        continue;
                    }
                    match out.iter_mut().find(|n| n.body_a == ba && n.body_b == bb) {
                        // both touching: keep the first touch along the move
                        Some(n) if n.dist <= 1e-9 && d <= 1e-9 => {
                            if t < n.t {
                                *n = Near { dist: d, body_a: ba, body_b: bb, at: pb, t };
                            }
                        }
                        Some(n) if n.dist <= d => {}
                        Some(n) => *n = Near { dist: d, body_a: ba, body_b: bb, at: pb, t },
                        None => out.push(Near { dist: d, body_a: ba, body_b: bb, at: pb, t }),
                    }
                }
            }
            continue;
        }
        // open the bigger box (or the one that isn't a leaf)
        let open_a = !xl && (yl || x.bb.volume() >= y.bb.volume());
        if open_a {
            stack.push((na + 1, nb));
            stack.push((x.right, nb));
        } else {
            stack.push((na, nb + 1));
            stack.push((na, y.right));
        }
    }
}

fn seg_seg(p1: DVec3, q1: DVec3, p2: DVec3, q2: DVec3) -> (f64, DVec3) {
    // Ericson 5.1.9
    let (d1, d2, r) = (q1 - p1, q2 - p2, p1 - p2);
    let (a, e, f) = (d1.length_squared(), d2.length_squared(), d2.dot(r));
    let (s, t);
    if a <= 1e-18 && e <= 1e-18 {
        return ((p1 - p2).length(), p2);
    }
    if a <= 1e-18 {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = d1.dot(r);
        if e <= 1e-18 {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = d1.dot(d2);
            let den = a * e - b * b;
            let mut ss = if den > 1e-18 { ((b * f - c * e) / den).clamp(0.0, 1.0) } else { 0.0 };
            let mut tt = (b * ss + f) / e;
            if tt < 0.0 {
                tt = 0.0;
                ss = (-c / a).clamp(0.0, 1.0);
            } else if tt > 1.0 {
                tt = 1.0;
                ss = ((b - c) / a).clamp(0.0, 1.0);
            }
            s = ss;
            t = tt;
        }
    }
    let (c1, c2) = (p1 + d1 * s, p2 + d2 * t);
    ((c1 - c2).length(), c2)
}

/// Does segment pq cross triangle t? (where)
fn seg_tri(p: DVec3, q: DVec3, t: &[DVec3; 3]) -> Option<DVec3> {
    let d = q - p;
    let (e1, e2) = (t[1] - t[0], t[2] - t[0]);
    let h = d.cross(e2);
    let a = e1.dot(h);
    if a.abs() < 1e-14 {
        return None;
    }
    let f = 1.0 / a;
    let s = p - t[0];
    let u = f * s.dot(h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let qv = s.cross(e1);
    let v = f * d.dot(qv);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let w = f * e2.dot(qv);
    if (0.0..=1.0).contains(&w) {
        Some(p + d * w)
    } else {
        None
    }
}

/// Exact distance between two triangles, and the closest point on b.
pub fn tri_dist(a: &[DVec3; 3], b: &[DVec3; 3]) -> (f64, DVec3) {
    for i in 0..3 {
        if let Some(x) = seg_tri(a[i], a[(i + 1) % 3], b) {
            return (0.0, x);
        }
        if let Some(x) = seg_tri(b[i], b[(i + 1) % 3], a) {
            return (0.0, x);
        }
    }
    let mut best = (f64::INFINITY, DVec3::ZERO);
    for i in 0..3 {
        for j in 0..3 {
            let r = seg_seg(a[i], a[(i + 1) % 3], b[j], b[(j + 1) % 3]);
            if r.0 < best.0 {
                best = r;
            }
        }
        let (q, _) = closest_tri(b[0] - a[i], b[1] - a[i], b[2] - a[i]);
        if q.length() < best.0 {
            best = (q.length(), a[i] + q);
        }
        let (q, _) = closest_tri(a[0] - b[i], a[1] - b[i], a[2] - b[i]);
        if q.length() < best.0 {
            best = (q.length(), b[i]);
        }
    }
    best
}

/// The closest triangle a (moving from r0 to r1) comes to b: distance, when (0..1, the first
/// touch when they touch) and where on b. The distance is convex along the move.
fn refine(a: &[DVec3; 3], b: &[DVec3; 3], r0: DVec3, r1: DVec3) -> (f64, f64, DVec3) {
    let f = |t: f64| {
        let r = r0 + (r1 - r0) * t;
        tri_dist(&[a[0] + r, a[1] + r, a[2] + r], b)
    };
    if (r1 - r0).length_squared() < 1e-12 {
        let (d, p) = f(0.0);
        return (d, 0.0, p);
    }
    // golden-section search: one distance per step (the distance is convex in t)
    const G: f64 = 0.618_033_988_749_895;
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    let mut m1 = hi - G * (hi - lo);
    let mut m2 = lo + G * (hi - lo);
    let (mut f1, mut f2) = (f(m1).0, f(m2).0);
    for _ in 0..30 {
        if f1 <= f2 {
            hi = m2;
            m2 = m1;
            f2 = f1;
            m1 = hi - G * (hi - lo);
            f1 = f(m1).0;
        } else {
            lo = m1;
            m1 = m2;
            f1 = f2;
            m2 = lo + G * (hi - lo);
            f2 = f(m2).0;
        }
    }
    let t = (lo + hi) / 2.0;
    let (d, p) = f(t);
    if d > 1e-9 {
        return (d, t, p);
    }
    let start = f(0.0);
    if start.0 <= 1e-9 {
        return (0.0, 0.0, start.1);
    }
    let (mut a0, mut b0) = (0.0, t);
    for _ in 0..24 {
        let m = (a0 + b0) / 2.0;
        if f(m).0 <= 1e-9 {
            b0 = m;
        } else {
            a0 = m;
        }
    }
    (0.0, b0, f(b0).1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(c: DVec3, h: f64) -> Vec<[DVec3; 3]> {
        let v = |x: f64, y: f64, z: f64| c + DVec3::new(x, y, z) * h;
        let p = [v(-1., -1., -1.), v(1., -1., -1.), v(1., 1., -1.), v(-1., 1., -1.), v(-1., -1., 1.), v(1., -1., 1.), v(1., 1., 1.), v(-1., 1., 1.)];
        let f = [[0, 1, 2], [0, 2, 3], [4, 6, 5], [4, 7, 6], [0, 5, 1], [0, 4, 5], [1, 6, 2], [1, 5, 6], [2, 7, 3], [2, 6, 7], [3, 4, 0], [3, 7, 4]];
        f.iter().map(|t| [p[t[0]], p[t[1]], p[t[2]]]).collect()
    }

    #[test]
    fn gjk_distances() {
        let a = [DVec3::ZERO, DVec3::X, DVec3::Y];
        let b = [DVec3::new(0.2, 0.2, 2.0), DVec3::new(1.0, 0.2, 2.0), DVec3::new(0.2, 1.0, 2.0)];
        assert!((gjk(&a, &b).0 - 2.0).abs() < 1e-9);
        let b2 = [DVec3::new(0.2, 0.2, -1.0), DVec3::new(0.2, 0.2, 1.0), DVec3::new(0.3, 0.3, 1.0)];
        assert!(gjk(&a, &b2).0 < 1e-9);
        // edge-edge
        let s1 = [DVec3::new(-1., 0., 0.), DVec3::new(1., 0., 0.)];
        let s2 = [DVec3::new(0., -1., 3.), DVec3::new(0., 1., 3.)];
        assert!((gjk(&s1, &s2).0 - 3.0).abs() < 1e-9);
    }

    #[test]
    fn triangle_distances() {
        let a = [DVec3::ZERO, DVec3::X, DVec3::Y];
        // coplanar, sliding past at 0.3
        let b = [DVec3::new(1.3, 0.0, 0.0), DVec3::new(2.0, 0.0, 0.0), DVec3::new(2.0, 1.0, 0.0)];
        assert!((tri_dist(&a, &b).0 - 0.3).abs() < 1e-9);
        // piercing through the middle
        let c = [DVec3::new(0.2, 0.2, -1.0), DVec3::new(0.25, 0.2, 1.0), DVec3::new(0.2, 0.25, 1.0)];
        assert_eq!(tri_dist(&a, &c).0, 0.0);
        let (d, t, _) = refine(&a, &b, DVec3::ZERO, DVec3::new(0.0, 0.0, 0.0));
        assert!((d - 0.3).abs() < 1e-9 && t == 0.0);
        let (d, _, _) = refine(&a, &b, DVec3::new(0.0, 5.0, 0.0), DVec3::new(0.0, -5.0, 0.0));
        assert!((d - 0.3).abs() < 1e-6, "{d}");
    }

    #[test]
    fn sweep_finds_a_pass_through() {
        // a cube passing right through another in one long move: the ends are both clear
        let a = Bvh::new(cube(DVec3::ZERO, 1.0), vec![0; 12]);
        let b = Bvh::new(cube(DVec3::new(50.0, 0.0, 0.0), 1.0), vec![1; 12]);
        let mut out = vec![];
        sweep(&a, &b, DVec3::ZERO, DVec3::new(100.0, 0.0, 0.0), 0.1, &|_, _| false, &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0].dist < 1e-9);
        assert!((out[0].t - 0.48).abs() < 0.01, "{}", out[0].t);
        let mut out = vec![];
        sweep(&a, &b, DVec3::new(0.0, 2.5, 0.0), DVec3::new(100.0, 2.5, 0.0), 0.1, &|_, _| false, &mut out);
        assert!(out.is_empty());
        sweep(&a, &b, DVec3::new(0.0, 2.05, 0.0), DVec3::new(100.0, 2.05, 0.0), 0.1, &|_, _| false, &mut out);
        assert!((out[0].dist - 0.05).abs() < 1e-6);
    }
}
