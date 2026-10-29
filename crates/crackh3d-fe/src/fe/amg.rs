//! Conjugate gradients preconditioned by smoothed-aggregation algebraic
//! multigrid: a pure-Rust port of what crackh3d's Python solve gets from
//! PETSc (`KSPCG` + `PCGAMG`, `crackh3d/fe/solve.py::_solve_linear`).
//!
//! The Python sets only `ksp cg`, `pc gamg`, `rtol`, `max_it 5000`, the
//! matrix block size 3 and the six rigid-body modes as near-nullspace, so
//! everything else is a PETSc 3.25 default, mirrored here:
//!
//! * **strength graph** -- one vertex per node (its free dofs), an edge
//!   wherever the node blocks of `A` couple. GAMG's default threshold is
//!   `-1` ("keep every entry"), so only the block sparsity pattern matters.
//! * **aggregation** -- greedy maximal independent set over a fixed
//!   pseudo-random vertex order (PETSc's own `rander48` generator and seed,
//!   so the permutation is PETSc's). On the first level (`aggressive_levels
//!   = 1`) it is MIS-1 on the *squared* graph (distance-2), after which
//!   every root steals its distance-1 neighbours (`fixAggregatesWithSquare`);
//!   coarser levels use plain MIS-1. A root's aggregate is itself plus the
//!   not-yet-taken neighbours. Isolated vertices are dropped (zero rows in
//!   `P`), as PETSc does.
//! * **tentative prolongator** -- per aggregate, the near-nullspace rows are
//!   QR-factorised (zero-padded to at least 6 rows, as `formProl0` does):
//!   `Q` goes into `P0`, `R` becomes the coarse near-nullspace. Coarse levels
//!   therefore carry 6 dofs per aggregate.
//! * **prolongator smoothing** -- one damped-Jacobi step
//!   `P = (I - 1.4/lmax D^-1 A) P0`, `lmax` from a 10-step Krylov
//!   (CG/Lanczos) estimate of `D^-1 A` on a noise right-hand side.
//! * **coarse operators** -- Galerkin `P^T A P`; coarsening stops once the
//!   coarse system has at most 50 equations (`coarse_eq_limit`), where it is
//!   solved directly (LU).
//! * **smoother** -- 2 iterations of first-kind Chebyshev with Jacobi, both
//!   pre and post, eigenvalue bounds `[0.1, 1.1] * lmax` (the SA estimate
//!   reused, `use_sa_esteig`).
//! * **cycle** -- one symmetric V-cycle per CG iteration.
//! * **CG convergence** -- PETSc's default for CG is the *preconditioned*
//!   norm: stop when `||M^-1 r_k||_2 <= rtol * ||M^-1 b||_2` (zero initial
//!   guess), divergence at 1e5x the initial norm.
//!
//! Everything is deterministic and independent of the thread count: rows
//! are computed independently, and reductions sum fixed-size chunks in a
//! fixed order. Callers must keep it so: the near-nullspace handed in
//! (`fe::bcs`'s Gram-Schmidt) uses [`det_dot`], since a rayon `sum` splits
//! where work stealing lands and its rounding then changes the whole solve
//! run to run (`tests/determinism.rs`).

use std::time::Instant;

use rayon::prelude::*;

use super::assemble::Csr;

/// Vectors shorter than this are processed sequentially.
const PAR_MIN: usize = 16 * 1024;
/// Chunk length for parallel vector kernels and deterministic reductions.
const CHUNK: usize = 4096;
/// Sparse kernels run in parallel above this many stored entries.
const PAR_MIN_NNZ: usize = 32 * 1024;

/// Rows per parallel task for a sparse kernel over `a` (row results are
/// independent, so this does not affect the numbers): ~16 tasks per thread.
fn row_chunk(a: &Csr) -> usize {
    (a.nrows / (16 * rayon::current_num_threads())).clamp(4, 512)
}

/// GAMG settings. `Default` is PETSc 3.25's `PCGAMG` default.
#[derive(Debug, Clone)]
pub struct AmgOptions {
    /// Levels (from the finest) aggregated on the squared graph.
    pub aggressive_levels: usize,
    /// Jacobi smoothing steps of the tentative prolongator.
    pub prolongator_smooths: usize,
    /// Damping numerator of the prolongator smoother (`1.4 / lmax`).
    pub prolongator_damping: f64,
    /// Chebyshev iterations per pre- and post-smooth.
    pub smoother_its: usize,
    /// Chebyshev bounds as fractions of the estimated `lmax(D^-1 A)`.
    pub cheby_lo: f64,
    pub cheby_hi: f64,
    /// Krylov steps for the `lmax` estimate.
    pub esteig_its: usize,
    /// Stop coarsening once the coarse system has at most this many rows.
    pub coarse_eq_limit: usize,
    /// Maximum number of levels (PETSc: `PETSC_MG_MAXLEVELS` = 30).
    pub max_levels: usize,
}

impl Default for AmgOptions {
    fn default() -> Self {
        AmgOptions {
            aggressive_levels: 1,
            prolongator_smooths: 1,
            prolongator_damping: 1.4,
            smoother_its: 2,
            cheby_lo: 0.1,
            cheby_hi: 1.1,
            esteig_its: 10,
            coarse_eq_limit: 50,
            max_levels: 30,
        }
    }
}

// ================================================================ kernels

#[inline]
fn row_dot(a: &Csr, r: usize, x: &[f64]) -> f64 {
    let (s, e) = (a.indptr[r], a.indptr[r + 1]);
    let idx = &a.indices[s..e];
    let val = &a.data[s..e];
    let mut acc = 0.0;
    for k in 0..idx.len() {
        // SAFETY: column indices are < ncols == x.len() (checked by callers' debug asserts)
        acc += val[k] * unsafe { *x.get_unchecked(idx[k] as usize) };
    }
    acc
}

