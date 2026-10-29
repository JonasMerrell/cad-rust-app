//! Automatic tetrahedral meshing for the Simulation tab.
//!
//! Isosurface stuffing (Labelle & Shewchuk 2007) on a body-centred cubic lattice: lattice points are
//! classified inside/outside the parts' triangulated surface, lattice edges that cross the surface get
//! a cut point on it, lattice points too close to a cut point are warped onto the surface, and every
//! lattice tetrahedron is clipped to the inside with a fixed set of stencils. The lattice is fitted to
//! the parts' bounding box, so flat faces on the box sides come out exact. The linear mesh is then
//! raised to 10-node quadratic tetrahedra, with mid-edge nodes on curved faces moved onto the surface.

use std::collections::HashMap;

use glam::DVec3;

/// A triangulated closed surface (the parts) with lookups for inside tests and closest points.
pub struct Surface {
    pub tris: Vec<[DVec3; 3]>,
    /// CAD face of each triangle (numbered across all parts).
    pub face: Vec<u32>,
    /// Part (body) of each triangle; inside tests count crossings per part.
    pub body: Vec<u32>,
    bodies: usize,
    lo: DVec3,
    hi: DVec3,
    /// Vertical-ray bins over xy.
    nb: [usize; 2],
    bin2: Vec<Vec<u32>>,
    /// Closest-point bins over xyz.
    n3: [usize; 3],
    cell3: f64,
    bin3: Vec<Vec<u32>>,
    eps: DVec3,
    /// Sharp edges between faces (`feature_edges`).
    pub features: Vec<[DVec3; 2]>,
}

impl Surface {
    pub fn new(tris: Vec<[DVec3; 3]>, face: Vec<u32>, body: Vec<u32>) -> Surface {
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for t in &tris {
            for p in t {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        if tris.is_empty() {
            lo = DVec3::ZERO;
            hi = DVec3::ONE;
        }
        let size = (hi - lo).max(DVec3::splat(1e-9));
        let diag = size.length();
        // ~ sqrt(n) bins across for the ray bins, ~ cbrt(n) for the 3D bins
        let n = tris.len().max(1) as f64;
        let k2 = (n.sqrt() * 0.7).clamp(1.0, 400.0);
        let area = size.x * size.y;
        let c2 = (area / (k2 * k2)).sqrt().max(1e-9);
        let nb = [((size.x / c2).ceil() as usize).clamp(1, 256), ((size.y / c2).ceil() as usize).clamp(1, 256)];
        let mut bin2 = vec![vec![]; nb[0] * nb[1]];
        let cell3 = (size.x * size.y * size.z / (n / 2.0)).cbrt().max(diag * 2e-3).max(size.max_element() / 48.0).max(1e-9);
        let n3 = [0, 1, 2].map(|a| ((size[a] / cell3).ceil() as usize).clamp(1, 48));
        let mut bin3 = vec![vec![]; n3[0] * n3[1] * n3[2]];
        let features = feature_edges(&tris, &face);
        let mut s = Surface { tris, face, body, bodies: 0, lo, hi, nb, bin2: vec![], n3, cell3, bin3: vec![], eps: DVec3::new(1.2345e-7, 2.71828e-7, 3.14159e-7) * diag, features };
        s.bodies = s.body.iter().map(|b| *b as usize + 1).max().unwrap_or(1);
        for (i, t) in s.tris.iter().enumerate() {
            let a = t[0].min(t[1]).min(t[2]);
            let b = t[0].max(t[1]).max(t[2]);
            let (x0, y0) = s.bin_xy(a.x, a.y);
            let (x1, y1) = s.bin_xy(b.x, b.y);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    bin2[x + y * nb[0]].push(i as u32);
                }
            }
            let c0 = s.bin3_of(a);
            let c1 = s.bin3_of(b);
            for z in c0[2]..=c1[2] {
                for y in c0[1]..=c1[1] {
                    for x in c0[0]..=c1[0] {
                        bin3[x + n3[0] * (y + n3[1] * z)].push(i as u32);
                    }
                }
            }
        }
        s.bin2 = bin2;
        s.bin3 = bin3;
        s
    }

    pub fn bounds(&self) -> (DVec3, DVec3) {
        (self.lo, self.hi)
    }

    #[allow(dead_code)] // the Python bindings use these (python/src/fea.rs)
    /// Number of parts.
    pub fn body_count(&self) -> usize {
        self.bodies
    }

    #[allow(dead_code)]
    /// Bounding box of one part's triangles.
    pub fn body_bounds(&self, b: u32) -> Option<(DVec3, DVec3)> {
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for (t, bi) in self.tris.iter().zip(&self.body) {
            if *bi == b {
                for p in t {
                    lo = lo.min(*p);
                    hi = hi.max(*p);
                }
            }
        }
        lo.x.is_finite().then_some((lo, hi))
    }

    fn bin_xy(&self, x: f64, y: f64) -> (usize, usize) {
        let fx = ((x - self.lo.x) / (self.hi.x - self.lo.x).max(1e-12) * self.nb[0] as f64).floor();
        let fy = ((y - self.lo.y) / (self.hi.y - self.lo.y).max(1e-12) * self.nb[1] as f64).floor();
        (fx.clamp(0.0, self.nb[0] as f64 - 1.0) as usize, fy.clamp(0.0, self.nb[1] as f64 - 1.0) as usize)
    }

    fn bin3_of(&self, p: DVec3) -> [usize; 3] {
        [0, 1, 2].map(|a| (((p[a] - self.lo[a]) / self.cell3).floor()).clamp(0.0, self.n3[a] as f64 - 1.0) as usize)
    }

