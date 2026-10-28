//! Linear elastic finite element solver for the Simulation tab: 10-node tetrahedra, static, modal
//! (shift-invert Lanczos) and transient (Newmark average acceleration with Rayleigh damping) analyses.
//! Units: mm, N, MPa, tonnes and seconds.

use faer::linalg::solvers::Solve;
use faer::sparse::linalg::solvers::{Llt, SymbolicLlt};
use faer::sparse::{SparseColMatRef, SymbolicSparseColMatRef};
use faer::{Mat, Side};
use glam::DVec3;

use crate::fea_mesh::Mesh;

/// Elastic material: Young's modulus (MPa), Poisson's ratio, density (t/mm³), yield strength (MPa)
/// and loss factor (structural damping η: the fraction of strain energy lost per radian of vibration).
#[derive(Clone, Copy, Debug)]
pub struct Elastic {
    pub e: f64,
    pub nu: f64,
    pub rho: f64,
    pub yield_mpa: f64,
    pub eta: f64,
}

/// What holds the part.
#[derive(Clone, Debug)]
pub enum Fix {
    /// All displacements zero at these nodes.
    Fixed(Vec<u32>),
    /// No displacement along each node's normal (sliding on the surface).
    Roller(Vec<(u32, DVec3)>),
}

/// A spring between two weighted groups of nodes, per direction: energy ½ k_c (Σ wa u_c − Σ wb u_c)².
/// With `b` empty it holds group a to the ground. Ties between parts meshed apart, springs between
/// patches (a rail block, a ballscrew) and elastic supports are all couplings.
#[derive(Clone, Debug, Default)]
pub struct Coupling {
    pub a: Vec<(u32, f64)>,
    pub b: Vec<(u32, f64)>,
    pub k: [f64; 3],
}

/// Loads, already turned into nodal forces (N) for a unit time factor.
pub struct Problem<'a> {
    pub mesh: &'a Mesh,
    pub mat: Elastic,
    /// Material of each part (`mesh.body` numbering); empty: `mat` everywhere.
    pub body_mats: Vec<Elastic>,
    pub fixes: Vec<Fix>,
    /// Nodal forces (3 per node).
    pub force: Vec<f64>,
    pub couplings: Vec<Coupling>,
    /// Lumped masses (tonnes), each spread over its nodes by weight (the weights sum to 1).
    pub masses: Vec<(Vec<(u32, f64)>, f64)>,
}

impl Problem<'_> {
    /// Material of element `ei`.
    pub fn mat_of(&self, ei: usize) -> &Elastic {
        mat_at(self.mesh, &self.body_mats, &self.mat, ei)
    }
}

/// Material of element `ei`: its part's from `body_mats`, else `mat`.
pub fn mat_at<'a>(mesh: &Mesh, body_mats: &'a [Elastic], mat: &'a Elastic, ei: usize) -> &'a Elastic {
    mesh.body.get(ei).and_then(|b| body_mats.get(*b as usize)).unwrap_or(mat)
}

// ---------------------------------------------------------------- element

const GP: [[f64; 4]; 4] = {
    let a = 0.585_410_196_624_968_5;
    let b = 0.138_196_601_125_010_5;
    [[a, b, b, b], [b, a, b, b], [b, b, a, b], [b, b, b, a]]
};

/// Natural (barycentric) coordinates of the 10 nodes.
const NODE_L: [[f64; 4]; 10] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
    [0.5, 0.5, 0.0, 0.0],
    [0.0, 0.5, 0.5, 0.0],
    [0.5, 0.0, 0.5, 0.0],
    [0.5, 0.0, 0.0, 0.5],
    [0.0, 0.5, 0.0, 0.5],
    [0.0, 0.0, 0.5, 0.5],
];

const EDGES: [(usize, usize); 6] = crate::fea_mesh::TET_EDGES;

/// Gradients of the 10 shape functions (x, y, z) at barycentric point `l`, and det J (6 × volume scale).
fn grads(x: &[DVec3; 10], l: [f64; 4]) -> Option<([DVec3; 10], f64)> {
    // dN/dL for each node (4 components)
    let mut dl = [[0.0f64; 4]; 10];
    for i in 0..4 {
        dl[i][i] = 4.0 * l[i] - 1.0;
    }
    for (k, (i, j)) in EDGES.iter().enumerate() {
        dl[4 + k][*i] = 4.0 * l[*j];
        dl[4 + k][*j] = 4.0 * l[*i];
    }
    // dN/dξ_a with L0 = 1 - ξ - η - ζ, L1..3 = ξ, η, ζ
    let mut dxi = [[0.0f64; 3]; 10];
    for n in 0..10 {
        for a in 0..3 {
            dxi[n][a] = dl[n][a + 1] - dl[n][0];
        }
    }
    // J[a] = d x / d ξ_a
    let mut j = [DVec3::ZERO; 3];
    for n in 0..10 {
        for a in 0..3 {
            j[a] += x[n] * dxi[n][a];
        }
    }
    let m = glam::DMat3::from_cols(j[0], j[1], j[2]).transpose(); // rows are dx/dξ_a
    let det = m.determinant();
    if det <= 0.0 || !det.is_finite() {
        return None;
    }
    let inv = m.inverse();
    let mut g = [DVec3::ZERO; 10];
    for n in 0..10 {
        g[n] = inv * DVec3::new(dxi[n][0], dxi[n][1], dxi[n][2]);
    }
    Some((g, det))
}

pub fn coords(mesh: &Mesh, e: &[u32; 10]) -> [DVec3; 10] {
    e.map(|n| mesh.nodes[n as usize])
}

