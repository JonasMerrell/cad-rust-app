//! Numeric helpers for scripts, in place of numpy: array maths, combinations, a seeded random
//! generator, ray casting against shapes, a machinability (accessibility) check, and headless renders.

use std::rc::Rc;

use glam::{DVec2, DVec3};
use opencascade::primitives::Shape;
use rhai::{Array, Dynamic, Engine, EvalAltResult, Map};

use crate::script::S;
use crate::script_api::{V3, num, v3};

type Res<T> = Result<T, Box<EvalAltResult>>;

fn err<T>(m: impl Into<String>) -> Res<T> {
    Err(m.into().into())
}

fn nums(a: &Array) -> Res<Vec<f64>> {
    a.iter().map(num).collect()
}

fn arr(v: impl IntoIterator<Item = f64>) -> Array {
    v.into_iter().map(Dynamic::from_float).collect()
}

fn ints(v: impl IntoIterator<Item = usize>) -> Array {
    v.into_iter().map(|i| Dynamic::from_int(i as i64)).collect()
}

/// Elementwise: array ∘ array, array ∘ number or number ∘ array.
fn zip(a: &Dynamic, b: &Dynamic, f: impl Fn(f64, f64) -> f64) -> Res<Array> {
    match (a.clone().into_array(), b.clone().into_array()) {
        (Ok(x), Ok(y)) => {
            if x.len() != y.len() {
                return err(format!("arrays of different lengths ({} and {})", x.len(), y.len()));
            }
            Ok(arr(nums(&x)?.into_iter().zip(nums(&y)?).map(|(p, q)| f(p, q))))
        }
        (Ok(x), Err(_)) => {
            let k = num(b)?;
            Ok(arr(nums(&x)?.into_iter().map(|p| f(p, k))))
        }
        (Err(_), Ok(y)) => {
            let k = num(a)?;
            Ok(arr(nums(&y)?.into_iter().map(|q| f(k, q))))
        }
        _ => err("expected arrays of numbers"),
    }
}