    /// Whether a point is inside part `only` (or any part): crossings of a slightly nudged vertical
    /// ray, per part.
    pub fn inside_of(&self, p: DVec3, only: Option<u32>) -> bool {
        let q = p + self.eps;
        if q.x < self.lo.x || q.y < self.lo.y || q.z < self.lo.z || q.x > self.hi.x || q.y > self.hi.y || q.z > self.hi.z {
            return false;
        }
        let (bx, by) = self.bin_xy(q.x, q.y);
        let mut odd = [false; 64];
        let mut odd_many = vec![];
        if self.bodies > 64 {
            odd_many = vec![false; self.bodies];
        }
        for &ti in &self.bin2[bx + by * self.nb[0]] {
            if only.is_some_and(|b| self.body[ti as usize] != b) {
                continue;
            }
            let t = &self.tris[ti as usize];
            // 2D barycentric of (q.x, q.y)
            let (a, b, c) = (t[0], t[1], t[2]);
            let d = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
            if d.abs() < 1e-300 {
                continue;
            }
            let u = ((b.x - q.x) * (c.y - q.y) - (c.x - q.x) * (b.y - q.y)) / d;
            let v = ((c.x - q.x) * (a.y - q.y) - (a.x - q.x) * (c.y - q.y)) / d;
            let w = 1.0 - u - v;
            if u < 0.0 || v < 0.0 || w < 0.0 {
                continue;
            }
            let z = u * a.z + v * b.z + w * c.z;
            if z > q.z {
                let bi = self.body[ti as usize] as usize;
                if bi < 64 { odd[bi] = !odd[bi] } else { odd_many[bi] = !odd_many[bi] }
            }
        }
        odd.iter().any(|x| *x) || odd_many.iter().any(|x| *x)
    }