/// Element stiffness (30×30, node-major) and volume.
fn stiffness(x: &[DVec3; 10], lam: f64, mu: f64) -> Option<([[f64; 30]; 30], f64)> {
    let mut k = [[0.0f64; 30]; 30];
    let mut vol = 0.0;
    for l in GP {
        let (g, det) = grads(x, l)?;
        let w = det / 24.0;
        vol += w;
        for i in 0..10 {
            for j in 0..10 {
                let gg = g[i].dot(g[j]) * mu;
                for a in 0..3 {
                    for b in 0..3 {
                        let mut v = lam * g[i][a] * g[j][b] + mu * g[i][b] * g[j][a];
                        if a == b {
                            v += gg;
                        }
                        k[3 * i + a][3 * j + b] += v * w;
                    }
                }
            }
        }
    }
    Some((k, vol))
}

/// Consistent mass coefficients of a straight-sided 10-node tetrahedron, times ρV/420.
pub fn mass_coef(i: usize, j: usize) -> f64 {
    match (i < 4, j < 4) {
        (true, true) => {
            if i == j {
                6.0
            } else {
                1.0
            }
        }
        (true, false) | (false, true) => {
            let (v, e) = if i < 4 { (i, j - 4) } else { (j, i - 4) };
            if EDGES[e].0 == v || EDGES[e].1 == v { -4.0 } else { -6.0 }
        }
        (false, false) => {
            if i == j {
                32.0
            } else {
                let (a, b) = (EDGES[i - 4], EDGES[j - 4]);
                if a.0 == b.0 || a.0 == b.1 || a.1 == b.0 || a.1 == b.1 { 16.0 } else { 8.0 }
            }
        }
    }
}

// ---------------------------------------------------------------- sparse system

/// Lower triangle of a symmetric matrix over the free degrees of freedom, in compressed columns.
pub struct System {
    pub n: usize,
    /// Free index of each DOF (usize::MAX when fixed).
    pub free: Vec<usize>,
    /// DOF of each free index.
    pub dof: Vec<usize>,
    col_ptr: Vec<usize>,
    row: Vec<usize>,
    pub k: Vec<f64>,
    pub m: Vec<f64>,
    /// Σ ηₑ Kₑ: the stiffness weighted by each element's loss factor (empty when every η is 0).
    pub k_eta: Vec<f64>,
    symbolic: Option<SymbolicLlt<usize>>,
    pub volume: f64,
    pub mass: f64,
}

impl System {
    fn idx(&self, r: usize, c: usize) -> usize {
        let (s, e) = (self.col_ptr[c], self.col_ptr[c + 1]);
        s + self.row[s..e].binary_search(&r).expect("pattern")
    }

    /// y = A x for the symmetric matrix with lower-triangle values `v`.
    pub fn mul(&self, v: &[f64], x: &[f64], y: &mut [f64]) {
        y.iter_mut().for_each(|y| *y = 0.0);
        for c in 0..self.n {
            let xc = x[c];
            let mut acc = 0.0;
            for p in self.col_ptr[c]..self.col_ptr[c + 1] {
                let r = self.row[p];
                y[r] += v[p] * xc;
                if r != c {
                    acc += v[p] * x[r];
                }
            }
            y[c] += acc;
        }
    }

    /// Factorise the matrix with lower-triangle values `v` (Cholesky).
    pub fn factor(&mut self, v: &[f64]) -> Result<Llt<usize, f64>, String> {
        let sym = SymbolicSparseColMatRef::new_checked(self.n, self.n, &self.col_ptr, None, &self.row);
        if self.symbolic.is_none() {
            self.symbolic = Some(SymbolicLlt::try_new(sym, Side::Lower).map_err(|e| format!("factorisation failed: {e:?}"))?);
        }
        let a = SparseColMatRef::new(sym, v);
        Llt::try_new_with_symbolic(self.symbolic.clone().unwrap(), a, Side::Lower)
            .map_err(|e| match e {
                faer::sparse::linalg::LltError::Numeric(_) => "The model is not held well enough: add supports so it cannot move freely".to_string(),
                _ => format!("Not enough memory to solve {} unknowns: use a bigger mesh size", self.n),
            })
    }
}

pub fn solve(llt: &Llt<usize, f64>, b: &[f64]) -> Vec<f64> {
    let rhs = Mat::<f64>::from_fn(b.len(), 1, |i, _| b[i]);
    let x = llt.solve(&rhs);
    (0..b.len()).map(|i| x[(i, 0)]).collect()
}

fn lame(m: &Elastic) -> (f64, f64) {
    let lam = m.e * m.nu / ((1.0 + m.nu) * (1.0 - 2.0 * m.nu));
    let mu = m.e / (2.0 * (1.0 + m.nu));
    (lam, mu)
}

