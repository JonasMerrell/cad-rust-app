//! A record of the last constrained solve on this thread, so a second load
//! can be solved with the same stiffness, the same constraints held at zero,
//! and (when the first solve was CG + AMG) the same AMG hierarchy.
//!
//! Off unless asked for: [`Recording::start`] turns it on for the guard's
//! lifetime; the solvers in `fe::solve` and `fe::bcs` then leave a
//! [`SolveMemo`] behind (the last solve wins) and [`take`] hands it over.
//! What it is for: `growth::k_derivative` solves `K u' = -r` for the
//! derivative of the displacement along a virtual crack extension, a load
//! supported only round the crack front with the body's supports unmoved.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use super::amg::{node_groups, rigid_body_modes, spgemm, transpose, Amg, AmgOptions};
use super::assemble::Csr;

/// How the reduced unknowns map onto the full dof vector.
#[derive(Clone)]
pub enum DofMap {
    /// `u[free[i]] = q[i]`, every other dof held at zero.
    Free(Vec<usize>),
    /// `u = T q` (`fe::bcs`' master dofs).
    Masters(Csr),
}

/// The reduced system of one solve.
#[derive(Clone)]
pub struct SolveMemo {
    pub ndof: usize,
    pub map: DofMap,
    /// the reduced (constrained) stiffness, full symmetric CSR
    pub k: Csr,
    /// near-nullspace (`k.nrows x nv`, row-major) and dof groups for AMG
    nullspace: Nullspace,
    /// the AMG hierarchy, shared by the systems morphed from this one
    amg: Option<Arc<Amg>>,
    /// CG tolerance of a re-solve
    pub rtol: f64,
    /// the solve's own reduced right-hand side, the full prescribed part of
    /// `u` (`u = map(q) + offset`), and which dofs carry an external load
    pub rhs: Vec<f64>,
    pub offset: Vec<f64>,
    pub loaded: Vec<bool>,
    /// the AMG hierarchy is a morphed-from matrix's / was rebuilt after a morph
    amg_inherited: bool,
    pub amg_rebuilt: bool,
}

#[derive(Clone)]
enum Nullspace {
    /// rigid-body modes of the free dofs at these points
    Points(Vec<[f64; 3]>),
    Given { ns: Vec<f64>, nv: usize, gptr: Vec<usize> },
}

impl SolveMemo {
    /// A plain Dirichlet solve: `k` on `free_dofs`, the rest held.
    #[allow(clippy::too_many_arguments)]
    pub fn free(
        ndof: usize, k: Csr, free_dofs: Vec<usize>, points: &[[f64; 3]], rtol: f64, rhs: Vec<f64>, offset: Vec<f64>, f: Option<&[f64]>,
    ) -> Self {
        let loaded = f.map_or(vec![false; ndof], |f| f.iter().map(|&x| x != 0.0).collect());
        SolveMemo { ndof, map: DofMap::Free(free_dofs), k, nullspace: Nullspace::Points(points.to_vec()), amg: None, rtol, rhs, offset, loaded, amg_inherited: false, amg_rebuilt: false }
    }

    /// A master-dof solve `u = T q + g`.
    #[allow(clippy::too_many_arguments)]
    pub fn masters(t: Csr, k: Csr, ns: Vec<f64>, nv: usize, gptr: Vec<usize>, rtol: f64, rhs: Vec<f64>, g: Vec<f64>, f: &[f64]) -> Self {
        let loaded = f.iter().map(|&x| x != 0.0).collect();
        SolveMemo {
            ndof: t.nrows,
            map: DofMap::Masters(t),
            k,
            nullspace: Nullspace::Given { ns, nv, gptr },
            amg: None,
            rtol,
            rhs,
            offset: g,
            loaded,
            amg_inherited: false,
            amg_rebuilt: false,
        }
    }

    /// The same constraints and loads on a stiffness changed by `dk` (full,
    /// e.g. the elements a mesh morph moved): the reduced stiffness updated,
    /// the AMG hierarchy kept as the preconditioner.
    pub fn morphed(&self, dk: &Csr) -> SolveMemo {
        let dr = match &self.map {
            DofMap::Free(free) => restrict(dk, free),
            DofMap::Masters(t) => {
                let tt = transpose(t);
                spgemm(&tt, &spgemm(dk, t, None), None)
            }
        };
        SolveMemo {
            ndof: self.ndof,
            map: self.map.clone(),
            k: sparse_add(&self.k, &dr),
            nullspace: self.nullspace.clone(),
            amg: self.amg.clone(),
            rtol: self.rtol,
            rhs: self.rhs.clone(),
            offset: self.offset.clone(),
            loaded: self.loaded.clone(),
            amg_inherited: self.amg.is_some(),
            amg_rebuilt: false,
        }
    }