    /// Whether a triangle of a part other than `body` lies within `tol` of `p`.
    pub fn touches_other(&self, p: DVec3, body: u32, tol: f64) -> bool {
        let (c0, c1) = (self.bin3_of(p - DVec3::splat(tol)), self.bin3_of(p + DVec3::splat(tol)));
        for z in c0[2]..=c1[2] {
            for y in c0[1]..=c1[1] {
                for x in c0[0]..=c1[0] {
                    for &ti in &self.bin3[x + self.n3[0] * (y + self.n3[1] * z)] {
                        let t = &self.tris[ti as usize];
                        if self.body[ti as usize] != body && closest_on_tri(p, t[0], t[1], t[2]).distance(p) <= tol {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// The closest point on the surface to `p` (searching outwards from p's bin), with its triangle.
    pub fn closest(&self, p: DVec3) -> Option<(DVec3, usize)> {
        let c = self.bin3_of(p);
        let mut best: Option<(f64, DVec3, usize)> = None;
        let maxr = self.n3[0].max(self.n3[1]).max(self.n3[2]);
        for r in 0..=maxr {
            // once something is found, a shell further out than the best distance cannot do better
            if let Some((d, ..)) = best {
                if (r as f64 - 1.0) * self.cell3 > d.sqrt() {
                    break;
                }
            }
            let lo = [0, 1, 2].map(|a| c[a].saturating_sub(r));
            let hi = [0, 1, 2].map(|a| (c[a] + r).min(self.n3[a] - 1));
            for z in lo[2]..=hi[2] {
                for y in lo[1]..=hi[1] {
                    for x in lo[0]..=hi[0] {
                        let shell = x + r == c[0] || x == c[0] + r || y + r == c[1] || y == c[1] + r || z + r == c[2] || z == c[2] + r;
                        if !shell {
                            continue;
                        }
                        for &ti in &self.bin3[x + self.n3[0] * (y + self.n3[1] * z)] {
                            let t = &self.tris[ti as usize];
                            let q = closest_on_tri(p, t[0], t[1], t[2]);
                            let d = q.distance_squared(p);
                            if best.map(|b| d < b.0).unwrap_or(true) {
                                best = Some((d, q, ti as usize));
                            }
                        }
                    }
                }
            }
        }
        best.map(|b| (b.1, b.2))
    }
}

type PointKey = [u64; 3];

/// The edges of a triangulated surface (by their end points' exact coordinates) with the triangles on them.
fn surface_edges(tris: &[[DVec3; 3]]) -> HashMap<(PointKey, PointKey), Vec<usize>> {
    let key = |p: DVec3| p.to_array().map(|x| x.to_bits());
    let mut edges: HashMap<(PointKey, PointKey), Vec<usize>> = HashMap::new();
    for (ti, t) in tris.iter().enumerate() {
        for (a, b) in [(0, 1), (1, 2), (2, 0)] {
            let (ka, kb) = (key(t[a]), key(t[b]));
            edges.entry(if ka < kb { (ka, kb) } else { (kb, ka) }).or_default().push(ti);
        }
    }
    edges
}

fn point_of(k: &PointKey) -> DVec3 {
    DVec3::from_array(k.map(f64::from_bits))
}

/// Radius of curvature of each curved CAD face of a triangulated surface (planar faces are left out),
/// from the bend between neighbouring triangles of the face: their widths across the shared edge over the
/// angle between their normals. A quarter of the way up the sorted estimates, so a fillet's small radius
/// wins over its length along the edge.
pub fn face_radii(tris: &[[DVec3; 3]], face: &[u32]) -> HashMap<u32, f64> {
    let mut est: HashMap<u32, Vec<f64>> = HashMap::new();
    for ((ka, kb), ts) in &surface_edges(tris) {
        let [i, j] = ts[..] else { continue };
        if face[i] != face[j] {
            continue;
        }
        let normal = |t: &[DVec3; 3]| (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
        let theta = normal(&tris[i]).dot(normal(&tris[j])).clamp(-1.0, 1.0).acos();
        if !(1e-3..1.2).contains(&theta) {
            continue;
        }
        // each triangle's width across the edge (its far corner's distance from the edge's line)
        let (a, b) = (point_of(ka), point_of(kb));
        let e = (b - a).normalize_or_zero();
        let width = |t: &[DVec3; 3]| t.iter().map(|p| (*p - a - e * (*p - a).dot(e)).length()).fold(0.0, f64::max);
        est.entry(face[i]).or_default().push((width(&tris[i]) + width(&tris[j])) / (2.0 * theta));
    }
    est.into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(f, mut v)| {
            v.sort_unstable_by(f64::total_cmp);
            (f, v[v.len() / 4])
        })
        .collect()
}

/// The sharp edges of a triangulated surface: segments between triangles of different CAD faces that meet
/// at more than about 20°.
pub fn feature_edges(tris: &[[DVec3; 3]], face: &[u32]) -> Vec<[DVec3; 2]> {
    let normal = |t: &[DVec3; 3]| (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
    let mut out: Vec<(PointKey, PointKey)> = surface_edges(tris)
        .into_iter()
        .filter(|(_, ts)| ts.len() == 2 && face[ts[0]] != face[ts[1]] && normal(&tris[ts[0]]).dot(normal(&tris[ts[1]])) < 0.94)
        .map(|(k, _)| k)
        .collect();
    out.sort_unstable();
    out.iter().map(|(a, b)| [point_of(a), point_of(b)]).collect()
}

/// Closest point on triangle abc to p (Ericson, Real-Time Collision Detection 5.1.5).
pub fn closest_on_tri(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> DVec3 {
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
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
    let denom = 1.0 / (va + vb + vc);
    a + ab * (vb * denom) + ac * (vc * denom)
}

/// A 10-node tetrahedral mesh. Node order per element: corners 0-3, then mid-edge nodes on
/// edges 01, 12, 20, 03, 13, 23.
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub nodes: Vec<DVec3>,
    pub tets: Vec<[u32; 10]>,
    /// Boundary triangles (6 nodes: corners 0-2, mid-edge 01, 12, 20; outward), with their CAD face.
    pub faces: Vec<([u32; 6], u32)>,
    pub min_dihedral: f64,
    /// Part of each element (Surface body numbering) when the parts were meshed separately; empty when
    /// they were meshed as one.
    pub body: Vec<u32>,
    /// The element size used (mm): the one asked for, unless that was too small for the parts' size.
    pub size: f64,
}

pub const TET_EDGES: [(usize, usize); 6] = [(0, 1), (1, 2), (2, 0), (0, 3), (1, 3), (2, 3)];

/// Whether a 10-node tetrahedron's Jacobian is positive at its corners, centre and Gauss points.
pub fn tet10_valid(x: &[DVec3; 10]) -> bool {
    let a = 0.585_410_196_624_968_5;
    let b = 0.138_196_601_125_010_5;
    let pts = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0], [0.25; 4], [a, b, b, b], [b, a, b, b], [b, b, a, b], [b, b, b, a]];
    let v0 = (x[1] - x[0]).cross(x[2] - x[0]).dot(x[3] - x[0]);
    if v0 <= 0.0 {
        return false;
    }
    pts.iter().all(|l| {
        // dN/dL for corners (4L - 1) and mid-edge nodes (4 L_j, 4 L_i); derivatives along L1-L0, L2-L0, L3-L0
        let mut d = [DVec3::ZERO; 3];
        for c in 0..4 {
            let g = 4.0 * l[c] - 1.0;
            for (k, dk) in d.iter_mut().enumerate() {
                let s = if c == k + 1 { 1.0 } else if c == 0 { -1.0 } else { 0.0 };
                *dk += x[c] * (g * s);
            }
        }
        for (m, (i, j)) in TET_EDGES.iter().enumerate() {
            for (k, dk) in d.iter_mut().enumerate() {
                let si = if *i == k + 1 { 1.0 } else if *i == 0 { -1.0 } else { 0.0 };
                let sj = if *j == k + 1 { 1.0 } else if *j == 0 { -1.0 } else { 0.0 };
                *dk += x[4 + m] * (4.0 * (l[*j] * si + l[*i] * sj));
            }
        }
        d[0].cross(d[1]).dot(d[2]) > 1e-9 * v0
    })
}

fn tet_volume(a: DVec3, b: DVec3, c: DVec3, d: DVec3) -> f64 {
    (b - a).cross(c - a).dot(d - a) / 6.0
}

/// Smallest dihedral angle of a tetrahedron (degrees).
pub fn min_dihedral(p: [DVec3; 4]) -> f64 {
    // face normals (outward up to sign); dihedral angle along edge (i,j) is between faces not containing k,l
    let faces = [[1, 2, 3], [0, 3, 2], [0, 1, 3], [0, 2, 1]];
    let n: Vec<DVec3> = faces.iter().map(|f| (p[f[1]] - p[f[0]]).cross(p[f[2]] - p[f[0]]).normalize_or_zero()).collect();
    let mut m = 180.0f64;
    for i in 0..4 {
        for j in i + 1..4 {
            // the faces opposite vertices i and j share the edge between the other two
            let c = (-n[i].dot(n[j])).clamp(-1.0, 1.0);
            m = m.min(c.acos().to_degrees());
        }
    }
    m
}

/// Mesh the inside of `surf` with elements about `size` across.
pub fn mesh(surf: &Surface, size: f64, cancel: &dyn Fn() -> bool) -> Result<Mesh, String> {
    let lat = lattice(surf, size)?;
    stuff(surf, &lat, None, cancel)
}

/// Mesh each part on its own, on one shared lattice, and join the meshes where the parts touch:
/// the nodes on a shared face coincide, so the parts are bonded there and each element keeps its part.
pub fn mesh_parts(surf: &Surface, size: f64, cancel: &dyn Fn() -> bool) -> Result<Mesh, String> {
    let lat = lattice(surf, size)?;
    let mut parts = vec![];
    for b in 0..surf.bodies as u32 {
        match stuff(surf, &lat, Some(b), cancel) {
            Ok(m) => parts.push((b, m)),
            Err(e) if e.starts_with("The mesh is empty") => {}
            Err(e) => return Err(e),
        }
    }
    if parts.is_empty() {
        return Err("The mesh is empty: the element size is larger than the parts".into());
    }
    // the parts' meshes come from one lattice, so they meet node for node; only the points on the
    // surface differ a little, by up to a few times the inside test's nudge (`Surface::eps`)
    let mut m = weld(parts, (surf.eps.length() * 100.0).max(lat.hs.min_element() * 1e-6).min(lat.hs.min_element() * 0.01));
    m.size = lat.size;
    Ok(m)
}

/// Mesh each part on its own lattice with its own element size (`sizes[b]` for part b). The parts' meshes
/// are not joined, so a thin steel wall can be meshed finely inside a coarsely meshed casting; tie them
/// where they touch with `ties`.
#[allow(dead_code)] // used by the Python bindings (python/src/fea.rs)
pub fn mesh_sized(surf: &Surface, sizes: &[f64], cancel: &dyn Fn() -> bool) -> Result<Mesh, String> {
    let mut out = Mesh { min_dihedral: 180.0, ..Default::default() };
    for b in 0..surf.bodies as u32 {
        let Some((lo, hi)) = surf.body_bounds(b) else { continue };
        let size = sizes.get(b as usize).copied().or(sizes.last().copied()).unwrap_or(10.0);
        let lat = lattice_box(lo, hi, size).map_err(|e| format!("part {b}: {e}"))?;
        let m = match stuff(surf, &lat, Some(b), cancel) {
            Ok(m) => m,
            Err(e) if e.starts_with("The mesh is empty") => return Err(format!("part {b}: element size {size} mm is larger than the part")),
            Err(e) => return Err(e),
        };
        let off = out.nodes.len() as u32;
        out.min_dihedral = out.min_dihedral.min(m.min_dihedral);
        out.size = out.size.max(m.size);
        out.nodes.extend(m.nodes);
        out.tets.extend(m.tets.iter().map(|t| t.map(|v| v + off)));
        out.body.extend(std::iter::repeat_n(b, m.tets.len()));
        out.faces.extend(m.faces.iter().map(|(f, c)| (f.map(|v| v + off), *c)));
    }
    if out.tets.is_empty() {
        return Err("The mesh is empty: no parts".into());
    }
    Ok(out)
}

/// Tie the parts of a `mesh_sized` mesh where they touch. Each boundary node of the finer part (the
/// smaller element size; the higher part number when equal) lying within `tol` of another part's
/// boundary follows the point it lies on, interpolated over that 6-node face, by a stiff spring of
/// `beta` × E × the finer element size (E the larger of the two parts' moduli). `bond(a, b)` says
/// whether parts a and b are joined at all.
#[allow(dead_code)] // used by the Python bindings (python/src/fea.rs)
pub fn ties(mesh: &Mesh, sizes: &[f64], moduli: &[f64], tol: f64, beta: f64, bond: &dyn Fn(u32, u32) -> bool) -> Vec<crate::fea_solve::Coupling> {
    let nn = mesh.nodes.len();
    let mut node_body = vec![u32::MAX; nn];
    for (t, b) in mesh.tets.iter().zip(&mesh.body) {
        for v in t {
            node_body[*v as usize] = *b;
        }
    }
    let size = |b: u32| sizes.get(b as usize).copied().or(sizes.last().copied()).unwrap_or(10.0);
    let modulus = |b: u32| moduli.get(b as usize).copied().unwrap_or(0.0);
    // the faces in a grid of cells about the largest element size
    let cell = sizes.iter().copied().fold(tol * 4.0, f64::max).max(1e-6);
    let key = |p: DVec3| [(p.x / cell).floor() as i64, (p.y / cell).floor() as i64, (p.z / cell).floor() as i64];
    let mut grid: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
    for (fi, (f, _)) in mesh.faces.iter().enumerate() {
        let p = [0, 1, 2].map(|i| mesh.nodes[f[i] as usize]);
        let lo = key(p[0].min(p[1]).min(p[2]) - DVec3::splat(tol));
        let hi = key(p[0].max(p[1]).max(p[2]) + DVec3::splat(tol));
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    grid.entry([x, y, z]).or_default().push(fi as u32);
                }
            }
        }
    }
    let mut seen = vec![false; nn];
    let mut out = vec![];
    for (f, _) in &mesh.faces {
        for &v in f {
            if seen[v as usize] {
                continue;
            }
            seen[v as usize] = true;
            let s = node_body[v as usize];
            let p = mesh.nodes[v as usize];
            let mut best: Option<(f64, u32, DVec3)> = None;
            for &fi in grid.get(&key(p)).map(|x| x.as_slice()).unwrap_or(&[]) {
                let g = &mesh.faces[fi as usize].0;
                let m = node_body[g[0] as usize];
                if m == s || m == u32::MAX {
                    continue;
                }
                // the finer part follows; equal sizes: the higher number follows
                let (hs, hm) = (size(s), size(m));
                if hs > hm || (hs == hm && s < m) || !bond(s, m) {
                    continue;
                }
                let (a, b, c) = (mesh.nodes[g[0] as usize], mesh.nodes[g[1] as usize], mesh.nodes[g[2] as usize]);
                let q = closest_on_tri(p, a, b, c);
                let d = q.distance(p);
                if d <= tol && best.is_none_or(|x| d < x.0) {
                    best = Some((d, fi, q));
                }
            }
            let Some((_, fi, q)) = best else { continue };
            let g = mesh.faces[fi as usize].0;
            let (a, b, c) = (mesh.nodes[g[0] as usize], mesh.nodes[g[1] as usize], mesh.nodes[g[2] as usize]);
            // barycentric coordinates of q
            let n = (b - a).cross(c - a);
            let n2 = n.length_squared().max(1e-300);
            let l1 = (q - a).cross(c - a).dot(n) / n2;
            let l2 = (b - a).cross(q - a).dot(n) / n2;
            let l0 = 1.0 - l1 - l2;
            let w = [l0 * (2.0 * l0 - 1.0), l1 * (2.0 * l1 - 1.0), l2 * (2.0 * l2 - 1.0), 4.0 * l0 * l1, 4.0 * l1 * l2, 4.0 * l2 * l0];
            let m = node_body[g[0] as usize];
            let k = beta * modulus(s).max(modulus(m)) * size(s);
            out.push(crate::fea_solve::Coupling { a: vec![(v, 1.0)], b: (0..6).map(|i| (g[i], w[i])).collect(), k: [k; 3] });
        }
    }
    out
}

/// Join part meshes: merge nodes closer than `tol` and drop the faces two parts share.
fn weld(parts: Vec<(u32, Mesh)>, tol: f64) -> Mesh {
    let mut nodes: Vec<DVec3> = vec![];
    let mut grid: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
    let cell = tol * 4.0;
    let key = |p: DVec3| [(p.x / cell).floor() as i64, (p.y / cell).floor() as i64, (p.z / cell).floor() as i64];
    let mut out = Mesh { nodes: vec![], tets: vec![], faces: vec![], min_dihedral: 180.0, body: vec![], size: 0.0 };
    let mut faces: Vec<([u32; 6], u32)> = vec![];
    for (b, m) in parts {
        let map: Vec<u32> = m
            .nodes
            .iter()
            .map(|p| {
                let k = key(*p);
                for dz in -1..=1 {
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            if let Some(ids) = grid.get(&[k[0] + dx, k[1] + dy, k[2] + dz]) {
                                if let Some(id) = ids.iter().find(|i| nodes[**i as usize].distance(*p) <= tol) {
                                    return *id;
                                }
                            }
                        }
                    }
                }
                let id = nodes.len() as u32;
                nodes.push(*p);
                grid.entry(k).or_default().push(id);
                id
            })
            .collect();
        out.min_dihedral = out.min_dihedral.min(m.min_dihedral);
        out.tets.extend(m.tets.iter().map(|t| t.map(|v| map[v as usize])));
        out.body.extend(std::iter::repeat_n(b, m.tets.len()));
        faces.extend(m.faces.iter().map(|(f, cad)| (f.map(|v| map[v as usize]), *cad)));
    }
    // faces where two parts meet are inside the joined mesh
    let mut count: HashMap<[u32; 3], u32> = HashMap::new();
    let sorted = |f: &[u32; 6]| {
        let mut k = [f[0], f[1], f[2]];
        k.sort_unstable();
        k
    };
    for (f, _) in &faces {
        *count.entry(sorted(f)).or_default() += 1;
    }
    out.faces = faces.into_iter().filter(|(f, _)| count[&sorted(f)] == 1).collect();
    out.nodes = nodes;
    out
}

