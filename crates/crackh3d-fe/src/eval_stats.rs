//! The solver statistics of crackh3d's `eval_stats`.

/// One linear solve.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SolverStats {
    /// `"cg_amg"`, `"cholesky"` or `"lu"`.
    pub kind: String,
    /// Free degrees of freedom (the system's order).
    pub n_dofs: usize,
    /// Stored entries of the system matrix.
    pub nnz: usize,
    /// CG iterations (`None` for a direct solve).
    pub iterations: Option<usize>,
    /// Final `||b - A x|| / ||b||` (true residual).
    pub rel_residual: f64,
    /// CG's own stopping measure (CG only).
    pub rel_precond_residual: Option<f64>,
    /// Whether it met its tolerance (CG) or passed the residual check (direct).
    pub converged: bool,
    pub setup_s: f64,
    pub solve_s: f64,
    /// AMG levels and operator complexity (CG only).
    pub levels: Option<usize>,
    pub operator_complexity: Option<f64>,
    /// Why this solve is not the solver that was asked for; `None` normally.
    pub fallback: Option<String>,
}
