//! Port of `crackh3d/fe/solve.py`: remote-tension and boundary-condition
//! driven linear-elastic solves on a [`Mesh`], plus `resolve_bcs`.
//!
//! The linear solve is chosen by [`LinearSolver`]: by default a `faer`
//! sparse Cholesky (the free-dof stiffness is SPD once the rigid-body modes
//! are pinned), falling back to sparse LU if the factorization reports a
//! non-positive pivot; or conjugate gradients preconditioned by smoothed-
//! aggregation AMG (`fe::amg`, a port of the PETSc CG+GAMG the Python uses),
//! falling back to the direct solve if CG does not converge. All agree to
//! solver tolerance. The direct solve in turn falls back to CG+AMG (rtol
//! 1e-10, with a warning on stderr) when faer cannot factor the system for
//! its size -- an index overflow or out-of-memory (not in the Python, whose
//! direct solver is MUMPS / PETSc).
//!
//! The boundary-condition input structs ([`BoundaryConditions`] and friends)
//! and [`match_face_tags`] are defined in `io::bc_json` and re-exported here.

use std::collections::{BTreeMap, HashMap};

use faer::linalg::solvers::Solve;
use faer::sparse::{SparseColMatRef, SymbolicSparseColMatRef};
use faer::{Mat, Side};
use thiserror::Error;

use super::amg::{solve_cg_amg, AmgOptions, CgInfo};
use super::assemble::{assemble_stiffness, AssembleError, Csr};
use super::surface::{facet_areas, traction_load_vector, Facet, Traction};
use crate::mesh_data::Mesh;

