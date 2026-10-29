//! Port of `crackh3d/fe/assemble.py`: global isotropic-elasticity stiffness
//! for a [`Mesh`] whose blocks are tet10 / wedge15 / hex20 / pyramid13.
//!
//! Two element forms, as in Python: [`element_stiffness`] is the textbook
//! `B^T D B` reference, [`element_stiffness_iso`] contracts the isotropic
//! stiffness tensor with the shape gradients directly (what assembly uses):
//!
//! `Ke[3a+i,3b+j] = lam dNa_i dNb_j + mu dNa_j dNb_i + mu d_ij (dNa . dNb)`.
//!
//! Assembly is a deterministic parallel gather: element matrices are computed
//! in parallel, then every node assembles its own three CSR rows from the
//! elements incident to it (no atomics, no sorting, fixed summation order).

use std::sync::OnceLock;

use rayon::prelude::*;
use thiserror::Error;

use super::shapefun;
use crate::mesh_data::Mesh;
use crate::smallmat::{det_inv_3x3, Singular};

#[derive(Debug, Error)]
pub enum AssembleError {
    #[error("unsupported element type {0:?} for assembly")]
    UnknownType(String),
    #[error("degenerate element Jacobian in block {block:?}, element {elem}")]
    Singular { block: String, elem: usize },
    #[error(transparent)]
    Singular3(#[from] Singular),
}

/// Square sparse matrix in CSR form, columns sorted within each row.
#[derive(Debug, Clone)]
pub struct Csr {
    pub nrows: usize,
    pub ncols: usize,
    pub indptr: Vec<usize>,
    pub indices: Vec<u32>,
    pub data: Vec<f64>,
}

impl Csr {
    pub fn nnz(&self) -> usize {
        self.data.len()
    }

    /// `y = A x`.
    pub fn matvec(&self, x: &[f64]) -> Vec<f64> {
        (0..self.nrows)
            .into_par_iter()
            .map(|r| {
                let (a, b) = (self.indptr[r], self.indptr[r + 1]);
                (a..b).map(|k| self.data[k] * x[self.indices[k] as usize]).sum()
            })
            .collect()
    }

    /// Dense copy (row-major); for tests on small matrices.
    pub fn to_dense(&self) -> Vec<f64> {
        let mut d = vec![0.0; self.nrows * self.ncols];
        for r in 0..self.nrows {
            for k in self.indptr[r]..self.indptr[r + 1] {
                d[r * self.ncols + self.indices[k] as usize] += self.data[k];
            }
        }
        d
    }
}

/// Gauss rule + local shape gradients of one element type, cached.
pub struct ElementRule {
    pub n: usize,
    pub gw: Vec<f64>,
    /// `dn_local[g][node]` = (dN/dxi, dN/deta, dN/dzeta)
    pub dn_local: Vec<Vec<[f64; 3]>>,
}

pub fn element_rule(element_type: &str) -> Result<&'static ElementRule, AssembleError> {
    const NAMES: [&str; 4] = ["tet10", "hex20", "wedge15", "pyramid13"];
    static RULES: OnceLock<Vec<ElementRule>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        NAMES
            .iter()
            .map(|name| {
                let (gp, gw) = shapefun::gauss_points(name).unwrap();
                let dn_local: Vec<Vec<[f64; 3]>> =
                    gp.iter().map(|p| shapefun::eval_shape_and_grad(name, p[0], p[1], p[2]).unwrap().1).collect();
                ElementRule { n: dn_local[0].len(), gw, dn_local }
            })
            .collect()
    });
    NAMES
        .iter()
        .position(|n| *n == element_type)
        .map(|i| &rules[i])
        .ok_or_else(|| AssembleError::UnknownType(element_type.into()))
}

/// `(lam, mu)` Lame parameters.
pub fn lame(e: f64, nu: f64) -> (f64, f64) {
    (e * nu / ((1.0 + nu) * (1.0 - 2.0 * nu)), e / (2.0 * (1.0 + nu)))
}

/// Global shape gradients and `detJ * w` at every Gauss point of one element.
pub fn grads_at_gauss(rule: &ElementRule, x: &[[f64; 3]]) -> Result<Vec<(Vec<[f64; 3]>, f64)>, Singular> {
    let mut out = Vec::with_capacity(rule.gw.len());
    for (g, dnl) in rule.dn_local.iter().enumerate() {
        let mut j = [[0.0; 3]; 3];
        for (a, xa) in x.iter().enumerate() {
            for k in 0..3 {
                for l in 0..3 {
                    j[k][l] += xa[k] * dnl[a][l];
                }
            }
        }
        let (det, jinv) = det_inv_3x3(&j)?;
        let dn: Vec<[f64; 3]> = dnl
            .iter()
            .map(|d| {
                let mut r = [0.0; 3];
                for l in 0..3 {
                    for m in 0..3 {
                        r[m] += d[l] * jinv[l][m];
                    }
                }
                r
            })
            .collect();
        out.push((dn, det * rule.gw[g]));
    }
    Ok(out)
}