fn register_arrays(e: &mut Engine) {
    e.register_fn("zeros", |n: i64| arr(std::iter::repeat_n(0.0, n.max(0) as usize)));
    e.register_fn("linspace", |a: Dynamic, b: Dynamic, n: i64| -> Res<Array> {
        let (a, b) = (num(&a)?, num(&b)?);
        let n = n.max(1) as usize;
        Ok(arr((0..n).map(|i| if n == 1 { a } else { a + (b - a) * i as f64 / (n - 1) as f64 })))
    });
    e.register_fn("sum", |a: Array| -> Res<f64> { Ok(nums(&a)?.iter().sum()) });
    e.register_fn("mean", |a: Array| -> Res<f64> {
        let v = nums(&a)?;
        if v.is_empty() {
            return err("mean of an empty array");
        }
        Ok(v.iter().sum::<f64>() / v.len() as f64)
    });
    e.register_fn("min_of", |a: Array| -> Res<f64> { nums(&a)?.into_iter().reduce(f64::min).ok_or_else(|| "min of an empty array".into()) });
    e.register_fn("max_of", |a: Array| -> Res<f64> { nums(&a)?.into_iter().reduce(f64::max).ok_or_else(|| "max of an empty array".into()) });
    e.register_fn("argmin", |a: Array| -> Res<i64> {
        let v = nums(&a)?;
        (0..v.len()).min_by(|i, j| v[*i].total_cmp(&v[*j])).map(|i| i as i64).ok_or_else(|| "argmin of an empty array".into())
    });
    e.register_fn("argmax", |a: Array| -> Res<i64> {
        let v = nums(&a)?;
        (0..v.len()).max_by(|i, j| v[*i].total_cmp(&v[*j])).map(|i| i as i64).ok_or_else(|| "argmax of an empty array".into())
    });
    // indices that sort the array (stable)
    e.register_fn("argsort", |a: Array| -> Res<Array> {
        let v = nums(&a)?;
        let mut idx: Vec<usize> = (0..v.len()).collect();
        idx.sort_by(|i, j| v[*i].total_cmp(&v[*j]));
        Ok(ints(idx))
    });
    e.register_fn("cumsum", |a: Array| -> Res<Array> {
        let mut s = 0.0;
        Ok(arr(nums(&a)?.into_iter().map(|x| {
            s += x;
            s
        })))
    });
    // pick elements by index, or where a mask is true
    e.register_fn("take", |a: Array, idx: Array| -> Res<Array> {
        idx.iter()
            .map(|i| {
                let i = i.as_int().map_err(|_| "take: indices must be integers")?;
                a.get(i as usize).cloned().ok_or_else(|| format!("take: index {i} out of range").into())
            })
            .collect()
    });
    e.register_fn("select", |a: Array, mask: Array| -> Res<Array> {
        if a.len() != mask.len() {
            return err("select: the mask must be as long as the array");
        }
        Ok(a.into_iter().zip(mask).filter(|(_, m)| m.as_bool().unwrap_or(false)).map(|(x, _)| x).collect())
    });
    e.register_fn("add", |a: Dynamic, b: Dynamic| zip(&a, &b, |p, q| p + q));
    e.register_fn("sub", |a: Dynamic, b: Dynamic| zip(&a, &b, |p, q| p - q));
    e.register_fn("mul", |a: Dynamic, b: Dynamic| zip(&a, &b, |p, q| p * q));
    e.register_fn("div", |a: Dynamic, b: Dynamic| zip(&a, &b, |p, q| p / q));
    e.register_fn("abs_all", |a: Array| -> Res<Array> { Ok(arr(nums(&a)?.into_iter().map(f64::abs))) });
    e.register_fn("lt", |a: Dynamic, b: Dynamic| -> Res<Array> { Ok(zip(&a, &b, |p, q| if p < q { 1.0 } else { 0.0 })?.into_iter().map(|x| Dynamic::from_bool(x.as_float().unwrap_or(0.0) > 0.5)).collect()) });
    e.register_fn("gt", |a: Dynamic, b: Dynamic| -> Res<Array> { Ok(zip(&a, &b, |p, q| if p > q { 1.0 } else { 0.0 })?.into_iter().map(|x| Dynamic::from_bool(x.as_float().unwrap_or(0.0) > 0.5)).collect()) });
    // 2D arrays (arrays of rows)
    e.register_fn("column", |rows: Array, j: i64| -> Res<Array> {
        rows.iter().map(|r| r.clone().into_array().ok().and_then(|r| r.get(j as usize).cloned()).ok_or_else(|| "column: not a 2D array or index out of range".into())).collect()
    });
    e.register_fn("transpose", |rows: Array| -> Res<Array> {
        let rs: Vec<Array> = rows.iter().map(|r| r.clone().into_array().map_err(|_| "transpose: an array of rows".into())).collect::<Res<_>>()?;
        let n = rs.first().map(|r| r.len()).unwrap_or(0);
        Ok((0..n).map(|j| Dynamic::from_array(rs.iter().map(|r| r.get(j).cloned().unwrap_or(Dynamic::UNIT)).collect())).collect())
    });
    // convex hull of points in the XY plane, counter-clockwise (z of the first point)
    e.register_fn("convex_hull", |pts: Array| -> Res<Array> {
        let p: Vec<DVec3> = pts.iter().map(v3).collect::<Res<_>>()?;
        let z = p.first().map(|q| q.z).unwrap_or(0.0);
        Ok(hull2d(p.iter().map(|q| q.truncate()).collect()).into_iter().map(|q| Dynamic::from(V3(q.extend(z)))).collect())
    });
    // every combination taking one element from each list: [[a0, b0], [a0, b1], …]
    e.register_fn("product", |lists: Array| -> Res<Array> {
        let ls: Vec<Array> = lists.iter().map(|l| l.clone().into_array().map_err(|_| "product: an array of arrays".into())).collect::<Res<_>>()?;
        let total = ls.iter().try_fold(1usize, |acc, l| acc.checked_mul(l.len()).filter(|t| *t <= 5_000_000).or(if l.is_empty() { Some(0) } else { None }));
        let Some(total) = total else {
            return err("product: more than 5000000 combinations");
        };
        let mut out = Vec::with_capacity(total);
        let mut idx = vec![0usize; ls.len()];
        if ls.iter().any(|l| l.is_empty()) {
            return Ok(out);
        }
        loop {
            out.push(Dynamic::from_array(idx.iter().zip(&ls).map(|(i, l)| l[*i].clone()).collect()));
            let mut k = ls.len();
            loop {
                if k == 0 {
                    return Ok(out);
                }
                k -= 1;
                idx[k] += 1;
                if idx[k] < ls[k].len() {
                    break;
                }
                idx[k] = 0;
            }
        }
    });
}

/// Convex hull (Andrew's monotone chain), counter-clockwise, without collinear points.
pub fn hull2d(mut p: Vec<DVec2>) -> Vec<DVec2> {
    p.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    p.dedup_by(|a, b| a.distance(*b) < 1e-12);
    if p.len() < 3 {
        return p;
    }
    let cross = |o: DVec2, a: DVec2, b: DVec2| (a - o).perp_dot(b - o);
    let mut h: Vec<DVec2> = vec![];
    for pass in 0..2 {
        let start = h.len();
        let it: Box<dyn Iterator<Item = &DVec2>> = if pass == 0 { Box::new(p.iter()) } else { Box::new(p.iter().rev()) };
        for q in it {
            while h.len() >= start + 2 && cross(h[h.len() - 2], h[h.len() - 1], *q) <= 1e-12 {
                h.pop();
            }
            h.push(*q);
        }
        h.pop();
    }
    h
}

