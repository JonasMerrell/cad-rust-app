//! Finite elements from Python: parts with their own materials and element sizes, tied where they
//! touch; supports, springs and lumped masses on patches; many load cases on one factorisation; modes.
//!
//! Units: mm, N, MPa; densities in g/cm³ and masses in kg (turned into tonnes inside, so frequencies
//! come out in Hz).

use glam::DVec3;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use crate::fea_mesh::{self, Mesh, Surface};
use crate::fea_solve::{self, Coupling, Elastic, Fix, Problem};
use crate::Shape;

struct Body {
    name: String,
    tris: Vec<[DVec3; 3]>,
    faces: Vec<u32>,
    mat: Elastic,
    size: f64,
}

/// A group of boundary nodes with weights summing to 1: the share of a uniform traction on the
/// selected boundary faces each node carries (6-node faces: all of it on the mid-edge nodes).
struct Patch {
    nodes: Vec<(u32, f64)>,
    area: f64,
}

#[pyclass(module = "cadrust._core", unsendable)]
pub struct FeModel {
    bodies: Vec<Body>,
    mesh: Option<Mesh>,
    ties: Vec<Coupling>,
    patches: Vec<Patch>,
    fixes: Vec<Fix>,
    springs: Vec<Coupling>,
    masses: Vec<(Vec<(u32, f64)>, f64)>,
    results: Vec<Vec<f64>>,
    modes: Vec<fea_solve::Mode>,
}

fn err(e: String) -> PyErr {
    PyRuntimeError::new_err(e)
}

impl FeModel {
    fn mesh_ref(&self) -> PyResult<&Mesh> {
        self.mesh.as_ref().ok_or_else(|| PyValueError::new_err("mesh the model first (FeModel.mesh)"))
    }

    fn problem<'a>(&'a self, mesh: &'a Mesh, force: Vec<f64>) -> Problem<'a> {
        let mut couplings = self.ties.clone();
        couplings.extend(self.springs.iter().cloned());
        Problem {
            mesh,
            mat: self.bodies[0].mat,
            body_mats: self.bodies.iter().map(|b| b.mat).collect(),
            fixes: self.fixes.clone(),
            force,
            couplings,
            masses: self.masses.clone(),
        }
    }

    fn patch_ref(&self, i: usize) -> PyResult<&Patch> {
        self.patches.get(i).ok_or_else(|| PyValueError::new_err(format!("no patch {i}")))
    }
}

#[pymethods]
impl FeModel {
    #[new]
    fn new() -> FeModel {
        FeModel { bodies: vec![], mesh: None, ties: vec![], patches: vec![], fixes: vec![], springs: vec![], masses: vec![], results: vec![], modes: vec![] }
    }

    /// Add a part: its solid, Young's modulus (MPa), Poisson's ratio, density (g/cm³) and element size (mm).
    /// Returns its number.
    #[pyo3(signature = (shape, e, nu, rho, size, name=String::new(), deflection=None))]
    #[allow(clippy::too_many_arguments)]
    fn add_body(&mut self, shape: PyRef<Shape>, e: f64, nu: f64, rho: f64, size: f64, name: String, deflection: Option<f64>) -> PyResult<usize> {
        let d = deflection.unwrap_or((size * 0.02).clamp(0.01, 0.1));
        let v = opencascade_sys::extra::ex_triangulate(shape.s(), d, 0.2, false);
        if v.len() < 2 {
            return Err(err("could not triangulate the part".into()));
        }
        let (nv, nt) = (v[0] as usize, v[1] as usize);
        let pts: Vec<DVec3> = v[2..2 + 3 * nv].chunks_exact(3).map(|c| DVec3::new(c[0], c[1], c[2])).collect();
        let idx = &v[2 + 3 * nv..2 + 3 * nv + 3 * nt];
        let tris = idx.chunks_exact(3).map(|c| [pts[c[0] as usize], pts[c[1] as usize], pts[c[2] as usize]]).collect();
        let faces = v[2 + 3 * nv + 3 * nt..].iter().map(|x| *x as u32).collect();
        self.bodies.push(Body { name, tris, faces, mat: Elastic::iso(e, nu, rho * 1e-9, 0.0, 0.0), size });
        self.mesh = None;
        Ok(self.bodies.len() - 1)
    }