/// Assemble stiffness and mass over the free DOFs. Rollers that are not along an axis become a stiff spring.
pub fn assemble(p: &Problem, cancel: &dyn Fn() -> bool) -> Result<System, String> {
    let mesh = p.mesh;
    let nn = mesh.nodes.len();
    let ndof = 3 * nn;
    let mut fixed = vec![false; ndof];
    let mut springs: Vec<(u32, DVec3)> = vec![];
    for f in &p.fixes {
        match f {
            Fix::Fixed(nodes) => {
                for n in nodes {
                    for c in 0..3 {
                        fixed[3 * *n as usize + c] = true;
                    }
                }
            }
            Fix::Roller(nodes) => {
                for (n, dir) in nodes {
                    let d = dir.normalize_or_zero();
                    match (0..3).find(|&a| (d[a].abs() - 1.0).abs() < 1e-9) {
                        Some(a) => fixed[3 * *n as usize + a] = true,
                        None => springs.push((*n, d)),
                    }
                }
            }
        }
    }
    // nodes no element uses (left over by meshing) carry no stiffness or mass: leave them out
    let mut used = vec![false; nn];
    for e in &mesh.tets {
        for &a in e {
            used[a as usize] = true;
        }
    }
    for (i, u) in used.iter().enumerate() {
        if !u {
            fixed[3 * i..3 * i + 3].fill(true);
        }
    }
    let mut free = vec![usize::MAX; ndof];
    let mut dof = vec![];
    for d in 0..ndof {
        if !fixed[d] {
            free[d] = dof.len();
            dof.push(d);
        }
    }
    let n = dof.len();
    if n == 0 {
        return Err("Everything is fixed: nothing can move".into());
    }
    // node adjacency
    let mut adj: Vec<Vec<u32>> = vec![vec![]; nn];
    for e in &mesh.tets {
        for &a in e {
            for &b in e {
                if b >= a {
                    adj[a as usize].push(b);
                }
            }
        }
    }
    for cp in &p.couplings {
        let nodes: Vec<u32> = cp.a.iter().chain(&cp.b).map(|x| x.0).collect();
        for &a in &nodes {
            for &b in &nodes {
                if b >= a {
                    adj[a as usize].push(b);
                }
            }
        }
    }
    for a in &mut adj {
        a.sort_unstable();
        a.dedup();
    }
    // pattern: column = free dof (node a, comp c), rows = free dofs of nodes b >= a (comps >= c on a itself)
    let mut col_ptr = vec![0usize; n + 1];
    let mut row = vec![];
    for (ci, &d) in dof.iter().enumerate() {
        let (a, c) = (d / 3, d % 3);
        for &b in &adj[a] {
            for cc in 0..3 {
                if b as usize == a && cc < c {
                    continue;
                }
                let f = free[3 * b as usize + cc];
                if f != usize::MAX {
                    row.push(f);
                }
            }
        }
        col_ptr[ci + 1] = row.len();
    }
    drop(adj);
    let nnz = row.len();
    let lossy = (0..mesh.tets.len()).any(|ei| p.mat_of(ei).eta > 0.0);
    let mut sys = System { n, free, dof, col_ptr, row, k: vec![0.0; nnz], m: vec![0.0; nnz], k_eta: if lossy { vec![0.0; nnz] } else { vec![] }, symbolic: None, volume: 0.0, mass: 0.0 };
    for (ei, e) in mesh.tets.iter().enumerate() {
        if ei % 4096 == 0 && cancel() {
            return Err("cancelled".into());
        }
        let mat = *p.mat_of(ei);
        let (lam, mu) = lame(&mat);
        let x = coords(mesh, e);
        let Some((ke, vol)) = stiffness(&x, lam, mu) else {
            let part = mesh.body.get(ei).map(|b| format!(" (part {b})")).unwrap_or_default();
            return Err(format!("The mesh has an inverted element{part}; try another element size"));
        };
        sys.volume += vol;
        sys.mass += mat.rho * vol;
        let mscale = mat.rho * vol / 420.0;
        for i in 0..10 {
            for a in 0..3 {
                let ri = sys.free[3 * e[i] as usize + a];
                if ri == usize::MAX {
                    continue;
                }
                for j in 0..10 {
                    for b in 0..3 {
                        let cj = sys.free[3 * e[j] as usize + b];
                        if cj == usize::MAX || ri < cj {
                            continue;
                        }
                        let k = sys.idx(ri, cj);
                        sys.k[k] += ke[3 * i + a][3 * j + b];
                        if lossy {
                            sys.k_eta[k] += mat.eta * ke[3 * i + a][3 * j + b];
                        }
                        if a == b {
                            sys.m[k] += mscale * mass_coef(i, j);
                        }
                    }
                }
            }
        }
    }
    // couplings: k (g·u)² with g the weights of group a minus those of group b
    for cp in &p.couplings {
        let g: Vec<(u32, f64)> = cp.a.iter().copied().chain(cp.b.iter().map(|(n, w)| (*n, -w))).collect();
        for c in 0..3 {
            if cp.k[c] == 0.0 {
                continue;
            }
            for &(ni, wi) in &g {
                let r = sys.free[3 * ni as usize + c];
                if r == usize::MAX {
                    continue;
                }
                for &(nj, wj) in &g {
                    let q = sys.free[3 * nj as usize + c];
                    if q == usize::MAX || r < q {
                        continue;
                    }
                    let k = sys.idx(r, q);
                    sys.k[k] += cp.k[c] * wi * wj;
                }
            }
        }
    }
    for (nodes, mass) in &p.masses {
        sys.mass += mass;
        for &(n, w) in nodes {
            for c in 0..3 {
                let r = sys.free[3 * n as usize + c];
                if r != usize::MAX {
                    let k = sys.idx(r, r);
                    sys.m[k] += mass * w;
                }
            }
        }
    }
    // stiff springs for slanted rollers
    if !springs.is_empty() {
        let maxd = (0..n).map(|c| sys.k[sys.col_ptr[c]]).fold(0.0, f64::max);
        let kp = maxd * 1e4;
        for (nd, d) in springs {
            for a in 0..3 {
                for b in 0..=a {
                    let (r, c) = (sys.free[3 * nd as usize + a], sys.free[3 * nd as usize + b]);
                    if r != usize::MAX && c != usize::MAX {
                        let (r, c) = (r.max(c), r.min(c));
                        let k = sys.idx(r, c);
                        sys.k[k] += kp * d[a] * d[b];
                    }
                }
            }
        }
    }
    Ok(sys)
}

/// Nodal forces of a uniform traction (N/mm²) on boundary triangles: all of it on the mid-edge nodes.
pub fn traction_loads(mesh: &Mesh, faces: &[usize], traction: impl Fn(DVec3) -> DVec3, f: &mut [f64]) {
    for &fi in faces {
        let (t, _) = &mesh.faces[fi];
        let p = |i: usize| mesh.nodes[t[i] as usize];
        let n2 = (p(1) - p(0)).cross(p(2) - p(0));
        let area = n2.length() / 2.0;
        let force = traction(n2.normalize_or_zero()) * area;
        for k in 3..6 {
            for a in 0..3 {
                f[3 * t[k] as usize + a] += force[a] / 3.0;
            }
        }
    }
}