/// `y = A x` (`beta == 0`), `y += A x` (`beta == 1`) or `y -= A x` (`beta == -1`).
fn spmv_into(a: &Csr, x: &[f64], y: &mut [f64], mode: i8) {
    debug_assert_eq!(x.len(), a.ncols);
    debug_assert_eq!(y.len(), a.nrows);
    let body = |r0: usize, ys: &mut [f64]| {
        for (i, yi) in ys.iter_mut().enumerate() {
            let v = row_dot(a, r0 + i, x);
            match mode {
                0 => *yi = v,
                1 => *yi += v,
                _ => *yi -= v,
            }
        }
    };
    if a.nnz() < PAR_MIN_NNZ {
        body(0, y);
    } else {
        let rc = row_chunk(a);
        y.par_chunks_mut(rc).enumerate().for_each(|(c, ys)| body(c * rc, ys));
    }
}

/// A CSR matrix (values regrouped, so only for products) plus its "inodes": runs of up to 6 consecutive rows with
/// one column pattern (the 3 dofs of a node, the 6 dofs of an aggregate),
/// whose column indices the product reads once -- PETSc's AIJ inode trick.
/// The solve is memory-bandwidth bound, so this saves a quarter of the
/// traffic of a fine-level product.
struct SpMat {
    a: Csr,
    /// group start rows, with `nrows` appended
    gstart: Vec<u32>,
    /// group index boundaries of the parallel tasks
    tasks: Vec<usize>,
}

impl SpMat {
    fn new(a: Csr) -> SpMat {
        let n = a.nrows;
        let mut gstart = vec![0u32];
        let mut r = 0;
        while r < n {
            let s = r;
            let pat = &a.indices[a.indptr[r]..a.indptr[r + 1]];
            r += 1;
            while r < n && r - s < 6 && a.indices[a.indptr[r]..a.indptr[r + 1]] == *pat {
                r += 1;
            }
            gstart.push(r as u32);
        }
        // tasks of ~32k stored entries each
        let mut tasks = vec![0usize];
        let mut acc = 0usize;
        for g in 0..gstart.len() - 1 {
            acc += a.indptr[gstart[g + 1] as usize] - a.indptr[gstart[g] as usize];
            if acc >= 32 * 1024 {
                tasks.push(g + 1);
                acc = 0;
            }
        }
        if *tasks.last().unwrap() != gstart.len() - 1 {
            tasks.push(gstart.len() - 1);
        }
        // interleave each group's values (see `group_kernel`)
        let mut a = a;
        for g in 0..gstart.len() - 1 {
            let (r0, r1) = (gstart[g] as usize, gstart[g + 1] as usize);
            let gs = r1 - r0;
            if gs > 1 {
                let (b, e) = (a.indptr[r0], a.indptr[r1]);
                let len = (e - b) / gs;
                let src = a.data[b..e].to_vec();
                for i in 0..gs {
                    for k in 0..len {
                        a.data[b + k * gs + i] = src[i * len + k];
                    }
                }
            }
        }
        SpMat { a, gstart, tasks }
    }
}

/// One inode group of `G` rows; its values are stored interleaved
/// (`data[base + k*G + i]` = row `i`, `k`-th entry), so the product streams
/// one contiguous block.
#[inline(always)]
fn group_kernel<const G: usize>(a: &Csr, r0: usize, x: &[f64], y: &mut [f64], mode: i8) {
    let base = a.indptr[r0];
    let len = a.indptr[r0 + 1] - base;
    let idx = &a.indices[base..base + len];
    let dat = &a.data[base..base + G * len];
    let mut acc = [0.0f64; G];
    for k in 0..len {
        // SAFETY: k < len, column indices are < ncols == x.len(), dat has G*len entries
        unsafe {
            let xj = *x.get_unchecked(*idx.get_unchecked(k) as usize);
            for i in 0..G {
                acc[i] += *dat.get_unchecked(k * G + i) * xj;
            }
        }
    }
    for i in 0..G {
        match mode {
            0 => y[i] = acc[i],
            1 => y[i] += acc[i],
            _ => y[i] -= acc[i],
        }
    }
}

/// `y = / += / -= A x` (`mode` 0 / 1 / -1) over inode groups.
fn spmv(m: &SpMat, x: &[f64], y: &mut [f64], mode: i8) {
    debug_assert_eq!(x.len(), m.a.ncols);
    debug_assert_eq!(y.len(), m.a.nrows);
    let a = &m.a;
    let run = |g0: usize, g1: usize, ys: &mut [f64]| {
        let base = m.gstart[g0] as usize;
        for g in g0..g1 {
            let (r0, r1) = (m.gstart[g] as usize, m.gstart[g + 1] as usize);
            let yg = &mut ys[r0 - base..r1 - base];
            match r1 - r0 {
                1 => group_kernel::<1>(a, r0, x, yg, mode),
                2 => group_kernel::<2>(a, r0, x, yg, mode),
                3 => group_kernel::<3>(a, r0, x, yg, mode),
                4 => group_kernel::<4>(a, r0, x, yg, mode),
                5 => group_kernel::<5>(a, r0, x, yg, mode),
                _ => group_kernel::<6>(a, r0, x, yg, mode),
            }
        }
    };
    if m.tasks.len() <= 2 {
        run(0, m.gstart.len() - 1, y);
        return;
    }
    let mut slices: Vec<(usize, usize, &mut [f64])> = Vec::with_capacity(m.tasks.len() - 1);
    let mut rest = y;
    for t in 0..m.tasks.len() - 1 {
        let (g0, g1) = (m.tasks[t], m.tasks[t + 1]);
        let rows = (m.gstart[g1] - m.gstart[g0]) as usize;
        let (head, tail) = rest.split_at_mut(rows);
        slices.push((g0, g1, head));
        rest = tail;
    }
    slices.into_par_iter().for_each(|(g0, g1, ys)| run(g0, g1, ys));
}

/// `a . b` summed over fixed [`CHUNK`]-long pieces combined in order: the
/// same bits whatever the thread count or scheduling (unlike a rayon
/// `sum`, whose split points follow work stealing).
pub(crate) fn det_dot(a: &[f64], b: &[f64]) -> f64 {
    dot(a, b)
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    let seq = |x: &[f64], y: &[f64]| x.iter().zip(y).map(|(p, q)| p * q).sum::<f64>();
    if a.len() < PAR_MIN {
        return a.chunks(CHUNK).zip(b.chunks(CHUNK)).map(|(x, y)| seq(x, y)).sum();
    }
    let parts: Vec<f64> = a.par_chunks(CHUNK).zip(b.par_chunks(CHUNK)).map(|(x, y)| seq(x, y)).collect();
    parts.iter().sum()
}