// ---------------------------------------------------------------- random numbers

/// A seeded random generator (SplitMix64): the same seed gives the same numbers on every run.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn register_random(e: &mut Engine) {
    e.register_type_with_name::<Rng>("Rng");
    e.register_fn("rng", |seed: i64| Rng(seed as u64));
    e.register_fn("uniform", |r: &mut Rng| r.uniform());
    e.register_fn("uniform", |r: &mut Rng, lo: Dynamic, hi: Dynamic| -> Res<f64> {
        let (a, b) = (num(&lo)?, num(&hi)?);
        Ok(a + (b - a) * r.uniform())
    });
    e.register_fn("int", |r: &mut Rng, n: i64| -> Res<i64> {
        if n <= 0 {
            return err("int: n must be positive");
        }
        Ok((r.next_u64() % n as u64) as i64)
    });
    e.register_fn("points_in_box", |r: &mut Rng, lo: Dynamic, hi: Dynamic, n: i64| -> Res<Array> {
        let (a, b) = (v3(&lo)?, v3(&hi)?);
        Ok((0..n.max(0)).map(|_| Dynamic::from(V3(a + (b - a) * DVec3::new(r.uniform(), r.uniform(), r.uniform())))).collect())
    });
}

// ---------------------------------------------------------------- ray casting

/// Triangles binned on a grid across a ray direction, for casting many parallel rays.
pub struct RayGrid {
    tris: Vec<[DVec3; 3]>,
    /// Frame: rays run along `w`; `u`, `v` span the grid.
    u: DVec3,
    v: DVec3,
    w: DVec3,
    lo: DVec2,
    cell: f64,
    n: [usize; 2],
    bins: Vec<Vec<u32>>,
}

impl RayGrid {
    pub fn new(tris: Vec<[DVec3; 3]>, dir: DVec3) -> RayGrid {
        let w = dir.normalize_or(DVec3::Z);
        let u = w.any_orthonormal_vector();
        let v = w.cross(u);
        let to2 = |p: DVec3| DVec2::new(p.dot(u), p.dot(v));
        let mut lo = DVec2::splat(f64::INFINITY);
        let mut hi = DVec2::splat(f64::NEG_INFINITY);
        for t in &tris {
            for p in t {
                lo = lo.min(to2(*p));
                hi = hi.max(to2(*p));
            }
        }
        if !lo.x.is_finite() {
            lo = DVec2::ZERO;
            hi = DVec2::ONE;
        }
        let size = (hi - lo).max(DVec2::splat(1e-9));
        let k = ((tris.len() as f64).sqrt() * 0.8).clamp(1.0, 512.0);
        let cell = (size.x * size.y / (k * k)).sqrt().max(1e-9);
        let n = [((size.x / cell).ceil() as usize).clamp(1, 1024), ((size.y / cell).ceil() as usize).clamp(1, 1024)];
        let mut bins = vec![vec![]; n[0] * n[1]];
        for (i, t) in tris.iter().enumerate() {
            let (a, b, c) = (to2(t[0]), to2(t[1]), to2(t[2]));
            let (mn, mx) = (a.min(b).min(c), a.max(b).max(c));
            let i0 = (((mn.x - lo.x) / cell).floor().max(0.0) as usize).min(n[0] - 1);
            let i1 = (((mx.x - lo.x) / cell).floor().max(0.0) as usize).min(n[0] - 1);
            let j0 = (((mn.y - lo.y) / cell).floor().max(0.0) as usize).min(n[1] - 1);
            let j1 = (((mx.y - lo.y) / cell).floor().max(0.0) as usize).min(n[1] - 1);
            for j in j0..=j1 {
                for ii in i0..=i1 {
                    bins[ii + j * n[0]].push(i as u32);
                }
            }
        }
        RayGrid { tris, u, v, w, lo, cell, n, bins }
    }

