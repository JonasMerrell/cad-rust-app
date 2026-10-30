//! Port of `crackh3d/sif/quadrature.py`: Gauss rules for wedge15 / hex20 in
//! the project's local coordinates `(xi, eta, zeta)`.
//!
//! hex20 uses the 3x3x3 Gauss-Legendre product rule (27 points, `xi` slowest
//! index). wedge15 uses a Duffy-collapsed 3x3 square rule on the triangle
//! `(xi, eta)` (9 points) times 3 Gauss points along `zeta` (27 points, `zeta`
//! slowest index), exactly the point/weight order of the Python.

/// Element types that have a rule (the Python dispatches on a string).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementType {
    Wedge15,
    Hex20,
}

impl ElementType {
    /// Parse the Python names `"wedge15"` / `"hex20"`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "wedge15" => Some(Self::Wedge15),
            "hex20" => Some(Self::Hex20),
            _ => None,
        }
    }
}

fn gl3_pts() -> [f64; 3] {
    let s = (3.0f64 / 5.0).sqrt();
    [-s, 0.0, s]
}
const GL3_WTS: [f64; 3] = [5.0 / 9.0, 8.0 / 9.0, 5.0 / 9.0];

/// Port of `_hex20_rule`.
fn hex20_rule() -> (Vec<[f64; 3]>, Vec<f64>) {
    let p = gl3_pts();
    let mut points = Vec::with_capacity(27);
    let mut weights = Vec::with_capacity(27);
    for i in 0..3 {
        for j in 0..3 {
            for k in 0..3 {
                points.push([p[i], p[j], p[k]]);
                weights.push(GL3_WTS[i] * GL3_WTS[j] * GL3_WTS[k]);
            }
        }
    }
    (points, weights)
}

/// Port of `_wedge15_rule`.
fn wedge15_rule() -> (Vec<[f64; 3]>, Vec<f64>) {
    let p = gl3_pts();
    let mut tri_pts = Vec::with_capacity(9);
    let mut tri_wts = Vec::with_capacity(9);
    for i in 0..3 {
        for j in 0..3 {
            let (u, v) = (p[i], p[j]);
            tri_pts.push([(1.0 + u) * (1.0 - v) / 4.0, (1.0 + v) / 2.0]);
            tri_wts.push(GL3_WTS[i] * GL3_WTS[j] * ((1.0 - v) / 8.0));
        }
    }
    let mut points = Vec::with_capacity(27);
    let mut weights = Vec::with_capacity(27);
    for (zeta, wz) in p.iter().zip(GL3_WTS.iter()) {
        for (xe, w_tri) in tri_pts.iter().zip(tri_wts.iter()) {
            points.push([xe[0], xe[1], *zeta]);
            weights.push(w_tri * wz);
        }
    }
    (points, weights)
}

/// Port of `gauss_points`: `(points (n,3), weights (n,))` in local
/// `(xi, eta, zeta)`.
pub fn gauss_points(element_type: ElementType) -> (Vec<[f64; 3]>, Vec<f64>) {
    match element_type {
        ElementType::Wedge15 => wedge15_rule(),
        ElementType::Hex20 => hex20_rule(),
    }
}