    /// Mesh every part at its own size and tie the parts where they touch (within `tol` mm), by springs
    /// `beta` × E × size stiff. `bonds`: the pairs of parts to tie (default: every pair that touches).
    /// Returns (nodes, elements, ties).
    #[pyo3(signature = (tol=0.3, beta=1000.0, bonds=None))]
    fn mesh(&mut self, py: Python<'_>, tol: f64, beta: f64, bonds: Option<Vec<(u32, u32)>>) -> PyResult<(usize, usize, usize)> {
        if self.bodies.is_empty() {
            return Err(PyValueError::new_err("add parts first"));
        }
        let mut tris = vec![];
        let mut face = vec![];
        let mut body = vec![];
        let mut off = 0u32;
        for (bi, b) in self.bodies.iter().enumerate() {
            tris.extend(b.tris.iter().copied());
            face.extend(b.faces.iter().map(|f| f + off));
            body.extend(std::iter::repeat_n(bi as u32, b.tris.len()));
            off += b.faces.iter().copied().max().map(|m| m + 1).unwrap_or(0);
        }
        let sizes: Vec<f64> = self.bodies.iter().map(|b| b.size).collect();
        let moduli: Vec<f64> = self.bodies.iter().map(|b| b.mat.e).collect();
        let (mesh, ties) = py
            .detach(|| {
                let surf = Surface::new(tris, face, body);
                let mesh = fea_mesh::mesh_sized(&surf, &sizes, &|| false)?;
                let bond = |a: u32, b: u32| bonds.as_ref().is_none_or(|l| l.iter().any(|&(x, y)| (x, y) == (a, b) || (y, x) == (a, b)));
                let ties = fea_mesh::ties(&mesh, &sizes, &moduli, tol, beta, &bond);
                Ok::<_, String>((mesh, ties))
            })
            .map_err(err)?;
        let out = (mesh.nodes.len(), mesh.tets.len(), ties.len());
        self.mesh = Some(mesh);
        self.ties = ties;
        self.patches.clear();
        self.fixes.clear();
        self.springs.clear();
        self.masses.clear();
        self.results.clear();
        self.modes.clear();
        Ok(out)
    }

    /// Ties per pair of parts: [(follower, followed, count)].
    fn tie_counts(&self) -> PyResult<Vec<(u32, u32, usize)>> {
        let mesh = self.mesh_ref()?;
        let mut node_body = vec![u32::MAX; mesh.nodes.len()];
        for (t, b) in mesh.tets.iter().zip(&mesh.body) {
            for v in t {
                node_body[*v as usize] = *b;
            }
        }
        let mut m = std::collections::BTreeMap::new();
        for t in &self.ties {
            *m.entry((node_body[t.a[0].0 as usize], node_body[t.b[0].0 as usize])).or_insert(0) += 1;
        }
        Ok(m.into_iter().map(|((a, b), n)| (a, b, n)).collect())
    }

    /// The boundary faces whose centres lie in the box (xmin, ymin, zmin, xmax, ymax, zmax), on the given
    /// parts (default all), facing along `normal` within 30° when given: a patch for supports, springs,
    /// masses and loads. Returns its number.
    #[pyo3(signature = (bbox, bodies=None, normal=None))]
    fn patch(&mut self, bbox: (f64, f64, f64, f64, f64, f64), bodies: Option<Vec<u32>>, normal: Option<(f64, f64, f64)>) -> PyResult<usize> {
        let mesh = self.mesh_ref()?;
        let mut node_body = vec![u32::MAX; mesh.nodes.len()];
        for (t, b) in mesh.tets.iter().zip(&mesh.body) {
            for v in t {
                node_body[*v as usize] = *b;
            }
        }
        let lo = DVec3::new(bbox.0, bbox.1, bbox.2);
        let hi = DVec3::new(bbox.3, bbox.4, bbox.5);
        let want = normal.map(|n| DVec3::new(n.0, n.1, n.2).normalize_or_zero());
        let mut w: std::collections::BTreeMap<u32, f64> = Default::default();
        let mut area = 0.0;
        for (f, _) in &mesh.faces {
            if let Some(bs) = &bodies {
                if !bs.contains(&node_body[f[0] as usize]) {
                    continue;
                }
            }
            let p = [0, 1, 2].map(|i| mesh.nodes[f[i] as usize]);
            let c = (p[0] + p[1] + p[2]) / 3.0;
            if c.cmplt(lo).any() || c.cmpgt(hi).any() {
                continue;
            }
            let n2 = (p[1] - p[0]).cross(p[2] - p[0]);
            if let Some(n) = want {
                if n2.normalize_or_zero().dot(n) < 0.866 {
                    continue;
                }
            }
            let a = n2.length() / 2.0;
            area += a;
            for k in 3..6 {
                *w.entry(f[k]).or_default() += a / 3.0;
            }
        }
        if area <= 0.0 {
            return Err(PyValueError::new_err("the patch is empty: no boundary faces in the box"));
        }
        self.patches.push(Patch { nodes: w.into_iter().map(|(n, a)| (n, a / area)).collect(), area });
        Ok(self.patches.len() - 1)
    }