    /// Triangles crossed by the ray from `o` along the grid direction (ahead of `o` by more than `eps`),
    /// with the distance to each, nearest first.
    pub fn hits(&self, o: DVec3, eps: f64, skip: Option<u32>) -> Vec<(f64, u32)> {
        let q = DVec2::new(o.dot(self.u), o.dot(self.v)) - self.lo;
        let (i, j) = ((q.x / self.cell).floor(), (q.y / self.cell).floor());
        if i < 0.0 || j < 0.0 || i >= self.n[0] as f64 || j >= self.n[1] as f64 {
            return vec![];
        }
        let mut out = vec![];
        for &ti in &self.bins[i as usize + j as usize * self.n[0]] {
            if Some(ti) == skip {
                continue;
            }
            let t = &self.tris[ti as usize];
            if let Some(d) = ray_tri(o, self.w, t) {
                if d > eps {
                    out.push((d, ti));
                }
            }
        }
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        // a ray through an edge or vertex shared by triangles crossing the same way is one crossing
        let side = |ti: u32| {
            let t = &self.tris[ti as usize];
            (t[1] - t[0]).cross(t[2] - t[0]).dot(self.w) > 0.0
        };
        out.dedup_by(|b, a| (b.0 - a.0).abs() < 1e-9 * (1.0 + a.0.abs()) && side(a.1) == side(b.1));
        out
    }
}

/// Möller–Trumbore: distance along `d` from `o` to triangle `t`, counting hits on its edges.
fn ray_tri(o: DVec3, d: DVec3, t: &[DVec3; 3]) -> Option<f64> {
    let (e1, e2) = (t[1] - t[0], t[2] - t[0]);
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-18 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - t[0];
    let u = s.dot(p) * inv;
    if !(-1e-12..=1.0 + 1e-12).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < -1e-12 || u + v > 1.0 + 1e-12 {
        return None;
    }
    Some(e2.dot(q) * inv)
}

fn shape_tris(s: &Shape, deflection: f64) -> Res<Vec<[DVec3; 3]>> {
    let (v, t, _) = crate::script_api::tessellate(s, deflection, 15.0).map_err(|e| -> Box<EvalAltResult> { e.into() })?;
    Ok(t.iter().map(|x| [v[x[0] as usize], v[x[1] as usize], v[x[2] as usize]]).collect())
}

/// Result of the machinability check.
pub struct Access {
    pub total_area: f64,
    pub unreachable_area: f64,
    /// Unreachable triangles: centre, normal, area — largest first.
    pub worst: Vec<(DVec3, DVec3, f64)>,
}

/// Which parts of a shape's surface a tool coming along one of `dirs` (pointing from the tool into the
/// part, e.g. −Z for a cutter coming down from above) can see. A triangle counts as reachable from a
/// direction when it does not face away and a ray back out from it meets nothing. Rays start a little
/// off the surface; a triangle that fails is checked again from three points inside it, and only counts
/// as unreachable if most of them fail too, so rays grazing edges and vertices don't flag it.
pub fn accessibility(s: &Shape, dirs: &[DVec3], deflection: f64) -> Result<Access, String> {
    let (v, t, _) = crate::script_api::tessellate(s, deflection, 15.0)?;
    let tris: Vec<[DVec3; 3]> = t.iter().map(|x| [v[x[0] as usize], v[x[1] as usize], v[x[2] as usize]]).collect();
    let (lo, hi) = tris.iter().flatten().fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)), |(a, b), p| (a.min(*p), b.max(*p)));
    let eps = ((hi - lo).length() * 1e-6).max(1e-6);
    let grids: Vec<RayGrid> = dirs.iter().map(|d| RayGrid::new(tris.clone(), -*d)).collect();
    let mut acc = Access { total_area: 0.0, unreachable_area: 0.0, worst: vec![] };
    for (i, tr) in tris.iter().enumerate() {
        let n2 = (tr[1] - tr[0]).cross(tr[2] - tr[0]);
        let area = n2.length() / 2.0;
        if area < 1e-14 {
            continue;
        }
        acc.total_area += area;
        let n = n2 / (2.0 * area);
        let c = (tr[0] + tr[1] + tr[2]) / 3.0;
        let clear = |g: &RayGrid, p: DVec3| g.hits(p + n * eps * 10.0, eps, Some(i as u32)).is_empty();
        let reachable = grids.iter().zip(dirs).any(|(g, d)| {
            // facing away from the tool (an overhang)
            if n.dot(-d.normalize_or(DVec3::Z)) < -1e-6 {
                return false;
            }
            if clear(g, c) {
                return true;
            }
            let ok = tr.iter().filter(|p| clear(g, c + (**p - c) * 0.6)).count();
            ok >= 2
        });
        if !reachable {
            acc.unreachable_area += area;
            acc.worst.push((c, n, area));
        }
    }
    acc.worst.sort_by(|a, b| b.2.total_cmp(&a.2));
    Ok(acc)
}