/// The background lattice: cells about `size` across fitted to the parts' bounding box.
struct Lattice {
    origin: DVec3,
    hs: DVec3,
    nc: [usize; 3],
    /// The element size used.
    size: f64,
}

fn lattice(surf: &Surface, size: f64) -> Result<Lattice, String> {
    if surf.tris.is_empty() {
        return Err("Nothing to mesh: no parts".into());
    }
    let (blo, bhi) = surf.bounds();
    lattice_box(blo, bhi, size)
}

fn lattice_box(blo: DVec3, bhi: DVec3, size: f64) -> Result<Lattice, String> {
    let ext = bhi - blo;
    let size = size.max(ext.max_element() / 400.0).max(1e-6);
    // cells fitted to the bounding box, one padding layer all round
    let cnt = [0, 1, 2].map(|a| ((ext[a] / size).round() as usize).max(1));
    let hs = DVec3::new(ext.x / cnt[0] as f64, ext.y / cnt[1] as f64, ext.z / cnt[2] as f64);
    let hs = hs.max(DVec3::splat(size * 1e-3));
    let nc = [cnt[0] + 2, cnt[1] + 2, cnt[2] + 2];
    if nc[0] * nc[1] * nc[2] > 3_000_000 {
        return Err(format!("Element size {size:.3} mm is too small for these parts; try a larger size"));
    }
    Ok(Lattice { origin: blo - hs, hs, nc, size })
}