/// Apply `f(i, &mut y[i])` over chunks, in parallel for long vectors.
fn for_each_mut(y: &mut [f64], f: impl Fn(usize, &mut f64) + Sync + Send) {
    if y.len() < PAR_MIN {
        y.iter_mut().enumerate().for_each(|(i, v)| f(i, v));
    } else {
        y.par_chunks_mut(CHUNK).enumerate().for_each(|(c, ys)| {
            let o = c * CHUNK;
            ys.iter_mut().enumerate().for_each(|(i, v)| f(o + i, v));
        });
    }
}

/// Build a CSR matrix from per-row-chunk pieces `(row lengths, indices, values)`.
fn csr_from_chunks(nrows: usize, ncols: usize, chunks: Vec<(Vec<usize>, Vec<u32>, Vec<f64>)>) -> Csr {
    let nnz: usize = chunks.iter().map(|c| c.1.len()).sum();
    let mut indptr = Vec::with_capacity(nrows + 1);
    indptr.push(0usize);
    let mut indices = Vec::with_capacity(nnz);
    let mut data = Vec::with_capacity(nnz);
    for (lens, idx, val) in chunks {
        for l in lens {
            let last = *indptr.last().unwrap();
            indptr.push(last + l);
        }
        indices.extend_from_slice(&idx);
        data.extend_from_slice(&val);
    }
    debug_assert_eq!(indptr.len(), nrows + 1);
    Csr { nrows, ncols, indptr, indices, data }
}

/// Row-parallel sparse product `C = A B`, optionally fused with the Jacobi
/// prolongator smoother: with `smooth = Some((dinv, alpha, P0))`, returns
/// `P0 + alpha * diag(dinv) * A B` (B == P0). Columns sorted per row.
pub(crate) fn spgemm(a: &Csr, b: &Csr, smooth: Option<(&[f64], f64)>) -> Csr {
    assert_eq!(a.ncols, b.nrows);
    let nc = b.ncols;
    let rc = row_chunk(a);
    let nchunks = a.nrows.div_ceil(rc);
    let work = |c: usize, acc: &mut Vec<f64>, mark: &mut Vec<u32>, cols: &mut Vec<u32>| {
        let r0 = c * rc;
        let r1 = (r0 + rc).min(a.nrows);
        let mut lens = Vec::with_capacity(r1 - r0);
        let mut idx = Vec::new();
        let mut val = Vec::new();
        for r in r0..r1 {
            cols.clear();
            let tag = r as u32;
            for p in a.indptr[r]..a.indptr[r + 1] {
                let k = a.indices[p] as usize;
                let av = a.data[p];
                for q in b.indptr[k]..b.indptr[k + 1] {
                    let j = b.indices[q] as usize;
                    if mark[j] != tag {
                        mark[j] = tag;
                        acc[j] = 0.0;
                        cols.push(j as u32);
                    }
                    acc[j] += av * b.data[q];
                }
            }
            if let Some((dinv, alpha)) = smooth {
                let s = alpha * dinv[r];
                for &j in cols.iter() {
                    acc[j as usize] *= s;
                }
                for q in b.indptr[r]..b.indptr[r + 1] {
                    let j = b.indices[q] as usize;
                    if mark[j] != tag {
                        mark[j] = tag;
                        acc[j] = 0.0;
                        cols.push(j as u32);
                    }
                    acc[j] += b.data[q];
                }
            }
            cols.sort_unstable();
            lens.push(cols.len());
            for &j in cols.iter() {
                idx.push(j);
                val.push(acc[j as usize]);
            }
        }
        (lens, idx, val)
    };
    let chunks: Vec<_> = if a.nnz() < PAR_MIN_NNZ {
        let (mut acc, mut mark, mut cols) = (vec![0.0; nc], vec![u32::MAX; nc], Vec::new());
        (0..nchunks).map(|c| work(c, &mut acc, &mut mark, &mut cols)).collect()
    } else {
        (0..nchunks)
            .into_par_iter()
            .map_init(|| (vec![0.0; nc], vec![u32::MAX; nc], Vec::new()), |(acc, mark, cols), c| work(c, acc, mark, cols))
            .collect()
    };
    csr_from_chunks(a.nrows, nc, chunks)
}

/// `A^T` (columns sorted per row).
pub(crate) fn transpose(a: &Csr) -> Csr {
    let mut cnt = vec![0usize; a.ncols + 1];
    for &j in &a.indices {
        cnt[j as usize + 1] += 1;
    }
    for i in 0..a.ncols {
        cnt[i + 1] += cnt[i];
    }
    let indptr = cnt.clone();
    let mut pos = cnt;
    let mut indices = vec![0u32; a.nnz()];
    let mut data = vec![0.0; a.nnz()];
    for r in 0..a.nrows {
        for p in a.indptr[r]..a.indptr[r + 1] {
            let j = a.indices[p] as usize;
            indices[pos[j]] = r as u32;
            data[pos[j]] = a.data[p];
            pos[j] += 1;
        }
    }
    Csr { nrows: a.ncols, ncols: a.nrows, indptr, indices, data }
}

fn diagonal(a: &Csr) -> Vec<f64> {
    (0..a.nrows)
        .map(|r| {
            let (s, e) = (a.indptr[r], a.indptr[r + 1]);
            match a.indices[s..e].binary_search(&(r as u32)) {
                Ok(k) => a.data[s + k],
                Err(_) => 0.0,
            }
        })
        .collect()
}

// ======================================================= graph + aggregation

/// PETSc's `rander48` generator (`PetscRandomCreate` default type, seed
/// `0x12345678` on rank 0), so the MIS visiting order is PETSc's.
struct Rander48 {
    seed: [u16; 3],
}