/// Nodal forces of an acceleration `g` (mm/s²) acting on each element's density `rho(element)`:
/// corners −1/20, mid-edge nodes 1/5 of each element's share.
pub fn body_loads(mesh: &Mesh, g: DVec3, rho: impl Fn(usize) -> f64, f: &mut [f64]) {
    for (ei, e) in mesh.tets.iter().enumerate() {
        let b = g * rho(ei);
        let x = coords(mesh, e);
        let vol: f64 = GP.iter().filter_map(|l| grads(&x, *l)).map(|(_, d)| d / 24.0).sum();
        for (i, n) in e.iter().enumerate() {
            let s = if i < 4 { -1.0 / 20.0 } else { 1.0 / 5.0 };
            for a in 0..3 {
                f[3 * *n as usize + a] += b[a] * vol * s;
            }
        }
    }
}

pub fn area_of(mesh: &Mesh, faces: &[usize]) -> f64 {
    faces
        .iter()
        .map(|&fi| {
            let t = &mesh.faces[fi].0;
            let p = |i: usize| mesh.nodes[t[i] as usize];
            (p(1) - p(0)).cross(p(2) - p(0)).length() / 2.0
        })
        .sum()
}

fn reduce(sys: &System, full: &[f64]) -> Vec<f64> {
    sys.dof.iter().map(|d| full[*d]).collect()
}

fn expand(sys: &System, x: &[f64], nn: usize) -> Vec<f64> {
    let mut u = vec![0.0; 3 * nn];
    for (i, d) in sys.dof.iter().enumerate() {
        u[*d] = x[i];
    }
    u
}

// ---------------------------------------------------------------- results

/// Stresses averaged at the nodes: [xx, yy, zz, xy, yz, zx] (MPa).
/// `mats`: each part's material (`mesh.body`), else `mat`.
pub fn nodal_stress(mesh: &Mesh, mat: &Elastic, mats: &[Elastic], u: &[f64]) -> Vec<[f32; 6]> {
    let mut acc = vec![[0.0f64; 6]; mesh.nodes.len()];
    let mut cnt = vec![0u32; mesh.nodes.len()];
    for (ei, e) in mesh.tets.iter().enumerate() {
        let (lam, mu) = lame(mat_at(mesh, mats, mat, ei));
        let x = coords(mesh, e);
        for (k, l) in NODE_L.iter().enumerate() {
            let Some((g, _)) = grads(&x, *l) else { continue };
            // displacement gradient
            let mut h = [[0.0f64; 3]; 3];
            for (i, n) in e.iter().enumerate() {
                for a in 0..3 {
                    let ua = u[3 * *n as usize + a];
                    for b in 0..3 {
                        h[a][b] += ua * g[i][b];
                    }
                }
            }
            let tr = h[0][0] + h[1][1] + h[2][2];
            let s = [
                lam * tr + 2.0 * mu * h[0][0],
                lam * tr + 2.0 * mu * h[1][1],
                lam * tr + 2.0 * mu * h[2][2],
                mu * (h[0][1] + h[1][0]),
                mu * (h[1][2] + h[2][1]),
                mu * (h[2][0] + h[0][2]),
            ];
            let n = e[k] as usize;
            for c in 0..6 {
                acc[n][c] += s[c];
            }
            cnt[n] += 1;
        }
    }
    acc.iter().zip(&cnt).map(|(s, c)| s.map(|v| (v / (*c).max(1) as f64) as f32)).collect()
}

pub fn von_mises(s: &[f32; 6]) -> f32 {
    let [xx, yy, zz, xy, yz, zx] = *s;
    (0.5 * ((xx - yy).powi(2) + (yy - zz).powi(2) + (zz - xx).powi(2)) + 3.0 * (xy * xy + yz * yz + zx * zx)).sqrt()
}

/// Reaction force (N) at the given nodes: internal forces K u minus the applied loads there.
pub fn reaction(mesh: &Mesh, mat: &Elastic, mats: &[Elastic], u: &[f64], force: &[f64], nodes: &[u32]) -> DVec3 {
    let mut on = vec![false; mesh.nodes.len()];
    for n in nodes {
        on[*n as usize] = true;
    }
    let mut r = DVec3::ZERO;
    for (ei, e) in mesh.tets.iter().enumerate().filter(|(_, e)| e.iter().any(|n| on[*n as usize])) {
        let (lam, mu) = lame(mat_at(mesh, mats, mat, ei));
        let Some((ke, _)) = stiffness(&coords(mesh, e), lam, mu) else { continue };
        for i in 0..10 {
            if !on[e[i] as usize] {
                continue;
            }
            for a in 0..3 {
                let mut s = 0.0;
                for j in 0..10 {
                    for b in 0..3 {
                        s += ke[3 * i + a][3 * j + b] * u[3 * e[j] as usize + b];
                    }
                }
                r[a] += s;
            }
        }
    }
    for n in nodes {
        for a in 0..3 {
            r[a] -= force[3 * *n as usize + a];
        }
    }
    r
}

pub struct StaticResult {
    pub u: Vec<f64>,
}

pub fn static_solve(p: &Problem, cancel: &dyn Fn() -> bool) -> Result<StaticResult, String> {
    let mut sys = assemble(p, cancel)?;
    let kv = sys.k.clone();
    let llt = sys.factor(&kv)?;
    let x = solve(&llt, &reduce(&sys, &p.force));
    if x.iter().any(|v| !v.is_finite()) {
        return Err("The solve failed: the model may not be held".into());
    }
    Ok(StaticResult { u: expand(&sys, &x, p.mesh.nodes.len()) })
}