    /// The full `u` of the solve this memo describes (its own load and
    /// prescribed values), and the CG iterations.
    pub fn solve_own(&mut self, rtol: f64) -> Result<(Vec<f64>, usize), String> {
        let rhs = self.rhs.clone();
        let (q, its) = self.solve_reduced(&rhs, rtol)?;
        let mut u = self.expand(&q);
        for (a, b) in u.iter_mut().zip(&self.offset) {
            *a += b;
        }
        Ok((u, its))
    }

    /// `|(reduce(k_full) - k) x| / |k x|` for a fixed pseudo-random `x`: how
    /// far the held reduced stiffness is from `k_full`'s (a check).
    pub fn reduced_mismatch(&self, k_full: &Csr) -> f64 {
        let kr = match &self.map {
            DofMap::Free(free) => restrict(k_full, free),
            DofMap::Masters(t) => {
                let tt = transpose(t);
                spgemm(&tt, &spgemm(k_full, t, None), None)
            }
        };
        let x: Vec<f64> = (0..self.k.ncols).map(|i| ((i * 7919) % 1000) as f64 / 1000.0 - 0.5).collect();
        let (a, b) = (kr.matvec(&x), self.k.matvec(&x));
        let e = a.iter().zip(&b).map(|(p, q)| (p - q).abs()).fold(0.0, f64::max);
        e / b.iter().fold(0.0f64, |m, v| m.max(v.abs()))
    }

    /// `||K q - rhs|| / ||rhs||` for the full `u = map(q) + offset` (`q`
    /// recovered for a free-dof map only; NaN otherwise).
    pub fn residual_of(&self, u: &[f64]) -> f64 {
        match &self.map {
            DofMap::Free(free) => {
                let q: Vec<f64> = free.iter().map(|&d| u[d] - self.offset[d]).collect();
                let kq = self.k.matvec(&q);
                let r: f64 = kq.iter().zip(&self.rhs).map(|(a, b)| (a - b) * (a - b)).sum::<f64>().sqrt();
                r / self.rhs.iter().map(|b| b * b).sum::<f64>().sqrt()
            }
            DofMap::Masters(_) => f64::NAN,
        }
    }

    fn expand(&self, q: &[f64]) -> Vec<f64> {
        match &self.map {
            DofMap::Free(free) => {
                let mut u = vec![0.0; self.ndof];
                for (&d, &v) in free.iter().zip(q) {
                    u[d] = v;
                }
                u
            }
            DofMap::Masters(t) => t.matvec(q),
        }
    }

    fn solve_reduced(&mut self, rhs: &[f64], rtol: f64) -> Result<(Vec<f64>, usize), String> {
        if self.amg.is_none() {
            let (ns, nv, gptr) = match &self.nullspace {
                Nullspace::Points(pts) => match &self.map {
                    DofMap::Free(free) => (rigid_body_modes(free, pts), 6, node_groups(free)),
                    DofMap::Masters(_) => unreachable!(),
                },
                Nullspace::Given { ns, nv, gptr } => (ns.clone(), *nv, gptr.clone()),
            };
            self.amg = Some(Arc::new(Amg::new(self.k.clone(), ns, nv, gptr, &AmgOptions::default())?));
        }
        let true_res = |k: &Csr, q: &[f64]| {
            let kq = k.matvec(q);
            let r: f64 = kq.iter().zip(rhs).map(|(a, b)| (a - b) * (a - b)).sum::<f64>().sqrt();
            r / rhs.iter().map(|b| b * b).sum::<f64>().sqrt().max(f64::MIN_POSITIVE)
        };
        let (mut q, mut its, mut rel, mut ok) = pcg_with(&self.k, self.amg.as_ref().unwrap(), rhs, rtol, 2000);
        // an inherited preconditioner can report convergence in its own norm
        // while the true residual is not small
        let tr = true_res(&self.k, &q);
        if std::env::var("MORPH_VERIFY").is_ok() {
            eprintln!("reuse: pcg {its} its, preconditioned {rel:.2e}, true residual {tr:.2e}, inherited {}", self.amg_inherited);
        }
        if self.amg_inherited && (!ok || tr > 1e3 * rtol) {
            ok = false;
        }
        if !ok && self.amg_inherited {
            // the hierarchy came from the matrix before a morph and is no
            // longer a valid preconditioner (its smoother's spectrum bound):
            // rebuild it for this one
            let (ns, nv, gptr) = match &self.nullspace {
                Nullspace::Points(pts) => match &self.map {
                    DofMap::Free(free) => (rigid_body_modes(free, pts), 6, node_groups(free)),
                    DofMap::Masters(_) => unreachable!(),
                },
                Nullspace::Given { ns, nv, gptr } => (ns.clone(), *nv, gptr.clone()),
            };
            self.amg = Some(Arc::new(Amg::new(self.k.clone(), ns, nv, gptr, &AmgOptions::default())?));
            self.amg_inherited = false;
            self.amg_rebuilt = true;
            (q, its, rel, ok) = pcg_with(&self.k, self.amg.as_ref().unwrap(), rhs, rtol, 2000);
            if std::env::var("MORPH_VERIFY").is_ok() {
                eprintln!("reuse: rebuilt AMG: pcg {its} its, preconditioned {rel:.2e}, true residual {:.2e}", true_res(&self.k, &q));
            }
        }
        if !ok {
            return Err(format!("SolveMemo: CG did not converge ({its} iterations, relative residual {rel:.2e})"));
        }
        Ok((q, its))
    }