impl Rander48 {
    fn petsc_default() -> Self {
        let s: u64 = 0x1234_5678;
        Rander48 { seed: [0x330e, s as u16, (s >> 16) as u16] }
    }
    fn next(&mut self) -> f64 {
        let (m0, m1, m2, add) = (0xe66du64, 0xdeecu64, 0x0005u64, 0x000bu64);
        let s = [self.seed[0] as u64, self.seed[1] as u64, self.seed[2] as u64];
        let mut accu = m0 * s[0] + add;
        let t0 = accu as u16;
        accu >>= 16;
        accu += m0 * s[1] + m1 * s[0];
        let t1 = accu as u16;
        accu >>= 16;
        accu += m0 * s[2] + m1 * s[1] + m2 * s[0];
        self.seed = [t0, t1, accu as u16];
        (self.seed[0] as f64) * 2f64.powi(-48) + (self.seed[1] as f64) * 2f64.powi(-32) + (self.seed[2] as f64) * 2f64.powi(-16)
    }
}

/// PETSc's MIS visiting order (`PCGAMGCoarsen_AGG`'s swap shuffle).
fn petsc_permutation(n: usize) -> Vec<usize> {
    let mut perm: Vec<usize> = (0..n).collect();
    let mut set = vec![false; n];
    let mut rng = Rander48::petsc_default();
    for i in 0..n {
        let h = rng.next();
        let j = ((h * n as f64) as usize) % n.max(1);
        if !set[j] && j != i {
            perm.swap(i, j);
            set[j] = true;
        }
    }
    perm
}

/// Node graph (no self loops): `ptr`/`adj`, adjacency sorted.
struct Graph {
    ptr: Vec<usize>,
    adj: Vec<u32>,
}

impl Graph {
    fn nbrs(&self, v: usize) -> &[u32] {
        &self.adj[self.ptr[v]..self.ptr[v + 1]]
    }
    fn n(&self) -> usize {
        self.ptr.len() - 1
    }
}

/// Block-sparsity graph of `a` with dof groups `gptr` (group g = dofs
/// `gptr[g]..gptr[g+1]`).
fn strength_graph(a: &Csr, gptr: &[usize]) -> Graph {
    let ng = gptr.len() - 1;
    let mut gid = vec![0u32; a.nrows];
    for g in 0..ng {
        for d in gptr[g]..gptr[g + 1] {
            gid[d] = g as u32;
        }
    }
    let rows: Vec<Vec<u32>> = (0..ng)
        .into_par_iter()
        .with_min_len(256)
        .map(|g| {
            let mut v: Vec<u32> = Vec::new();
            for r in gptr[g]..gptr[g + 1] {
                for p in a.indptr[r]..a.indptr[r + 1] {
                    let h = gid[a.indices[p] as usize];
                    if h as usize != g {
                        v.push(h);
                    }
                }
            }
            v.sort_unstable();
            v.dedup();
            v
        })
        .collect();
    let mut ptr = Vec::with_capacity(ng + 1);
    ptr.push(0);
    let mut adj = Vec::with_capacity(rows.iter().map(Vec::len).sum());
    for r in rows {
        adj.extend_from_slice(&r);
        ptr.push(adj.len());
    }
    Graph { ptr, adj }
}

const NONE: u32 = u32::MAX;

/// Greedy MIS aggregation (PETSc `MATCOARSENMIS`, strict aggregates).
/// `square`: MIS-1 on the squared graph followed by
/// `fixAggregatesWithSquare`. Returns (aggregate of each node or NONE,
/// number of aggregates).
fn aggregate(g: &Graph, square: bool) -> (Vec<u32>, usize) {
    let n = g.n();
    let perm = petsc_permutation(n);
    let mut parent = vec![NONE; n];
    let mut removed = vec![false; n];
    let mut root_of_agg: Vec<usize> = Vec::new();
    for &v in &perm {
        if parent[v] != NONE || removed[v] {
            continue;
        }
        if g.nbrs(v).is_empty() {
            removed[v] = true; // singleton: no aggregate (PETSc drops it)
            continue;
        }
        let agg = root_of_agg.len() as u32;
        root_of_agg.push(v);
        parent[v] = agg;
        for &u in g.nbrs(v) {
            let u = u as usize;
            if parent[u] == NONE && !removed[u] {
                parent[u] = agg;
            }
        }
        if square {
            for &u in g.nbrs(v) {
                for &w in g.nbrs(u as usize) {
                    let w = w as usize;
                    if parent[w] == NONE && !removed[w] {
                        parent[w] = agg;
                    }
                }
            }
        }
    }
    if square {
        // fixAggregatesWithSquare: roots, in node order, steal their
        // distance-1 neighbours from other aggregates.
        let mut is_root = vec![NONE; n];
        for (a, &r) in root_of_agg.iter().enumerate() {
            is_root[r] = a as u32;
        }
        for v in 0..n {
            let a = is_root[v];
            if a == NONE {
                continue;
            }
            for &u in g.nbrs(v) {
                let u = u as usize;
                if is_root[u] == NONE && parent[u] != NONE {
                    parent[u] = a;
                }
            }
        }
    }
    (parent, root_of_agg.len())
}

// ================================================== tentative prolongator