/// Several load cases on one factorisation: the displacements (3 per node) for each force vector.
#[allow(dead_code)] // used by the Python bindings (python/src/fea.rs)
pub fn static_cases(p: &Problem, forces: &[Vec<f64>], cancel: &dyn Fn() -> bool) -> Result<Vec<Vec<f64>>, String> {
    let mut sys = assemble(p, cancel)?;
    let kv = sys.k.clone();
    let llt = sys.factor(&kv)?;
    let rhs: Vec<Vec<f64>> = forces.iter().map(|f| reduce(&sys, f)).collect();
    let b = Mat::<f64>::from_fn(sys.n, rhs.len(), |i, j| rhs[j][i]);
    let x = llt.solve(&b);
    let mut out = vec![];
    for j in 0..rhs.len() {
        let xj: Vec<f64> = (0..sys.n).map(|i| x[(i, j)]).collect();
        if xj.iter().any(|v| !v.is_finite()) {
            return Err("The solve failed: the model may not be held".into());
        }
        out.push(expand(&sys, &xj, p.mesh.nodes.len()));
    }
    Ok(out)
}

pub struct Mode {
    pub freq: f64,
    /// Mass-normalised shape over all DOFs.
    pub shape: Vec<f64>,
    /// Effective mass fraction along x, y, z.
    pub participation: [f64; 3],
    /// Modal loss factor: the materials' loss factors weighted by where the mode stores its strain
    /// energy. Vibration in the mode dies away as e^(−η ω t / 2).
    pub loss: f64,
}

/// Lowest `count` natural frequencies by shift-invert Lanczos (shifted just below zero, so parts
/// without supports work too: their rigid-body modes come out near 0 Hz).
pub fn modal(p: &Problem, count: usize, cancel: &dyn Fn() -> bool) -> Result<Vec<Mode>, String> {
    let mut sys = assemble(p, cancel)?;
    modes_of(&mut sys, p, count, cancel)
}

fn modes_of(sys: &mut System, p: &Problem, count: usize, cancel: &dyn Fn() -> bool) -> Result<Vec<Mode>, String> {
    let n = sys.n;
    let count = count.clamp(1, n);
    // shift: a small negative value relative to the stiffness/mass scale; bigger for a free model,
    // whose stiffness alone is singular, so that rounding cannot break the factorisation
    let kd: f64 = (0..n).map(|c| sys.k[sys.col_ptr[c]]).sum::<f64>() / n as f64;
    let md: f64 = (0..n).map(|c| sys.m[sys.col_ptr[c]]).sum::<f64>() / n as f64;
    let sigma = -(kd / md.max(1e-300)) * if p.fixes.is_empty() { 1e-5 } else { 1e-8 };
    let shifted: Vec<f64> = sys.k.iter().zip(&sys.m).map(|(k, m)| k - sigma * m).collect();
    let llt = sys.factor(&shifted)?;
    let mut m_steps = (2 * count + 20).min(n);
    loop {
        let (vals, vecs, ok) = lanczos(sys, &llt, m_steps, count, cancel)?;
        if ok || m_steps >= n {
            let total_mass = sys.mass;
            let mut out = vec![];
            for (theta, x) in vals.into_iter().zip(vecs) {
                let lam = sigma + 1.0 / theta;
                let freq = lam.max(0.0).sqrt() / std::f64::consts::TAU;
                // participation: (φᵀ M r)² / total mass, r the unit rigid translation
                let mut mx = vec![0.0; n];
                sys.mul(&sys.m, &x, &mut mx);
                let mut part = [0.0; 3];
                for (i, d) in sys.dof.iter().enumerate() {
                    part[d % 3] += mx[i];
                }
                let participation = part.map(|g| g * g / total_mass.max(1e-300));
                let loss = if sys.k_eta.is_empty() {
                    0.0
                } else {
                    let (mut kx, mut ex) = (vec![0.0; n], vec![0.0; n]);
                    sys.mul(&sys.k, &x, &mut kx);
                    sys.mul(&sys.k_eta, &x, &mut ex);
                    let dot = |a: &[f64]| a.iter().zip(&x).map(|(p, q)| p * q).sum::<f64>();
                    (dot(&ex) / dot(&kx).max(1e-300)).clamp(0.0, 1.0)
                };
                out.push(Mode { freq, shape: expand(sys, &x, p.mesh.nodes.len()), participation, loss });
            }
            return Ok(out);
        }
        m_steps = (m_steps * 2).min(n);
    }
}

