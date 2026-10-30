//! General boundary conditions on a [`Mesh`] (the solver side of the `"fe"`
//! config, `io::fe_setup`): fixed / prescribed-displacement / symmetry
//! constraints, pressure / traction / total-force / body loads, elastic
//! foundations (springs), and reference-point couplings (rigid = kinematic,
//! distributing = load spreading), with a reaction force and moment per
//! constraining BC.
//!
//! Everything here works on resolved facet sets ([`BcSet`]); turning face
//! selectors into facets is `io::fe_setup::resolve_fe_load`.
//!
//! # How constraints are imposed
//!
//! Master-slave elimination: the full displacement (mesh dofs) is written as
//! `u = T q + g` over independent master dofs `q`, the system solved is
//! `T^T K T q = T^T (f - K g)` (plus the generalized loads on free reference
//! point dofs), and `u` is recovered as `T q + g`.
//!
//! * A node held by component-wise constraints only (fixed, displacement, a
//!   symmetry plane normal to an axis) keeps its free components as masters
//!   (identity rows of `T`) and its held components in `g` -- the same
//!   elimination as [`super::solve::solve_with_bcs`].
//! * A node with any other set of linear constraints `d_a . u = c_a`
//!   (symmetry on an inclined or curved face, or several planes meeting at an
//!   edge) is solved in a rotated frame: its masters are the orthonormal
//!   directions left free by the constraints, `g` the least-norm particular
//!   solution.
//! * A rigidly coupled node follows its reference point, `u = U + theta x
//!   (x - x_rp)`: it has no masters of its own, and the RP gets six dofs,
//!   each prescribed (into `g`) or a master carrying the applied force or
//!   moment.
//!
//! Reactions are `K u - f` (the force the supports exert on the body) summed
//! over a BC's nodes; at a node held by several BCs the residual is split
//! between them along their constraint directions. A spring's reaction is
//! `-K_s u`, a rigid coupling's the generalized residual at the RP's
//! prescribed dofs. With the total applied load they balance to solver
//! precision.

use std::collections::{BTreeMap, HashMap};

use rayon::prelude::*;

use super::amg::{solve_cg_amg_with_nullspace, spgemm, transpose, AmgOptions};
use super::assemble::{assemble_stiffness, Csr};
use super::shapefun;
use super::solve::{is_size_error, linear_solver, solve_free_system_direct, FreeDofSystem, LinearSolver, SolveError};
use super::surface::{tri6_shape_and_grad, Facet};
use crate::elements::{element_type, face_nodes};
use crate::mesh_data::{ElementBlock, Mesh};

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}
fn unit(k: usize) -> V3 {
    let mut e = [0.0; 3];
    e[k] = 1.0;
    e
}
fn invalid<T>(msg: impl Into<String>) -> Result<T, SolveError> {
    Err(SolveError::Invalid(msg.into()))
}

// ------------------------------------------------------------- facets

/// A quadratic boundary facet: a 6-node triangle (gmsh order: corners, then
/// the midsides of edges 01, 12, 20) or an 8-node quadrilateral (corners in
/// cyclic order, then the midsides of edges 01, 12, 23, 30) -- the faces of
/// tet10 / wedge15 / pyramid13 / hex20 elements.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SurfFacet {
    Tri6([usize; 6]),
    Quad8([usize; 8]),
}

impl SurfFacet {
    pub fn nodes(&self) -> &[usize] {
        match self {
            SurfFacet::Tri6(n) => n,
            SurfFacet::Quad8(n) => n,
        }
    }
    fn corners(&self) -> &[usize] {
        match self {
            SurfFacet::Tri6(n) => &n[..3],
            SurfFacet::Quad8(n) => &n[..4],
        }
    }
    /// The same facet with its winding (and so its normal) reversed.
    pub fn flipped(&self) -> SurfFacet {
        match *self {
            SurfFacet::Tri6(n) => SurfFacet::Tri6([n[0], n[2], n[1], n[5], n[4], n[3]]),
            SurfFacet::Quad8(n) => SurfFacet::Quad8([n[0], n[3], n[2], n[1], n[7], n[6], n[5], n[4]]),
        }
    }
    /// Sorted corner ids: the facet's identity regardless of winding.
    fn key(&self) -> Vec<usize> {
        let mut k = self.corners().to_vec();
        k.sort_unstable();
        k
    }
}

impl From<Facet> for SurfFacet {
    fn from(f: Facet) -> Self {
        SurfFacet::Tri6(f)
    }
}

const TRI_A1: f64 = 0.445948490915965;
const TRI_W1: f64 = 0.223381589678011;
const TRI_A2: f64 = 0.091576213509771;
const TRI_W2: f64 = 0.109951743655322;
const TRI6_LOCAL: [[f64; 2]; 6] = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [0.5, 0.0], [0.5, 0.5], [0.0, 0.5]];
const QUAD8_LOCAL: [[f64; 2]; 8] =
    [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0], [0.0, -1.0], [1.0, 0.0], [0.0, 1.0], [-1.0, 0.0]];

/// Serendipity quad8 `N` and `dN/d(xi, eta)`.
fn quad8_shape_and_grad(x: f64, y: f64) -> ([f64; 8], [[f64; 2]; 8]) {
    let mut n = [0.0; 8];
    let mut d = [[0.0; 2]; 8];
    for (a, &[xi, yi]) in QUAD8_LOCAL.iter().enumerate() {
        if a < 4 {
            n[a] = 0.25 * (1.0 + x * xi) * (1.0 + y * yi) * (x * xi + y * yi - 1.0);
            d[a][0] = 0.25 * xi * (1.0 + y * yi) * (2.0 * x * xi + y * yi);
            d[a][1] = 0.25 * yi * (1.0 + x * xi) * (x * xi + 2.0 * y * yi);
        } else if xi == 0.0 {
            n[a] = 0.5 * (1.0 - x * x) * (1.0 + y * yi);
            d[a][0] = -x * (1.0 + y * yi);
            d[a][1] = 0.5 * (1.0 - x * x) * yi;
        } else {
            n[a] = 0.5 * (1.0 + x * xi) * (1.0 - y * y);
            d[a][0] = 0.5 * xi * (1.0 - y * y);
            d[a][1] = -y * (1.0 + x * xi);
        }
    }
    (n, d)
}

/// `(N, dN)` of a facet at a local point.
pub(crate) fn facet_shape(f: &SurfFacet, p: [f64; 2]) -> (Vec<f64>, Vec<[f64; 2]>) {
    match f {
        SurfFacet::Tri6(_) => {
            let (n, d) = tri6_shape_and_grad(p[0], p[1]);
            (n.to_vec(), d.to_vec())
        }
        SurfFacet::Quad8(_) => {
            let (n, d) = quad8_shape_and_grad(p[0], p[1]);
            (n.to_vec(), d.to_vec())
        }
    }
}

/// `(position, unnormalised normal = dx/dxi x dx/deta)` from `dN`.
fn facet_frame(points: &[V3], f: &SurfFacet, n: &[f64], d: &[[f64; 2]]) -> (V3, V3) {
    let mut x = [0.0; 3];
    let mut t = [[0.0; 3]; 2];
    for (a, &id) in f.nodes().iter().enumerate() {
        let p = points[id];
        for k in 0..3 {
            x[k] += n[a] * p[k];
            t[0][k] += d[a][0] * p[k];
            t[1][k] += d[a][1] * p[k];
        }
    }
    (x, cross(t[0], t[1]))
}

/// One quadrature point on a facet.
#[derive(Debug, Clone)]
struct SurfPoint {
    n: Vec<f64>,
    x: V3,
    /// unit normal (winding sign)
    normal: V3,
    /// `detJ * weight`
    w: f64,
}

fn facet_rule(f: &SurfFacet) -> Vec<([f64; 2], f64)> {
    match f {
        SurfFacet::Tri6(_) => {
            let (a1, a2) = (TRI_A1, TRI_A2);
            let p = [[a1, a1], [1.0 - 2.0 * a1, a1], [a1, 1.0 - 2.0 * a1], [a2, a2], [1.0 - 2.0 * a2, a2], [a2, 1.0 - 2.0 * a2]];
            let w = [TRI_W1, TRI_W1, TRI_W1, TRI_W2, TRI_W2, TRI_W2];
            p.iter().zip(w).map(|(p, w)| (*p, 0.5 * w)).collect()
        }
        SurfFacet::Quad8(_) => {
            let g = [-(0.6f64).sqrt(), 0.0, (0.6f64).sqrt()];
            let w = [5.0 / 9.0, 8.0 / 9.0, 5.0 / 9.0];
            let mut out = Vec::with_capacity(9);
            for i in 0..3 {
                for j in 0..3 {
                    out.push(([g[i], g[j]], w[i] * w[j]));
                }
            }
            out
        }
    }
}

fn facet_points(points: &[V3], f: &SurfFacet) -> Vec<SurfPoint> {
    facet_rule(f)
        .into_iter()
        .map(|(p, wt)| {
            let (n, d) = facet_shape(f, p);
            let (x, c) = facet_frame(points, f, &n, &d);
            let det = norm(c);
            SurfPoint { n, x, normal: scale(c, 1.0 / det), w: det * wt }
        })
        .collect()
}

/// Area of a facet set, integrated over the curved geometry.
pub fn surf_area(points: &[V3], facets: &[SurfFacet]) -> f64 {
    facets.iter().map(|f| facet_points(points, f).iter().map(|q| q.w).sum::<f64>()).sum()
}

/// Area-weighted centroid of a facet set.
pub fn surf_centroid(points: &[V3], facets: &[SurfFacet]) -> V3 {
    let (mut a, mut c) = (0.0, [0.0; 3]);
    for f in facets {
        for q in facet_points(points, f) {
            a += q.w;
            c = add(c, scale(q.x, q.w));
        }
    }
    if a > 0.0 {
        scale(c, 1.0 / a)
    } else {
        c
    }
}

/// How a uniform surface load is given.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SurfLoad {
    /// a fixed traction vector
    Traction(V3),
    /// `t = -p n` with `n` each facet's own (winding) normal
    Pressure(f64),
}

/// Consistent nodal forces `int N t dA`, added into `f` (length `3 N`).
pub fn add_surface_load(points: &[V3], facets: &[SurfFacet], load: SurfLoad, f: &mut [f64]) {
    for fc in facets {
        let nodes = fc.nodes();
        for q in facet_points(points, fc) {
            let t = match load {
                SurfLoad::Traction(t) => t,
                SurfLoad::Pressure(p) => scale(q.normal, -p),
            };
            for (a, &id) in nodes.iter().enumerate() {
                for k in 0..3 {
                    f[3 * id + k] += q.n[a] * t[k] * q.w;
                }
            }
        }
    }
}

/// `K_s u` of an elastic foundation `k` (per unit area) on `facets`.
fn spring_force(points: &[V3], facets: &[SurfFacet], k: V3, u: &[f64]) -> Vec<(usize, V3)> {
    let mut acc: HashMap<usize, V3> = HashMap::new();
    for fc in facets {
        let nodes = fc.nodes();
        for q in facet_points(points, fc) {
            let mut uq = [0.0; 3];
            for (b, &id) in nodes.iter().enumerate() {
                for c in 0..3 {
                    uq[c] += q.n[b] * u[3 * id + c];
                }
            }
            for (a, &id) in nodes.iter().enumerate() {
                let e = acc.entry(id).or_insert([0.0; 3]);
                for c in 0..3 {
                    e[c] += q.n[a] * k[c] * uq[c] * q.w;
                }
            }
        }
    }
    let mut v: Vec<(usize, V3)> = acc.into_iter().collect();
    v.sort_unstable_by_key(|e| e.0);
    v
}