/// `P0` (`n x 6*naggs`) and the coarse near-nullspace (`6*naggs x 6`,
/// row-major) from per-aggregate QR of `b` (`n x nv`, row-major).
fn tentative_prolongator(n: usize, nv: usize, b: &[f64], gptr: &[usize], parent: &[u32], naggs: usize) -> (Csr, Vec<f64>) {
    // nodes of each aggregate, in node order
    let mut cnt = vec![0usize; naggs + 1];
    for &p in parent {
        if p != NONE {
            cnt[p as usize + 1] += 1;
        }
    }
    for i in 0..naggs {
        cnt[i + 1] += cnt[i];
    }
    let mut pos = cnt.clone();
    let mut nodes = vec![0usize; cnt[naggs]];
    for (v, &p) in parent.iter().enumerate() {
        if p != NONE {
            nodes[pos[p as usize]] = v;
            pos[p as usize] += 1;
        }
    }
    // QR per aggregate
    let blocks: Vec<(Vec<usize>, nalgebra::DMatrix<f64>, nalgebra::DMatrix<f64>)> = (0..naggs)
        .into_par_iter()
        .with_min_len(16)
        .map(|a| {
            let dofs: Vec<usize> = nodes[cnt[a]..cnt[a + 1]].iter().flat_map(|&v| gptr[v]..gptr[v + 1]).collect();
            let m = dofs.len();
            let md = m.max(nv);
            let mat = nalgebra::DMatrix::from_fn(md, nv, |i, j| if i < m { b[dofs[i] * nv + j] } else { 0.0 });
            let qr = mat.qr();
            let q = qr.q(); // md x nv
            let r = qr.r(); // nv x nv
            (dofs, q, r)
        })
        .collect();
    let mut row_agg = vec![(NONE, 0usize); n];
    for (a, (dofs, _, _)) in blocks.iter().enumerate() {
        for (li, &d) in dofs.iter().enumerate() {
            row_agg[d] = (a as u32, li);
        }
    }
    let mut indptr = Vec::with_capacity(n + 1);
    indptr.push(0usize);
    let mut indices = Vec::with_capacity(n * nv);
    let mut data = Vec::with_capacity(n * nv);
    for &(a, li) in &row_agg {
        if a != NONE {
            let q = &blocks[a as usize].1;
            for j in 0..nv {
                indices.push((a as usize * nv + j) as u32);
                data.push(q[(li, j)]);
            }
        }
        indptr.push(indices.len());
    }
    let mut bc = vec![0.0; naggs * nv * nv];
    for (a, (_, _, r)) in blocks.iter().enumerate() {
        for i in 0..nv {
            for j in 0..nv {
                bc[(a * nv + i) * nv + j] = r[(i, j)];
            }
        }
    }
    (Csr { nrows: n, ncols: naggs * nv, indptr, indices, data }, bc)
}

// ============================================================ eigen estimate