/// Isosurface stuffing of the inside of part `only` (or of all the parts) on the lattice.
fn stuff(surf: &Surface, lat: &Lattice, only: Option<u32>, cancel: &dyn Fn() -> bool) -> Result<Mesh, String> {
    let (origin, hs, nc) = (lat.origin, lat.hs, lat.nc);
    let total_cells = nc[0] * nc[1] * nc[2];
    let nbk = [nc[0] + 1, nc[1] + 1, nc[2] + 1];
    let nblack = nbk[0] * nbk[1] * nbk[2];
    let black = |i: usize, j: usize, k: usize| i + nbk[0] * (j + nbk[1] * k);
    let red = |i: usize, j: usize, k: usize| nblack + i + nc[0] * (j + nc[1] * k);
    let nlat = nblack + total_cells;
    let mut pos = Vec::with_capacity(nlat);
    for k in 0..nbk[2] {
        for j in 0..nbk[1] {
            for i in 0..nbk[0] {
                pos.push(origin + DVec3::new(i as f64, j as f64, k as f64) * hs);
            }
        }
    }
    for k in 0..nc[2] {
        for j in 0..nc[1] {
            for i in 0..nc[0] {
                pos.push(origin + (DVec3::new(i as f64, j as f64, k as f64) + 0.5) * hs);
            }
        }
    }
    // sign of every lattice vertex: 1 inside, -1 outside, 0 on the surface (after warping)
    let mut sign: Vec<i8> = pos.iter().map(|p| if surf.inside_of(*p, only) { 1 } else { -1 }).collect();
    if cancel() {
        return Err("cancelled".into());
    }
    // lattice tetrahedra: two adjacent cell centres and the four corners of the face between them
    let mut tets: Vec<[u32; 4]> = vec![];
    for k in 0..nc[2] {
        for j in 0..nc[1] {
            for i in 0..nc[0] {
                let c0 = red(i, j, k);
                for a in 0..3 {
                    let (i1, j1, k1) = match a {
                        0 => (i + 1, j, k),
                        1 => (i, j + 1, k),
                        _ => (i, j, k + 1),
                    };
                    if i1 >= nc[0] || j1 >= nc[1] || k1 >= nc[2] {
                        continue;
                    }
                    let c1 = red(i1, j1, k1);
                    let q = match a {
                        0 => [black(i1, j, k), black(i1, j + 1, k), black(i1, j + 1, k + 1), black(i1, j, k + 1)],
                        1 => [black(i, j1, k), black(i, j1, k + 1), black(i + 1, j1, k + 1), black(i + 1, j1, k)],
                        _ => [black(i, j, k1), black(i + 1, j, k1), black(i + 1, j + 1, k1), black(i, j + 1, k1)],
                    };
                    for m in 0..4 {
                        let t = [c0, c1, q[m], q[(m + 1) % 4]].map(|x| x as u32);
                        // skip tets far from the inside
                        if t.iter().any(|v| sign[*v as usize] > 0) {
                            tets.push(t);
                        }
                    }
                }
            }
        }
    }
    // cut points on edges between inside and outside vertices (by bisection on the inside test)
    let mut cuts: HashMap<(u32, u32), DVec3> = HashMap::new();
    let key = |a: u32, b: u32| if a < b { (a, b) } else { (b, a) };
    for t in &tets {
        for (x, y) in TET_EDGES.iter().map(|(x, y)| (t[*x], t[*y])) {
            let (sa, sb) = (sign[x as usize], sign[y as usize]);
            if sa * sb < 0 {
                let k = key(x, y);
                if cuts.contains_key(&k) {
                    continue;
                }
                let (mut a, mut b) = if sa > 0 { (pos[x as usize], pos[y as usize]) } else { (pos[y as usize], pos[x as usize]) };
                for _ in 0..42 {
                    let m = (a + b) * 0.5;
                    if surf.inside_of(m, only) { a = m } else { b = m }
                }
                cuts.insert(k, (a + b) * 0.5);
            }
        }
    }
    if cancel() {
        return Err("cancelled".into());
    }
    // warp: a vertex closer to a cut point than a fraction of the edge length moves onto it
    let long = 0.24151;
    let short = 0.33269;
    let mut by_vertex: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();
    for k in cuts.keys() {
        by_vertex.entry(k.0).or_default().push(*k);
        by_vertex.entry(k.1).or_default().push(*k);
    }
    let mut verts: Vec<u32> = by_vertex.keys().copied().collect();
    verts.sort_unstable();
    for v in verts {
        let mut best: Option<(f64, DVec3)> = None;
        for k in &by_vertex[&v] {
            let o = if k.0 == v { k.1 } else { k.0 };
            if sign[o as usize] == 0 || sign[v as usize] == 0 {
                continue;
            }
            let Some(c) = cuts.get(k) else { continue };
            let len = pos[v as usize].distance(pos[o as usize]);
            // red-black edges are the short ones
            let is_short = (v as usize >= nblack) != (o as usize >= nblack);
            let alpha = if is_short { short } else { long };
            let d = c.distance(pos[v as usize]);
            if d < alpha * len && best.map(|b| d < b.0).unwrap_or(true) {
                best = Some((d, *c));
            }
        }
        if let Some((_, c)) = best {
            pos[v as usize] = c;
            sign[v as usize] = 0;
        }
    }
    // global ids for cut points that survive (both ends still strictly inside/outside)
    let mut cut_id: HashMap<(u32, u32), u32> = HashMap::new();
    let mut points = pos;
    let mut cut_list: Vec<(&(u32, u32), &DVec3)> = cuts.iter().collect();
    cut_list.sort_unstable_by_key(|(k, _)| **k);
    for (k, c) in cut_list {
        if sign[k.0 as usize] * sign[k.1 as usize] < 0 {
            cut_id.insert(*k, points.len() as u32);
            points.push(*c);
        }
    }
    // stencils
    let mut out: Vec<[u32; 4]> = vec![];
    let cut = |a: u32, b: u32| cut_id[&key(a, b)];
    for t in &tets {
        let s: Vec<i8> = t.iter().map(|v| sign[*v as usize]).collect();
        let pv: Vec<u32> = (0..4).filter(|&i| s[i] > 0).map(|i| t[i]).collect();
        let zv: Vec<u32> = (0..4).filter(|&i| s[i] == 0).map(|i| t[i]).collect();
        let nv: Vec<u32> = (0..4).filter(|&i| s[i] < 0).map(|i| t[i]).collect();
        if pv.is_empty() {
            continue;
        }
        if nv.is_empty() {
            out.push(*t);
            continue;
        }
        match (pv.len(), zv.len(), nv.len()) {
            (1, _, _) => {
                let mut e = vec![pv[0]];
                e.extend(&zv);
                e.extend(nv.iter().map(|n| cut(pv[0], *n)));
                out.push([e[0], e[1], e[2], e[3]]);
            }
            (2, 1, 1) => {
                // pyramid: base quad a b cb ca, apex z
                let (a, b, z, n) = (pv[0], pv[1], zv[0], nv[0]);
                let (ca, cb) = (cut(a, n), cut(b, n));
                for q in split_quad([a, b, cb, ca]) {
                    out.push([q[0], q[1], q[2], z]);
                }
            }
            (2, 0, 2) => {
                let (a, b, c, d) = (pv[0], pv[1], nv[0], nv[1]);
                prism(&mut out, [a, cut(a, c), cut(a, d)], [b, cut(b, c), cut(b, d)]);
            }
            (3, 0, 1) => {
                let (a, b, c, n) = (pv[0], pv[1], pv[2], nv[0]);
                prism(&mut out, [a, b, c], [cut(a, n), cut(b, n), cut(c, n)]);
            }
            _ => {}
        }
    }
    // orient, drop degenerate pieces, compact the vertex numbering
    let tiny = hs.x * hs.y * hs.z * 1e-9;
    let mut map = vec![u32::MAX; points.len()];
    let mut nodes = vec![];
    let mut corners_tets = vec![];
    for mut t in out {
        let p = t.map(|v| points[v as usize]);
        let v = tet_volume(p[0], p[1], p[2], p[3]);
        if v.abs() < tiny {
            continue;
        }
        if v < 0.0 {
            t.swap(2, 3);
        }
        for x in &mut t {
            if map[*x as usize] == u32::MAX {
                map[*x as usize] = nodes.len() as u32;
                nodes.push(points[*x as usize]);
            }
            *x = map[*x as usize];
        }
        corners_tets.push(t);
    }
    if corners_tets.is_empty() {
        return Err("The mesh is empty: the element size is larger than the parts".into());
    }
    snap_features(surf, &mut nodes, &corners_tets, hs.max_element(), only);
    let min_dihedral = corners_tets.iter().map(|t| min_dihedral(t.map(|v| nodes[v as usize]))).fold(180.0, f64::min);
    let mut m = quadratic(surf, nodes, corners_tets, min_dihedral);
    m.size = lat.size;
    Ok(m)
}