#[derive(Debug, Error)]
pub enum SolveError {
    #[error(transparent)]
    Assemble(#[from] AssembleError),
    #[error("{0}")]
    Invalid(String),
    #[error("sparse factorization failed: {0}")]
    Factor(String),
}

// ---------------------------------------------------------------- bc_json
// The boundary-condition structs and `match_face_tags` live in
// `crate::io::bc_json` (port of `crackh3d/io/bc_json.py`); re-exported here.
pub use crate::io::bc_json::{match_face_tags, BoundaryConditions, DisplacementCase, FaceSelection, LoadCase};

// ------------------------------------------------------------ linear algebra

/// Dirichlet node-set: `(node ids, {component: value})`.
pub type DirichletSet = (Vec<usize>, BTreeMap<usize, f64>);
/// Traction on a facet list.
pub type TractionSet = (Vec<Facet>, [f64; 3]);

/// Free-dof system: lower triangle (CSC, == CSR rows of the symmetric
/// matrix) or the full matrix, plus `rhs0 - K_fb u_b`.
struct FreeSystem {
    n: usize,
    col_ptr: Vec<u32>,
    row_idx: Vec<u32>,
    vals: Vec<f64>,
    rhs: Vec<f64>,
}

fn free_system(
    k: &Csr, free_map: &[u32], u_bc: &[f64], rhs_full: Option<&[f64]>, lower: bool,
) -> Result<FreeSystem, String> {
    const NONE: u32 = u32::MAX;
    let n = free_map.iter().filter(|&&m| m != NONE).count();
    let mut col_ptr = Vec::with_capacity(n + 1);
    let mut row_idx = Vec::new();
    let mut vals = Vec::new();
    let mut rhs = Vec::with_capacity(n);
    col_ptr.push(0u32);
    for r in 0..k.nrows {
        let fr = free_map[r];
        if fr == NONE {
            continue;
        }
        let mut b = rhs_full.map_or(0.0, |f| f[r]);
        for p in k.indptr[r]..k.indptr[r + 1] {
            let c = k.indices[p] as usize;
            let fc = free_map[c];
            if fc == NONE {
                b -= k.data[p] * u_bc[c];
            } else if !lower || fc >= fr {
                row_idx.push(fc);
                vals.push(k.data[p]);
            }
        }
        col_ptr.push(nnz_u32(row_idx.len())?);
        rhs.push(b);
    }
    Ok(FreeSystem { n, col_ptr, row_idx, vals, rhs })
}

/// A non-zero count as faer's `u32` column pointer, or an `IndexOverflow`
/// error (a size error: [`is_size_error`] sends it to the CG+AMG fallback)
/// instead of a silent wrap.
fn nnz_u32(n: usize) -> Result<u32, String> {
    u32::try_from(n).map_err(|_| format!("IndexOverflow: {n} stored entries do not fit faer's u32 sparse indices"))
}

/// Normwise backward error above which a direct solve's answer is refused.
/// A backward-stable factorization of a well-posed stiffness lands near
/// 1e-15 whatever its conditioning; a singular one does not.
pub const DIRECT_RESIDUAL_TOL: f64 = 1e-8;

/// The residual of a [`FreeSystem`] solve (`lower`: only the lower triangle
/// of the symmetric matrix is stored): `(||A x - b|| / ||b||, eta)` with `eta
/// = ||A x - b|| / (||A|| ||x|| + ||b||)` the normwise backward error
/// (`||A||` the max absolute row sum, 2-norms otherwise). Both are 0 when
/// `b` and `A x` vanish.
fn residuals(sys: &FreeSystem, x: &[f64], lower: bool) -> (f64, f64) {
    let mut ax = vec![0.0; sys.n];
    let mut row_abs = vec![0.0; sys.n];
    for c in 0..sys.n {
        for p in sys.col_ptr[c] as usize..sys.col_ptr[c + 1] as usize {
            let r = sys.row_idx[p] as usize;
            let v = sys.vals[p];
            ax[r] += v * x[c];
            row_abs[r] += v.abs();
            if lower && r != c {
                ax[c] += v * x[r];
                row_abs[c] += v.abs();
            }
        }
    }
    let norm2 = |v: &mut dyn Iterator<Item = f64>| v.map(|a| a * a).sum::<f64>().sqrt();
    let rn = norm2(&mut ax.iter().zip(&sys.rhs).map(|(a, b)| a - b));
    let bn = norm2(&mut sys.rhs.iter().copied());
    let xn = norm2(&mut x.iter().copied());
    let an = row_abs.iter().cloned().fold(0.0, f64::max);
    if rn == 0.0 {
        return (0.0, 0.0);
    }
    let rel = if bn > 0.0 { rn / bn } else { f64::INFINITY };
    let den = an * xn + bn;
    (rel, if den > 0.0 { rn / den } else { f64::INFINITY })
}

/// Threads used by the faer factorization (process-global in faer). Default is
/// sequential: on a busy or many-core node faer's rayon scheduler measured
/// far slower than one thread (469 s vs 21 s on the 44k-node case at 120
/// threads under load). Call before solving to opt in to `n > 1`.
pub fn set_solver_threads(n: usize) {
    SOLVER_THREADS.store(n, std::sync::atomic::Ordering::Relaxed);
    SOLVER_THREADS_SET.store(true, std::sync::atomic::Ordering::Relaxed);
    faer::set_global_parallelism(if n <= 1 { faer::Par::Seq } else { faer::Par::rayon(n) });
}
static SOLVER_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
/// The thread count last given to [`set_solver_threads`] (1 until then).
pub fn solver_threads() -> usize {
    SOLVER_THREADS.load(std::sync::atomic::Ordering::Relaxed)
}
static SOLVER_THREADS_SET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Which linear solver the free-dof system goes to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LinearSolver {
    /// `faer` sparse Cholesky (LU fallback).
    Direct,
    /// CG + smoothed-aggregation AMG (`fe::amg`, PETSc CG+GAMG's algorithm),
    /// stopping at `rtol` in the preconditioned residual norm (PETSc's CG
    /// default). Falls back to `Direct` if CG does not converge. The default,
    /// at [`DEFAULT_CG_RTOL`].
    CgAmg { rtol: f64 },
}

/// The default CG relative tolerance. 1e-8 changes K_I by ~2e-7 relative
/// against 1e-10 (the Python's), for ~20% less PCG time.
pub const DEFAULT_CG_RTOL: f64 = 1e-8;

impl Default for LinearSolver {
    fn default() -> Self {
        LinearSolver::cg_amg()
    }
}

impl LinearSolver {
    /// CG+AMG at [`DEFAULT_CG_RTOL`].
    pub const fn cg_amg() -> Self {
        LinearSolver::CgAmg { rtol: DEFAULT_CG_RTOL }
    }
}

static LINEAR_SOLVER: std::sync::RwLock<LinearSolver> = std::sync::RwLock::new(LinearSolver::cg_amg());

/// Process-global solver used by [`solve_remote_tension`] and
/// [`solve_with_bcs`] (and so by the pipeline); the `_with` variants take
/// it explicitly.
pub fn set_linear_solver(s: LinearSolver) {
    *LINEAR_SOLVER.write().unwrap() = s;
}

/// The current process-global solver (default [`LinearSolver::Direct`]).
pub fn linear_solver() -> LinearSolver {
    *LINEAR_SOLVER.read().unwrap()
}

fn solve_system(sys: &FreeSystem, side: Option<Side>) -> Result<Vec<f64>, String> {
    if !SOLVER_THREADS_SET.load(std::sync::atomic::Ordering::Relaxed) {
        set_solver_threads(1);
    }
    let sym = SymbolicSparseColMatRef::<u32>::new_checked(sys.n, sys.n, &sys.col_ptr, None, &sys.row_idx);
    let a = SparseColMatRef::<u32, f64>::new(sym, &sys.vals);
    let mut x = Mat::<f64>::from_fn(sys.n, 1, |i, _| sys.rhs[i]);
    let t_factor = std::time::Instant::now();
    let t_solve;
    match side {
        Some(side) => {
            let llt = a.sp_cholesky(side).map_err(|e| format!("{e:?}"))?;
            let factor_s = t_factor.elapsed().as_secs_f64();
            let t = std::time::Instant::now();
            llt.solve_in_place(x.as_mut());
            t_solve = (factor_s, t.elapsed().as_secs_f64());
        }
        None => {
            let lu = a.sp_lu().map_err(|e| format!("{e:?}"))?;
            let factor_s = t_factor.elapsed().as_secs_f64();
            let t = std::time::Instant::now();
            lu.solve_in_place(x.as_mut());
            t_solve = (factor_s, t.elapsed().as_secs_f64());
        }
    }
    let x: Vec<f64> = (0..sys.n).map(|i| x[(i, 0)]).collect();
    // every direct answer is checked: a factorization that "succeeds" on a
    // singular or indefinite matrix returns garbage, never an error
    let t_res = std::time::Instant::now();
    let (rel, eta) = residuals(sys, &x, side.is_some());
    let ok = eta <= DIRECT_RESIDUAL_TOL && x.iter().all(|v| v.is_finite());
    crate::progress::record_solve(crate::eval_stats::SolverStats {
        kind: if side.is_some() { "cholesky" } else { "lu" }.into(),
        n_dofs: sys.n,
        nnz: sys.vals.len(),
        iterations: None,
        rel_residual: rel,
        rel_precond_residual: None,
        converged: ok,
        setup_s: t_solve.0,
        solve_s: t_solve.1 + t_res.elapsed().as_secs_f64(),
        levels: None,
        operator_complexity: None,
        fallback: None,
    });
    if !ok {
        return Err(format!(
            "Singular: the {} solution does not satisfy the system -- relative residual ||Ku - f|| / ||f|| = {rel:.2e}, backward error {eta:.2e} (> {DIRECT_RESIDUAL_TOL:e}); the stiffness is singular or not positive definite (an under-constrained model, or inverted elements)",
            if side.is_some() { "Cholesky" } else { "LU" }
        ));
    }
    Ok(x)
}

/// Free-dof system of a constrained solve: the full symmetric free-dof
/// stiffness (CSR), `rhs = f_free - K_fb u_b`, and the free dof ids (sorted)
/// -- what goes to the linear solver.
#[derive(Debug, Clone)]
pub struct FreeDofSystem {
    pub k: Csr,
    pub rhs: Vec<f64>,
    pub free_dofs: Vec<usize>,
}

/// `free_map` (free index of each dof, `u32::MAX` if constrained) and the
/// prescribed-value vector.
fn constraint_maps(ndof: usize, bc_dofs: &[usize], bc_vals: &[f64]) -> (Vec<u32>, Vec<f64>) {
    let mut u_bc = vec![0.0; ndof];
    let mut free_map = vec![u32::MAX; ndof];
    for (&d, &v) in bc_dofs.iter().zip(bc_vals) {
        u_bc[d] = v;
        free_map[d] = u32::MAX - 1; // mark constrained
    }
    let mut nf = 0u32;
    for m in free_map.iter_mut() {
        if *m == u32::MAX {
            *m = nf;
            nf += 1;
        } else {
            *m = u32::MAX;
        }
    }
    (free_map, u_bc)
}

fn free_dof_system(
    k: &Csr, bc_dofs: &[usize], bc_vals: &[f64], rhs_full: Option<&[f64]>,
) -> Result<FreeDofSystem, SolveError> {
    let (free_map, u_bc) = constraint_maps(k.nrows, bc_dofs, bc_vals);
    let sys = free_system(k, &free_map, &u_bc, rhs_full, false).map_err(SolveError::Factor)?;
    let free_dofs = (0..k.nrows).filter(|&i| free_map[i] != u32::MAX).collect();
    Ok(FreeDofSystem {
        k: Csr {
            nrows: sys.n,
            ncols: sys.n,
            indptr: sys.col_ptr.iter().map(|&x| x as usize).collect(),
            indices: sys.row_idx,
            data: sys.vals,
        },
        rhs: sys.rhs,
        free_dofs,
    })
}

/// Solve a [`FreeDofSystem`] by CG+AMG (rigid-body near-nullspace from
/// `points`). Returns the free-dof solution and the solver statistics.
pub fn solve_free_system_cg_amg(
    sys: &FreeDofSystem, points: &[[f64; 3]], rtol: f64, opts: &AmgOptions,
) -> Result<(Vec<f64>, CgInfo), String> {
    solve_cg_amg(&sys.k, &sys.rhs, &sys.free_dofs, points, rtol, opts)
}

/// Solve a [`FreeDofSystem`] directly (faer Cholesky, LU fallback).
///
/// The Cholesky answer is residual-checked; if the factorization fails or its
/// answer does not satisfy the system, the LU answer is used only when it
/// does (residual <= [`DIRECT_RESIDUAL_TOL`]), and that is announced on
/// stderr. Otherwise the error names both failures.
pub fn solve_free_system_direct(sys: &FreeDofSystem) -> Result<Vec<f64>, SolveError> {
    let lower = lower_of(sys).map_err(SolveError::Factor)?;
    match solve_system(&lower, Some(Side::Lower)) {
        Ok(x) => Ok(x),
        Err(e) if is_size_error(&e) => Err(SolveError::Factor(e)),
        Err(chol) => {
            let full = FreeSystem {
                n: sys.k.nrows,
                col_ptr: sys.k.indptr.iter().map(|&x| nnz_u32(x)).collect::<Result<_, _>>().map_err(SolveError::Factor)?,
                row_idx: sys.k.indices.clone(),
                vals: sys.k.data.clone(),
                rhs: sys.rhs.clone(),
            };
            lu_after_cholesky(&full, &chol)
        }
    }
}

/// The LU retry after a failed Cholesky: its answer (announced on stderr)
/// when it satisfies the system, else an error naming both failures.
fn lu_after_cholesky(full: &FreeSystem, chol: &str) -> Result<Vec<f64>, SolveError> {
    match solve_system(full, None) {
        Ok(x) => {
            eprintln!(
                "warning: the Cholesky factorization of the stiffness failed ({chol}); the sparse LU solution satisfies the system and is used"
            );
            Ok(x)
        }
        Err(e) if is_size_error(&e) => Err(SolveError::Factor(e)),
        Err(lu) => Err(SolveError::Factor(format!(
            "the stiffness is singular: Cholesky failed ({chol}) and the LU solution was refused ({lu}). The BCs probably leave a rigid-body motion free, or elements are inverted."
        ))),
    }
}

/// Whether a faer factorization error says the system is too big for it
/// (its `u32`/`usize` index range, or memory) rather than numerically bad.
pub fn is_size_error(msg: &str) -> bool {
    msg.contains("IndexOverflow") || msg.contains("OutOfMemory")
}

/// Free-dof count above which the direct solve is not attempted and the
/// system goes straight to the CG+AMG fallback (`None`, the default: always
/// attempt it). Set by [`set_direct_dof_limit`].
static DIRECT_DOF_LIMIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);
static DIRECT_FALLBACKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// The relative tolerance of the CG+AMG fallback (the Python's `rtol`).
pub const DIRECT_FALLBACK_RTOL: f64 = 1e-10;