/// Lanczos with full reorthogonalisation in the M inner product on (K − σM)⁻¹M. Returns the `count`
/// largest Ritz values θ (λ = σ + 1/θ), their M-normalised vectors and whether they converged.
fn lanczos(sys: &System, llt: &Llt<usize, f64>, m: usize, count: usize, cancel: &dyn Fn() -> bool) -> Result<(Vec<f64>, Vec<Vec<f64>>, bool), String> {
    let n = sys.n;
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    };
    // M-orthogonalise r against the basis (twice), returning M r and its M-norm
    let orth = |r: &mut Vec<f64>, q: &[Vec<f64>], mq: &[Vec<f64>]| -> (Vec<f64>, f64) {
        for _ in 0..2 {
            for (qi, mqi) in q.iter().zip(mq) {
                let c = dot(mqi, r);
                r.iter_mut().zip(qi).for_each(|(x, y)| *x -= c * y);
            }
        }
        let mut mr = vec![0.0; n];
        sys.mul(&sys.m, r, &mut mr);
        let b = dot(r, &mr).max(0.0).sqrt();
        (mr, b)
    };
    let mut q: Vec<Vec<f64>> = vec![];
    let mut mq: Vec<Vec<f64>> = vec![];
    let mut alpha: Vec<f64> = vec![];
    let mut beta: Vec<f64> = vec![];
    let mut r: Vec<f64> = (0..n).map(|_| rnd()).collect();
    let mut b_last = 0.0;
    for step in 0..m {
        if cancel() {
            return Err("cancelled".into());
        }
        let (mut mr, mut b) = orth(&mut r, &q, &mq);
        if step > 0 {
            beta.push(b);
        }
        let scale = alpha.iter().fold(0.0f64, |a, x| a.max(x.abs()));
        if b < 1e-300 || (step > 0 && b < 1e-10 * scale) {
            // invariant subspace: carry on from a fresh direction, decoupled
            if step > 0 {
                *beta.last_mut().unwrap() = 0.0;
            }
            r = (0..n).map(|_| rnd()).collect();
            (mr, b) = orth(&mut r, &q, &mq);
            if b < 1e-300 {
                beta.pop();
                break;
            }
        }
        r.iter_mut().for_each(|x| *x /= b);
        mr.iter_mut().for_each(|x| *x /= b);
        // w = (K − σM)⁻¹ M q
        let w = solve(llt, &mr);
        alpha.push(dot(&w, &mr));
        q.push(std::mem::replace(&mut r, w));
        mq.push(mr);
        if step + 1 == m {
            b_last = orth(&mut r, &q, &mq).1;
        }
    }
    let k = q.len();
    let tm = Mat::<f64>::from_fn(k, k, |i, j| {
        if i == j {
            alpha[i]
        } else if i == j + 1 {
            beta[j]
        } else if j == i + 1 {
            beta[i]
        } else {
            0.0
        }
    });
    let eig = tm.self_adjoint_eigen(Side::Lower).map_err(|e| format!("eigen solve failed: {e:?}"))?;
    let s = eig.S();
    let u = eig.U();
    let mut vals = vec![];
    let mut vecs = vec![];
    let mut ok = true;
    for idx in (0..k).rev().take(count) {
        let theta = s[idx];
        if theta <= 0.0 {
            break;
        }
        if (b_last * u[(k - 1, idx)]).abs() > 1e-8 * theta.abs() {
            ok = false;
        }
        let mut x = vec![0.0; n];
        for (j, qj) in q.iter().enumerate() {
            let c = u[(j, idx)];
            x.iter_mut().zip(qj).for_each(|(a, b)| *a += c * b);
        }
        vals.push(theta);
        vecs.push(x);
    }
    if vals.len() < count.min(k) {
        ok = false;
    }
    Ok((vals, vecs, ok))
}

/// How the loads change over time in a transient analysis.
#[derive(Clone, Copy, Debug)]
pub enum Profile {
    /// Applied at t = 0 and held.
    Step,
    /// Rising linearly to full over this time (s), then held.
    Ramp(f64),
    /// A half-sine pulse lasting this long (s).
    HalfSine(f64),
    /// sin(2π f t) at this frequency (Hz).
    Sine(f64),
}

impl Profile {
    /// A simulated time that shows the response: five periods of the lowest natural frequency after
    /// the load is in place (ten periods of the shaking for a sine).
    pub fn auto_duration(&self, f1: f64) -> f64 {
        let t1 = 1.0 / f1.max(1e-6);
        match *self {
            Profile::Step => 5.0 * t1,
            Profile::Ramp(t) | Profile::HalfSine(t) => t + 5.0 * t1,
            Profile::Sine(f) => (5.0 * t1).max(10.0 / f.max(1e-6)),
        }
    }

    pub fn at(&self, t: f64) -> f64 {
        match *self {
            Profile::Step => {
                if t > 0.0 {
                    1.0
                } else {
                    0.0
                }
            }
            Profile::Ramp(tr) => {
                if tr <= 0.0 {
                    1.0
                } else {
                    (t / tr).clamp(0.0, 1.0)
                }
            }
            Profile::HalfSine(tp) => {
                if t > 0.0 && t < tp {
                    (std::f64::consts::PI * t / tp).sin()
                } else {
                    0.0
                }
            }
            Profile::Sine(f) => (std::f64::consts::TAU * f * t).sin(),
        }
    }
}

pub struct TransientSettings {
    /// Simulated time (s); None: long enough to see the response settle (from the lowest frequency).
    pub duration: Option<f64>,
    pub steps: usize,
    pub damping: f64,
    pub profile: Profile,
    /// Snapshots kept for playback.
    pub frames: usize,
}

pub struct TransientResult {
    pub times: Vec<f64>,
    /// Displacements (all DOFs) at the kept frames.
    pub frames: Vec<(f64, Vec<f64>)>,
    /// Largest displacement magnitude at every step.
    pub max_disp: Vec<f64>,
    /// Lowest natural frequency (Hz), used for damping and time-step advice.
    pub f1: f64,
    pub rayleigh: (f64, f64),
    pub duration: f64,
    pub dt: f64,
    /// The part is not held: displacements are its vibration, with the rigid motion taken out.
    pub free: bool,
    /// Time from the largest response until it has died down to a tenth of it (None: not within the run).
    pub decay: Option<f64>,
}

/// Time from the peak of `amp` until it stays below a tenth of the peak.
pub fn decay_time(times: &[f64], amp: &[f64]) -> Option<f64> {
    let (ip, peak) = amp.iter().copied().enumerate().fold((0, 0.0), |b, (i, a)| if a > b.1 { (i, a) } else { b });
    if peak <= 0.0 {
        return None;
    }
    let last = amp.iter().rposition(|a| *a >= 0.1 * peak)?;
    (last + 1 < amp.len()).then(|| times[last] - times[ip])
}