/// Put the mesh back on the parts' sharp edges. Isosurface stuffing rounds them off: a boundary edge that
/// runs from one face to the next cuts across the corner, which leaves a sawtooth along straight edges and
/// notches round the rim of a hole (stress raisers that are not in the part). For each such edge, its end
/// nearer the sharp edge moves onto it, unless that flattens an element. Nodes where part `only` touches
/// another part stay put, so the two parts' meshes still meet node for node there.
fn snap_features(surf: &Surface, nodes: &mut [DVec3], tets: &[[u32; 4]], size: f64, only: Option<u32>) {
    if surf.features.is_empty() || tets.is_empty() {
        return;
    }
    let cell = size.max(1e-9);
    let key = |p: DVec3| [(p.x / cell).floor() as i64, (p.y / cell).floor() as i64, (p.z / cell).floor() as i64];
    let mut grid: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
    for (i, f) in surf.features.iter().enumerate() {
        let (lo, hi) = (key(f[0].min(f[1])), key(f[0].max(f[1])));
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    grid.entry([x, y, z]).or_default().push(i as u32);
                }
            }
        }
    }
    let nearest = |p: DVec3| -> Option<(f64, DVec3)> {
        let k = key(p);
        let mut best: Option<(f64, DVec3)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for &i in grid.get(&[k[0] + dx, k[1] + dy, k[2] + dz]).map(|v| v.as_slice()).unwrap_or(&[]) {
                        let [a, b] = surf.features[i as usize];
                        let ab = b - a;
                        let t = ((p - a).dot(ab) / ab.length_squared().max(1e-300)).clamp(0.0, 1.0);
                        let q = a + ab * t;
                        let d = q.distance(p);
                        if best.is_none_or(|x| d < x.0) {
                            best = Some((d, q));
                        }
                    }
                }
            }
        }
        best
    };
    // boundary edges, and the CAD face of each boundary node
    let mut count: HashMap<[u32; 3], u32> = HashMap::new();
    for t in tets {
        for mut f in [[t[1], t[2], t[3]], [t[0], t[3], t[2]], [t[0], t[1], t[3]], [t[0], t[2], t[1]]] {
            f.sort_unstable();
            *count.entry(f).or_default() += 1;
        }
    }
    let mut bfaces: Vec<[u32; 3]> = count.into_iter().filter(|x| x.1 == 1).map(|x| x.0).collect();
    bfaces.sort_unstable();
    let mut node_face: HashMap<u32, u32> = HashMap::new();
    let mut edges: Vec<(u32, u32)> = vec![];
    for f in &bfaces {
        for v in f {
            node_face.entry(*v).or_insert_with(|| surf.closest(nodes[*v as usize]).map(|c| surf.face[c.1]).unwrap_or(u32::MAX));
        }
        edges.extend([(f[0], f[1]), (f[1], f[2]), (f[0], f[2])]);
    }
    edges.sort_unstable();
    edges.dedup();
    // each end that cuts a corner moves onto the sharp edge (the shortest move when there are several)
    let mut moves: HashMap<u32, (f64, DVec3)> = HashMap::new();
    for (a, b) in edges {
        if node_face[&a] == node_face[&b] {
            continue;
        }
        let (pa, pb) = (nodes[a as usize], nodes[b as usize]);
        let len = pa.distance(pb);
        // an edge with an end already on the sharp edge (the box sides, which the lattice fits) cuts no
        // corner: sliding that end along the sharp edge would only spoil the mesh
        let on = |p: DVec3| nearest(p).is_some_and(|x| x.0 < 1e-3 * size);
        if on(pa) || on(pb) {
            continue;
        }
        let Some((d, q)) = nearest((pa + pb) * 0.5) else { continue };
        if d > 0.5 * len {
            continue;
        }
        let (v, dv) = if pa.distance(q) <= pb.distance(q) { (a, pa.distance(q)) } else { (b, pb.distance(q)) };
        if dv < 1e-9 * size || dv > 0.6 * len || only.is_some_and(|b| surf.touches_other(nodes[v as usize], b, 1e-3 * size)) {
            continue;
        }
        if moves.get(&v).is_none_or(|m| dv < m.0) {
            moves.insert(v, (dv, q));
        }
    }
    if moves.is_empty() {
        return;
    }
    let quality = |nodes: &[DVec3], t: &[u32; 4]| {
        let p = t.map(|v| nodes[v as usize]);
        if tet_volume(p[0], p[1], p[2], p[3]) <= 0.0 { -1.0 } else { min_dihedral(p) }
    };
    let q0: Vec<f64> = tets.iter().map(|t| quality(nodes, t)).collect();
    let mut old: HashMap<u32, DVec3> = HashMap::new();
    for (v, (_, q)) in &moves {
        old.insert(*v, nodes[*v as usize]);
        nodes[*v as usize] = *q;
    }
    // undo the moves that flatten an element (its smallest dihedral angle under 10°, and worse than before)
    for _ in 0..4 {
        let mut undo = vec![];
        for (t, q) in tets.iter().zip(&q0) {
            let qn = quality(nodes, t);
            if qn < q.min(10.0) {
                undo.extend(t.iter().filter(|n| old.contains_key(n) && nodes[**n as usize] != old[n]).copied());
            }
        }
        if undo.is_empty() {
            break;
        }
        for n in undo {
            nodes[n as usize] = old[&n];
        }
    }
}