/// Treat a direct solve of more than `limit` free dofs as past faer's size
/// range (see [`solve_direct_or_fallback`]); `None` restores the default.
pub fn set_direct_dof_limit(limit: Option<usize>) {
    DIRECT_DOF_LIMIT.store(limit.unwrap_or(usize::MAX), std::sync::atomic::Ordering::Relaxed);
}

/// How many direct solves in this process fell back to CG+AMG.
pub fn direct_fallbacks() -> usize {
    DIRECT_FALLBACKS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The direct solve, falling back to CG+AMG (`rtol` [`DIRECT_FALLBACK_RTOL`])
/// when faer cannot factor the system for its size -- an index overflow or
/// out-of-memory on the Cholesky or the LU after it, or a free-dof count past
/// [`set_direct_dof_limit`]. A numerical failure is still an error. The
/// fallback is announced on stderr.
fn solve_direct_or_fallback(
    k: &Csr, points: &[[f64; 3]], bc_dofs: &[usize], bc_vals: &[f64], rhs_full: Option<&[f64]>,
    free_map: &[u32], u_bc: &[f64],
) -> Result<Vec<f64>, SolveError> {
    let n_free = free_map.iter().filter(|&&m| m != u32::MAX).count();
    let limit = DIRECT_DOF_LIMIT.load(std::sync::atomic::Ordering::Relaxed);
    let size_err = if n_free > limit {
        format!("past the direct-solve limit of {limit} free dofs")
    } else {
        match free_system(k, free_map, u_bc, rhs_full, true).and_then(|sys| solve_system(&sys, Some(Side::Lower))) {
            Ok(x) => return Ok(x),
            Err(e) if is_size_error(&e) => format!("faer: {e}"),
            Err(chol) => {
                let full = free_system(k, free_map, u_bc, rhs_full, false).map_err(SolveError::Factor)?;
                match lu_after_cholesky(&full, &chol) {
                    Ok(x) => return Ok(x),
                    Err(SolveError::Factor(e)) if is_size_error(&e) => format!("faer: {e}"),
                    Err(e) => return Err(e),
                }
            }
        }
    };
    DIRECT_FALLBACKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    crate::progress::attempt_failed("solver fallback (direct -> CG+AMG)", &format!("direct solve of {n_free} free dofs not possible: {size_err}"));
    eprintln!(
        "warning: the direct sparse solve cannot take this system of {n_free} free dofs ({size_err}); solving it by CG + smoothed-aggregation AMG instead (rtol {DIRECT_FALLBACK_RTOL:e})"
    );
    let sys = free_dof_system(k, bc_dofs, bc_vals, rhs_full)?;
    match solve_free_system_cg_amg(&sys, points, DIRECT_FALLBACK_RTOL, &AmgOptions::default()) {
        Ok((x, info)) => {
            crate::progress::annotate_last_solve(|s| s.fallback = Some(format!("direct solve not possible: {size_err}")));
            eprintln!(
                "warning: the CG+AMG fallback converged in {} iterations (relative residual {:.2e})",
                info.iterations, info.rel_true_residual
            );
            Ok(x)
        }
        Err(e) => Err(SolveError::Factor(format!(
            "the direct solve cannot take this system of {n_free} free dofs ({size_err}), and the CG+AMG fallback did not solve it either: {e}"
        ))),
    }
}

/// Lower triangle (CSC) of a full symmetric free-dof system.
fn lower_of(sys: &FreeDofSystem) -> Result<FreeSystem, String> {
    let k = &sys.k;
    let mut col_ptr = Vec::with_capacity(k.nrows + 1);
    let mut row_idx = Vec::new();
    let mut vals = Vec::new();
    col_ptr.push(0u32);
    for r in 0..k.nrows {
        for p in k.indptr[r]..k.indptr[r + 1] {
            if k.indices[p] as usize >= r {
                row_idx.push(k.indices[p]);
                vals.push(k.data[p]);
            }
        }
        col_ptr.push(nnz_u32(row_idx.len())?);
    }
    Ok(FreeSystem { n: k.nrows, col_ptr, row_idx, vals, rhs: sys.rhs.clone() })
}

/// Constrain `bc_dofs` (sorted, unique) to `bc_vals` and solve for the rest.
/// `rhs_full` is the external load (None = zero).
fn solve_constrained(
    k: &Csr, points: &[[f64; 3]], bc_dofs: &[usize], bc_vals: &[f64], rhs_full: Option<&[f64]>, solver: LinearSolver,
) -> Result<Vec<f64>, SolveError> {
    let ndof = k.nrows;
    let (free_map, u_bc) = constraint_maps(ndof, bc_dofs, bc_vals);
    if super::reuse::recording() {
        // for a second solve with the same constraints (`fe::reuse`)
        let rtol = match solver {
            LinearSolver::CgAmg { rtol } => rtol,
            LinearSolver::Direct => DIRECT_FALLBACK_RTOL,
        };
        let sys = free_dof_system(k, bc_dofs, bc_vals, rhs_full)?;
        super::reuse::record(super::reuse::SolveMemo::free(ndof, sys.k, sys.free_dofs, points, rtol, sys.rhs, u_bc.clone(), rhs_full));
    }
    let u_free = match solver {
        LinearSolver::Direct => solve_direct_or_fallback(k, points, bc_dofs, bc_vals, rhs_full, &free_map, &u_bc)?,
        LinearSolver::CgAmg { rtol } => {
            let sys = free_dof_system(k, bc_dofs, bc_vals, rhs_full)?;
            match solve_free_system_cg_amg(&sys, points, rtol, &AmgOptions::default()) {
                Ok((x, _)) => x,
                Err(e) => {
                    // as the Python does: warn and fall back to the direct solve
                    eprintln!("warning: {e}; falling back to the direct sparse solve");
                    crate::progress::attempt_failed("solver fallback (CG+AMG -> direct)", &e);
                    let x = solve_free_system_direct(&sys)?;
                    crate::progress::annotate_last_solve(|s| s.fallback = Some(e.clone()));
                    x
                }
            }
        }
    };
    let mut u = u_bc;
    let mut it = u_free.into_iter();
    for (i, m) in free_map.iter().enumerate() {
        if *m != u32::MAX {
            u[i] = it.next().unwrap();
        }
    }
    Ok(u)
}

fn as_points(u: Vec<f64>) -> Vec<[f64; 3]> {
    u.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
}

// ----------------------------------------------------------------- solves

/// 3x3 strain for uniform remote uniaxial stress `sigma0` along `axis`.
pub fn remote_uniaxial_strain(e: f64, nu: f64, sigma0: f64, axis: usize) -> [[f64; 3]; 3] {
    let mut eps = [[0.0; 3]; 3];
    for i in 0..3 {
        eps[i][i] = if i == axis { sigma0 / e } else { -nu * sigma0 / e };
    }
    eps
}

/// Dofs and values prescribed by a remote-tension solve.
#[allow(clippy::too_many_arguments)]
fn remote_tension_bcs(
    mesh: &Mesh, e: f64, nu: f64, sigma0: f64, center: [f64; 3], cube_half: [f64; 3], axis: usize, tol: f64,
    free_axes: &[usize],
) -> Result<(Vec<usize>, Vec<f64>), SolveError> {
    let eps = remote_uniaxial_strain(e, nu, sigma0, axis);
    let mut bc_dofs = Vec::new();
    let mut bc_vals = Vec::new();
    for (i, p) in mesh.points.iter().enumerate() {
        let rel = [p[0] - center[0], p[1] - center[1], p[2] - center[2]];
        let on = (0..3).any(|a| !free_axes.contains(&a) && (rel[a].abs() - cube_half[a]).abs() < tol);
        if on {
            for c in 0..3 {
                bc_dofs.push(3 * i + c);
                bc_vals.push((0..3).map(|k| rel[k] * eps[c][k]).sum::<f64>());
            }
        }
    }
    if bc_dofs.is_empty() {
        return Err(SolveError::Invalid("no mesh points found on the domain boundary -- check center/cube_half/tol".into()));
    }
    Ok((bc_dofs, bc_vals))
}

/// Remote-tension solve: the far-field displacement is prescribed on every
/// node within `tol` of +-`cube_half` (about `center`) on any axis not in
/// `free_axes`. Returns the `(N,3)` displacement. Uses the process-global
/// [`linear_solver`].
#[allow(clippy::too_many_arguments)]
pub fn solve_remote_tension(
    mesh: &Mesh,
    e: f64,
    nu: f64,
    sigma0: f64,
    center: [f64; 3],
    cube_half: [f64; 3],
    axis: usize,
    tol: f64,
    free_axes: &[usize],
) -> Result<Vec<[f64; 3]>, SolveError> {
    solve_remote_tension_with(mesh, e, nu, sigma0, center, cube_half, axis, tol, free_axes, linear_solver())
}

/// [`solve_remote_tension`] with an explicit [`LinearSolver`].
#[allow(clippy::too_many_arguments)]
pub fn solve_remote_tension_with(
    mesh: &Mesh,
    e: f64,
    nu: f64,
    sigma0: f64,
    center: [f64; 3],
    cube_half: [f64; 3],
    axis: usize,
    tol: f64,
    free_axes: &[usize],
    solver: LinearSolver,
) -> Result<Vec<[f64; 3]>, SolveError> {
    let (bc_dofs, bc_vals) = remote_tension_bcs(mesh, e, nu, sigma0, center, cube_half, axis, tol, free_axes)?;
    let k = assemble_stiffness(mesh, e, nu)?;
    Ok(as_points(solve_constrained(&k, &mesh.points, &bc_dofs, &bc_vals, None, solver)?))
}

/// The free-dof system [`solve_remote_tension`] hands to its linear solver
/// (for benchmarking solvers on exactly the same system).
#[allow(clippy::too_many_arguments)]
pub fn remote_tension_system(
    mesh: &Mesh, e: f64, nu: f64, sigma0: f64, center: [f64; 3], cube_half: [f64; 3], axis: usize, tol: f64,
    free_axes: &[usize],
) -> Result<FreeDofSystem, SolveError> {
    let (bc_dofs, bc_vals) = remote_tension_bcs(mesh, e, nu, sigma0, center, cube_half, axis, tol, free_axes)?;
    let k = assemble_stiffness(mesh, e, nu)?;
    free_dof_system(&k, &bc_dofs, &bc_vals, None)
}

/// Face-based BCs -> node sets and facet lists. `force` loads become
/// tractions by dividing by the total meshed facet area.
pub fn resolve_bcs(
    bcs: &BoundaryConditions,
    face_centers: &[(i64, [f64; 3])],
    face_facets: &HashMap<i64, Vec<Facet>>,
    points: &[[f64; 3]],
    tol: f64,
) -> Result<(Vec<DirichletSet>, Vec<TractionSet>), SolveError> {
    let case_facets = |name: &str, faces: &[FaceSelection]| -> Result<Vec<Facet>, SolveError> {
        let tags = match_face_tags(face_centers, faces, tol)?;
        let out: Vec<Facet> =
            tags.iter().filter_map(|t| face_facets.get(t)).flat_map(|f| f.iter().copied()).collect();
        if out.is_empty() {
            return Err(SolveError::Invalid(format!(
                "{name}: CAD face tags {tags:?} matched the selection, but none of them carry any surviving mesh \
                 facets -- the faces exist in the geometry but not in the mesh (they may have been cropped away by \
                 the crack)."
            )));
        }
        Ok(out)
    };

    let mut dirichlet = Vec::new();
    for case in &bcs.displacement {
        let facets = case_facets(&case.name, &case.faces)?;
        let mut nodes: Vec<usize> = facets.iter().flatten().copied().collect();
        nodes.sort_unstable();
        nodes.dedup();
        dirichlet.push((nodes, case.values.clone()));
    }
    let mut tractions = Vec::new();
    for case in &bcs.load {
        let facets = case_facets(&case.name, &case.faces)?;
        let mut magnitude = case.magnitude;
        if case.is_force {
            let area: f64 = facet_areas(points, &facets).iter().sum();
            if area <= 0.0 {
                return Err(SolveError::Invalid(format!("{}: selected load faces have zero total area", case.name)));
            }
            magnitude /= area;
        }
        let d = case.direction;
        tractions.push((facets, [magnitude * d[0], magnitude * d[1], magnitude * d[2]]));
    }
    Ok((dirichlet, tractions))
}

/// General solve: explicit per-component Dirichlet constraints plus surface
/// tractions. Overlapping Dirichlet dofs: the last case wins. Uses the
/// process-global [`linear_solver`].
pub fn solve_with_bcs(
    mesh: &Mesh,
    e: f64,
    nu: f64,
    dirichlet: &[DirichletSet],
    tractions: &[TractionSet],
) -> Result<Vec<[f64; 3]>, SolveError> {
    solve_with_bcs_with(mesh, e, nu, dirichlet, tractions, linear_solver())
}

/// [`solve_with_bcs`] with an explicit [`LinearSolver`].
pub fn solve_with_bcs_with(
    mesh: &Mesh,
    e: f64,
    nu: f64,
    dirichlet: &[DirichletSet],
    tractions: &[TractionSet],
    solver: LinearSolver,
) -> Result<Vec<[f64; 3]>, SolveError> {
    let k = assemble_stiffness(mesh, e, nu)?;
    let ndof = k.nrows;
    let mut rhs = vec![0.0; ndof];
    for (facets, t) in tractions {
        let f = traction_load_vector(&mesh.points, facets, Traction::Vector(*t), ndof);
        for (r, v) in rhs.iter_mut().zip(f) {
            *r += v;
        }
    }
    let mut map: BTreeMap<usize, f64> = BTreeMap::new();
    for (nodes, values) in dirichlet {
        for (&comp, &val) in values {
            for &n in nodes {
                map.insert(3 * n + comp, val);
            }
        }
    }
    if map.is_empty() {
        return Err(SolveError::Invalid("solve_with_bcs: no Dirichlet constraints -- the system would be singular".into()));
    }
    let restraints: Vec<super::bcs::Restraint> = map
        .keys()
        .map(|&d| {
            let mut dir = [0.0; 3];
            dir[d % 3] = 1.0;
            super::bcs::Restraint { point: mesh.points[d / 3], dir, rotation: false }
        })
        .collect();
    super::bcs::check_rigid_modes_held(&restraints)?;
    let bc_dofs: Vec<usize> = map.keys().copied().collect();
    let bc_vals: Vec<f64> = map.values().copied().collect();
    Ok(as_points(solve_constrained(&k, &mesh.points, &bc_dofs, &bc_vals, Some(&rhs), solver)?))
}
