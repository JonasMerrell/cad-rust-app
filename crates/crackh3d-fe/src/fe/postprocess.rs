//! Port of `crackh3d/fe/postprocess.py`: element-centroid stress recovery
//! plus plain nodal averaging, for visualization only.

use super::material::elasticity_matrix_voigt;
use super::pyramid13_tables::CENTROID as PYRAMID13_CENTROID;
use super::shapefun;
use crate::mesh_data::Mesh;
use crate::smallmat::{det_inv_3x3, Mat3, Singular};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PostprocessError {
    #[error(transparent)]
    Shape(#[from] shapefun::ShapeError),
    #[error(transparent)]
    Singular(#[from] Singular),
    #[error("element type {0:?} has no centroid entry")]
    UnknownType(String),
}

/// Reference-element centroid in each type's own local (xi, eta, zeta).
fn centroid_local(et: &str) -> Option<[f64; 3]> {
    match et {
        "tet10" => Some([0.25, 0.25, 0.25]),
        "hex20" => Some([0.0, 0.0, 0.0]),
        "wedge15" => Some([1.0 / 3.0, 1.0 / 3.0, 0.0]),
        "pyramid13" => Some(PYRAMID13_CENTROID),
        _ => None,
    }
}

/// Von Mises from a symmetric stress tensor.
pub fn von_mises(s: &Mat3) -> f64 {
    (0.5 * ((s[0][0] - s[1][1]).powi(2)
        + (s[1][1] - s[2][2]).powi(2)
        + (s[2][2] - s[0][0]).powi(2)
        + 6.0 * (s[0][1].powi(2) + s[0][2].powi(2) + s[1][2].powi(2))))
    .sqrt()
}

fn voigt_to_tensor(v: &[f64; 6]) -> Mat3 {
    [[v[0], v[3], v[4]], [v[3], v[1], v[5]], [v[4], v[5], v[2]]]
}

/// `(N,)` von Mises stress at every node: element-centroid stress averaged
/// onto the element's nodes.
pub fn nodal_von_mises_stress(mesh: &Mesh, displacement: &[[f64; 3]], e: f64, nu: f64) -> Result<Vec<f64>, PostprocessError> {
    let d = elasticity_matrix_voigt(e, nu);
    let mut sums = vec![0.0; mesh.num_points()];
    let mut counts = vec![0.0; mesh.num_points()];
    for block in &mesh.blocks {
        let et = block.element_type.as_str();
        let [xi, eta, zeta] = centroid_local(et).ok_or_else(|| PostprocessError::UnknownType(et.into()))?;
        let (_, dn_local) = shapefun::eval_shape_and_grad(et, xi, eta, zeta)?;
        for conn in &block.connectivity {
            let mut j: Mat3 = [[0.0; 3]; 3];
            for (a, &id) in conn.iter().enumerate() {
                for k in 0..3 {
                    for l in 0..3 {
                        j[k][l] += mesh.points[id][k] * dn_local[a][l];
                    }
                }
            }
            let (_, jinv) = det_inv_3x3(&j)?;
            // grad_u[c][d] = sum_a u_a[c] * dN_global[a][d]
            let mut g = [[0.0; 3]; 3];
            for (a, &id) in conn.iter().enumerate() {
                let mut dng = [0.0; 3];
                for l in 0..3 {
                    for m in 0..3 {
                        dng[m] += dn_local[a][l] * jinv[l][m];
                    }
                }
                for c in 0..3 {
                    for m in 0..3 {
                        g[c][m] += displacement[id][c] * dng[m];
                    }
                }
            }
            let eps = |i: usize, k: usize| 0.5 * (g[i][k] + g[k][i]);
            let ev = [eps(0, 0), eps(1, 1), eps(2, 2), 2.0 * eps(0, 1), 2.0 * eps(0, 2), 2.0 * eps(1, 2)];
            let mut sv = [0.0; 6];
            for i in 0..6 {
                for k in 0..6 {
                    sv[i] += d[i][k] * ev[k];
                }
            }
            let vm = von_mises(&voigt_to_tensor(&sv));
            for &id in conn {
                sums[id] += vm;
                counts[id] += 1.0;
            }
        }
    }
    Ok(sums.iter().zip(&counts).map(|(s, &c)| s / if c == 0.0 { 1.0 } else { c }).collect())
}