fn register_rays(e: &mut Engine) {
    // crossings of rays from each origin along `dir`: counts, or [{distance, point}] lists
    e.register_fn("ray_hits", |s: &mut S, origins: Array, dir: Dynamic| -> Res<Array> {
        let g = RayGrid::new(shape_tris(&s.0, 0.01)?, v3(&dir)?);
        origins.iter().map(|o| Ok(Dynamic::from_int(g.hits(v3(o)?, 0.0, None).len() as i64))).collect()
    });
    e.register_fn("ray_cast", |s: &mut S, origins: Array, dir: Dynamic| -> Res<Array> {
        let d = v3(&dir)?.normalize_or(DVec3::Z);
        let g = RayGrid::new(shape_tris(&s.0, 0.01)?, d);
        origins
            .iter()
            .map(|o| {
                let o = v3(o)?;
                Ok(Dynamic::from_array(
                    g.hits(o, 0.0, None)
                        .into_iter()
                        .map(|(t, _)| {
                            let mut m = Map::new();
                            m.insert("distance".into(), t.into());
                            m.insert("point".into(), Dynamic::from(V3(o + d * t)));
                            Dynamic::from_map(m)
                        })
                        .collect(),
                ))
            })
            .collect()
    });
    // the machinability check: unreachable area and where it is
    let check = |s: &S, dirs: &Array, defl: f64| -> Res<Map> {
        let ds: Vec<DVec3> = dirs.iter().map(v3).collect::<Res<_>>()?;
        let a = accessibility(&s.0, &ds, defl).map_err(|e| -> Box<EvalAltResult> { e.into() })?;
        let mut m = Map::new();
        m.insert("total_area".into(), a.total_area.into());
        m.insert("unreachable_area".into(), a.unreachable_area.into());
        m.insert(
            "worst".into(),
            a.worst
                .iter()
                .take(20)
                .map(|(c, n, ar)| {
                    let mut w = Map::new();
                    w.insert("point".into(), Dynamic::from(V3(*c)));
                    w.insert("normal".into(), Dynamic::from(V3(*n)));
                    w.insert("area".into(), (*ar).into());
                    Dynamic::from_map(w)
                })
                .collect::<Array>()
                .into(),
        );
        Ok(m)
    };
    e.register_fn("accessibility", move |s: S, dirs: Array| check(&s, &dirs, 0.02));
    e.register_fn("accessibility", move |s: S, dirs: Array, deflection: Dynamic| check(&s, &dirs, num(&deflection)?));
}

// ---------------------------------------------------------------- rendering

/// One part in a render.
pub struct Item {
    pub tris: Vec<[DVec3; 3]>,
    /// Smooth normal at each corner.
    pub normals: Vec<[DVec3; 3]>,
    pub color: [f32; 3],
    pub opacity: f32,
}

pub struct View {
    pub eye: DVec3,
    pub target: DVec3,
    pub up: DVec3,
    /// Vertical field of view in degrees; 0: orthographic, fitted to the parts.
    pub fov: f64,
}

/// Triangles with smooth normals per CAD face (corners shared within a face are averaged).
pub fn render_item(s: &Shape, deflection: f64) -> Result<(Vec<[DVec3; 3]>, Vec<[DVec3; 3]>), String> {
    let (v, t, f) = crate::script_api::tessellate(s, deflection, 10.0)?;
    let tris: Vec<[DVec3; 3]> = t.iter().map(|x| [v[x[0] as usize], v[x[1] as usize], v[x[2] as usize]]).collect();
    let key = |face: u32, p: DVec3| (face, (p.x * 1e5).round() as i64, (p.y * 1e5).round() as i64, (p.z * 1e5).round() as i64);
    let mut acc: std::collections::HashMap<(u32, i64, i64, i64), DVec3> = Default::default();
    for (tr, fi) in tris.iter().zip(&f) {
        let n = (tr[1] - tr[0]).cross(tr[2] - tr[0]);
        for p in tr {
            *acc.entry(key(*fi, *p)).or_default() += n;
        }
    }
    let normals = tris.iter().zip(&f).map(|(tr, fi)| tr.map(|p| acc[&key(*fi, p)].normalize_or_zero())).collect();
    Ok((tris, normals))
}