/// Add the consistent foundation stiffness `int N_a N_b diag(k) dA` into the
/// assembled `K` (its pattern already holds every facet node pair).
fn add_spring_stiffness(points: &[V3], facets: &[SurfFacet], k: V3, kmat: &mut Csr, name: &str) -> Result<(), SolveError> {
    for fc in facets {
        let nodes = fc.nodes();
        let m = nodes.len();
        let mut mab = vec![0.0; m * m];
        for q in facet_points(points, fc) {
            for a in 0..m {
                for b in 0..m {
                    mab[a * m + b] += q.n[a] * q.n[b] * q.w;
                }
            }
        }
        for a in 0..m {
            for c in 0..3 {
                let r = 3 * nodes[a] + c;
                let (s, e) = (kmat.indptr[r], kmat.indptr[r + 1]);
                for b in 0..m {
                    let col = (3 * nodes[b] + c) as u32;
                    match kmat.indices[s..e].binary_search(&col) {
                        Ok(p) => kmat.data[s + p] += k[c] * mab[a * m + b],
                        Err(_) => {
                            return invalid(format!(
                                "{name}: a spring facet's nodes {nodes:?} are not all in one element -- the facet is not a face of the mesh"
                            ))
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Area-and-normal weighted unit normal at every node of `facets` (the
/// normal evaluated at the node's own place on each facet, times that
/// facet's area).
fn node_normals(points: &[V3], facets: &[SurfFacet]) -> Vec<(usize, V3)> {
    let mut acc: HashMap<usize, V3> = HashMap::new();
    for fc in facets {
        let area: f64 = facet_points(points, fc).iter().map(|q| q.w).sum();
        let local: &[[f64; 2]] = match fc {
            SurfFacet::Tri6(_) => &TRI6_LOCAL,
            SurfFacet::Quad8(_) => &QUAD8_LOCAL,
        };
        for (a, &id) in fc.nodes().iter().enumerate() {
            let (n, d) = facet_shape(fc, local[a]);
            let (_, c) = facet_frame(points, fc, &n, &d);
            let nc = norm(c);
            if nc > 0.0 {
                let e = acc.entry(id).or_insert([0.0; 3]);
                *e = add(*e, scale(c, area / nc));
            }
        }
    }
    let mut v: Vec<(usize, V3)> = acc.into_iter().filter(|(_, n)| norm(*n) > 0.0).map(|(i, n)| (i, scale(n, 1.0 / norm(n)))).collect();
    v.sort_unstable_by_key(|e| e.0);
    v
}

/// Lumped area share of every facet node (`area / nodes per facet`).
fn node_area_weights(points: &[V3], facets: &[SurfFacet]) -> Vec<(usize, f64)> {
    let mut acc: HashMap<usize, f64> = HashMap::new();
    for fc in facets {
        let area: f64 = facet_points(points, fc).iter().map(|q| q.w).sum();
        let share = area / fc.nodes().len() as f64;
        for &id in fc.nodes() {
            *acc.entry(id).or_insert(0.0) += share;
        }
    }
    let mut v: Vec<(usize, f64)> = acc.into_iter().collect();
    v.sort_unstable_by_key(|e| e.0);
    v
}

fn unique_nodes(facets: &[SurfFacet]) -> Vec<usize> {
    let mut n: Vec<usize> = facets.iter().flat_map(|f| f.nodes().iter().copied()).collect();
    n.sort_unstable();
    n.dedup();
    n
}

// ------------------------------------------------------ boundary faces

/// Every exterior face of the mesh (an element face no other element
/// shares), wound so its normal points out of its element. Crack faces are
/// exterior too (the split leaves the two sides unshared).
pub fn boundary_facets(mesh: &Mesh) -> Vec<SurfFacet> {
    let mut seen: HashMap<Vec<usize>, (SurfFacet, u32)> = HashMap::new();
    let mut order: Vec<Vec<usize>> = vec![];
    for b in &mesh.blocks {
        let Some(faces) = face_nodes(&b.element_type) else { continue };
        for row in &b.connectivity {
            for (local, _) in faces {
                let f = facet_of(row, local);
                let k = f.key();
                match seen.get_mut(&k) {
                    Some(e) => e.1 += 1,
                    None => {
                        let f = orient_out(&mesh.points, f, row, b);
                        order.push(k.clone());
                        seen.insert(k, (f, 1));
                    }
                }
            }
        }
    }
    order.into_iter().filter_map(|k| seen.remove(&k).filter(|e| e.1 == 1).map(|e| e.0)).collect()
}

fn facet_of(row: &[usize], local: &[usize]) -> SurfFacet {
    if local.len() == 6 {
        SurfFacet::Tri6(std::array::from_fn(|i| row[local[i]]))
    } else {
        SurfFacet::Quad8(std::array::from_fn(|i| row[local[i]]))
    }
}

fn elem_corner_centroid(points: &[V3], row: &[usize], b: &ElementBlock) -> V3 {
    let nc = element_type(&b.element_type).map_or(row.len(), |e| e.num_corners);
    let mut c = [0.0; 3];
    for &v in &row[..nc] {
        c = add(c, points[v]);
    }
    scale(c, 1.0 / nc as f64)
}

fn orient_out(points: &[V3], f: SurfFacet, row: &[usize], b: &ElementBlock) -> SurfFacet {
    let ec = elem_corner_centroid(points, row, b);
    let local: [f64; 2] = match f {
        SurfFacet::Tri6(_) => [1.0 / 3.0, 1.0 / 3.0],
        SurfFacet::Quad8(_) => [0.0, 0.0],
    };
    let (n, d) = facet_shape(&f, local);
    let (x, c) = facet_frame(points, &f, &n, &d);
    if dot(c, sub(x, ec)) < 0.0 {
        f.flipped()
    } else {
        f
    }
}

/// Exterior faces lying on the plane `x[axis] == value` (every node within
/// `tol`), wound outward.
pub fn boundary_facets_on_plane(mesh: &Mesh, all: &[SurfFacet], axis: usize, value: f64, tol: f64) -> Vec<SurfFacet> {
    all.iter().filter(|f| f.nodes().iter().all(|&v| (mesh.points[v][axis] - value).abs() <= tol)).cloned().collect()
}

/// `facets` wound so each normal points out of the element it is a face of.
/// A facet that is not an element face is left as given.
pub fn orient_facets_outward(mesh: &Mesh, facets: &[SurfFacet]) -> Vec<SurfFacet> {
    let mut want: HashMap<Vec<usize>, Option<V3>> = facets.iter().map(|f| (f.key(), None)).collect();
    for b in &mesh.blocks {
        let Some(faces) = face_nodes(&b.element_type) else { continue };
        for row in &b.connectivity {
            for (local, _) in faces {
                let mut k: Vec<usize> = local.iter().take(if local.len() == 6 { 3 } else { 4 }).map(|&i| row[i]).collect();
                k.sort_unstable();
                if let Some(slot) = want.get_mut(&k) {
                    if slot.is_none() {
                        *slot = Some(elem_corner_centroid(&mesh.points, row, b));
                    }
                }
            }
        }
    }
    facets
        .iter()
        .map(|f| match want.get(&f.key()).copied().flatten() {
            Some(ec) => {
                let local: [f64; 2] = match f {
                    SurfFacet::Tri6(_) => [1.0 / 3.0, 1.0 / 3.0],
                    SurfFacet::Quad8(_) => [0.0, 0.0],
                };
                let (n, d) = facet_shape(f, local);
                let (x, c) = facet_frame(&mesh.points, f, &n, &d);
                if dot(c, sub(x, ec)) < 0.0 {
                    f.flipped()
                } else {
                    f.clone()
                }
            }
            None => f.clone(),
        })
        .collect()
}

// ------------------------------------------------------------ body load

/// Consistent nodal forces `int N b dV` of a uniform body force `b` (force
/// per unit volume) over every element of the mesh, added into `f`.
pub fn add_body_force(mesh: &Mesh, b: V3, f: &mut [f64]) -> Result<(), SolveError> {
    for blk in &mesh.blocks {
        let ty = blk.element_type.as_str();
        let (gp, gw) = shapefun::gauss_points(ty)
            .ok_or_else(|| SolveError::Invalid(format!("body_force: no quadrature rule for element type {ty}")))?;
        let shapes: Vec<(Vec<f64>, Vec<[f64; 3]>)> = gp
            .iter()
            .map(|p| shapefun::eval_shape_and_grad(ty, p[0], p[1], p[2]))
            .collect::<Result<_, _>>()
            .map_err(|e| SolveError::Invalid(format!("body_force: {e}")))?;
        for (ei, row) in blk.connectivity.iter().enumerate() {
            for (g, (n, dn)) in shapes.iter().enumerate() {
                let mut j = [[0.0; 3]; 3];
                for (a, &v) in row.iter().enumerate() {
                    let x = mesh.points[v];
                    for k in 0..3 {
                        for l in 0..3 {
                            j[k][l] += x[k] * dn[a][l];
                        }
                    }
                }
                let det = j[0][0] * (j[1][1] * j[2][2] - j[1][2] * j[2][1]) - j[0][1] * (j[1][0] * j[2][2] - j[1][2] * j[2][0])
                    + j[0][2] * (j[1][0] * j[2][1] - j[1][1] * j[2][0]);
                if det <= 0.0 {
                    return invalid(format!("body_force: element {ei} of block {:?} has a non-positive Jacobian", blk.name));
                }
                let w = det * gw[g];
                for (a, &v) in row.iter().enumerate() {
                    for k in 0..3 {
                        f[3 * v + k] += n[a] * b[k] * w;
                    }
                }
            }
        }
    }
    Ok(())
}

// --------------------------------------------------------------- the model

/// What a BC does to its faces.
#[derive(Debug, Clone, PartialEq)]
pub enum BcKind {
    /// `u = 0`
    Fixed,
    /// prescribed components (`None` = free)
    Displacement([Option<f64>; 3]),
    /// `u . n = 0` with `n` the node's area-averaged face normal
    Symmetry,
    /// `t = -p n`, `n` outward (the facets must be wound outward)
    Pressure(f64),
    /// a uniform traction
    Traction(V3),
    /// a total force, spread as the uniform traction `f / area`
    Force(V3),
    /// an elastic foundation, stiffness per unit area per component
    Spring(V3),
    /// kinematic coupling to a reference point at `rp`: each of the RP's six
    /// dofs (`u`, then `rot`) is prescribed, or free under `force` / `moment`
    RemoteRigid { rp: V3, force: V3, moment: V3, u: [Option<f64>; 3], rot: [Option<f64>; 3] },
    /// `force` and `moment` (about `rp`) spread over the faces' nodes with
    /// their lumped area weights
    RemoteDistributing { rp: V3, force: V3, moment: V3 },
    /// a concentrated force on the BC's nodes (split equally between them;
    /// a point BC has one node, given as the degenerate facet
    /// `SurfFacet::Tri6([node; 6])`). Gives a singular local stress at the
    /// node -- fine away from the crack.
    PointForce(V3),
}

impl BcKind {
    /// The config's type name.
    pub fn type_name(&self) -> &'static str {
        match self {
            BcKind::Fixed => "fixed",
            BcKind::Displacement(_) => "displacement",
            BcKind::Symmetry => "symmetry",
            BcKind::Pressure(_) => "pressure",
            BcKind::Traction(_) => "traction",
            BcKind::Force(_) => "force",
            BcKind::Spring(_) => "spring",
            BcKind::RemoteRigid { .. } => "remote-rigid",
            BcKind::RemoteDistributing { .. } => "remote-distributing",
            BcKind::PointForce(_) => "point_force",
        }
    }
    /// Does this BC hold the body (and so get a reaction)?
    pub fn constrains(&self) -> bool {
        matches!(
            self,
            BcKind::Fixed | BcKind::Displacement(_) | BcKind::Symmetry | BcKind::Spring(_) | BcKind::RemoteRigid { .. }
        )
    }
}

/// One BC on a resolved facet set.
#[derive(Debug, Clone)]
pub struct BcSet {
    pub name: String,
    pub kind: BcKind,
    pub facets: Vec<SurfFacet>,
}

/// A whole load case.
#[derive(Debug, Clone, Default)]
pub struct FeProblem {
    pub bcs: Vec<BcSet>,
    /// force per unit volume over the whole body
    pub body_force: Option<V3>,
    /// Remove the six rigid-body modes of a model no BC holds (a
    /// self-equilibrated, traction-controlled load): a 3-2-1 pin set on three
    /// well-separated corner nodes far from the crack front (`A` fully,
    /// `B` across `AB`, `C` normal to `ABC`), which reacts nothing when the
    /// load is in equilibrium. Its reaction is reported as `rigid_body`.
    pub auto_rigid_body: bool,
    /// Periodic BCs (`fe::periodic`).
    pub periodic: Vec<PeriodicSet>,
}

/// A periodic BC: the `follower` faces are the `leader` faces moved by `offset`, and each follower node
/// moves as the leader surface does at `x - offset`, plus `jump`: `u(x) = u(x - offset) + jump`. A unit
/// cell under a mean strain `E` has `jump = E . offset`.
#[derive(Debug, Clone, PartialEq)]
pub struct PeriodicSet {
    pub name: String,
    pub leader: Vec<SurfFacet>,
    pub follower: Vec<SurfFacet>,
    pub offset: V3,
    pub jump: V3,
}

/// Relative out-of-balance of a free-free load above which `auto_rigid_body`
/// warns on stderr, and above which it refuses.
pub const AUTO_RIGID_WARN: f64 = 1e-6;
pub const AUTO_RIGID_ERROR: f64 = 1e-3;

/// The reaction of one constraining BC: the total force the support exerts
/// on the body, and its moment about `about` (the reference point of a
/// remote BC, else the area centroid of the BC's faces).
#[derive(Debug, Clone, PartialEq)]
pub struct Reaction {
    pub name: String,
    /// the BC type (`fixed`, `displacement`, `symmetry`, `spring`, `remote-rigid`)
    pub kind: String,
    pub force: V3,
    pub moment: V3,
    pub about: V3,
}

/// The total applied load (surface, body, distributing and rigid-RP loads)
/// as a force and its moment about `about` (the origin). With the reactions
/// it sums to zero.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppliedLoad {
    pub force: V3,
    pub moment: V3,
    pub about: V3,
}

/// A solved load case.
#[derive(Debug, Clone)]
pub struct FeSolution {
    pub displacement: Vec<V3>,
    pub reactions: Vec<Reaction>,
    pub applied: AppliedLoad,
    /// each rigid reference point's motion `(bc name, [U, theta])`
    pub rp_motion: Vec<(String, [f64; 6])>,
}

// ------------------------------------------------------------ constraints

/// One linear constraint `dir . u_node = value`, from BC `owner`.
#[derive(Debug, Clone, Copy)]
struct Row {
    dir: V3,
    value: f64,
    owner: usize,
}

/// How one constrained node's displacement is written.
#[derive(Debug, Clone)]
struct NodeFrame {
    /// orthonormal free directions (one master each)
    basis: Vec<V3>,
    /// particular solution of the constraints
    up: V3,
    /// the independent constraint rows kept (for splitting reactions)
    rows: Vec<Row>,
}

/// The unit axis `±e_k` a direction is within `1e-6` of, if any.
fn axis_of(d: V3) -> Option<(usize, f64)> {
    for k in 0..3 {
        for s in [1.0, -1.0] {
            let e = scale(unit(k), s);
            if norm(sub(d, e)) < 1e-6 {
                return Some((k, s));
            }
        }
    }
    None
}

fn solve_small(g: &[Vec<f64>], c: &[f64]) -> Option<Vec<f64>> {
    // Gaussian elimination with partial pivoting, r <= 3
    let r = c.len();
    let mut a: Vec<Vec<f64>> = (0..r).map(|i| {
        let mut row = g[i].clone();
        row.push(c[i]);
        row
    }).collect();
    for col in 0..r {
        let p = (col..r).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[p][col].abs() < 1e-14 {
            return None;
        }
        a.swap(col, p);
        for i in 0..r {
            if i != col {
                let f = a[i][col] / a[col][col];
                for j in col..=r {
                    a[i][j] -= f * a[col][j];
                }
            }
        }
    }
    Some((0..r).map(|i| a[i][r] / a[i][i]).collect())
}

/// Turn a node's constraint rows into its frame. `None` rows -> no frame.
fn node_frame(node: usize, rows: &[Row], names: &[String]) -> Result<NodeFrame, SolveError> {
    // later parallel rows replace earlier ones ("last case wins")
    let mut kept: Vec<Row> = vec![];
    for r in rows {
        match kept.iter_mut().find(|k| dot(k.dir, r.dir).abs() > 1.0 - 1e-9) {
            Some(k) => {
                let s = dot(k.dir, r.dir).signum();
                *k = Row { dir: k.dir, value: s * r.value, owner: r.owner };
            }
            None => kept.push(*r),
        }
    }
    let axes: Vec<Option<usize>> = kept.iter().map(|r| (0..3).find(|&k| r.dir == unit(k))).collect();
    if axes.iter().all(Option::is_some) {
        let mut up = [0.0; 3];
        let mut held = [false; 3];
        for (r, k) in kept.iter().zip(&axes) {
            up[k.unwrap()] = r.value;
            held[k.unwrap()] = true;
        }
        let basis = (0..3).filter(|&k| !held[k]).map(unit).collect();
        return Ok(NodeFrame { basis, up, rows: kept });
    }
    // independent rows by Gram-Schmidt; dependent ones must agree
    let mut q: Vec<V3> = vec![];
    let mut ind: Vec<Row> = vec![];
    let mut dep: Vec<Row> = vec![];
    for r in &kept {
        let mut v = r.dir;
        for b in &q {
            v = sub(v, scale(*b, dot(v, *b)));
        }
        if norm(v) > 1e-8 {
            q.push(scale(v, 1.0 / norm(v)));
            ind.push(*r);
        } else {
            dep.push(*r);
        }
    }
    let g: Vec<Vec<f64>> = ind.iter().map(|a| ind.iter().map(|b| dot(a.dir, b.dir)).collect()).collect();
    let c: Vec<f64> = ind.iter().map(|r| r.value).collect();
    let lam = solve_small(&g, &c).ok_or_else(|| SolveError::Invalid(format!("node {node}: singular constraint set")))?;
    let mut up = [0.0; 3];
    for (r, l) in ind.iter().zip(&lam) {
        up = add(up, scale(r.dir, *l));
    }
    let scale_v = c.iter().fold(1e-30f64, |m, v| m.max(v.abs()));
    for r in &dep {
        if (dot(r.dir, up) - r.value).abs() > 1e-9 * scale_v.max(1.0) {
            return invalid(format!(
                "node {node} is held by conflicting constraints: {} asks for {} along {:?}, but the other constraints there already fix that direction to {}. Two BCs prescribe different motions of the same point.",
                names[r.owner], r.value, r.dir, dot(r.dir, up)
            ));
        }
    }
    let basis: Vec<V3> = match q.len() {
        0 => vec![unit(0), unit(1), unit(2)],
        1 => {
            let n = q[0];
            let a = if n[0].abs() < 0.9 { unit(0) } else { unit(1) };
            let t1 = cross(n, a);
            let t1 = scale(t1, 1.0 / norm(t1));
            let t2 = cross(n, t1);
            vec![t1, scale(t2, 1.0 / norm(t2))]
        }
        2 => {
            let t = cross(q[0], q[1]);
            vec![scale(t, 1.0 / norm(t))]
        }
        _ => vec![],
    };
    Ok(NodeFrame { basis, up, rows: ind })
}

/// A rigid coupling, resolved.
struct RigidRp {
    bc: usize,
    rp: V3,
    nodes: Vec<usize>,
    /// prescribed value per RP dof (`U`, then `theta`)
    fixed: [Option<f64>; 6],
    load: [f64; 6],
    /// master index of each free RP dof
    master: [Option<usize>; 6],
}

/// External loads of `problem` into `f` (body force, tractions, pressures,
/// forces, distributing couplings); with `k`, spring stiffness into the
/// assembled `K`.
fn assemble_loads(mesh: &Mesh, problem: &FeProblem, f: &mut [f64], mut k: Option<&mut Csr>) -> Result<(), SolveError> {
    let pts = &mesh.points;
    if let Some(b) = problem.body_force {
        add_body_force(mesh, b, f)?;
    }
    for b in &problem.bcs {
        match &b.kind {
            BcKind::Pressure(p) => add_surface_load(pts, &b.facets, SurfLoad::Pressure(*p), f),
            BcKind::Traction(t) => add_surface_load(pts, &b.facets, SurfLoad::Traction(*t), f),
            BcKind::Force(fv) => {
                let a = surf_area(pts, &b.facets);
                if !(a > 0.0) {
                    return invalid(format!("{}: the selected faces have zero area, so a total force cannot be spread over them", b.name));
                }
                add_surface_load(pts, &b.facets, SurfLoad::Traction(scale(*fv, 1.0 / a)), f);
            }
            BcKind::Spring(kv) => {
                if let Some(k) = k.as_deref_mut() {
                    add_spring_stiffness(pts, &b.facets, *kv, k, &b.name)?;
                }
            }
            BcKind::RemoteDistributing { rp, force, moment } => {
                distribute(pts, &b.facets, *rp, *force, *moment, &b.name, f)?;
            }
            BcKind::PointForce(fv) => {
                let nodes = unique_nodes(&b.facets);
                let share = scale(*fv, 1.0 / nodes.len() as f64);
                for n in nodes {
                    for c in 0..3 {
                        f[3 * n + c] += share[c];
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Per-node constraint rows and rigid couplings of a problem's BCs.
struct Constraints {
    node_rows: BTreeMap<usize, Vec<Row>>,
    rigid: Vec<RigidRp>,
    rigid_of: HashMap<usize, usize>,
    /// loads on free reference-point dofs: net force and moment (about the origin)
    rp_force: V3,
    rp_moment: V3,
}

/// Constraint rows per node, and rigid couplings (no `rigid_body` pins).
fn build_constraints(mesh: &Mesh, problem: &FeProblem, names: &[String]) -> Result<Constraints, SolveError> {
    let pts = &mesh.points;
    let mut rp_loads_moment = [0.0; 3];
    let mut rp_loads_force = [0.0; 3];
    let mut node_rows: BTreeMap<usize, Vec<Row>> = BTreeMap::new();
    let mut rigid: Vec<RigidRp> = vec![];
    let mut rigid_of: HashMap<usize, usize> = HashMap::new();
    for (bi, b) in problem.bcs.iter().enumerate() {
        match &b.kind {
            BcKind::Fixed => {
                for n in unique_nodes(&b.facets) {
                    let r = node_rows.entry(n).or_default();
                    for c in 0..3 {
                        r.push(Row { dir: unit(c), value: 0.0, owner: bi });
                    }
                }
            }
            BcKind::Displacement(u) => {
                for n in unique_nodes(&b.facets) {
                    let r = node_rows.entry(n).or_default();
                    for c in 0..3 {
                        if let Some(v) = u[c] {
                            r.push(Row { dir: unit(c), value: v, owner: bi });
                        }
                    }
                }
            }
            BcKind::Symmetry => {
                for (n, nrm) in node_normals(pts, &b.facets) {
                    let dir = match axis_of(nrm) {
                        Some((k, _)) => unit(k),
                        None => nrm,
                    };
                    node_rows.entry(n).or_default().push(Row { dir, value: 0.0, owner: bi });
                }
            }
            BcKind::RemoteRigid { rp, force, moment, u, rot } => {
                let nodes = unique_nodes(&b.facets);
                for &n in &nodes {
                    if let Some(&other) = rigid_of.get(&n) {
                        return invalid(format!(
                            "{}: node {n} is rigidly coupled to two reference points ({} and {}) -- the coupled face sets overlap",
                            b.name, names[rigid[other].bc], b.name
                        ));
                    }
                    rigid_of.insert(n, rigid.len());
                }
                let fixed = [u[0], u[1], u[2], rot[0], rot[1], rot[2]];
                let load = [force[0], force[1], force[2], moment[0], moment[1], moment[2]];
                // free rotations need a rotational stiffness path
                if fixed[3..].iter().any(Option::is_none) {
                    let c = nodes.iter().fold([0.0; 3], |a, &n| add(a, pts[n]));
                    let c = scale(c, 1.0 / nodes.len() as f64);
                    let mut j = [[0.0; 3]; 3];
                    for &n in &nodes {
                        let r = sub(pts[n], c);
                        let rr = dot(r, r);
                        for a in 0..3 {
                            for bb in 0..3 {
                                j[a][bb] += if a == bb { rr } else { 0.0 } - r[a] * r[bb];
                            }
                        }
                    }
                    if inertia_singular(&j) {
                        return invalid(format!(
                            "{}: the coupled nodes all lie on one line (or are one point), so a free rotation of the reference point has nothing to turn against. Prescribe its rot, or couple a face that is not degenerate.",
                            b.name
                        ));
                    }
                }
                for c in 0..6 {
                    if fixed[c].is_none() {
                        if c < 3 {
                            rp_loads_force[c] += load[c];
                        } else {
                            rp_loads_moment[c - 3] += load[c];
                        }
                    }
                }
                let lf = [if fixed[0].is_none() { load[0] } else { 0.0 }, if fixed[1].is_none() { load[1] } else { 0.0 }, if fixed[2].is_none() { load[2] } else { 0.0 }];
                rp_loads_moment = add(rp_loads_moment, cross(*rp, lf));
                rigid.push(RigidRp { bc: bi, rp: *rp, nodes, fixed, load, master: [None; 6] });
            }
            _ => {}
        }
    }
    Ok(Constraints { node_rows, rigid, rigid_of, rp_force: rp_loads_force, rp_moment: rp_loads_moment })
}

/// Every kinematic restraint the constraint rows, prescribed RP dofs and springs impose.
fn restraints_of(mesh: &Mesh, problem: &FeProblem, c: &Constraints) -> Vec<Restraint> {
    let pts = &mesh.points;
    let (node_rows, rigid) = (&c.node_rows, &c.rigid);
    let mut restraints: Vec<Restraint> = vec![];
    for (n, rows) in node_rows {
        for r in rows {
            restraints.push(Restraint { point: pts[*n], dir: r.dir, rotation: false });
        }
    }
    for r in rigid {
        for c in 0..3 {
            if r.fixed[c].is_some() {
                restraints.push(Restraint { point: r.rp, dir: unit(c), rotation: false });
            }
            if r.fixed[3 + c].is_some() {
                restraints.push(Restraint { point: r.rp, dir: unit(c), rotation: true });
            }
        }
    }
    for b in &problem.bcs {
        if let BcKind::Spring(kv) = &b.kind {
            for n in unique_nodes(&b.facets) {
                for c in 0..3 {
                    if kv[c] != 0.0 {
                        restraints.push(Restraint { point: pts[n], dir: unit(c), rotation: false });
                    }
                }
            }
        }
    }
    // a periodic pair turns with the body only about its offset: turning about any other axis moves
    // one side against the other
    for p in &problem.periodic {
        let d = scale(p.offset, 1.0 / norm(p.offset).max(f64::MIN_POSITIVE));
        let a = if d[0].abs() < 0.9 { unit(0) } else { unit(1) };
        let e1 = cross(d, a);
        let e1 = scale(e1, 1.0 / norm(e1));
        let e2 = cross(d, e1);
        let at = unique_nodes(&p.follower).first().map_or([0.0; 3], |n| pts[*n]);
        restraints.push(Restraint { point: at, dir: e1, rotation: true });
        restraints.push(Restraint { point: at, dir: e2, rotation: true });
    }
    restraints
}

/// A node both rigidly coupled and held by a constraint BC is overconstrained.
fn check_overconstrained(c: &Constraints, names: &[String]) -> Result<(), SolveError> {
    let (node_rows, rigid, rigid_of) = (&c.node_rows, &c.rigid, &c.rigid_of);
    for (n, rows) in node_rows {
        if let Some(&ri) = rigid_of.get(n) {
            return invalid(format!(
                "node {n} is both rigidly coupled ({}) and held by {} -- the model is overconstrained there. Keep the coupled faces apart from the faces other BCs hold (they may only share edges if the coupling is distributing).",
                names[rigid[ri].bc],
                names[rows[0].owner]
            ));
        }
    }
    Ok(())
}

/// Whether any BC of `problem` restrains the body (fixed, displacement,
/// symmetry, spring, or a rigid remote BC with a prescribed dof).
fn bcs_hold_body(problem: &FeProblem) -> bool {
    problem.bcs.iter().any(|b| match &b.kind {
        BcKind::RemoteRigid { u, rot, .. } => u.iter().chain(rot).any(Option::is_some),
        BcKind::Displacement(u) => u.iter().any(Option::is_some),
        k => matches!(k, BcKind::Fixed | BcKind::Symmetry | BcKind::Spring(_)),
    })
}

/// Solve `problem` on `mesh` with the process-global [`linear_solver`].
pub fn solve_fe(mesh: &Mesh, e: f64, nu: f64, problem: &FeProblem) -> Result<FeSolution, SolveError> {
    solve_fe_with(mesh, e, nu, problem, linear_solver())
}

/// [`solve_fe`] with an explicit [`LinearSolver`].
pub fn solve_fe_with(mesh: &Mesh, e: f64, nu: f64, problem: &FeProblem, solver: LinearSolver) -> Result<FeSolution, SolveError> {
    let pts = &mesh.points;
    let npts = pts.len();
    let ndof = 3 * npts;
    let mut names: Vec<String> = problem.bcs.iter().map(|b| b.name.clone()).collect();
    let pin_owner = names.len();
    names.push("rigid_body".into());
    for b in &problem.bcs {
        if b.facets.is_empty() {
            return invalid(format!("{}: the BC has no faces to act on", b.name));
        }
    }
    let holds = bcs_hold_body(problem);
    if problem.auto_rigid_body && holds {
        return invalid(
            "rigid_body \"auto\" is for models no BC holds (a self-equilibrated, traction-controlled load), but this one has fixed / displacement / symmetry / spring / prescribed-RP BCs. Drop \"rigid_body\": \"auto\", or the holding BCs.",
        );
    }
    if !holds && !problem.auto_rigid_body {
        return invalid(
            "the model has rigid-body modes: no BC holds the body. Add at least one fixed, displacement, symmetry or spring BC, or a rigid remote BC with a prescribed u or rot.",
        );
    }

    // ---- stiffness (+ springs) and external loads
    let mut k = assemble_stiffness(mesh, e, nu)?;
    let mut f = vec![0.0; ndof];
    assemble_loads(mesh, problem, &mut f, Some(&mut k))?;

    let Masters { node_rows, rigid, rp_force: rp_loads_force, rp_moment: rp_loads_moment, frames, first, count, n_node_masters, nm, gptr, g, t } =
        masters_of(mesh, problem, &names, pin_owner, &f)?;

    // ---- reduced system
    let kg = k.matvec(&g);
    let b_full: Vec<f64> = f.iter().zip(&kg).map(|(a, b)| a - b).collect();
    let tt = transpose(&t);
    let mut rhs = tt.matvec(&b_full);
    for r in &rigid {
        for c in 0..6 {
            if let Some(m) = r.master[c] {
                rhs[m] += r.load[c];
            }
        }
    }
    let kt = spgemm(&k, &t, None);
    let kr = spgemm(&tt, &kt, None);
    drop(kt);

    if crate::repro::trace_enabled() {
        crate::repro::trace_hash("fe.k", kr.data.iter().copied().chain(kr.indices.iter().map(|&i| i as f64)));
        crate::repro::trace_hash("fe.rhs", rhs.iter().copied());
        crate::repro::trace_hash("fe.nullspace", nullspace(pts, &first, &count, &frames, &rigid, n_node_masters, nm));
    }
    if super::reuse::recording() {
        // for a second solve with the same constraints (`fe::reuse`)
        let rtol = match solver {
            LinearSolver::CgAmg { rtol } => rtol,
            LinearSolver::Direct => super::solve::DIRECT_FALLBACK_RTOL,
        };
        let ns = nullspace(pts, &first, &count, &frames, &rigid, n_node_masters, nm);
        super::reuse::record(super::reuse::SolveMemo::masters(t.clone(), kr.clone(), ns, 6, gptr.clone(), rtol, rhs.clone(), g.clone(), &f));
    }
    let q = solve_reduced(&kr, &rhs, solver, || nullspace(pts, &first, &count, &frames, &rigid, n_node_masters, nm), &gptr)?;
    crate::repro::trace_hash("fe.q", q.iter().copied());
    let tq = t.matvec(&q);
    let u: Vec<f64> = tq.iter().zip(&g).map(|(a, b)| a + b).collect();

    // ---- reactions
    let ku = k.matvec(&u);
    let res: Vec<f64> = ku.iter().zip(&f).map(|(a, b)| a - b).collect();
    let mut react: Vec<(V3, V3)> = vec![([0.0; 3], [0.0; 3]); problem.bcs.len() + 1];
    let mut about: Vec<V3> = problem
        .bcs
        .iter()
        .map(|b| match &b.kind {
            BcKind::RemoteRigid { rp, .. } | BcKind::RemoteDistributing { rp, .. } => *rp,
            // a point BC (degenerate facets, no area): about its node(s)
            _ if surf_area(pts, &b.facets) == 0.0 => {
                let nodes = unique_nodes(&b.facets);
                scale(nodes.iter().fold([0.0; 3], |a, &n| add(a, pts[n])), 1.0 / nodes.len() as f64)
            }
            _ => surf_centroid(pts, &b.facets),
        })
        .collect();
    // the pins' moment is taken about their first node
    about.push(
        node_rows.iter().filter(|(_, r)| r.iter().any(|x| x.owner == pin_owner)).min_by_key(|(_, r)| r.len()).map_or([0.0; 3], |(n, _)| pts[*n]),
    );
    let add_to = |owner: usize, x: V3, fv: V3, react: &mut Vec<(V3, V3)>| {
        let e = &mut react[owner];
        e.0 = add(e.0, fv);
        e.1 = add(e.1, cross(sub(x, about[owner]), fv));
    };
    let mut fk: Vec<&usize> = frames.keys().collect();
    fk.sort_unstable();
    for &n in fk {
        let fr = &frames[&n];
        let ri = [res[3 * n], res[3 * n + 1], res[3 * n + 2]];
        let rr = &fr.rows;
        let gm: Vec<Vec<f64>> = rr.iter().map(|a| rr.iter().map(|b| dot(a.dir, b.dir)).collect()).collect();
        let c: Vec<f64> = rr.iter().map(|a| dot(a.dir, ri)).collect();
        if let Some(lam) = solve_small(&gm, &c) {
            for (row, l) in rr.iter().zip(lam) {
                add_to(row.owner, pts[n], scale(row.dir, l), &mut react);
            }
        }
    }
    let mut rp_motion = vec![];
    for r in &rigid {
        let mut gen = [0.0; 6];
        for &n in &r.nodes {
            let ri = [res[3 * n], res[3 * n + 1], res[3 * n + 2]];
            let m = cross(sub(pts[n], r.rp), ri);
            for c in 0..3 {
                gen[c] += ri[c];
                gen[3 + c] += m[c];
            }
        }
        let mut fv = [0.0; 3];
        let mut mv = [0.0; 3];
        for c in 0..3 {
            fv[c] = gen[c] - if r.fixed[c].is_none() { r.load[c] } else { 0.0 };
            mv[c] = gen[3 + c] - if r.fixed[3 + c].is_none() { r.load[3 + c] } else { 0.0 };
        }
        react[r.bc] = (fv, mv);
        let mut motion = [0.0; 6];
        for c in 0..6 {
            motion[c] = match (r.fixed[c], r.master[c]) {
                (Some(v), _) => v,
                (None, Some(m)) => q[m],
                _ => 0.0,
            };
        }
        rp_motion.push((names[r.bc].clone(), motion));
    }
    for (bi, b) in problem.bcs.iter().enumerate() {
        if let BcKind::Spring(kv) = &b.kind {
            for (n, ks) in spring_force(pts, &b.facets, *kv, &u) {
                add_to(bi, pts[n], scale(ks, -1.0), &mut react);
            }
        }
    }
    let reactions = problem
        .bcs
        .iter()
        .enumerate()
        .filter(|(_, b)| b.kind.constrains())
        .map(|(bi, b)| Reaction {
            name: b.name.clone(),
            kind: b.kind.type_name().to_string(),
            force: react[bi].0,
            moment: react[bi].1,
            about: about[bi],
        })
        .chain(problem.auto_rigid_body.then(|| Reaction {
            name: "rigid_body".into(),
            kind: "rigid-body-pins".into(),
            force: react[pin_owner].0,
            moment: react[pin_owner].1,
            about: about[pin_owner],
        }))
        .collect();

    // ---- the applied total, about the origin
    let mut af = rp_loads_force;
    let mut am = rp_loads_moment;
    for i in 0..npts {
        let fi = [f[3 * i], f[3 * i + 1], f[3 * i + 2]];
        af = add(af, fi);
        am = add(am, cross(pts[i], fi));
    }
    Ok(FeSolution {
        displacement: u.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
        reactions,
        applied: AppliedLoad { force: af, moment: am, about: [0.0; 3] },
        rp_motion,
    })
}

/// Net load of a free-free model about the centroid of its corner nodes.
struct Balance {
    /// element corner nodes not rigidly coupled (the pin candidates)
    cand: Vec<usize>,
    /// centroid of `cand`: the moment reference
    c: V3,
    fs: V3,
    ms: V3,
    rf: f64,
    rm: f64,
    worst: f64,
}

/// A problem's constrained dofs written through masters: `u = T q + g`, with
/// the per-node frames, rigid couplings and the RP loads (the part of
/// [`solve_fe_with`] that does not depend on the stiffness, so a nonlinear
/// solve can reuse it at every iteration).
pub(crate) struct Masters {
    node_rows: BTreeMap<usize, Vec<Row>>,
    rigid: Vec<RigidRp>,
    rp_force: V3,
    rp_moment: V3,
    frames: HashMap<usize, NodeFrame>,
    first: Vec<usize>,
    count: Vec<u8>,
    n_node_masters: usize,
    nm: usize,
    gptr: Vec<usize>,
    g: Vec<f64>,
    t: Csr,
}

fn masters_of(mesh: &Mesh, problem: &FeProblem, names: &[String], pin_owner: usize, f: &[f64]) -> Result<Masters, SolveError> {
    let pts = &mesh.points;
    let npts = pts.len();
    let ndof = 3 * npts;
    // ---- constraint rows per node, and rigid couplings
    let mut cons = build_constraints(mesh, problem, names)?;
    if problem.auto_rigid_body {
        auto_pins(mesh, f, cons.rp_force, cons.rp_moment, &cons.rigid_of, pin_owner, &mut cons.node_rows, true)?;
    }
    // ---- every rigid-body mode must be held (rank test)
    check_rigid_modes_held(&restraints_of(mesh, problem, &cons))?;
    check_overconstrained(&cons, names)?;
    let (rp_loads_force, rp_loads_moment) = (cons.rp_force, cons.rp_moment);
    let Constraints { mut node_rows, mut rigid, rigid_of, .. } = cons;
    // ---- periodic followers: no masters of their own
    let ties = super::periodic::ties(pts, &problem.periodic, problem.bcs.len())?;
    // a follower node's own constraints must follow from its leaders' (a symmetry plane across both
    // sides of a unit cell): then they are dropped, the tie carries them
    let mut implied: Vec<usize> = vec![];
    for (n, tie) in &ties {
        let pname = &problem.periodic[tie.owner - problem.bcs.len()].name;
        if let Some(rows) = node_rows.get(n) {
            let follows = rows.iter().all(|r| {
                let mut v = dot(r.dir, tie.jump);
                // leaders of next to no weight (a node a hair off the leading side's edge) don't count
                for &(j, w) in tie.leaders.iter().filter(|x| x.1.abs() > 1e-3) {
                    let Some(lr) = node_rows.get(&j).and_then(|lr| lr.iter().find(|x| dot(x.dir, r.dir).abs() > 1.0 - 1e-6)) else { return false };
                    v += w * lr.value * dot(lr.dir, r.dir).signum();
                }
                // (a normal tilted by round-off picks up a little of the jump)
                (v - r.value).abs() <= 1e-6 * (1.0 + r.value.abs()) + 1e-3 * norm(tie.jump)
            });
            if !follows {
                return invalid(format!(
                    "{pname}: a node of the following side is also held by {} -- hold the leading side instead",
                    names.get(rows[0].owner).map(String::as_str).unwrap_or("another BC")
                ));
            }
            implied.push(*n);
        }
        if let Some(&ri) = rigid_of.get(n) {
            return invalid(format!("{pname}: a node of the following side is also rigidly coupled by {}", names[rigid[ri].bc]));
        }
    }
    for n in implied {
        node_rows.remove(&n);
    }
    // ---- nodes nothing references are held at zero
    let mut used = vec![false; npts];
    for b in &mesh.blocks {
        for row in &b.connectivity {
            for &v in row {
                used[v] = true;
            }
        }
    }

    // ---- frames and masters
    let mut frames: HashMap<usize, NodeFrame> = HashMap::new();
    let mut keys: Vec<usize> = node_rows.keys().copied().collect();
    keys.sort_unstable();
    for n in keys {
        frames.insert(n, node_frame(n, &node_rows[&n], names)?);
    }
    // master index of each node's first master, and how many
    let mut first = vec![0usize; npts];
    let mut count = vec![0u8; npts];
    let mut nm = 0usize;
    let mut gptr = vec![0usize];
    for i in 0..npts {
        first[i] = nm;
        let c = if !used[i] || rigid_of.contains_key(&i) || ties.contains_key(&i) {
            0
        } else {
            frames.get(&i).map_or(3, |fr| fr.basis.len())
        };
        count[i] = c as u8;
        nm += c;
        if c > 0 {
            gptr.push(nm);
        }
    }
    let n_node_masters = nm;
    for r in rigid.iter_mut() {
        for c in 0..6 {
            if r.fixed[c].is_none() {
                r.master[c] = Some(nm);
                nm += 1;
            }
        }
        if nm > *gptr.last().unwrap() {
            gptr.push(nm);
        }
    }
    if nm == 0 {
        return invalid("every dof of the model is prescribed -- there is nothing to solve for");
    }

    // ---- T (ndof x nm) and g
    let mut g = vec![0.0; ndof];
    let mut t_rows: Vec<Vec<(u32, f64)>> = vec![vec![]; ndof];
    for i in 0..npts {
        if let Some(&ri) = rigid_of.get(&i) {
            let r = &rigid[ri];
            let d = sub(pts[i], r.rp);
            // u = U + theta x d
            let terms: [[(usize, f64); 3]; 3] = [
                [(0, 1.0), (4, d[2]), (5, -d[1])],
                [(1, 1.0), (5, d[0]), (3, -d[2])],
                [(2, 1.0), (3, d[1]), (4, -d[0])],
            ];
            for c in 0..3 {
                for &(dof, coef) in &terms[c] {
                    if coef == 0.0 {
                        continue;
                    }
                    match (r.fixed[dof], r.master[dof]) {
                        (Some(v), _) => g[3 * i + c] += coef * v,
                        (None, Some(m)) => t_rows[3 * i + c].push((m as u32, coef)),
                        _ => unreachable!(),
                    }
                }
            }
            continue;
        }
        if !used[i] || ties.contains_key(&i) {
            continue;
        }
        match frames.get(&i) {
            None => {
                for c in 0..3 {
                    t_rows[3 * i + c].push(((first[i] + c) as u32, 1.0));
                }
            }
            Some(fr) => {
                for c in 0..3 {
                    g[3 * i + c] = fr.up[c];
                    for (m, b) in fr.basis.iter().enumerate() {
                        if b[c] != 0.0 {
                            t_rows[3 * i + c].push(((first[i] + m) as u32, b[c]));
                        }
                    }
                }
            }
        }
    }
    // a follower's rows are its leaders' rows, weighted (leaders that follow in turn first)
    let mut state: HashMap<usize, bool> = HashMap::new();
    fn resolve(
        n: usize,
        ties: &BTreeMap<usize, super::periodic::Tie>,
        state: &mut HashMap<usize, bool>,
        t_rows: &mut [Vec<(u32, f64)>],
        g: &mut [f64],
    ) -> Result<(), SolveError> {
        match state.get(&n) {
            Some(true) => return Ok(()),
            Some(false) => return invalid("periodic BCs: a node ends up following itself round a loop of periodic pairs"),
            None => {}
        }
        state.insert(n, false);
        let tie = &ties[&n];
        for &(j, _) in &tie.leaders {
            if ties.contains_key(&j) {
                resolve(j, ties, state, t_rows, g)?;
            }
        }
        for c in 0..3 {
            let mut acc: BTreeMap<u32, f64> = BTreeMap::new();
            let mut gv = tie.jump[c];
            for &(j, w) in &tie.leaders {
                for &(m, v) in &t_rows[3 * j + c] {
                    *acc.entry(m).or_default() += w * v;
                }
                gv += w * g[3 * j + c];
            }
            t_rows[3 * n + c] = acc.into_iter().filter(|x| x.1 != 0.0).collect();
            g[3 * n + c] = gv;
        }
        state.insert(n, true);
        Ok(())
    }
    for &n in ties.keys() {
        resolve(n, &ties, &mut state, &mut t_rows, &mut g)?;
    }
    for r in t_rows.iter_mut() {
        r.sort_unstable_by_key(|e| e.0);
    }
    let mut indptr = Vec::with_capacity(ndof + 1);
    indptr.push(0usize);
    let mut indices = vec![];
    let mut data = vec![];
    for r in &t_rows {
        for &(m, v) in r {
            indices.push(m);
            data.push(v);
        }
        indptr.push(indices.len());
    }
    drop(t_rows);
    let t = Csr { nrows: ndof, ncols: nm, indptr, indices, data };
    Ok(Masters {
        node_rows,
        rigid,
        rp_force: rp_loads_force,
        rp_moment: rp_loads_moment,
        frames,
        first,
        count,
        n_node_masters,
        nm,
        gptr,
        g,
        t,
    })
}

fn auto_balance(mesh: &Mesh, f: &[f64], rp_force: V3, rp_moment: V3, rigid_of: &HashMap<usize, usize>) -> Result<Balance, SolveError> {
    let pts = &mesh.points;
    // element corner nodes (not midsides), not rigidly coupled
    let mut corner = vec![false; pts.len()];
    for b in &mesh.blocks {
        let nc = element_type(&b.element_type).map_or(0, |e| e.num_corners);
        for row in &b.connectivity {
            for &v in &row[..nc.min(row.len())] {
                corner[v] = true;
            }
        }
    }
    let cand: Vec<usize> = (0..pts.len()).filter(|&i| corner[i] && !rigid_of.contains_key(&i)).collect();
    if cand.len() < 3 {
        return invalid("rigid_body auto: the mesh has fewer than three free corner nodes to pin");
    }
    let c = scale(cand.iter().fold([0.0; 3], |a, &i| add(a, pts[i])), 1.0 / cand.len() as f64);
    // the load must be in equilibrium (about the centroid)
    let mut fs = rp_force;
    let mut ms = sub(rp_moment, cross(c, rp_force));
    let mut diam: f64 = 0.0;
    for &i in &cand {
        diam = diam.max(norm(sub(pts[i], c)));
    }
    let (mut f_mag, mut m_mag) = (norm(rp_force), norm(rp_moment) + diam * norm(rp_force));
    for i in 0..pts.len() {
        let fi = [f[3 * i], f[3 * i + 1], f[3 * i + 2]];
        fs = add(fs, fi);
        ms = add(ms, cross(sub(pts[i], c), fi));
        f_mag += norm(fi);
        m_mag += norm(sub(pts[i], c)) * norm(fi);
    }
    if !(f_mag > 0.0) {
        return invalid("rigid_body auto: the model carries no load");
    }
    let (rf, rm) = (norm(fs) / f_mag, if m_mag > 0.0 { norm(ms) / m_mag } else { 0.0 });
    Ok(Balance { cand, c, fs, ms, rf, rm, worst: rf.max(rm) })
}

/// The 3-2-1 pin set of `auto_rigid_body`, after checking the load is
/// self-equilibrated.
fn auto_pins(
    mesh: &Mesh, f: &[f64], rp_force: V3, rp_moment: V3, rigid_of: &HashMap<usize, usize>, owner: usize,
    node_rows: &mut BTreeMap<usize, Vec<Row>>, warn: bool,
) -> Result<(), SolveError> {
    let pts = &mesh.points;
    let Balance { cand, c, fs, ms, rf, rm, worst } = auto_balance(mesh, f, rp_force, rp_moment, rigid_of)?;
    if worst > AUTO_RIGID_ERROR {
        return invalid(format!(
            "rigid_body auto: the applied load is not self-equilibrated -- net force {fs:?} ({rf:.2e} of the total), net moment about the body's centre {ms:?} ({rm:.2e}). With no BC holding the body every load must balance; add the missing reaction as a load, or hold the body with a BC instead."
        ));
    }
    if warn && worst > AUTO_RIGID_WARN {
        eprintln!(
            "warning: rigid_body auto: the load is out of balance by {worst:.2e} of its magnitude (net force {fs:?}, net moment {ms:?}); the pins will react it"
        );
    }
    // A: the corner node farthest from the crack front (or the centre)
    let front: V3 = match &mesh.crack_front {
        Some(cf) if !cf.node_ids.is_empty() => {
            scale(cf.node_ids.iter().fold([0.0; 3], |a, &i| add(a, pts[i])), 1.0 / cf.node_ids.len() as f64)
        }
        _ => c,
    };
    let far = |from: &dyn Fn(V3) -> f64| *cand.iter().max_by(|&&a, &&b| from(pts[a]).total_cmp(&from(pts[b]))).unwrap();
    let a = far(&|x| norm(sub(x, front)));
    let b = far(&|x| norm(sub(x, pts[a])));
    let ab = sub(pts[b], pts[a]);
    let e = scale(ab, 1.0 / norm(ab));
    let cn = far(&|x| {
        let r = sub(x, pts[a]);
        norm(sub(r, scale(e, dot(r, e))))
    });
    let n = cross(ab, sub(pts[cn], pts[a]));
    if !(norm(n) > 1e-9 * norm(ab) * norm(ab)) {
        return invalid("rigid_body auto: the mesh's corner nodes are collinear, so they cannot be pinned against rotation");
    }
    let n = scale(n, 1.0 / norm(n));
    let t = cross(n, e);
    let mut push = |node: usize, dir: V3| node_rows.entry(node).or_default().push(Row { dir, value: 0.0, owner });
    for k in 0..3 {
        push(a, unit(k));
    }
    // across AB: n and t (both normal to AB); snapped to an axis when they are one
    let snap = |d: V3| match axis_of(d) {
        Some((k, _)) => unit(k),
        None => d,
    };
    push(b, snap(n));
    push(b, snap(t));
    push(cn, snap(n));
    Ok(())
}

/// One kinematic restraint for [`free_rigid_modes`]: the motion of `point`
/// along `dir` is held (a constraint row, a spring component, a pin), or --
/// with `rotation` -- the rotation about `dir` is held (a rigid reference
/// point's prescribed `rot`).
#[derive(Debug, Clone, Copy)]
pub struct Restraint {
    pub point: V3,
    pub dir: V3,
    pub rotation: bool,
}

/// The rigid-body modes a set of restraints leaves free, by a rank test: each
/// restraint is the row `a_j = dir . (mode j at point)` over the six modes
/// (translations, and rotations about the points' centroid scaled by their
/// extent so both are O(1)); the modes the 6x6 Gram matrix `sum a a^T` does
/// not see (eigenvalue <= 1e-9 x the largest) are free. Each is returned as
/// a readable combination (`"translation y"`, `"0.71 translation y + 0.71
/// rotation about x"`).
pub fn free_rigid_modes(restraints: &[Restraint]) -> Vec<String> {
    if restraints.is_empty() {
        return (0..6).map(mode_name).collect();
    }
    let n = restraints.len() as f64;
    let c = scale(restraints.iter().fold([0.0; 3], |a, r| add(a, r.point)), 1.0 / n);
    let ext = restraints.iter().fold(0.0f64, |m, r| m.max(norm(sub(r.point, c))));
    let len = if ext > 0.0 { ext } else { 1.0 };
    let mut g = nalgebra::Matrix6::<f64>::zeros();
    for r in restraints {
        let d = scale(r.dir, 1.0 / norm(r.dir).max(f64::MIN_POSITIVE));
        let mut a = nalgebra::Vector6::<f64>::zeros();
        for j in 0..6 {
            a[j] = if r.rotation {
                // the rotation vector of mode j is e_{j-3} / len; scaled by len
                if j < 3 { 0.0 } else { d[j - 3] }
            } else if j < 3 {
                d[j]
            } else {
                dot(d, scale(cross(unit(j - 3), sub(r.point, c)), 1.0 / len))
            };
        }
        g += a * a.transpose();
    }
    let eig = nalgebra::SymmetricEigen::new(g);
    let lmax = eig.eigenvalues.iter().cloned().fold(0.0f64, f64::max);
    let mut out = vec![];
    for i in 0..6 {
        if eig.eigenvalues[i] <= 1e-9 * lmax || !(lmax > 0.0) {
            let v = eig.eigenvectors.column(i);
            let mut terms: Vec<(f64, usize)> = (0..6).map(|j| (v[j], j)).filter(|t| t.0.abs() > 0.1).collect();
            terms.sort_by(|a, b| b.0.abs().total_cmp(&a.0.abs()));
            out.push(if terms.len() == 1 {
                mode_name(terms[0].1)
            } else {
                let s = if terms[0].0 < 0.0 { -1.0 } else { 1.0 };
                terms.iter().map(|(w, j)| format!("{:.2} {}", s * w, mode_name(*j))).collect::<Vec<_>>().join(" + ")
            });
        }
    }
    out
}

fn mode_name(j: usize) -> String {
    let ax = ["x", "y", "z"];
    if j < 3 { format!("translation {}", ax[j]) } else { format!("rotation about {}", ax[j - 3]) }
}

/// The error for an under-constrained model, or `Ok` when every rigid-body
/// mode is restrained.
pub fn check_rigid_modes_held(restraints: &[Restraint]) -> Result<(), SolveError> {
    let free = free_rigid_modes(restraints);
    if free.is_empty() {
        return Ok(());
    }
    invalid(format!(
        "under-constrained: {} rigid-body mode{} free ({}). The BCs do not hold every translation and rotation, so the stiffness is singular; add a BC (fixed, displacement, symmetry, spring, a rigid remote BC with a prescribed u / rot) that holds {}, or use \"rigid_body\": \"auto\" for a self-equilibrated load with no holding BC.",
        free.len(),
        if free.len() == 1 { " is" } else { "s are" },
        free.join("; "),
        if free.len() == 1 { "it" } else { "them" }
    ))
}

/// The kind of one free rigid-body mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RigidModeKind {
    Translation,
    Rotation,
}

/// One free rigid-body mode of an under-restrained model.
#[derive(Debug, Clone, PartialEq)]
pub struct RigidMode {
    pub kind: RigidModeKind,
    /// unit direction of the translation, or of the rotation axis (sign:
    /// the largest component is positive)
    pub axis: V3,
    /// rotations: the point of the mode's axis nearest the centroid of the
    /// restrained points (the body's centroid when nothing is restrained);
    /// `None` for translations. In the mesh's units.
    pub point: Option<V3>,
    pub description: String,
    /// The mode as `[translation (3), rotation (3) x len]` about `centre`:
    /// the rigid motion `u(x) = t + (theta / len) x (x - centre)`. Unit norm;
    /// the modes of one report are mutually orthogonal in these coordinates.
    pub coefficients: [f64; 6],
}

/// The load's out-of-balance for `rigid_body: "auto"`, about `about`.
#[derive(Debug, Clone, PartialEq)]
pub struct EquilibriumImbalance {
    /// net applied force
    pub force: V3,
    /// net applied moment about `about`
    pub moment: V3,
    /// centroid of the model's corner nodes
    pub about: V3,
    /// `|net force| / sum |force|` and the same for moments
    pub relative_force: f64,
    pub relative_moment: f64,
    /// the larger of the two; `auto` refuses above [`AUTO_RIGID_ERROR`]
    /// (warns above [`AUTO_RIGID_WARN`])
    pub relative: f64,
}

/// What [`rigid_modes_free`] found. All points and lengths are in the mesh's
/// units (the same as the points `io::fe_setup::resolve_fe_load_detailed`
/// returns); there is no separate nondimensional scaling at this level.
#[derive(Debug, Clone, PartialEq)]
pub struct RigidModeReport {
    /// the free modes, an orthonormal basis (see [`RigidMode::coefficients`])
    pub free: Vec<RigidMode>,
    /// `free.len()`; 0 means the BCs hold the body
    pub n_free: usize,
    /// some BC holds the body (fixed / displacement / symmetry / spring /
    /// rigid remote with a prescribed dof); `rigid_body: "auto"` is refused then
    pub bcs_hold: bool,
    /// `rigid_body: "auto"` would make the model well-posed: no BC holds the
    /// body, the load is self-equilibrated within [`AUTO_RIGID_ERROR`], and
    /// three non-collinear corner nodes can be pinned
    pub auto_would_fix: bool,
    /// why `auto` cannot fix it (when `n_free > 0 && !auto_would_fix`)
    pub auto_blocker: Option<String>,
    /// the load balance, when it can be computed (it needs a load)
    pub imbalance: Option<EquilibriumImbalance>,
    /// centre the mode coefficients refer to
    pub centre: V3,
    /// length scale of the rotation coefficients
    pub len: f64,
}

/// Check the restraints of `problem` on `mesh` without assembling or solving:
/// the same constraint assembly as [`solve_fe_with`], up to the rigid-body
/// rank test. The `problem.auto_rigid_body` flag is ignored -- the BCs are
/// judged alone and `auto_would_fix` says whether `auto` would help.
/// Errors are the ones `solve_fe_with` would give for the same BCs (an empty
/// BC, overlapping rigid couplings, a degenerate coupled face, ...).
pub fn rigid_modes_free(mesh: &Mesh, problem: &FeProblem) -> Result<RigidModeReport, SolveError> {
    let mut names: Vec<String> = problem.bcs.iter().map(|b| b.name.clone()).collect();
    let pin_owner = names.len();
    names.push("rigid_body".into());
    for b in &problem.bcs {
        if b.facets.is_empty() {
            return invalid(format!("{}: the BC has no faces to act on", b.name));
        }
    }
    let bcs_hold = bcs_hold_body(problem);
    let mut f = vec![0.0; 3 * mesh.points.len()];
    assemble_loads(mesh, problem, &mut f, None)?;
    let cons = build_constraints(mesh, problem, &names)?;
    check_overconstrained(&cons, &names)?;
    let restraints = restraints_of(mesh, problem, &cons);
    let npts = mesh.points.len().max(1) as f64;
    let body_c = scale(mesh.points.iter().fold([0.0; 3], |a, &p| add(a, p)), 1.0 / npts);
    let body_len = mesh.points.iter().fold(0.0f64, |m, &p| m.max(norm(sub(p, body_c))));
    let (free, centre, len) = rigid_mode_basis(&restraints, body_c, body_len);

    let bal = auto_balance(mesh, &f, cons.rp_force, cons.rp_moment, &cons.rigid_of);
    let imbalance = bal.as_ref().ok().map(|b| EquilibriumImbalance {
        force: b.fs,
        moment: b.ms,
        about: b.c,
        relative_force: b.rf,
        relative_moment: b.rm,
        relative: b.worst,
    });
    let (mut auto_would_fix, mut auto_blocker) = (false, None);
    if !free.is_empty() {
        if bcs_hold {
            auto_blocker = Some("some BC already holds the body (fixed / displacement / symmetry / spring / rigid remote with a prescribed dof), so \"rigid_body\": \"auto\" is refused; add the missing restraint instead".to_string());
        } else {
            let mut rows = cons.node_rows.clone();
            match auto_pins(mesh, &f, cons.rp_force, cons.rp_moment, &cons.rigid_of, pin_owner, &mut rows, false) {
                Ok(()) => auto_would_fix = true,
                Err(e) => auto_blocker = Some(e.to_string()),
            }
        }
    }
    Ok(RigidModeReport { n_free: free.len(), free, bcs_hold, auto_would_fix, auto_blocker, imbalance, centre, len })
}

/// The free rigid-body modes of `restraints` as an orthonormal basis of
/// physical modes (translations / rotations with an axis location), plus the
/// centre and length the coefficients are scaled by. The null space of the
/// restraint Gram matrix is resolved on the canonical modes by pivoted
/// Gram-Schmidt, so a null space spanned by translations and rotations
/// about the centre comes out as exactly those.
fn rigid_mode_basis(restraints: &[Restraint], fallback_c: V3, fallback_len: f64) -> (Vec<RigidMode>, V3, f64) {
    let (c, len) = if restraints.is_empty() {
        (fallback_c, if fallback_len > 0.0 { fallback_len } else { 1.0 })
    } else {
        let n = restraints.len() as f64;
        let c = scale(restraints.iter().fold([0.0; 3], |a, r| add(a, r.point)), 1.0 / n);
        let ext = restraints.iter().fold(0.0f64, |m, r| m.max(norm(sub(r.point, c))));
        (c, if ext > 0.0 { ext } else { 1.0 })
    };
    let mut g = nalgebra::Matrix6::<f64>::zeros();
    for r in restraints {
        let d = scale(r.dir, 1.0 / norm(r.dir).max(f64::MIN_POSITIVE));
        let mut a = nalgebra::Vector6::<f64>::zeros();
        for j in 0..6 {
            a[j] = if r.rotation {
                if j < 3 { 0.0 } else { d[j - 3] }
            } else if j < 3 {
                d[j]
            } else {
                dot(d, scale(cross(unit(j - 3), sub(r.point, c)), 1.0 / len))
            };
        }
        g += a * a.transpose();
    }
    let eig = nalgebra::SymmetricEigen::new(g);
    let lmax = eig.eigenvalues.iter().cloned().fold(0.0f64, f64::max);
    let null: Vec<nalgebra::Vector6<f64>> = (0..6)
        .filter(|&i| eig.eigenvalues[i] <= 1e-9 * lmax || !(lmax > 0.0))
        .map(|i| eig.eigenvectors.column(i).into_owned())
        .collect();
    let project = |v: &nalgebra::Vector6<f64>| null.iter().fold(nalgebra::Vector6::<f64>::zeros(), |a, n| a + n * n.dot(v));
    let mut basis: Vec<nalgebra::Vector6<f64>> = vec![];
    while basis.len() < null.len() {
        let mut best: Option<(f64, nalgebra::Vector6<f64>)> = None;
        for j in 0..6 {
            let mut e = nalgebra::Vector6::<f64>::zeros();
            e[j] = 1.0;
            let mut r = project(&e);
            for b in &basis {
                r -= b * b.dot(&r);
            }
            let nr = r.norm();
            if best.as_ref().map_or(true, |(m, _)| nr > *m + 1e-12) {
                best = Some((nr, r));
            }
        }
        let (nr, r) = best.unwrap();
        basis.push(r / nr);
    }
    let modes = basis
        .iter()
        .map(|v| {
            let mut v = *v;
            let big = (0..6).max_by(|&a, &b| v[a].abs().total_cmp(&v[b].abs())).unwrap();
            if v[big] < 0.0 {
                v = -v;
            }
            // clean rounding noise
            for x in v.iter_mut() {
                if x.abs() < 1e-12 {
                    *x = 0.0;
                }
            }
            let (t, th) = ([v[0], v[1], v[2]], [v[3], v[4], v[5]]);
            let mut co = [0.0; 6];
            co.copy_from_slice(v.as_slice());
            if norm(th) <= 1e-6 {
                let axis = scale(t, 1.0 / norm(t));
                RigidMode { kind: RigidModeKind::Translation, axis, point: None, description: format!("translation along {}", dir_name(axis)), coefficients: co }
            } else {
                let w = scale(th, 1.0 / len);
                let axis = scale(w, 1.0 / norm(w));
                // u(x0) = 0 on the axis: x0 = c + (w x t) / |w|^2
                let mut p = add(c, scale(cross(w, t), 1.0 / dot(w, w)));
                for x in p.iter_mut() {
                    if x.abs() < 1e-12 * len {
                        *x = 0.0;
                    }
                }
                let pitch = dot(t, axis);
                let mut d = format!("rotation about {} through ({:.6e}, {:.6e}, {:.6e})", dir_name(axis), p[0], p[1], p[2]);
                if pitch.abs() > 1e-6 * norm(t).max(1e-300) {
                    d += &format!(" (screw: also translates {:.3e} per unit rotation along the axis)", pitch * len / norm(th));
                }
                RigidMode { kind: RigidModeKind::Rotation, axis, point: Some(p), description: d, coefficients: co }
            }
        })
        .collect();
    (modes, c, len)
}

/// "x" / "y" / "z" for an axis-aligned direction, else "(a, b, c)".
fn dir_name(d: V3) -> String {
    for (k, n) in ["x", "y", "z"].iter().enumerate() {
        if d[k].abs() > 1.0 - 1e-9 {
            return n.to_string();
        }
    }
    format!("({:.4}, {:.4}, {:.4})", d[0], d[1], d[2])
}

fn inertia_singular(j: &[[f64; 3]; 3]) -> bool {
    let tr = j[0][0] + j[1][1] + j[2][2];
    if !(tr > 0.0) {
        return true;
    }
    let det = j[0][0] * (j[1][1] * j[2][2] - j[1][2] * j[2][1]) - j[0][1] * (j[1][0] * j[2][2] - j[1][2] * j[2][0])
        + j[0][2] * (j[1][0] * j[2][1] - j[1][1] * j[2][0]);
    det.abs() < 1e-10 * (tr / 3.0).powi(3)
}

/// The distributing coupling's nodal forces, added into `f`.
fn distribute(points: &[V3], facets: &[SurfFacet], rp: V3, force: V3, moment: V3, name: &str, f: &mut [f64]) -> Result<(), SolveError> {
    let w = node_area_weights(points, facets);
    let wsum: f64 = w.iter().map(|e| e.1).sum();
    if !(wsum > 0.0) {
        return invalid(format!("{name}: the selected faces have zero area"));
    }
    let xbar = scale(w.iter().fold([0.0; 3], |a, &(n, wi)| add(a, scale(points[n], wi))), 1.0 / wsum);
    let m_eff = add(moment, cross(sub(rp, xbar), force));
    let mut j = [[0.0; 3]; 3];
    for &(n, wi) in &w {
        let r = sub(points[n], xbar);
        let rr = dot(r, r);
        for a in 0..3 {
            for b in 0..3 {
                j[a][b] += wi * (if a == b { rr } else { 0.0 } - r[a] * r[b]);
            }
        }
    }
    let alpha = if norm(m_eff) == 0.0 {
        [0.0; 3]
    } else {
        if inertia_singular(&j) {
            return invalid(format!(
                "{name}: the coupled nodes all lie on one line (or are one point), so a moment cannot be distributed over them"
            ));
        }
        let (_, ji) = crate::smallmat::det_inv_3x3(&j).map_err(|_| SolveError::Invalid(format!("{name}: singular coupling inertia")))?;
        [0, 1, 2].map(|a| (0..3).map(|b| ji[a][b] * m_eff[b]).sum::<f64>())
    };
    for &(n, wi) in &w {
        let r = sub(points[n], xbar);
        let fi = add(scale(force, wi / wsum), scale(cross(alpha, r), wi));
        for k in 0..3 {
            f[3 * n + k] += fi[k];
        }
    }
    Ok(())
}

/// The six rigid-body modes in master coordinates, orthonormalised.
fn nullspace(
    pts: &[V3], first: &[usize], count: &[u8], frames: &HashMap<usize, NodeFrame>, rigid: &[RigidRp], n_node: usize,
    nm: usize,
) -> Vec<f64> {
    let mut m = vec![vec![0.0; nm]; 6];
    // mode j displacement at x: translation e_j (j<3), rotation e_{j-3} x x
    let mode = |j: usize, x: V3| -> V3 { if j < 3 { unit(j) } else { cross(unit(j - 3), x) } };
    for i in 0..pts.len() {
        let c = count[i] as usize;
        if c == 0 {
            continue;
        }
        for j in 0..6 {
            let d = mode(j, pts[i]);
            match frames.get(&i) {
                None => {
                    for k in 0..3 {
                        m[j][first[i] + k] = d[k];
                    }
                }
                Some(fr) => {
                    for (k, b) in fr.basis.iter().enumerate() {
                        m[j][first[i] + k] = dot(*b, d);
                    }
                }
            }
        }
    }
    let _ = n_node;
    for r in rigid {
        for j in 0..6 {
            // the RP's (U, theta) under mode j
            let (uu, th) = if j < 3 { (unit(j), [0.0; 3]) } else { (cross(unit(j - 3), r.rp), unit(j - 3)) };
            for c in 0..6 {
                if let Some(mi) = r.master[c] {
                    m[j][mi] = if c < 3 { uu[c] } else { th[c - 3] };
                }
            }
        }
    }
    // fixed-chunk reduction: a rayon `sum` splits where work stealing
    // lands, so its rounding (and so the AMG near-nullspace, and every
    // digit downstream) changed run to run
    let dotv = |a: &[f64], b: &[f64]| crate::fe::amg::det_dot(a, b);
    for i in 0..6 {
        for j in 0..i {
            let (head, tail) = m.split_at_mut(i);
            let c = dotv(&tail[0], &head[j]);
            let mj = &head[j];
            tail[0].par_iter_mut().zip(mj.par_iter()).for_each(|(v, w)| *v -= c * w);
        }
        let nrm = dotv(&m[i], &m[i]).sqrt();
        if nrm > 1e-30 {
            m[i].par_iter_mut().for_each(|v| *v /= nrm);
        }
    }
    let mut out = vec![0.0; nm * 6];
    for i in 0..nm {
        for j in 0..6 {
            out[i * 6 + j] = m[j][i];
        }
    }
    out
}

fn solve_reduced(
    kr: &Csr, rhs: &[f64], solver: LinearSolver, ns: impl Fn() -> Vec<f64>, gptr: &[usize],
) -> Result<Vec<f64>, SolveError> {
    let sys = FreeDofSystem { k: kr.clone(), rhs: rhs.to_vec(), free_dofs: vec![] };
    let cg = |rtol: f64| solve_cg_amg_with_nullspace(kr, rhs, ns(), 6, gptr.to_vec(), rtol, &AmgOptions::default());
    match solver {
        LinearSolver::Direct => match solve_free_system_direct(&sys) {
            Ok(x) => Ok(x),
            Err(SolveError::Factor(e)) if is_size_error(&e) => {
                eprintln!("warning: the direct sparse solve cannot take this system of {} dofs ({e}); solving it by CG + AMG instead", kr.nrows);
                let r = cg(super::solve::DIRECT_FALLBACK_RTOL).map(|r| r.0).map_err(SolveError::Factor);
                crate::progress::annotate_last_solve(|s| s.fallback = Some(format!("direct solve not possible: {e}")));
                r
            }
            Err(SolveError::Factor(e)) => Err(SolveError::Factor(format!(
                "{e} -- the stiffness is singular: the BCs probably leave a rigid-body motion free (check that the constraints hold every translation and rotation)"
            ))),
            Err(e) => Err(e),
        },
        LinearSolver::CgAmg { rtol } => match cg(rtol) {
            Ok((x, _)) => Ok(x),
            Err(e) => {
                eprintln!("warning: {e} on the constrained (master-dof) system; falling back to the direct sparse solve");
                let x = solve_free_system_direct(&sys);
                crate::progress::annotate_last_solve(|s| s.fallback = Some(e.clone()));
                x
            }
        },
    }
}

// ------------------------------------------------------- elastic-plastic

/// Load stepping and Newton settings of [`solve_fe_plastic`].
#[derive(Debug, Clone)]
pub struct PlasticSolveOptions {
    /// equal load-factor increments from first yield (J2) or zero
    /// (deformation plasticity) to the full load; an increment that fails is
    /// halved
    pub n_steps: usize,
    /// Newton: `||r|| <= rtol max(||f_ext||, ||f_int||)` on the master dofs
    pub rtol: f64,
    pub max_newton: usize,
    pub max_cutbacks: usize,
}

impl Default for PlasticSolveOptions {
    fn default() -> Self {
        PlasticSolveOptions { n_steps: 10, rtol: 1e-6, max_newton: 30, max_cutbacks: 8 }
    }
}

/// One converged load increment of [`solve_fe_plastic`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlasticStep {
    pub load_factor: f64,
    pub newton_iterations: usize,
    pub residual: f64,
    /// Gauss points with plastic strain (J2) / beyond `sigma0` (deformation)
    pub n_plastic_points: usize,
    pub eqps_max: f64,
}

/// An elastic-plastic solve: the linear-elastic solution of the same load
/// (whose K is the LEFM one), the elastic-plastic displacement, and the state
/// at every Gauss point, element-major over the blocks in order, in the
/// order of [`super::assemble::element_rule`]'s rule.
#[derive(Debug, Clone)]
pub struct PlasticFeSolution {
    pub elastic: FeSolution,
    pub displacement: Vec<V3>,
    pub states: Vec<super::elastoplastic::GpState>,
    /// Gauss points of element `g` are `states[gp_off[g]..gp_off[g + 1]]`
    pub gp_off: Vec<usize>,
    /// load factor of first yield (J2; 0 for deformation plasticity)
    pub onset_load_factor: f64,
    pub steps: Vec<PlasticStep>,
    pub cutbacks: usize,
}

struct PElem<'a> {
    conn: &'a [usize],
    rule: &'static super::assemble::ElementRule,
    gp0: usize,
}

type Evaluated = (Vec<super::elastoplastic::GpState>, Vec<[[f64; 6]; 6]>, Vec<f64>);

/// Every Gauss point at displacement `u` from the converged states `old`:
/// the new states, the tangents, the internal force.
fn plastic_evaluate(
    mesh: &Mesh, elems: &[PElem], mat: &super::elastoplastic::Material, u: &[f64], old: &[super::elastoplastic::GpState],
) -> Result<Evaluated, String> {
    use super::assemble::{grads_at_gauss, voigt_b};
    use super::elastoplastic::GpState;
    let per: Vec<(Vec<GpState>, Vec<[[f64; 6]; 6]>, Vec<f64>)> = elems
        .par_iter()
        .map(|el| -> Result<_, String> {
            let xs: Vec<V3> = el.conn.iter().map(|&i| mesh.points[i]).collect();
            let w = 3 * el.rule.n;
            let ue: Vec<f64> = el.conn.iter().flat_map(|&i| [u[3 * i], u[3 * i + 1], u[3 * i + 2]]).collect();
            let gg = grads_at_gauss(el.rule, &xs).map_err(|_| "a degenerate element".to_string())?;
            let mut f = vec![0.0; w];
            let mut sts = Vec::with_capacity(gg.len());
            let mut dvs = Vec::with_capacity(gg.len());
            for (q, (dn, wt)) in gg.into_iter().enumerate() {
                let b = voigt_b(&dn);
                let ev: [f64; 6] = std::array::from_fn(|i| (0..w).map(|c| b[i * w + c] * ue[c]).sum());
                let et = [ev[0], ev[1], ev[2], 0.5 * ev[3], 0.5 * ev[4], 0.5 * ev[5]];
                let (s, d) = mat.update(&et, &old[el.gp0 + q])?;
                for c in 0..w {
                    f[c] += (0..6).map(|i| b[i * w + c] * s.sig[i]).sum::<f64>() * wt;
                }
                sts.push(s);
                dvs.push(d);
            }
            Ok((sts, dvs, f))
        })
        .collect::<Result<_, _>>()?;
    let mut st = Vec::with_capacity(old.len());
    let mut dv = Vec::with_capacity(old.len());
    let mut fint = vec![0.0; u.len()];
    for (el, (s, d, f)) in elems.iter().zip(per) {
        st.extend(s);
        dv.extend(d);
        for (a, &v) in el.conn.iter().enumerate() {
            for c in 0..3 {
                fint[3 * v + c] += f[3 * a + c];
            }
        }
    }
    Ok((st, dv, fint))
}

/// The tangent stiffness from per-Gauss-point Voigt tangents.
fn plastic_tangent(mesh: &Mesh, elems: &[PElem], dv: &[[[f64; 6]; 6]]) -> Result<Csr, SolveError> {
    use super::assemble::{assemble_element_matrices, grads_at_gauss, voigt_b};
    Ok(assemble_element_matrices(mesh, |g, rule, xs, out| {
        let el = &elems[g];
        let w = 3 * rule.n;
        out[..w * w].fill(0.0);
        let mut db = vec![0.0; 6 * w];
        for (q, (dn, wt)) in grads_at_gauss(rule, xs)?.into_iter().enumerate() {
            let b = voigt_b(&dn);
            let d = &dv[el.gp0 + q];
            db.fill(0.0);
            for i in 0..6 {
                for k in 0..6 {
                    if d[i][k] != 0.0 {
                        for c in 0..w {
                            db[i * w + c] += d[i][k] * b[k * w + c];
                        }
                    }
                }
            }
            for r in 0..w {
                for c in 0..w {
                    let s: f64 = (0..6).map(|i| b[i * w + r] * db[i * w + c]).sum();
                    out[r * w + c] += s * wt;
                }
            }
        }
        Ok(())
    })?)
}

fn vnorm(v: &[f64]) -> f64 {
    crate::fe::amg::det_dot(v, v).sqrt()
}

/// Elastic-plastic solve of `problem`: every load of the problem (tractions,
/// forces, prescribed displacements and RP motions, body force) ramps with
/// one load factor to 1 in increments, each a full Newton solve with the
/// consistent tangent on the same constrained (master-dof) system
/// [`solve_fe_with`] solves. Springs stay linear.
pub fn solve_fe_plastic(
    mesh: &Mesh, mat: &super::elastoplastic::Material, problem: &FeProblem, opts: &PlasticSolveOptions,
) -> Result<PlasticFeSolution, SolveError> {
    use super::elastoplastic::{GpState, Plasticity};
    let solver = linear_solver();
    let (e, nu) = (mat.e, mat.nu);
    // the linear-elastic solution (and every check of the BCs)
    let elastic = solve_fe_with(mesh, e, nu, problem, solver)?;
    let pts = &mesh.points;
    let ndof = 3 * pts.len();
    let mut names: Vec<String> = problem.bcs.iter().map(|b| b.name.clone()).collect();
    let pin_owner = names.len();
    names.push("rigid_body".into());
    let mut f = vec![0.0; ndof];
    assemble_loads(mesh, problem, &mut f, None)?;
    let ms = masters_of(mesh, problem, &names, pin_owner, &f)?;
    let tt = transpose(&ms.t);
    let mut rp_load = vec![0.0; ms.nm];
    for r in &ms.rigid {
        for c in 0..6 {
            if let Some(m) = r.master[c] {
                rp_load[m] += r.load[c];
            }
        }
    }
    let ns = || nullspace(pts, &ms.first, &ms.count, &ms.frames, &ms.rigid, ms.n_node_masters, ms.nm);

    let mut elems = Vec::with_capacity(mesh.num_elements());
    let mut gp_off = vec![0usize];
    for b in &mesh.blocks {
        let rule = super::assemble::element_rule(&b.element_type)?;
        for c in &b.connectivity {
            let gp0 = *gp_off.last().unwrap();
            elems.push(PElem { conn: c, rule, gp0 });
            gp_off.push(gp0 + rule.gw.len());
        }
    }
    let ngp = *gp_off.last().unwrap();
    let u_el: Vec<f64> = elastic.displacement.iter().flatten().copied().collect();
    // q of the elastic solution: u_el = T q_el + g
    let mut states = vec![GpState::default(); ngp];
    let ev = |u: &[f64], old: &[GpState]| plastic_evaluate(mesh, &elems, mat, u, old).map_err(SolveError::Invalid);

    // springs: their (linear) stiffness on the tangent's pattern
    let has_springs = problem.bcs.iter().any(|b| matches!(b.kind, BcKind::Spring(_)));
    let (_, dv0, _) = ev(&vec![0.0; ndof], &states)?;
    let k0 = plastic_tangent(mesh, &elems, &dv0)?;
    let k_spring = if has_springs {
        let mut ks = Csr { data: vec![0.0; k0.data.len()], ..k0.clone() };
        let mut scratch = vec![0.0; ndof];
        assemble_loads(mesh, problem, &mut scratch, Some(&mut ks))?;
        Some(ks)
    } else {
        None
    };
    drop(k0);

    // first yield: the elastic solution scales with the load
    let onset = match &mat.plasticity {
        Plasticity::J2 { .. } => {
            let (st, _, _) = ev(&u_el, &states)?;
            let smax = st.iter().map(|s| super::elastoplastic::Material::mises(&s.sig)).fold(0.0, f64::max);
            if smax > 0.0 { (mat.sigma_ref() / smax).min(1.0) } else { 1.0 }
        }
        Plasticity::Deformation { .. } => 0.0,
    };
    let mut lam = onset;
    let mut u: Vec<f64> = u_el.iter().map(|x| lam * x).collect();
    let mut steps = vec![];
    if lam > 0.0 {
        states = ev(&u, &states)?.0;
        steps.push(PlasticStep { load_factor: lam, newton_iterations: 0, residual: 0.0, n_plastic_points: 0, eqps_max: 0.0 });
    }
    let dlam0 = (1.0 - lam) / opts.n_steps.max(1) as f64;
    let mut dlam = dlam0;
    let (mut halvings, mut cutbacks) = (0usize, 0usize);
    let sref = mat.sigma_ref();
    while lam < 1.0 - 1e-12 {
        let lam1 = (lam + dlam).min(1.0);
        // predictor: the elastic increment (it carries the prescribed motion)
        let mut ut: Vec<f64> = u.iter().zip(&u_el).map(|(a, b)| a + (lam1 - lam) * b).collect();
        let mut done = None;
        let mut fail = String::new();
        for it in 0..=opts.max_newton {
            let (st, dv, mut fint) = match ev(&ut, &states) {
                Ok(x) => x,
                Err(e) => {
                    fail = e.to_string();
                    break;
                }
            };
            if let Some(ks) = &k_spring {
                for (a, b) in fint.iter_mut().zip(ks.matvec(&ut)) {
                    *a += b;
                }
            }
            let r_full: Vec<f64> = f.iter().zip(&fint).map(|(fe, fi)| lam1 * fe - fi).collect();
            let mut r = tt.matvec(&r_full);
            for (a, b) in r.iter_mut().zip(&rp_load) {
                *a += lam1 * b;
            }
            let rn = vnorm(&r);
            let fe_red: Vec<f64> = tt.matvec(&f).iter().zip(&rp_load).map(|(a, b)| lam1 * (a + b)).collect();
            let refn = vnorm(&fe_red).max(vnorm(&fint)).max(1e-300);
            if rn <= opts.rtol * refn {
                done = Some((st, it, rn / refn));
                break;
            }
            if it == opts.max_newton {
                fail = format!("Newton: no convergence in {} iterations (residual {:.2e})", opts.max_newton, rn / refn);
                break;
            }
            let mut kt = plastic_tangent(mesh, &elems, &dv)?;
            if let Some(ks) = &k_spring {
                for (a, b) in kt.data.iter_mut().zip(&ks.data) {
                    *a += b;
                }
            }
            let kr = spgemm(&tt, &spgemm(&kt, &ms.t, None), None);
            drop(kt);
            let dq = match solve_reduced(&kr, &r, solver, ns, &ms.gptr) {
                Ok(x) => x,
                Err(e) => {
                    fail = e.to_string();
                    break;
                }
            };
            for (a, b) in ut.iter_mut().zip(ms.t.matvec(&dq)) {
                *a += b;
            }
        }
        match done {
            Some((st, its, res)) => {
                states = st;
                u = ut;
                lam = lam1;
                let n_plastic = states.iter().filter(|s| is_plastic(s, sref, mat)).count();
                let eqps_max = states.iter().map(|s| s.p).fold(0.0, f64::max);
                eprintln!("  plastic step: load {lam:.4}, {its} Newton iterations, {n_plastic} plastic points, eqps max {eqps_max:.3e}");
                steps.push(PlasticStep { load_factor: lam, newton_iterations: its, residual: res, n_plastic_points: n_plastic, eqps_max });
                if halvings > 0 && its <= 4 {
                    halvings -= 1;
                    dlam = (2.0 * dlam).min(dlam0);
                }
            }
            None => {
                if halvings >= opts.max_cutbacks {
                    return Err(SolveError::Invalid(format!(
                        "elastic-plastic solve: the increment {lam:.4} -> {lam1:.4} failed after {halvings} cutbacks: {fail}"
                    )));
                }
                halvings += 1;
                cutbacks += 1;
                dlam *= 0.5;
            }
        }
    }
    Ok(PlasticFeSolution {
        elastic,
        displacement: u.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
        states,
        gp_off,
        onset_load_factor: onset,
        steps,
        cutbacks,
    })
}

/// Is a state plastic: J2 with plastic strain, or deformation plasticity
/// past `sigma0`.
fn is_plastic(s: &super::elastoplastic::GpState, sref: f64, mat: &super::elastoplastic::Material) -> bool {
    match mat.plasticity {
        super::elastoplastic::Plasticity::J2 { .. } => s.p > 0.0,
        super::elastoplastic::Plasticity::Deformation { .. } => super::elastoplastic::Material::mises(&s.sig) > sref,
    }
}

// ------------------------------------------------------------ test meshes

/// A structured quadratic mesh of the box `lo..hi` with `n` cells per axis:
/// `"tet10"` (six tets per cell) or `"hex20"`. For tests and examples.
pub fn structured_box_mesh(lo: V3, hi: V3, n: [usize; 3], element: &str) -> Mesh {
    let m = [2 * n[0] + 1, 2 * n[1] + 1, 2 * n[2] + 1];
    let id = |i: usize, j: usize, k: usize| (i * m[1] + j) * m[2] + k;
    let mut rows: Vec<Vec<usize>> = vec![];
    let et = element_type(element).expect("element type");
    for ci in 0..n[0] {
        for cj in 0..n[1] {
            for ck in 0..n[2] {
                let o = [2 * ci, 2 * cj, 2 * ck];
                // doubled-grid index of cube-local corner (a, b, c) in {0, 2}
                let at = |p: [usize; 3]| id(o[0] + p[0], o[1] + p[1], o[2] + p[2]);
                if element == "hex20" {
                    let row: Vec<usize> = et
                        .local_coords
                        .iter()
                        .map(|lc| at([(lc[0] + 1.0) as usize, (lc[1] + 1.0) as usize, (lc[2] + 1.0) as usize]))
                        .collect();
                    rows.push(row);
                } else {
                    // Kuhn split: 6 tets sharing the main diagonal 0 -> 7
                    let c = |b: usize| [2 * (b & 1), 2 * ((b >> 1) & 1), 2 * ((b >> 2) & 1)];
                    let paths = [[1, 3], [1, 5], [2, 3], [2, 6], [4, 5], [4, 6]];
                    for p in paths {
                        let mut cs = [c(0), c(p[0]), c(p[1]), c(7)];
                        let x = |q: [usize; 3]| [q[0] as f64, q[1] as f64, q[2] as f64];
                        let v = dot(cross(sub(x(cs[1]), x(cs[0])), sub(x(cs[2]), x(cs[0]))), sub(x(cs[3]), x(cs[0])));
                        if v < 0.0 {
                            cs.swap(1, 2);
                        }
                        let mid = |a: [usize; 3], b: [usize; 3]| [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2, (a[2] + b[2]) / 2];
                        let row = vec![
                            at(cs[0]), at(cs[1]), at(cs[2]), at(cs[3]),
                            at(mid(cs[0], cs[1])), at(mid(cs[1], cs[2])), at(mid(cs[0], cs[2])),
                            at(mid(cs[0], cs[3])), at(mid(cs[2], cs[3])), at(mid(cs[1], cs[3])),
                        ];
                        rows.push(row);
                    }
                }
            }
        }
    }
    let mut remap = vec![usize::MAX; m[0] * m[1] * m[2]];
    let mut points = vec![];
    for r in rows.iter_mut() {
        for v in r.iter_mut() {
            if remap[*v] == usize::MAX {
                let k = *v % m[2];
                let j = (*v / m[2]) % m[1];
                let i = *v / (m[1] * m[2]);
                let f = |t: usize, a: usize| lo[a] + (hi[a] - lo[a]) * t as f64 / (2 * n[a]) as f64;
                remap[*v] = points.len();
                points.push([f(i, 0), f(j, 1), f(k, 2)]);
            }
            *v = remap[*v];
        }
    }
    Mesh {
        points,
        blocks: vec![ElementBlock::new("body", element, rows, 1).expect("block")],
        ..Default::default()
    }
}