    /// Forget the AMG hierarchy (the next solve builds one for `k`).
    pub fn drop_amg(&mut self) {
        self.amg = None;
        self.amg_inherited = false;
    }

    /// Whether `node` has a constrained or loaded dof (a mesh morph must not move it).
    pub fn node_fixed_or_loaded(&self, node: usize) -> bool {
        (0..3).any(|c| self.loaded.get(3 * node + c).copied().unwrap_or(false)) || self.allowed_dirs(node).len() < 3
    }

    /// An orthonormal basis of the directions the constraints leave `node`
    /// free to move in (empty: held; three axes: unconstrained).
    pub fn allowed_dirs(&self, node: usize) -> Vec<[f64; 3]> {
        let mut cols: Vec<[f64; 3]> = vec![];
        match &self.map {
            DofMap::Free(free) => {
                for c in 0..3 {
                    if free.binary_search(&(3 * node + c)).is_ok() {
                        let mut e = [0.0; 3];
                        e[c] = 1.0;
                        cols.push(e);
                    }
                }
                return cols;
            }
            DofMap::Masters(t) => {
                let mut by_master: Vec<(u32, [f64; 3])> = vec![];
                for c in 0..3 {
                    let row = 3 * node + c;
                    for p in t.indptr[row]..t.indptr[row + 1] {
                        let m = t.indices[p];
                        match by_master.iter_mut().find(|x| x.0 == m) {
                            Some(x) => x.1[c] += t.data[p],
                            None => {
                                let mut v = [0.0; 3];
                                v[c] = t.data[p];
                                by_master.push((m, v));
                            }
                        }
                    }
                }
                cols.extend(by_master.into_iter().map(|x| x.1));
            }
        }
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let mut basis: Vec<[f64; 3]> = vec![];
        for c in cols {
            let mut w = c;
            for b in &basis {
                let k = dot(w, *b);
                w = [w[0] - k * b[0], w[1] - k * b[1], w[2] - k * b[2]];
            }
            let nw = dot(w, w).sqrt();
            if nw > 1e-8 * dot(c, c).sqrt().max(f64::MIN_POSITIVE) && basis.len() < 3 {
                basis.push([w[0] / nw, w[1] / nw, w[2] / nw]);
            }
        }
        basis
    }

    /// Whether the first solve's AMG hierarchy is held.
    pub fn has_amg(&self) -> bool {
        self.amg.is_some()
    }

    /// `u` with `K u = f` on the unknowns and every constraint at zero, for a
    /// full-length load `f`. Returns `u` (full) and the CG iteration count.
    pub fn solve_homogeneous(&mut self, f: &[f64]) -> Result<(Vec<f64>, usize), String> {
        if f.len() != self.ndof {
            return Err(format!("SolveMemo: load of {} dofs for a system of {}", f.len(), self.ndof));
        }
        let rhs: Vec<f64> = match &self.map {
            DofMap::Free(free) => free.iter().map(|&d| f[d]).collect(),
            DofMap::Masters(t) => {
                let mut r = vec![0.0; t.ncols];
                for row in 0..t.nrows {
                    let fr = f[row];
                    if fr != 0.0 {
                        for p in t.indptr[row]..t.indptr[row + 1] {
                            r[t.indices[p] as usize] += t.data[p] * fr;
                        }
                    }
                }
                r
            }
        };
        let rtol = self.rtol;
        let (q, its) = self.solve_reduced(&rhs, rtol)?;
        Ok((self.expand(&q), its))
    }
}