/// Shade the items into an RGB image of `w` × `h` (2× supersampled): smooth shading, no edges;
/// translucent items are blended over the opaque ones, farthest first.
pub fn render(items: &[Item], view: &View, w: usize, h: usize, bg: [f32; 3]) -> Vec<u8> {
    let ss = 2;
    let (ww, hh) = (w * ss, h * ss);
    let fwd = (view.target - view.eye).normalize_or(DVec3::NEG_Z);
    let right = fwd.cross(view.up).normalize_or(DVec3::X);
    let up = right.cross(fwd);
    let (lo, hi) = items.iter().flat_map(|i| i.tris.iter().flatten()).fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)), |(a, b), p| (a.min(*p), b.max(*p)));
    let ortho_half = if view.fov <= 0.0 {
        // fit the parts' bounding box
        let mut m: f64 = 1e-6;
        for k in 0..8 {
            let p = DVec3::new([lo.x, hi.x][k & 1], [lo.y, hi.y][(k >> 1) & 1], [lo.z, hi.z][(k >> 2) & 1]) - view.target;
            m = m.max(p.dot(up).abs()).max(p.dot(right).abs() * h as f64 / w as f64);
        }
        m * 1.08
    } else {
        0.0
    };
    let f = 1.0 / (view.fov.max(1.0).to_radians() / 2.0).tan();
    let aspect = w as f64 / h as f64;
    // screen x, y (pixels) and depth
    let project = |p: DVec3| -> Option<DVec3> {
        let d = p - view.eye;
        let z = d.dot(fwd);
        let (x, y) = if view.fov > 0.0 {
            if z <= 1e-6 {
                return None;
            }
            (d.dot(right) * f / (z * aspect), d.dot(up) * f / z)
        } else {
            let q = p - view.target;
            (q.dot(right) / (ortho_half * aspect), q.dot(up) / ortho_half)
        };
        Some(DVec3::new((x + 1.0) * 0.5 * ww as f64, (1.0 - y) * 0.5 * hh as f64, z))
    };
    let light = (-fwd + up * 0.6 + right * 0.35).normalize();
    let mut col = vec![bg; ww * hh];
    let mut depth = vec![f64::INFINITY; ww * hh];
    let shade = |n: DVec3, base: [f32; 3]| -> [f32; 3] {
        let n = if n.dot(-fwd) < 0.0 { -n } else { n };
        let diff = n.dot(light).max(0.0) as f32;
        let half = (light - fwd).normalize();
        let spec = n.dot(half).max(0.0).powi(40) as f32;
        let l = 0.32 + 0.62 * diff;
        base.map(|c| (c * l + 0.25 * spec).min(1.0))
    };
    // opaque items first, then translucent triangles far to near
    let mut glass: Vec<(f64, usize, usize)> = vec![];
    for (ii, it) in items.iter().enumerate() {
        for ti in 0..it.tris.len() {
            if it.opacity < 0.999 {
                let c = (it.tris[ti][0] + it.tris[ti][1] + it.tris[ti][2]) / 3.0;
                glass.push(((c - view.eye).dot(fwd), ii, ti));
                continue;
            }
            fill(&mut col, &mut depth, ww, hh, it, ti, &project, &shade, true);
        }
    }
    glass.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, ii, ti) in glass {
        fill(&mut col, &mut depth, ww, hh, &items[ii], ti, &project, &shade, false);
    }
    // downsample
    let mut out = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let mut s = [0.0f32; 3];
            for dy in 0..ss {
                for dx in 0..ss {
                    let c = col[(y * ss + dy) * ww + x * ss + dx];
                    for k in 0..3 {
                        s[k] += c[k];
                    }
                }
            }
            for k in 0..3 {
                out[(y * w + x) * 3 + k] = (s[k] / (ss * ss) as f32 * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn fill(col: &mut [[f32; 3]], depth: &mut [f64], ww: usize, hh: usize, it: &Item, ti: usize, project: &dyn Fn(DVec3) -> Option<DVec3>, shade: &dyn Fn(DVec3, [f32; 3]) -> [f32; 3], opaque: bool) {
    let tr = &it.tris[ti];
    let (Some(a), Some(b), Some(c)) = (project(tr[0]), project(tr[1]), project(tr[2])) else { return };
    let area = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
    if area.abs() < 1e-12 {
        return;
    }
    let x0 = a.x.min(b.x).min(c.x).floor().max(0.0) as usize;
    let x1 = (a.x.max(b.x).max(c.x).ceil() as usize).min(ww.saturating_sub(1));
    let y0 = a.y.min(b.y).min(c.y).floor().max(0.0) as usize;
    let y1 = (a.y.max(b.y).max(c.y).ceil() as usize).min(hh.saturating_sub(1));
    let nr = &it.normals[ti];
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let w0 = ((b.x - px) * (c.y - py) - (c.x - px) * (b.y - py)) / area;
            let w1 = ((c.x - px) * (a.y - py) - (a.x - px) * (c.y - py)) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let z = w0 * a.z + w1 * b.z + w2 * c.z;
            let k = y * ww + x;
            if z >= depth[k] {
                continue;
            }
            let n = nr[0] * w0 + nr[1] * w1 + nr[2] * w2;
            let s = shade(n.normalize_or_zero(), it.color);
            if opaque {
                depth[k] = z;
                col[k] = s;
            } else {
                let o = it.opacity.clamp(0.0, 1.0);
                for q in 0..3 {
                    col[k][q] = col[k][q] * (1.0 - o) + s[q] * o;
                }
            }
        }
    }
}

