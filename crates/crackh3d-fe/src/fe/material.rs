//! Port of `crackh3d/fe/material.py`: isotropic elasticity in Voigt form.

/// 6x6 D relating Voigt stress [11,22,33,12,13,23] to engineering strain
/// [e11,e22,e33,2e12,2e13,2e23].
pub fn elasticity_matrix_voigt(e: f64, nu: f64) -> [[f64; 6]; 6] {
    let lam = e * nu / ((1.0 + nu) * (1.0 - 2.0 * nu));
    let mu = e / (2.0 * (1.0 + nu));
    let mut d = [[0.0; 6]; 6];
    for i in 0..3 {
        for j in 0..3 {
            d[i][j] = if i == j { lam + 2.0 * mu } else { lam };
        }
        d[i + 3][i + 3] = mu;
    }
    d
}