    /// Area (mm²) and node count of a patch.
    fn patch_info(&self, patch: usize) -> PyResult<(f64, usize)> {
        let p = self.patch_ref(patch)?;
        Ok((p.area, p.nodes.len()))
    }

    /// Hold a patch's nodes in the given directions ("xyz", "z", "xy", …).
    #[pyo3(signature = (patch, dofs="xyz"))]
    fn fix(&mut self, patch: usize, dofs: &str) -> PyResult<()> {
        let nodes: Vec<u32> = self.patch_ref(patch)?.nodes.iter().map(|x| x.0).collect();
        let mesh = self.mesh_ref()?;
        // all the patch's face nodes, corners too
        let set: std::collections::HashSet<u32> = nodes.iter().copied().collect();
        let mut all: std::collections::BTreeSet<u32> = Default::default();
        for (f, _) in &mesh.faces {
            if (3..6).any(|k| set.contains(&f[k])) {
                all.extend(f.iter().copied());
            }
        }
        if dofs == "xyz" {
            self.fixes.push(Fix::Fixed(all.into_iter().collect()));
            return Ok(());
        }
        let mut list = vec![];
        for c in dofs.chars() {
            let d = match c {
                'x' => DVec3::X,
                'y' => DVec3::Y,
                'z' => DVec3::Z,
                _ => return Err(PyValueError::new_err("dofs: letters x, y and z")),
            };
            list.extend(all.iter().map(|n| (*n, d)));
        }
        self.fixes.push(Fix::Roller(list));
        Ok(())
    }

    /// A spring (N/mm per direction) between the average motions of two patches, or of one patch and the
    /// ground when `b` is None.
    #[pyo3(signature = (a, b, k))]
    fn spring(&mut self, a: usize, b: Option<usize>, k: (f64, f64, f64)) -> PyResult<()> {
        let na = self.patch_ref(a)?.nodes.clone();
        let nb = match b {
            Some(b) => self.patch_ref(b)?.nodes.clone(),
            None => vec![],
        };
        self.springs.push(Coupling { a: na, b: nb, k: [k.0, k.1, k.2] });
        self.results.clear();
        Ok(())
    }

    /// A lumped mass (kg) spread over a patch.
    fn mass(&mut self, patch: usize, kg: f64) -> PyResult<()> {
        let n = self.patch_ref(patch)?.nodes.clone();
        self.masses.push((n, kg * 1e-3));
        Ok(())
    }

    /// Solve load cases on one factorisation. Each case is a list of (patch, (Fx, Fy, Fz)) in N, spread over
    /// the patch. Returns the number of cases solved.
    fn solve(&mut self, py: Python<'_>, cases: Vec<Vec<(usize, (f64, f64, f64))>>) -> PyResult<usize> {
        let mesh = self.mesh_ref()?;
        let nn = mesh.nodes.len();
        let mut forces = vec![];
        for case in &cases {
            let mut f = vec![0.0; 3 * nn];
            for (pi, (fx, fy, fz)) in case {
                for &(n, w) in &self.patch_ref(*pi)?.nodes {
                    f[3 * n as usize] += fx * w;
                    f[3 * n as usize + 1] += fy * w;
                    f[3 * n as usize + 2] += fz * w;
                }
            }
            forces.push(f);
        }
        let p = self.problem(mesh, vec![0.0; 3 * nn]);
        let res = py.detach(|| fea_solve::static_cases(&p, &forces, &|| false)).map_err(err)?;
        self.results = res;
        Ok(self.results.len())
    }