/// Voigt `B` (6 x 3n, row-major) for global gradients `dn` (n x 3), order
/// [11,22,33,12,13,23], engineering shear.
pub fn voigt_b(dn: &[[f64; 3]]) -> Vec<f64> {
    let n = dn.len();
    let w = 3 * n;
    let mut b = vec![0.0; 6 * w];
    for (a, d) in dn.iter().enumerate() {
        b[3 * a] = d[0];
        b[w + 3 * a + 1] = d[1];
        b[2 * w + 3 * a + 2] = d[2];
        b[3 * w + 3 * a] = d[1];
        b[3 * w + 3 * a + 1] = d[0];
        b[4 * w + 3 * a] = d[2];
        b[4 * w + 3 * a + 2] = d[0];
        b[5 * w + 3 * a + 1] = d[2];
        b[5 * w + 3 * a + 2] = d[1];
    }
    b
}

/// Reference `(3n)^2` local stiffness (row-major) from `B^T D B`, with `D`
/// the 6x6 Voigt elasticity matrix.
pub fn element_stiffness(element_type: &str, x: &[[f64; 3]], d: &[[f64; 6]; 6]) -> Result<Vec<f64>, AssembleError> {
    let rule = element_rule(element_type)?;
    let w = 3 * rule.n;
    let mut ke = vec![0.0; w * w];
    for (dn, wt) in grads_at_gauss(rule, x)? {
        let b = voigt_b(&dn);
        // DB (6 x w)
        let mut db = vec![0.0; 6 * w];
        for i in 0..6 {
            for k in 0..6 {
                for c in 0..w {
                    db[i * w + c] += d[i][k] * b[k * w + c];
                }
            }
        }
        for r in 0..w {
            for c in 0..w {
                let mut s = 0.0;
                for i in 0..6 {
                    s += b[i * w + r] * db[i * w + c];
                }
                ke[r * w + c] += s * wt;
            }
        }
    }
    Ok(ke)
}