/// Largest eigenvalue of `D^-1 A` from `its` steps of Jacobi-PCG (Lanczos
/// coefficients) on a deterministic noise right-hand side.
fn estimate_lmax(a: &Csr, dinv: &[f64], its: usize) -> f64 {
    let n = a.nrows;
    // deterministic "noise" (splitmix64)
    let mut r: Vec<f64> = (0..n)
        .map(|i| {
            let mut z = (i as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            (z >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        })
        .collect();
    let mut z: Vec<f64> = r.iter().zip(dinv).map(|(x, d)| x * d).collect();
    let mut p = z.clone();
    let mut q = vec![0.0; n];
    let mut rz = dot(&r, &z);
    let (mut alphas, mut betas) = (Vec::new(), Vec::new());
    for _ in 0..its.min(n) {
        spmv_into(a, &p, &mut q, 0);
        let pq = dot(&p, &q);
        if pq <= 0.0 || !pq.is_finite() {
            break;
        }
        let alpha = rz / pq;
        alphas.push(alpha);
        for_each_mut(&mut r, |i, v| *v -= alpha * q[i]);
        for_each_mut(&mut z, |i, v| *v = r[i] * dinv[i]);
        let rz_new = dot(&r, &z);
        let beta = rz_new / rz;
        betas.push(beta);
        rz = rz_new;
        if rz_new.abs() < 1e-300 {
            break;
        }
        for_each_mut(&mut p, |i, v| *v = z[i] + beta * *v);
    }
    let k = alphas.len();
    if k == 0 {
        return 1.0;
    }
    let mut t = nalgebra::DMatrix::<f64>::zeros(k, k);
    for i in 0..k {
        t[(i, i)] = 1.0 / alphas[i] + if i > 0 { betas[i - 1] / alphas[i - 1] } else { 0.0 };
        if i + 1 < k {
            let o = betas[i].sqrt() / alphas[i];
            t[(i, i + 1)] = o;
            t[(i + 1, i)] = o;
        }
    }
    t.symmetric_eigenvalues().iter().cloned().fold(f64::MIN, f64::max)
}

// ============================================================ coarse solve

/// Dense LU with partial pivoting (zero pivots shifted, as PETSc's
/// `MAT_SHIFT_INBLOCKS` would).
struct DenseLu {
    n: usize,
    lu: Vec<f64>,
    piv: Vec<usize>,
}

impl DenseLu {
    fn new(a: &Csr) -> Self {
        let n = a.nrows;
        let mut lu = vec![0.0; n * n];
        for r in 0..n {
            for p in a.indptr[r]..a.indptr[r + 1] {
                lu[r * n + a.indices[p] as usize] += a.data[p];
            }
        }
        let scale = lu.iter().fold(0.0f64, |m, x| m.max(x.abs())).max(1e-300);
        let mut piv = vec![0; n];
        for k in 0..n {
            let mut pr = k;
            for i in k + 1..n {
                if lu[i * n + k].abs() > lu[pr * n + k].abs() {
                    pr = i;
                }
            }
            piv[k] = pr;
            if pr != k {
                for j in 0..n {
                    lu.swap(k * n + j, pr * n + j);
                }
            }
            if lu[k * n + k].abs() < 1e-14 * scale {
                lu[k * n + k] = 1e-14 * scale;
            }
            let d = lu[k * n + k];
            for i in k + 1..n {
                let l = lu[i * n + k] / d;
                lu[i * n + k] = l;
                if l != 0.0 {
                    for j in k + 1..n {
                        lu[i * n + j] -= l * lu[k * n + j];
                    }
                }
            }
        }
        DenseLu { n, lu, piv }
    }
    fn solve(&self, b: &[f64], x: &mut [f64]) {
        let n = self.n;
        x.copy_from_slice(b);
        for k in 0..n {
            x.swap(k, self.piv[k]);
        }
        for i in 0..n {
            let mut s = x[i];
            for j in 0..i {
                s -= self.lu[i * n + j] * x[j];
            }
            x[i] = s;
        }
        for i in (0..n).rev() {
            let mut s = x[i];
            for j in i + 1..n {
                s -= self.lu[i * n + j] * x[j];
            }
            x[i] = s / self.lu[i * n + i];
        }
    }
}

enum Coarse {
    Dense(DenseLu),
    Sparse(faer::sparse::linalg::solvers::Lu<u32, f64>),
}

impl Coarse {
    fn new(a: &Csr) -> Result<Self, String> {
        if a.nrows <= 4000 {
            return Ok(Coarse::Dense(DenseLu::new(a)));
        }
        // CSR of a symmetric matrix == CSC
        let col_ptr: Vec<u32> = a.indptr.iter().map(|&x| x as u32).collect();
        let sym = faer::sparse::SymbolicSparseColMatRef::<u32>::new_checked(a.nrows, a.ncols, &col_ptr, None, &a.indices);
        let m = faer::sparse::SparseColMatRef::<u32, f64>::new(sym, &a.data);
        m.sp_lu().map(Coarse::Sparse).map_err(|e| format!("coarse LU: {e:?}"))
    }
    fn solve(&self, b: &[f64], x: &mut [f64]) {
        match self {
            Coarse::Dense(lu) => lu.solve(b, x),
            Coarse::Sparse(lu) => {
                use faer::linalg::solvers::Solve;
                let mut m = faer::Mat::<f64>::from_fn(b.len(), 1, |i, _| b[i]);
                lu.solve_in_place(m.as_mut());
                for (i, v) in x.iter_mut().enumerate() {
                    *v = m[(i, 0)];
                }
            }
        }
    }
}

// ================================================================ hierarchy

struct Level {
    a: SpMat,
    dinv: Vec<f64>,
    lmax: f64,
    p: SpMat,
    r: SpMat,
}

/// Per-level scratch vectors.
struct Work {
    b: Vec<f64>,
    x: Vec<f64>,
    r: Vec<f64>,
    d: Vec<f64>,
}

impl Work {
    fn new(n: usize) -> Self {
        Work { b: vec![0.0; n], x: vec![0.0; n], r: vec![0.0; n], d: vec![0.0; n] }
    }
}

/// Setup statistics of one level.
#[derive(Debug, Clone)]
pub struct LevelInfo {
    pub rows: usize,
    pub nnz: usize,
    pub lmax: f64,
}

/// A smoothed-aggregation multigrid hierarchy (the preconditioner).
pub struct Amg {
    levels: Vec<Level>,
    coarse_a: SpMat,
    coarse: Coarse,
    opts: AmgOptions,
    /// Wall seconds of each setup phase.
    pub setup_times: Vec<(&'static str, f64)>,
}

impl Amg {
    /// Build the hierarchy for SPD `a` with near-nullspace `nullspace`
    /// (`a.nrows x nv`, row-major) and dof groups (nodes) `gptr`.
    pub fn new(a: Csr, nullspace: Vec<f64>, nv: usize, gptr: Vec<usize>, opts: &AmgOptions) -> Result<Amg, String> {
        let mut times: Vec<(&'static str, f64)> = Vec::new();
        let mut add = |k: &'static str, t: Instant| {
            let s = t.elapsed().as_secs_f64();
            match times.iter_mut().find(|(n, _)| *n == k) {
                Some(e) => e.1 += s,
                None => times.push((k, s)),
            }
        };
        let mut levels = Vec::new();
        let mut a = a;
        let mut b = nullspace;
        let mut gptr = gptr;
        let mut nv_cur = nv;
        loop {
            let lvl = levels.len();
            if lvl + 1 >= opts.max_levels {
                break;
            }
            let t = Instant::now();
            let g = strength_graph(&a, &gptr);
            add("graph", t);
            let t = Instant::now();
            let (parent, naggs) = aggregate(&g, lvl < opts.aggressive_levels);
            add("aggregate", t);
            if naggs == 0 {
                break;
            }
            let t = Instant::now();
            let (p0, bc) = tentative_prolongator(a.nrows, nv_cur, &b, &gptr, &parent, naggs);
            add("tentative_P", t);
            let t = Instant::now();
            let dinv: Vec<f64> = diagonal(&a).iter().map(|&d| if d.abs() > 0.0 { 1.0 / d } else { 1.0 }).collect();
            let lmax = estimate_lmax(&a, &dinv, opts.esteig_its);
            add("eig_estimate", t);
            let t = Instant::now();
            let mut p = p0;
            for _ in 0..opts.prolongator_smooths {
                p = spgemm(&a, &p, Some((&dinv, -opts.prolongator_damping / lmax)));
            }
            add("smooth_P", t);
            let t = Instant::now();
            let r = transpose(&p);
            add("transpose", t);
            let t = Instant::now();
            let ap = spgemm(&a, &p, None);
            let ac = spgemm(&r, &ap, None);
            drop(ap);
            add("RAP", t);
            let nc = ac.nrows;
            let t = Instant::now();
            levels.push(Level { a: SpMat::new(a), dinv, lmax, p: SpMat::new(p), r: SpMat::new(r) });
            add("inodes", t);
            a = ac;
            b = bc;
            gptr = (0..=naggs).map(|i| i * nv).collect();
            nv_cur = nv;
            if nc <= opts.coarse_eq_limit || naggs < 2 {
                break;
            }
        }
        let t = Instant::now();
        let coarse = Coarse::new(&a)?;
        add("coarse_factor", t);
        Ok(Amg { levels, coarse_a: SpMat::new(a), coarse, opts: opts.clone(), setup_times: times })
    }

    /// Rows / nnz / lmax per level (finest first, coarse direct level last).
    pub fn level_info(&self) -> Vec<LevelInfo> {
        let mut v: Vec<LevelInfo> = self.levels.iter().map(|l| LevelInfo { rows: l.a.a.nrows, nnz: l.a.a.nnz(), lmax: l.lmax }).collect();
        v.push(LevelInfo { rows: self.coarse_a.a.nrows, nnz: self.coarse_a.a.nnz(), lmax: f64::NAN });
        v
    }

    /// Operator complexity: sum of nnz over levels / nnz of the finest.
    pub fn operator_complexity(&self) -> f64 {
        let i = self.level_info();
        i.iter().map(|l| l.nnz as f64).sum::<f64>() / i[0].nnz as f64
    }

    /// The finest-level operator.
    fn fine(&self) -> &SpMat {
        self.levels.first().map_or(&self.coarse_a, |l| &l.a)
    }

    fn work(&self) -> Vec<Work> {
        let mut w: Vec<Work> = self.levels.iter().map(|l| Work::new(l.a.a.nrows)).collect();
        w.push(Work::new(self.coarse_a.a.nrows));
        w
    }

    /// Chebyshev(-Jacobi) smoothing of `A x = b` on level `l`; `zero`: x == 0 on entry.
    /// On exit `w.r` holds `b - A x` when `want_residual`.
    fn chebyshev(&self, l: usize, w: &mut Work, zero: bool, want_residual: bool) {
        let lv = &self.levels[l];
        let (a, dinv) = (&lv.a, &lv.dinv);
        let mx = self.opts.cheby_hi * lv.lmax;
        let mn = self.opts.cheby_lo * lv.lmax;
        let theta = 0.5 * (mx + mn);
        let delta = 0.5 * (mx - mn);
        let sigma = theta / delta;
        let mut rho = 1.0 / sigma;
        let Work { b, x, r, d } = w;
        if zero {
            r.copy_from_slice(b);
        } else {
            r.copy_from_slice(b);
            spmv(a, x, r, -1);
        }
        let inv_theta = 1.0 / theta;
        for_each_mut(d, |i, v| *v = r[i] * dinv[i] * inv_theta);
        let its = self.opts.smoother_its;
        for k in 0..its {
            {
                let d: &[f64] = d;
                for_each_mut(x, |i, v| *v += d[i]);
            }
            if k + 1 == its && !want_residual {
                break;
            }
            spmv(a, d, r, -1); // r = b - A x
            if k + 1 == its {
                break;
            }
            let rho_new = 1.0 / (2.0 * sigma - rho);
            let (c1, c2) = (rho_new * rho, 2.0 * rho_new / delta);
            {
                let r: &[f64] = r;
                for_each_mut(d, |i, v| *v = c1 * *v + c2 * dinv[i] * r[i]);
            }
            rho = rho_new;
        }
    }

    /// One V-cycle: `z = M^-1 r` (starting from zero).
    fn vcycle(&self, rhs: &[f64], z: &mut [f64], ws: &mut [Work]) {
        let nl = self.levels.len();
        ws[0].b.copy_from_slice(rhs);
        for l in 0..nl {
            ws[l].x.iter_mut().for_each(|v| *v = 0.0);
            self.chebyshev(l, &mut ws[l], true, true);
            let (lo, hi) = ws.split_at_mut(l + 1);
            spmv(&self.levels[l].r, &lo[l].r, &mut hi[0].b, 0);
        }
        {
            let w = &mut ws[nl];
            self.coarse.solve(&w.b, &mut w.x);
        }
        for l in (0..nl).rev() {
            let (lo, hi) = ws.split_at_mut(l + 1);
            spmv(&self.levels[l].p, &hi[0].x, &mut lo[l].x, 1);
            self.chebyshev(l, &mut lo[l], false, false);
        }
        z.copy_from_slice(&ws[0].x);
    }

    /// Apply the preconditioner once (allocates scratch; for tests).
    pub fn apply(&self, r: &[f64], z: &mut [f64]) {
        let mut ws = self.work();
        self.vcycle(r, z, &mut ws);
    }
}

// ===================================================================== CG

/// Result of a CG+AMG solve.
#[derive(Debug, Clone)]
pub struct CgInfo {
    pub converged: bool,
    pub iterations: usize,
    /// `||M^-1 r_k|| / ||M^-1 b||` at exit (PETSc's convergence measure).
    pub rel_precond_residual: f64,
    /// `||b - A x|| / ||b||` at exit.
    pub rel_true_residual: f64,
    pub setup_s: f64,
    pub solve_s: f64,
    pub levels: Vec<LevelInfo>,
    pub operator_complexity: f64,
    pub setup_times: Vec<(&'static str, f64)>,
}

/// Preconditioned CG with PETSc's defaults: zero initial guess, the
/// preconditioned residual norm, `rtol` relative to `||M^-1 b||`,
/// divergence at `1e5 x`.
pub fn pcg(amg: &Amg, b: &[f64], rtol: f64, max_it: usize) -> (Vec<f64>, usize, f64, bool) {
    let a = amg.fine();
    let n = a.a.nrows;
    let mut x = vec![0.0; n];
    let mut r = b.to_vec();
    let mut z = vec![0.0; n];
    let mut ws = amg.work();
    amg.vcycle(&r, &mut z, &mut ws);
    let rnorm0 = dot(&z, &z).sqrt();
    if rnorm0 == 0.0 {
        return (x, 0, 0.0, true);
    }
    let mut p = z.clone();
    let mut q = vec![0.0; n];
    let mut rz = dot(&r, &z);
    let mut rnorm = rnorm0;
    for it in 0..max_it {
        spmv(a, &p, &mut q, 0);
        let pq = dot(&p, &q);
        if pq <= 0.0 || !pq.is_finite() {
            return (x, it, rnorm / rnorm0, false);
        }
        let alpha = rz / pq;
        {
            let p: &[f64] = &p;
            let q: &[f64] = &q;
            for_each_mut(&mut x, |i, v| *v += alpha * p[i]);
            for_each_mut(&mut r, |i, v| *v -= alpha * q[i]);
        }
        amg.vcycle(&r, &mut z, &mut ws);
        rnorm = dot(&z, &z).sqrt();
        crate::progress::solver_iteration(it + 1, rnorm / rnorm0);
        if rnorm <= rtol * rnorm0 {
            return (x, it + 1, rnorm / rnorm0, true);
        }
        if rnorm > 1e5 * rnorm0 || !rnorm.is_finite() {
            return (x, it + 1, rnorm / rnorm0, false);
        }
        let rz_new = dot(&r, &z);
        if rz_new < 0.0 {
            return (x, it + 1, rnorm / rnorm0, false); // indefinite preconditioner
        }
        let beta = rz_new / rz;
        rz = rz_new;
        {
            let z: &[f64] = &z;
            for_each_mut(&mut p, |i, v| *v = z[i] + beta * *v);
        }
    }
    (x, max_it, rnorm / rnorm0, false)
}

/// The six rigid-body modes on the free dofs, Gram-Schmidt orthonormalised
/// (`solve.py::_rigid_body_modes`). Returns `n x 6`, row-major.
pub fn rigid_body_modes(free_dofs: &[usize], points: &[[f64; 3]]) -> Vec<f64> {
    let n = free_dofs.len();
    let mut m = vec![vec![0.0; n]; 6];
    for (i, &d) in free_dofs.iter().enumerate() {
        let (node, c) = (d / 3, d % 3);
        let x = points[node];
        m[c][i] = 1.0;
        // rotation about axis k: u = e_k x r
        match c {
            0 => {
                m[4][i] = x[2];
                m[5][i] = -x[1];
            }
            1 => {
                m[3][i] = -x[2];
                m[5][i] = x[0];
            }
            _ => {
                m[3][i] = x[1];
                m[4][i] = -x[0];
            }
        }
    }
    for i in 0..6 {
        for j in 0..i {
            let (head, tail) = m.split_at_mut(i);
            let c = dot(&tail[0], &head[j]);
            let mj = &head[j];
            for_each_mut(&mut tail[0], |k, v| *v -= c * mj[k]);
        }
        let nrm = dot(&m[i], &m[i]).sqrt();
        if nrm > 1e-30 {
            for_each_mut(&mut m[i], |_, v| *v /= nrm);
        }
    }
    let mut out = vec![0.0; n * 6];
    for i in 0..n {
        for j in 0..6 {
            out[i * 6 + j] = m[j][i];
        }
    }
    out
}

/// Node groups of the free dofs (sorted): dofs of the same mesh node are
/// one strength-graph vertex (PETSc's block size 3 when every node keeps
/// all three components).
pub fn node_groups(free_dofs: &[usize]) -> Vec<usize> {
    let mut g = vec![0usize];
    for i in 1..free_dofs.len() {
        if free_dofs[i] / 3 != free_dofs[i - 1] / 3 {
            g.push(i);
        }
    }
    if !free_dofs.is_empty() {
        g.push(free_dofs.len());
    }
    g
}

/// Solve the free-dof system `a x = b` (full symmetric CSR) by CG + SA-AMG
/// with the rigid-body near-nullspace. `max_it` 5000 as the Python sets.
pub fn solve_cg_amg(
    a: &Csr, b: &[f64], free_dofs: &[usize], points: &[[f64; 3]], rtol: f64, opts: &AmgOptions,
) -> Result<(Vec<f64>, CgInfo), String> {
    let ns = rigid_body_modes(free_dofs, points);
    solve_cg_amg_with_nullspace(a, b, ns, 6, node_groups(free_dofs), rtol, opts)
}

/// [`solve_cg_amg`] with an explicit near-nullspace (`a.nrows x nv`,
/// row-major) and dof groups `gptr` (as [`Amg::new`] takes them): for
/// systems whose unknowns are not plain node components (`fe::bcs`'
/// master dofs).
pub fn solve_cg_amg_with_nullspace(
    a: &Csr, b: &[f64], ns: Vec<f64>, nv: usize, gptr: Vec<usize>, rtol: f64, opts: &AmgOptions,
) -> Result<(Vec<f64>, CgInfo), String> {
    let t = Instant::now();
    let stage = crate::progress::stage("solver_setup");
    let amg = Amg::new(a.clone(), ns, nv, gptr, opts)?;
    drop(stage);
    let setup_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let stage = crate::progress::stage("cg_iterations");
    let (x, iterations, rel, converged) = pcg(&amg, b, rtol, 5000);
    drop(stage);
    let solve_s = t.elapsed().as_secs_f64();
    let mut ax = vec![0.0; a.nrows];
    spmv(amg.fine(), &x, &mut ax, 0);
    let rr: f64 = ax.iter().zip(b).map(|(p, q)| (p - q) * (p - q)).sum::<f64>().sqrt();
    let bn = dot(b, b).sqrt();
    let info = CgInfo {
        converged,
        iterations,
        rel_precond_residual: rel,
        rel_true_residual: if bn > 0.0 { rr / bn } else { 0.0 },
        setup_s,
        solve_s,
        levels: amg.level_info(),
        operator_complexity: amg.operator_complexity(),
        setup_times: amg.setup_times.clone(),
    };
    crate::progress::record_solve(crate::eval_stats::SolverStats {
        kind: "cg_amg".into(),
        n_dofs: a.nrows,
        nnz: a.data.len(),
        iterations: Some(iterations),
        rel_residual: info.rel_true_residual,
        rel_precond_residual: Some(rel),
        converged,
        setup_s,
        solve_s,
        levels: Some(info.levels.len()),
        operator_complexity: Some(info.operator_complexity),
        fallback: None,
    });
    if super::reuse::recording() {
        super::reuse::stash_amg(amg);
    }
    if !converged {
        return Err(format!(
            "CG+AMG did not converge ({iterations} iterations, relative preconditioned residual {rel:.3e})"
        ));
    }
    Ok((x, info))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rander48_matches_petsc_formula() {
        // first value of drand48-style generator with PETSc's default seed is in [0,1)
        let mut r = Rander48::petsc_default();
        for _ in 0..1000 {
            let v = r.next();
            assert!((0.0..1.0).contains(&v));
        }
        let p = petsc_permutation(1000);
        let mut s = p.clone();
        s.sort();
        assert_eq!(s, (0..1000).collect::<Vec<_>>());
    }

    #[test]
    fn transpose_and_spgemm() {
        // 3x3 tridiagonal times identity-ish
        let a = Csr { nrows: 3, ncols: 3, indptr: vec![0, 2, 5, 7], indices: vec![0, 1, 0, 1, 2, 1, 2], data: vec![2., -1., -1., 2., -1., -1., 2.] };
        let at = transpose(&a);
        assert_eq!(at.to_dense(), a.to_dense());
        let aa = spgemm(&a, &a, None);
        let d = a.to_dense();
        let mut want = vec![0.0; 9];
        for i in 0..3 {
            for j in 0..3 {
                for k in 0..3 {
                    want[i * 3 + j] += d[i * 3 + k] * d[k * 3 + j];
                }
            }
        }
        assert_eq!(aa.to_dense(), want);
    }
}
