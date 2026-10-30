//! What crackh3d's `progress` module gives the solver: stage guards (no-ops here), CG iteration reports
//! (passed to a hook the caller may set on its thread) and the statistics of the last solves.

use std::cell::RefCell;

use crate::eval_stats::SolverStats;

thread_local! {
    static SOLVES: RefCell<Vec<SolverStats>> = const { RefCell::new(Vec::new()) };
    static ITER: RefCell<Option<Box<dyn Fn(usize, f64)>>> = const { RefCell::new(None) };
}

pub struct StageGuard;

pub fn stage(_name: impl Into<std::borrow::Cow<'static, str>>) -> StageGuard {
    StageGuard
}

/// Call `f(iteration, relative residual)` on each CG iteration of solves on this thread (None: stop).
pub fn on_iteration(f: Option<Box<dyn Fn(usize, f64)>>) {
    ITER.with(|i| *i.borrow_mut() = f);
}

pub fn solver_iteration(iteration: usize, rel_residual: f64) {
    ITER.with(|i| {
        if let Some(f) = i.borrow().as_ref() {
            f(iteration, rel_residual)
        }
    });
}

pub fn record_solve(s: SolverStats) {
    SOLVES.with(|v| {
        let mut v = v.borrow_mut();
        if v.len() >= 64 {
            v.remove(0);
        }
        v.push(s)
    });
}

pub fn annotate_last_solve(f: impl FnOnce(&mut SolverStats)) {
    SOLVES.with(|v| {
        if let Some(s) = v.borrow_mut().last_mut() {
            f(s)
        }
    });
}

/// The statistics of the solves on this thread since the last call (and forget them).
pub fn take_solves() -> Vec<SolverStats> {
    SOLVES.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

pub fn attempt_failed(_context: &str, _err: &dyn std::fmt::Display) {}