/// Fast isotropic form (see module docs); writes the `(3n)^2` row-major
/// matrix into `out` (overwritten).
pub fn element_stiffness_iso(rule: &ElementRule, x: &[[f64; 3]], lam: f64, mu: f64, out: &mut [f64]) -> Result<(), Singular> {
    let n = rule.n;
    let w = 3 * n;
    out[..w * w].fill(0.0);
    for (dn, wt) in grads_at_gauss(rule, x)? {
        for a in 0..n {
            for b in 0..n {
                let dot = dn[a][0] * dn[b][0] + dn[a][1] * dn[b][1] + dn[a][2] * dn[b][2];
                for i in 0..3 {
                    for j in 0..3 {
                        let mut v = lam * dn[a][i] * dn[b][j] + mu * dn[a][j] * dn[b][i];
                        if i == j {
                            v += mu * dot;
                        }
                        out[(3 * a + i) * w + 3 * b + j] += v * wt;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Global `(3N, 3N)` sparse stiffness for the whole mesh.
pub fn assemble_stiffness(mesh: &Mesh, e: f64, nu: f64) -> Result<Csr, AssembleError> {
    let _stage = crate::progress::stage("assemble");
    let (lam, mu) = lame(e, nu);
    assemble_element_matrices(mesh, |_, rule, xs, out| element_stiffness_iso(rule, xs, lam, mu, out))
}

/// Global `(3N, 3N)` sparse matrix from per-element `(3n)^2` row-major
/// matrices: `elem(g, rule, x, out)` fills element `g`'s (numbered across
/// the blocks in order) from its node coordinates `x`. Same deterministic
/// gather as [`assemble_stiffness`].
pub fn assemble_element_matrices<F>(mesh: &Mesh, elem: F) -> Result<Csr, AssembleError>
where
    F: Fn(usize, &ElementRule, &[[f64; 3]], &mut [f64]) -> Result<(), Singular> + Sync,
{
    let npts = mesh.num_points();
    let ndof = 3 * npts;

    // Flat element table: (block index, row) with offsets into the Ke store.
    struct Elem<'a> {
        conn: &'a [usize],
        rule: &'static ElementRule,
        block: usize,
        local: usize,
        off: usize,
    }
    let mut elems: Vec<Elem> = Vec::with_capacity(mesh.num_elements());
    let mut total = 0usize;
    for (bi, b) in mesh.blocks.iter().enumerate() {
        let rule = element_rule(&b.element_type)?;
        for (li, c) in b.connectivity.iter().enumerate() {
            elems.push(Elem { conn: c, rule, block: bi, local: li, off: total });
            total += (3 * rule.n) * (3 * rule.n);
        }
    }

    // Element matrices, in parallel, into one flat store.
    let mut store = vec![0.0f64; total];
    {
        let mut slices: Vec<&mut [f64]> = Vec::with_capacity(elems.len());
        let mut rest = store.as_mut_slice();
        for el in &elems {
            let (h, t) = rest.split_at_mut(9 * el.rule.n * el.rule.n);
            slices.push(h);
            rest = t;
        }
        let res: Result<(), AssembleError> = elems
            .par_iter()
            .enumerate()
            .zip(slices.into_par_iter())
            .try_for_each_init(Vec::<[f64; 3]>::new, |xs, ((g, el), out)| {
                xs.clear();
                xs.extend(el.conn.iter().map(|&i| mesh.points[i]));
                elem(g, el.rule, xs, out).map_err(|_| AssembleError::Singular {
                    block: mesh.blocks[el.block].name.clone(),
                    elem: el.local,
                })
            });
        res?;
    }

    // node -> incident (element, local node index), CSR.
    let mut cnt = vec![0usize; npts + 1];
    for el in &elems {
        for &v in el.conn {
            cnt[v + 1] += 1;
        }
    }
    for i in 0..npts {
        cnt[i + 1] += cnt[i];
    }
    let mut fill = cnt.clone();
    let mut inc = vec![(0u32, 0u8); cnt[npts]];
    for (g, el) in elems.iter().enumerate() {
        for (a, &v) in el.conn.iter().enumerate() {
            inc[fill[v]] = (g as u32, a as u8);
            fill[v] += 1;
        }
    }

    // Node neighbour lists (sorted, unique, includes the node itself).
    let nbrs: Vec<Vec<u32>> = (0..npts)
        .into_par_iter()
        .map(|v| {
            let mut l: Vec<u32> = inc[cnt[v]..cnt[v + 1]]
                .iter()
                .flat_map(|&(g, _)| elems[g as usize].conn.iter().map(|&u| u as u32))
                .collect();
            l.sort_unstable();
            l.dedup();
            l
        })
        .collect();

    // CSR structure: rows 3v+i each hold 3*nbrs[v].len() entries.
    let mut indptr = vec![0usize; ndof + 1];
    for v in 0..npts {
        for i in 0..3 {
            indptr[3 * v + i + 1] = indptr[3 * v + i] + 3 * nbrs[v].len();
        }
    }
    let nnz = indptr[ndof];
    let mut indices = vec![0u32; nnz];
    let mut data = vec![0.0f64; nnz];

    // Split indices/data into per-node blocks (3 consecutive rows each).
    let mut ind_chunks: Vec<&mut [u32]> = Vec::with_capacity(npts);
    let mut dat_chunks: Vec<&mut [f64]> = Vec::with_capacity(npts);
    {
        let (mut ri, mut rd) = (indices.as_mut_slice(), data.as_mut_slice());
        for l in &nbrs {
            let (hi, ti) = ri.split_at_mut(9 * l.len());
            let (hd, td) = rd.split_at_mut(9 * l.len());
            ind_chunks.push(hi);
            dat_chunks.push(hd);
            ri = ti;
            rd = td;
        }
    }
    ind_chunks.into_par_iter().zip(dat_chunks.into_par_iter()).enumerate().for_each(|(v, (ind, dat))| {
        let nb = &nbrs[v];
        let m = 3 * nb.len();
        for i in 0..3 {
            for (p, &u) in nb.iter().enumerate() {
                for j in 0..3 {
                    ind[i * m + 3 * p + j] = 3 * u + j as u32;
                }
            }
        }
        for &(g, a) in &inc[cnt[v]..cnt[v + 1]] {
            let el = &elems[g as usize];
            let w = 3 * el.rule.n;
            let ke = &store[el.off..el.off + w * w];
            let a = a as usize;
            for (b, &u) in el.conn.iter().enumerate() {
                let p = nb.binary_search(&(u as u32)).unwrap();
                for i in 0..3 {
                    for j in 0..3 {
                        dat[i * m + 3 * p + j] += ke[(3 * a + i) * w + 3 * b + j];
                    }
                }
            }
        }
    });

    Ok(Csr { nrows: ndof, ncols: ndof, indptr, indices, data })
}