/// Rigid-body motions (3 translations, 3 rotations about the centroid) over the free DOFs, M-orthonormal.
fn rigid_modes(sys: &System, mesh: &Mesh) -> Vec<Vec<f64>> {
    let n = sys.n;
    let c = mesh.nodes.iter().fold(DVec3::ZERO, |a, p| a + *p) / mesh.nodes.len().max(1) as f64;
    let mut out: Vec<Vec<f64>> = vec![];
    for k in 0..6 {
        let mut r: Vec<f64> = sys
            .dof
            .iter()
            .map(|d| {
                let (node, comp) = (d / 3, d % 3);
                if k < 3 {
                    if comp == k { 1.0 } else { 0.0 }
                } else {
                    let axis = DVec3::from_array(std::array::from_fn(|a| if a == k - 3 { 1.0 } else { 0.0 }));
                    axis.cross(mesh.nodes[node] - c)[comp]
                }
            })
            .collect();
        for _ in 0..2 {
            for q in &out {
                let mut mq = vec![0.0; n];
                sys.mul(&sys.m, q, &mut mq);
                let d: f64 = mq.iter().zip(&r).map(|(a, b)| a * b).sum();
                r.iter_mut().zip(q).for_each(|(x, y)| *x -= d * y);
            }
        }
        let mut mr = vec![0.0; n];
        sys.mul(&sys.m, &r, &mut mr);
        let nrm = mr.iter().zip(&r).map(|(a, b)| a * b).sum::<f64>().max(0.0).sqrt();
        if nrm > 1e-300 {
            r.iter_mut().for_each(|x| *x /= nrm);
            out.push(r);
        }
    }
    out
}