/// `a[free][:, free]`.
fn restrict(a: &Csr, free: &[usize]) -> Csr {
    let mut map = vec![u32::MAX; a.ncols];
    for (i, &d) in free.iter().enumerate() {
        map[d] = i as u32;
    }
    let (mut indptr, mut indices, mut data) = (vec![0usize], vec![], vec![]);
    for &r in free {
        for p in a.indptr[r]..a.indptr[r + 1] {
            let c = map[a.indices[p] as usize];
            if c != u32::MAX {
                indices.push(c);
                data.push(a.data[p]);
            }
        }
        indptr.push(indices.len());
    }
    Csr { nrows: free.len(), ncols: free.len(), indptr, indices, data }
}

/// `a + b` (columns sorted in each row of both).
fn sparse_add(a: &Csr, b: &Csr) -> Csr {
    let (mut indptr, mut indices, mut data) = (vec![0usize], Vec::with_capacity(a.data.len()), Vec::with_capacity(a.data.len()));
    for r in 0..a.nrows {
        let (mut p, pe) = (a.indptr[r], a.indptr[r + 1]);
        let (mut q, qe) = if r < b.nrows { (b.indptr[r], b.indptr[r + 1]) } else { (0, 0) };
        while p < pe || q < qe {
            let ca = if p < pe { a.indices[p] } else { u32::MAX };
            let cb = if q < qe { b.indices[q] } else { u32::MAX };
            if ca == cb {
                indices.push(ca);
                data.push(a.data[p] + b.data[q]);
                p += 1;
                q += 1;
            } else if ca < cb {
                indices.push(ca);
                data.push(a.data[p]);
                p += 1;
            } else {
                indices.push(cb);
                data.push(b.data[q]);
                q += 1;
            }
        }
        indptr.push(indices.len());
    }
    Csr { nrows: a.nrows, ncols: a.ncols, indptr, indices, data }
}

/// Preconditioned CG on `op` with `amg` as the preconditioner (which may
/// have been built for a nearby matrix): zero start, the preconditioned
/// residual norm relative to `||M^-1 b||`, as `amg::pcg`.
pub fn pcg_with(op: &Csr, amg: &Amg, b: &[f64], rtol: f64, max_it: usize) -> (Vec<f64>, usize, f64, bool) {
    let n = b.len();
    let dot = |x: &[f64], y: &[f64]| x.iter().zip(y).map(|(a, b)| a * b).sum::<f64>();
    let mut x = vec![0.0; n];
    let mut r = b.to_vec();
    let mut z = vec![0.0; n];
    amg.apply(&r, &mut z);
    let rn0 = dot(&z, &z).sqrt();
    if rn0 == 0.0 {
        return (x, 0, 0.0, true);
    }
    let mut p = z.clone();
    let mut rz = dot(&r, &z);
    let mut rn = rn0;
    for it in 0..max_it {
        let q = op.matvec(&p);
        let pq = dot(&p, &q);
        if !(pq > 0.0) {
            return (x, it, rn / rn0, false);
        }
        let alpha = rz / pq;
        for i in 0..n {
            x[i] += alpha * p[i];
            r[i] -= alpha * q[i];
        }
        amg.apply(&r, &mut z);
        rn = dot(&z, &z).sqrt();
        if rn <= rtol * rn0 {
            return (x, it + 1, rn / rn0, true);
        }
        let rz2 = dot(&r, &z);
        let beta = rz2 / rz;
        rz = rz2;
        for i in 0..n {
            p[i] = z[i] + beta * p[i];
        }
    }
    (x, max_it, rn / rn0, false)
}

thread_local! {
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    static MEMO: RefCell<Option<SolveMemo>> = const { RefCell::new(None) };
}

/// Whether solves on this thread are being recorded.
pub fn recording() -> bool {
    RECORDING.with(|r| r.get())
}

/// Keep `memo` as the last solve (replacing any earlier one).
pub fn record(memo: SolveMemo) {
    MEMO.with(|m| *m.borrow_mut() = Some(memo));
}

/// Hand the AMG hierarchy just used to the recorded solve, if it has none.
pub fn stash_amg(amg: Amg) {
    MEMO.with(|m| {
        if let Some(memo) = m.borrow_mut().as_mut() {
            if memo.amg.is_none() {
                memo.amg = Some(Arc::new(amg));
            }
        }
    });
}

/// The last recorded solve, removed.
pub fn take() -> Option<SolveMemo> {
    MEMO.with(|m| m.borrow_mut().take())
}

/// Records solves while alive; dropping it stops recording and clears
/// whatever was not taken.
pub struct Recording {
    was: bool,
}

impl Recording {
    pub fn start() -> Self {
        let was = RECORDING.with(|r| r.replace(true));
        MEMO.with(|m| *m.borrow_mut() = None);
        Recording { was }
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        RECORDING.with(|r| r.set(self.was));
        if !self.was {
            MEMO.with(|m| *m.borrow_mut() = None);
        }
    }
}