/// Split a quad (cyclic order) into two triangles along the diagonal through its smallest vertex id,
/// so neighbouring elements split a shared quad the same way.
fn split_quad(q: [u32; 4]) -> [[u32; 3]; 2] {
    let m = (0..4).min_by_key(|&i| q[i]).unwrap();
    let a = q[m];
    let b = q[(m + 1) % 4];
    let c = q[(m + 2) % 4];
    let d = q[(m + 3) % 4];
    [[a, b, c], [a, c, d]]
}

/// Three tetrahedra filling the prism between triangles a and b (a[i] joined to b[i]); the quad faces
/// are split through their smallest vertex, which always leaves a valid split.
fn prism(out: &mut Vec<[u32; 4]>, a: [u32; 3], b: [u32; 3]) {
    // rotate so the smallest vertex is a[0]
    let all = [a[0], a[1], a[2], b[0], b[1], b[2]];
    let m = (0..6).min_by_key(|&i| all[i]).unwrap();
    let (a, b) = if m < 3 { (a, b) } else { (b, a) };
    let r = m % 3;
    let a = [a[r], a[(r + 1) % 3], a[(r + 2) % 3]];
    let b = [b[r], b[(r + 1) % 3], b[(r + 2) % 3]];
    // a0 is the smallest: quads a0 a1 b1 b0 and a0 a2 b2 b0 split through a0; the third quad a1 a2 b2 b1
    // through its own smallest vertex
    out.push([a[0], a[1], a[2], if b[1].min(a[2]) < a[1].min(b[2]) { b[1] } else { b[2] }]);
    if b[1].min(a[2]) < a[1].min(b[2]) {
        // diagonal a2-b1 (smallest of quad is a2 or b1)
        out.push([a[0], b[1], a[2], b[2]]);
        out.push([a[0], b[0], b[1], b[2]]);
    } else {
        // diagonal a1-b2
        out.push([a[0], a[1], b[1], b[2]]);
        out.push([a[0], b[0], b[1], b[2]]);
    }
}