/// Newmark average-acceleration time stepping with Rayleigh damping set to `damping` of critical at the
/// first natural frequency and five times it, plus the materials' loss factors as stiffness-proportional
/// damping (ηₑ Kₑ / ω₁). A part with no supports floats freely: its rigid motion is taken out of the results.
pub fn transient(p: &Problem, s: &TransientSettings, progress: &dyn Fn(f32), cancel: &dyn Fn() -> bool) -> Result<TransientResult, String> {
    let mut sys = assemble(p, cancel)?;
    let free = p.fixes.is_empty();
    let modes = modes_of(&mut sys, p, if free { 7 } else { 1 }, cancel)?;
    let f1 = modes.last().map(|m| m.freq).unwrap_or(0.0).max(1e-6);
    let (w1, w2) = (std::f64::consts::TAU * f1, std::f64::consts::TAU * f1 * 5.0);
    let ar = 2.0 * s.damping * w1 * w2 / (w1 + w2);
    let br = 2.0 * s.damping / (w1 + w2);
    let steps = s.steps.max(1);
    let duration = s.duration.unwrap_or_else(|| s.profile.auto_duration(f1));
    let dt = duration / steps as f64;
    let (beta, gamma) = (0.25, 0.5);
    let a0 = 1.0 / (beta * dt * dt);
    let a1 = gamma / (beta * dt);
    let a2 = 1.0 / (beta * dt);
    let a3 = 1.0 / (2.0 * beta) - 1.0;
    let a4 = gamma / beta - 1.0;
    let a5 = dt / 2.0 * (gamma / beta - 2.0);
    // loss factors: C += Σ ηₑ Kₑ / ω₁
    let ce = 1.0 / w1;
    // K_eff = K + a0 M + a1 C, C = αM + βK + ce K_η
    let mut keff: Vec<f64> = sys.k.iter().zip(&sys.m).map(|(k, m)| (1.0 + a1 * br) * k + (a0 + a1 * ar) * m).collect();
    for (x, e) in keff.iter_mut().zip(&sys.k_eta) {
        *x += a1 * ce * e;
    }
    let rigid = if free { rigid_modes(&sys, p.mesh) } else { vec![] };
    let llt = sys.factor(&keff)?;
    let n = sys.n;
    let nn = p.mesh.nodes.len();
    let f0 = reduce(&sys, &p.force);
    let (mut u, mut v, mut a) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    let mut frames = vec![(0.0, vec![0.0; 3 * nn])];
    let every = (steps as f64 / s.frames.max(1) as f64).max(1.0);
    let mut next_frame = every;
    let mut times = vec![0.0];
    let mut max_disp = vec![0.0];
    let (mut tm, mut tc, mut ku, mut ke) = (vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    let mut mu = vec![0.0; n];
    for step in 1..=steps {
        if step % 16 == 0 {
            if cancel() {
                return Err("cancelled".into());
            }
            progress(step as f32 / steps as f32);
        }
        let t = step as f64 * dt;
        let ft = s.profile.at(t);
        // rhs = f + M(a0 u + a2 v + a3 a) + C(a1 u + a4 v + a5 a), C = αM + βK
        let mvec: Vec<f64> = (0..n).map(|i| a0 * u[i] + a2 * v[i] + a3 * a[i]).collect();
        let cvec: Vec<f64> = (0..n).map(|i| a1 * u[i] + a4 * v[i] + a5 * a[i]).collect();
        sys.mul(&sys.m, &mvec, &mut tm);
        let mc: Vec<f64> = cvec.iter().map(|x| x * ar).collect();
        sys.mul(&sys.m, &mc, &mut tc);
        let kc: Vec<f64> = cvec.iter().map(|x| x * br).collect();
        sys.mul(&sys.k, &kc, &mut ku);
        if !sys.k_eta.is_empty() {
            let ec: Vec<f64> = cvec.iter().map(|x| x * ce).collect();
            sys.mul(&sys.k_eta, &ec, &mut ke);
        }
        let rhs: Vec<f64> = (0..n).map(|i| f0[i] * ft + tm[i] + tc[i] + ku[i] + ke[i]).collect();
        let un = solve(&llt, &rhs);
        for i in 0..n {
            let an = a0 * (un[i] - u[i]) - a2 * v[i] - a3 * a[i];
            v[i] += dt * ((1.0 - gamma) * a[i] + gamma * an);
            a[i] = an;
            u[i] = un[i];
        }
        let full = if rigid.is_empty() {
            expand(&sys, &u, nn)
        } else {
            // the vibration: u less its rigid motion
            sys.mul(&sys.m, &u, &mut mu);
            let mut el = u.clone();
            for r in &rigid {
                let c: f64 = r.iter().zip(&mu).map(|(a, b)| a * b).sum();
                el.iter_mut().zip(r).for_each(|(x, y)| *x -= c * y);
            }
            expand(&sys, &el, nn)
        };
        let md = full.chunks(3).map(|c| (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt()).fold(0.0, f64::max);
        times.push(t);
        max_disp.push(md);
        if step as f64 >= next_frame - 1e-9 || step == steps {
            frames.push((t, full));
            next_frame += every;
        }
    }
    let decay = decay_time(&times, &max_disp);
    Ok(TransientResult { times, frames, max_disp, f1, rayleigh: (ar, br), duration, dt, free, decay })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fea_mesh::{Surface, mesh};

    /// A 100 × 10 × 10 mm box as triangles, faces numbered −x, +x, −y, +y, −z, +z.
    fn beam() -> Surface {
        let (lo, hi) = (DVec3::ZERO, DVec3::new(100.0, 10.0, 10.0));
        let c = |i: usize| DVec3::new([lo.x, hi.x][i & 1], [lo.y, hi.y][(i >> 1) & 1], [lo.z, hi.z][(i >> 2) & 1]);
        let quads = [[0, 4, 6, 2], [1, 3, 7, 5], [0, 1, 5, 4], [2, 6, 7, 3], [0, 2, 3, 1], [4, 5, 7, 6]];
        let mut t = vec![];
        let mut f = vec![];
        for (fi, q) in quads.iter().enumerate() {
            t.push([c(q[0]), c(q[1]), c(q[2])]);
            t.push([c(q[0]), c(q[2]), c(q[3])]);
            f.extend([fi as u32; 2]);
        }
        Surface::new(t, f, vec![0; 12])
    }

    const STEEL: Elastic = Elastic { e: 200000.0, nu: 0.3, rho: 7.85e-9, yield_mpa: 250.0, eta: 0.0 };

    fn cantilever(m: &Mesh) -> Problem<'_> {
        let fixed: Vec<u32> = m.faces.iter().filter(|f| f.1 == 0).flat_map(|f| f.0).collect();
        let tip: Vec<usize> = (0..m.faces.len()).filter(|i| m.faces[*i].1 == 1).collect();
        let area = area_of(m, &tip);
        let mut force = vec![0.0; 3 * m.nodes.len()];
        traction_loads(m, &tip, |_| DVec3::new(0.0, 0.0, -100.0 / area), &mut force);
        Problem { mesh: m, mat: STEEL, body_mats: vec![], fixes: vec![Fix::Fixed(fixed)], force, couplings: vec![], masses: vec![] }
    }

    #[test]
    fn mass_matrix_sums_to_mass() {
        let s: f64 = (0..10).flat_map(|i| (0..10).map(move |j| mass_coef(i, j))).sum();
        assert_eq!(s, 420.0);
    }

    #[test]
    fn cantilever_matches_beam_theory() {
        let m = mesh(&beam(), 2.5, &|| false).unwrap();
        assert!(m.min_dihedral > 20.0);
        let p = cantilever(&m);
        let u = static_solve(&p, &|| false).unwrap().u;
        // P L³ / 3EI plus shear: 0.2016 mm
        let tip = u.chunks(3).map(|c| c[2]).fold(0.0, f64::min);
        assert!((tip + 0.2016).abs() < 0.006, "tip {tip}");
        // the support carries the whole load
        let fixed: Vec<u32> = m.faces.iter().filter(|f| f.1 == 0).flat_map(|f| f.0).collect();
        let r = reaction(&m, &STEEL, &[], &u, &p.force, &fixed);
        assert!((r.z - 100.0).abs() < 1e-6 && r.x.abs() < 1e-6);
        // first bending frequency 1.875² / 2π √(EI / ρAL⁴): 815 Hz, twice (square section)
        let modes = modal(&p, 3, &|| false).unwrap();
        assert!((modes[0].freq - 815.0).abs() < 10.0 && (modes[1].freq - modes[0].freq).abs() < 1.0, "{}", modes[0].freq);
        // a load applied suddenly overshoots to twice the static deflection
        let s = TransientSettings { duration: Some(3.0 / modes[0].freq), steps: 120, damping: 0.0, profile: Profile::Step, frames: 10 };
        let t = transient(&p, &s, &|_| {}, &|| false).unwrap();
        let peak = t.max_disp.iter().copied().fold(0.0, f64::max);
        let max_static = u.chunks(3).map(|c| (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt()).fold(0.0, f64::max);
        assert!((peak / max_static - 2.0).abs() < 0.08, "{}", peak / max_static);
    }

    #[test]
    fn free_part_has_six_rigid_modes() {
        let m = mesh(&beam(), 3.4, &|| false).unwrap();
        let p = Problem { mesh: &m, mat: STEEL, body_mats: vec![], fixes: vec![], force: vec![0.0; 3 * m.nodes.len()], couplings: vec![], masses: vec![] };
        let modes = modal(&p, 7, &|| false).unwrap();
        assert!(modes[..6].iter().all(|m| m.freq < 5.0));
        // free-free first bending: 4.730² / 2π √(EI / ρAL⁴) = 5185 Hz slender-beam theory, a few percent
        // less for a beam this stocky (shear and rotary inertia)
        assert!(modes[6].freq > 4900.0 && modes[6].freq < 5200.0, "{}", modes[6].freq);
    }
}