fn color_of(d: Option<&Dynamic>) -> Res<[f32; 3]> {
    let Some(d) = d else { return Ok([0.62, 0.66, 0.72]) };
    if let Ok(s) = d.clone().into_string() {
        let h = s.trim_start_matches('#');
        if h.len() == 6 {
            let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map(|v| v as f32 / 255.0);
            if let (Ok(r), Ok(g), Ok(b)) = (p(0), p(2), p(4)) {
                return Ok([r, g, b]);
            }
        }
        return err("colour: \"#rrggbb\" or [r, g, b] (0–1)");
    }
    let v = v3(d)?;
    let k = if v.max_element() > 1.0 { 1.0 / 255.0 } else { 1.0 };
    Ok([(v.x * k) as f32, (v.y * k) as f32, (v.z * k) as f32])
}

/// Parts from an array of shapes or of maps {shape, color, opacity, deflection}.
fn items_of(parts: &Array) -> Res<Vec<Item>> {
    let mut out = vec![];
    for p in parts {
        let (shape, m): (Rc<Shape>, Map) = if let Some(s) = p.clone().try_cast::<S>() {
            (s.0, Map::new())
        } else if let Some(m) = p.clone().try_cast::<Map>() {
            let s = m.get("shape").and_then(|s| s.clone().try_cast::<S>()).ok_or("render: each part needs a shape")?;
            (s.0, m)
        } else {
            return err("render: parts are shapes or maps {shape, color, opacity}");
        };
        let defl = m.get("deflection").map(num).transpose()?.unwrap_or(0.02);
        let (tris, normals) = render_item(&shape, defl).map_err(|e| -> Box<EvalAltResult> { e.into() })?;
        let opacity = m.get("opacity").map(num).transpose()?.unwrap_or(1.0) as f32;
        out.push(Item { tris, normals, color: color_of(m.get("color"))?, opacity });
    }
    Ok(out)
}

fn view_of(items: &[Item], m: &Map) -> Res<View> {
    let (lo, hi) = items.iter().flat_map(|i| i.tris.iter().flatten()).fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)), |(a, b), p| (a.min(*p), b.max(*p)));
    let (lo, hi) = if lo.x.is_finite() { (lo, hi) } else { (DVec3::ZERO, DVec3::ONE) };
    let target = m.get("target").map(v3).transpose()?.unwrap_or((lo + hi) / 2.0);
    let fov = m.get("fov").map(num).transpose()?.unwrap_or(0.0);
    let dist = (hi - lo).length() * if fov > 0.0 { 0.6 / (fov.to_radians() / 2.0).tan() } else { 2.0 };
    let dir = match m.get("view").and_then(|v| v.clone().into_string().ok()).as_deref() {
        Some("top") => DVec3::Z,
        Some("bottom") => DVec3::NEG_Z,
        Some("front") => DVec3::NEG_Y,
        Some("back") => DVec3::Y,
        Some("left") => DVec3::NEG_X,
        Some("right") => DVec3::X,
        Some("iso") | None => DVec3::new(1.0, -1.0, 0.8).normalize(),
        Some(v) => return err(format!("view {v}: top, bottom, front, back, left, right or iso")),
    };
    let eye = match m.get("eye") {
        Some(e) => v3(e)?,
        None => target + dir * dist,
    };
    let default_up = if (eye - target).normalize_or(DVec3::Z).cross(DVec3::Z).length() < 1e-6 { DVec3::Y } else { DVec3::Z };
    let up = m.get("up").map(v3).transpose()?.unwrap_or(default_up);
    Ok(View { eye, target, up, fov })
}

fn write_png(path: &str, px: &[u8], w: usize, h: usize) -> Res<()> {
    image::save_buffer_with_format(path, px, w as u32, h as u32, image::ColorType::Rgb8, image::ImageFormat::Png).map_err(|e| format!("{path}: {e}").into())
}

