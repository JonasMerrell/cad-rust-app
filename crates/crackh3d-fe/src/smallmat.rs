//! Port of `crackh3d/smallmat.py`: closed-form 3x3 determinant and inverse,
//! used for element Jacobians in assembly and the interaction integral.

use thiserror::Error;

pub type Mat3 = [[f64; 3]; 3];

#[derive(Debug, Error, PartialEq)]
#[error("singular 3x3 matrix (a degenerate or inverted element Jacobian)")]
pub struct Singular;

/// `(det, inverse)` of a 3x3 matrix by cofactor expansion, as in
/// `smallmat.det_inv_3x3`. An exactly zero determinant is an error, matching
/// the Python's `LinAlgError`.
pub fn det_inv_3x3(j: &Mat3) -> Result<(f64, Mat3), Singular> {
    let [[a, b, c], [d, e, f], [g, h, i]] = *j;
    let ca = e * i - f * h;
    let cb = f * g - d * i;
    let cc = d * h - e * g;
    let det = a * ca + b * cb + c * cc;
    if det == 0.0 {
        return Err(Singular);
    }
    let r = 1.0 / det;
    Ok((
        det,
        [
            [ca * r, (c * h - b * i) * r, (b * f - c * e) * r],
            [cb * r, (a * i - c * g) * r, (c * d - a * f) * r],
            [cc * r, (b * g - a * h) * r, (a * e - b * d) * r],
        ],
    ))
}