/// Add mid-edge nodes, find the boundary faces and their CAD faces.
fn quadratic(surf: &Surface, mut nodes: Vec<DVec3>, tets: Vec<[u32; 4]>, min_dihedral: f64) -> Mesh {
    // boundary faces of the linear mesh
    let mut face_count: HashMap<[u32; 3], (u32, [u32; 3])> = HashMap::new();
    for t in &tets {
        for f in [[t[1], t[2], t[3]], [t[0], t[3], t[2]], [t[0], t[1], t[3]], [t[0], t[2], t[1]]] {
            let mut k = f;
            k.sort_unstable();
            face_count.entry(k).and_modify(|e| e.0 += 1).or_insert((1, f));
        }
    }
    let mut bfaces: Vec<[u32; 3]> = face_count.values().filter(|e| e.0 == 1).map(|e| e.1).collect();
    bfaces.sort_unstable();
    // CAD face of every boundary node and face (closest surface triangle)
    let mut node_face: HashMap<u32, u32> = HashMap::new();
    for f in &bfaces {
        for v in f {
            node_face.entry(*v).or_insert_with(|| surf.closest(nodes[*v as usize]).map(|c| surf.face[c.1]).unwrap_or(0));
        }
    }
    let mut boundary_edges: HashMap<(u32, u32), ()> = HashMap::new();
    for f in &bfaces {
        for (a, b) in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
            boundary_edges.insert((a.min(b), a.max(b)), ());
        }
    }
    let mut mid: HashMap<(u32, u32), u32> = HashMap::new();
    let mut out = Vec::with_capacity(tets.len());
    for t in &tets {
        let mut e = [0u32; 10];
        e[..4].copy_from_slice(t);
        for (k, (i, j)) in TET_EDGES.iter().enumerate() {
            let (a, b) = (t[*i].min(t[*j]), t[*i].max(t[*j]));
            let id = *mid.entry((a, b)).or_insert_with(|| {
                let pa = nodes[a as usize];
                let pb = nodes[b as usize];
                let mut m = (pa + pb) * 0.5;
                // on a curved face, put the node on the surface
                if boundary_edges.contains_key(&(a, b)) && node_face.get(&a) == node_face.get(&b) {
                    if let Some((q, ti)) = surf.closest(m) {
                        if Some(&surf.face[ti]) == node_face.get(&a) && q.distance(m) < 0.25 * pa.distance(pb) {
                            m = q;
                        }
                    }
                }
                nodes.push(m);
                (nodes.len() - 1) as u32
            });
            e[4 + k] = id;
        }
        out.push(e);
    }
    // a mid-edge node moved onto a curved face can turn a thin element inside out: put the mid-edge nodes
    // of any such element back on its straight edges
    for _ in 0..3 {
        let mut bad = false;
        for e in &out {
            let x: [DVec3; 10] = std::array::from_fn(|i| nodes[e[i] as usize]);
            if tet10_valid(&x) {
                continue;
            }
            bad = true;
            for (k, (i, j)) in TET_EDGES.iter().enumerate() {
                nodes[e[4 + k] as usize] = (x[*i] + x[*j]) * 0.5;
            }
        }
        if !bad {
            break;
        }
    }
    let faces = bfaces
        .iter()
        .map(|f| {
            let m = |a: u32, b: u32| mid[&(a.min(b), a.max(b))];
            let c = (nodes[f[0] as usize] + nodes[f[1] as usize] + nodes[f[2] as usize]) / 3.0;
            let face = surf.closest(c).map(|x| surf.face[x.1]).unwrap_or(0);
            ([f[0], f[1], f[2], m(f[0], f[1]), m(f[1], f[2]), m(f[2], f[0])], face)
        })
        .collect();
    Mesh { nodes, tets: out, faces, min_dihedral, body: vec![], size: 0.0 }
}