pub fn register_render(e: &mut Engine) {
    // render(parts, options): options {path, width, height, view | eye/target/up, fov, background}
    e.register_fn("render", |parts: Array, opts: Map| -> Res<()> {
        let items = items_of(&parts)?;
        let path = opts.get("path").and_then(|p| p.clone().into_string().ok()).ok_or("render: give path")?;
        let w = opts.get("width").map(num).transpose()?.unwrap_or(1200.0) as usize;
        let h = opts.get("height").map(num).transpose()?.unwrap_or(900.0) as usize;
        let bg = match opts.get("background") {
            Some(b) => color_of(Some(b))?,
            None => [1.0, 1.0, 1.0],
        };
        let px = render(&items, &view_of(&items, &opts)?, w.max(8), h.max(8), bg);
        write_png(&path, &px, w.max(8), h.max(8))
    });
    // several viewports in one image: views [{parts, view/eye/…}, …], columns, width/height per view
    e.register_fn("render_views", |views: Array, opts: Map| -> Res<()> {
        let path = opts.get("path").and_then(|p| p.clone().into_string().ok()).ok_or("render_views: give path")?;
        let w = opts.get("width").map(num).transpose()?.unwrap_or(600.0) as usize;
        let h = opts.get("height").map(num).transpose()?.unwrap_or(450.0) as usize;
        let cols = (opts.get("columns").map(num).transpose()?.unwrap_or(2.0) as usize).max(1);
        let bg = match opts.get("background") {
            Some(b) => color_of(Some(b))?,
            None => [1.0, 1.0, 1.0],
        };
        let rows = views.len().div_ceil(cols).max(1);
        let (tw, th) = (w * cols, h * rows);
        let mut img = vec![255u8; tw * th * 3];
        for (k, v) in views.iter().enumerate() {
            let m = v.clone().try_cast::<Map>().ok_or("render_views: each view is a map {parts, …}")?;
            let parts = m.get("parts").and_then(|p| p.clone().into_array().ok()).ok_or("render_views: each view needs parts")?;
            let items = items_of(&parts)?;
            let px = render(&items, &view_of(&items, &m)?, w, h, bg);
            let (ox, oy) = ((k % cols) * w, (k / cols) * h);
            for y in 0..h {
                let src = &px[y * w * 3..(y + 1) * w * 3];
                let at = ((oy + y) * tw + ox) * 3;
                img[at..at + w * 3].copy_from_slice(src);
            }
        }
        write_png(&path, &img, tw, th)
    });
}

/// Add the numeric helpers to an engine.
pub fn register(e: &mut Engine) {
    register_arrays(e);
    register_random(e);
    register_rays(e);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rays_count_crossings_through_a_box() {
        let b = Shape::box_from_corners(DVec3::ZERO, DVec3::new(10.0, 10.0, 10.0));
        let tris = shape_tris(&b, 0.01).unwrap();
        let g = RayGrid::new(tris, DVec3::Z);
        // through the middle: in and out
        assert_eq!(g.hits(DVec3::new(5.0, 5.0, -1.0), 0.0, None).len(), 2);
        // a ray along a diagonal of the bottom face grazes two triangles' shared edge but still counts as two crossings
        let h = g.hits(DVec3::new(3.0, 3.0, -1.0), 0.0, None);
        assert!(h.len() >= 2, "{h:?}");
        assert!(g.hits(DVec3::new(15.0, 5.0, -1.0), 0.0, None).is_empty());
    }

    #[test]
    fn a_pocket_is_reachable_from_above_but_an_undercut_is_not() {
        let b = Shape::box_from_corners(DVec3::ZERO, DVec3::new(20.0, 20.0, 10.0));
        let pocket = Shape::box_from_corners(DVec3::new(5.0, 5.0, 4.0), DVec3::new(15.0, 15.0, 11.0));
        let part = crate::occ::cut(&b, &pocket).unwrap();
        let a = accessibility(&part, &[DVec3::NEG_Z, DVec3::Z], 0.01).unwrap();
        assert!(a.unreachable_area < 1e-6, "{}", a.unreachable_area);
        // a slot under an overhang: its ceiling faces down but is hidden from below
        let slot = Shape::box_from_corners(DVec3::new(-1.0, 5.0, 3.0), DVec3::new(10.0, 15.0, 6.0));
        let part2 = crate::occ::cut(&b, &slot).unwrap();
        let a2 = accessibility(&part2, &[DVec3::NEG_Z, DVec3::Z], 0.01).unwrap();
        assert!(a2.unreachable_area > 50.0, "{}", a2.unreachable_area);
    }

    #[test]
    fn product_and_argsort() {
        let mut e = Engine::new();
        register(&mut e);
        let n: i64 = e.eval("product([[1, 2, 3, 4, 5], [1, 2, 3, 4, 5], [1, 2, 3, 4, 5], [1, 2, 3, 4, 5]]).len()").unwrap();
        assert_eq!(n, 625);
        let a: Array = e.eval("argsort([3.0, 1.0, 2.0])").unwrap();
        assert_eq!(a.iter().map(|x| x.as_int().unwrap()).collect::<Vec<_>>(), vec![1, 2, 0]);
        let same: bool = e.eval("let a = rng(7); let b = rng(7); a.uniform() == b.uniform()").unwrap();
        assert!(same);
    }
}