    /// The average displacement (mm) of a patch in a solved case.
    fn displacement(&self, case: usize, patch: usize) -> PyResult<(f64, f64, f64)> {
        let u = self.results.get(case).ok_or_else(|| PyValueError::new_err(format!("no case {case}: solve first")))?;
        let mut d = [0.0; 3];
        for &(n, w) in &self.patch_ref(patch)?.nodes {
            for c in 0..3 {
                d[c] += w * u[3 * n as usize + c];
            }
        }
        Ok((d[0], d[1], d[2]))
    }

    /// Displacements of every node in a solved case, [ux, uy, uz, …] (mm).
    fn node_displacements(&self, case: usize) -> PyResult<Vec<f64>> {
        Ok(self.results.get(case).ok_or_else(|| PyValueError::new_err(format!("no case {case}")))?.clone())
    }

    /// The lowest `count` natural frequencies (Hz).
    #[pyo3(signature = (count=6))]
    fn modal(&mut self, py: Python<'_>, count: usize) -> PyResult<Vec<f64>> {
        let mesh = self.mesh_ref()?;
        let p = self.problem(mesh, vec![0.0; 3 * mesh.nodes.len()]);
        let modes = py.detach(|| fea_solve::modal(&p, count, &|| false)).map_err(err)?;
        let f = modes.iter().map(|m| m.freq).collect();
        self.modes = modes;
        Ok(f)
    }

    /// Each part's share of a mode's kinetic energy (consistent element masses), then the lumped masses'
    /// share last.
    fn energy_shares(&self, mode: usize) -> PyResult<Vec<f64>> {
        let mesh = self.mesh_ref()?;
        let m = self.modes.get(mode).ok_or_else(|| PyValueError::new_err(format!("no mode {mode}: run modal first")))?;
        let phi = &m.shape;
        let mut e = vec![0.0; self.bodies.len() + 1];
        for (t, b) in mesh.tets.iter().zip(&mesh.body) {
            let x = fea_solve::coords(mesh, t);
            let vol = tet10_volume(&x);
            let s = self.bodies[*b as usize].mat.rho * vol / 420.0;
            let mut acc = 0.0;
            for i in 0..10 {
                for j in 0..10 {
                    let c = fea_solve::mass_coef(i, j);
                    for a in 0..3 {
                        acc += c * phi[3 * t[i] as usize + a] * phi[3 * t[j] as usize + a];
                    }
                }
            }
            e[*b as usize] += s * acc;
        }
        for (nodes, mass) in &self.masses {
            for &(n, w) in nodes {
                for a in 0..3 {
                    e[self.bodies.len()] += mass * w * phi[3 * n as usize + a].powi(2);
                }
            }
        }
        let tot: f64 = e.iter().sum::<f64>().max(1e-300);
        Ok(e.into_iter().map(|x| x / tot).collect())
    }

    /// A mode's shape at every node, [ux, uy, uz, …] (mass-normalised).
    fn mode_shape(&self, mode: usize) -> PyResult<Vec<f64>> {
        Ok(self.modes.get(mode).ok_or_else(|| PyValueError::new_err(format!("no mode {mode}")))?.shape.clone())
    }

    /// (nodes, elements, ties, parts).
    fn size(&self) -> PyResult<(usize, usize, usize, usize)> {
        let m = self.mesh_ref()?;
        Ok((m.nodes.len(), m.tets.len(), self.ties.len(), self.bodies.len()))
    }

    /// Element count per part.
    fn elements_per_body(&self) -> PyResult<Vec<(String, usize)>> {
        let m = self.mesh_ref()?;
        let mut n = vec![0usize; self.bodies.len()];
        for b in &m.body {
            n[*b as usize] += 1;
        }
        Ok(self.bodies.iter().zip(n).map(|(b, k)| (b.name.clone(), k)).collect())
    }

    /// Node coordinates, [x, y, z, …].
    fn nodes(&self) -> PyResult<Vec<f64>> {
        Ok(self.mesh_ref()?.nodes.iter().flat_map(|p| [p.x, p.y, p.z]).collect())
    }
}

fn tet10_volume(x: &[DVec3; 10]) -> f64 {
    (x[1] - x[0]).cross(x[2] - x[0]).dot(x[3] - x[0]).abs() / 6.0
}
