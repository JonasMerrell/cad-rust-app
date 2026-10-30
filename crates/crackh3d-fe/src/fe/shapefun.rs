//! Port of `crackh3d/fe/shapefun.py`: shape functions, local gradients and
//! Gauss rules for tet10 / hex20 / wedge15 / pyramid13.
//!
//! Python gets dN/dlocal by exact complex-step differentiation; here a small
//! forward-mode dual number ([`Dual`]) plays that role (also exact for the
//! polynomial bases). hex20/wedge15 shapes and rules delegate to
//! `sif::shape_functions` / `sif::quadrature`, as `shapefun.py` does.
//!
//! pyramid13 is tabulated, not evaluable in closed form: only the 27
//! quadrature points and the reference centroid are available, anything else
//! is an error (as in the Python `ValueError`).

use super::pyramid13_tables as pyr13;
use crate::sif::quadrature as sif_quad;
use crate::sif::shape_functions as sif_shape;
use std::ops::{Add, Mul, Neg, Sub};
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum ShapeError {
    #[error("unknown element type {0:?}")]
    UnknownElement(String),
    #[error(
        "pyramid13 shape functions are tabulated, not evaluable in closed form: only available at the {n} quadrature \
         points of gauss_points(\"pyramid13\") and at the reference centroid. Requested point {point:?} is {off:.3e} \
         away from the nearest tabulated point {nearest:?}."
    )]
    PyramidOffTable { n: usize, point: [f64; 3], off: f64, nearest: [f64; 3] },
}

/// Value plus gradient w.r.t. (xi, eta, zeta); forward-mode AD.
#[derive(Clone, Copy, Debug)]
struct Dual {
    v: f64,
    d: [f64; 3],
}

impl Dual {
    fn var(v: f64, axis: usize) -> Self {
        let mut d = [0.0; 3];
        d[axis] = 1.0;
        Dual { v, d }
    }
    fn c(v: f64) -> Self {
        Dual { v, d: [0.0; 3] }
    }
}
impl Add for Dual {
    type Output = Dual;
    fn add(self, o: Dual) -> Dual {
        Dual { v: self.v + o.v, d: [self.d[0] + o.d[0], self.d[1] + o.d[1], self.d[2] + o.d[2]] }
    }
}
impl Sub for Dual {
    type Output = Dual;
    fn sub(self, o: Dual) -> Dual {
        Dual { v: self.v - o.v, d: [self.d[0] - o.d[0], self.d[1] - o.d[1], self.d[2] - o.d[2]] }
    }
}
impl Mul for Dual {
    type Output = Dual;
    fn mul(self, o: Dual) -> Dual {
        Dual {
            v: self.v * o.v,
            d: [
                self.d[0] * o.v + self.v * o.d[0],
                self.d[1] * o.v + self.v * o.d[1],
                self.d[2] * o.v + self.v * o.d[2],
            ],
        }
    }
}
impl Neg for Dual {
    type Output = Dual;
    fn neg(self) -> Dual {
        Dual { v: -self.v, d: [-self.d[0], -self.d[1], -self.d[2]] }
    }
}
impl Add<f64> for Dual {
    type Output = Dual;
    fn add(self, o: f64) -> Dual {
        Dual { v: self.v + o, d: self.d }
    }
}
impl Sub<f64> for Dual {
    type Output = Dual;
    fn sub(self, o: f64) -> Dual {
        Dual { v: self.v - o, d: self.d }
    }
}
impl Mul<f64> for Dual {
    type Output = Dual;
    fn mul(self, o: f64) -> Dual {
        Dual { v: self.v * o, d: [self.d[0] * o, self.d[1] * o, self.d[2] * o] }
    }
}
impl Mul<Dual> for f64 {
    type Output = Dual;
    fn mul(self, o: Dual) -> Dual {
        o * self
    }
}
impl Add<Dual> for f64 {
    type Output = Dual;
    fn add(self, o: Dual) -> Dual {
        o + self
    }
}
impl Sub<Dual> for f64 {
    type Output = Dual;
    fn sub(self, o: Dual) -> Dual {
        -o + self
    }
}

/// Port of `shapefun.tet10_shape`. Nodes 0-3 corners (barycentric
/// L1 = 1-xi-eta-zeta, L2 = xi, L3 = eta, L4 = zeta); 4=mid(0,1), 5=mid(1,2),
/// 6=mid(0,2), 7=mid(0,3), 8=mid(2,3), 9=mid(1,3).
fn tet10_shape(xi: Dual, eta: Dual, zeta: Dual) -> Vec<Dual> {
    let l1 = 1.0 - xi - eta - zeta;
    let (l2, l3, l4) = (xi, eta, zeta);
    vec![
        l1 * (2.0 * l1 - 1.0),
        l2 * (2.0 * l2 - 1.0),
        l3 * (2.0 * l3 - 1.0),
        l4 * (2.0 * l4 - 1.0),
        4.0 * l1 * l2,
        4.0 * l2 * l3,
        4.0 * l1 * l3,
        4.0 * l1 * l4,
        4.0 * l3 * l4,
        4.0 * l2 * l4,
    ]
}

// Tabulated points of pyramid13: the quadrature rule's points, then the centroid.
const PYR_TOL: f64 = 1e-12;

/// Port of `shapefun._pyramid13_lookup` for a single point.
fn pyramid13_lookup(p: [f64; 3]) -> Result<(Vec<f64>, Vec<[f64; 3]>), ShapeError> {
    let n_gp = pyr13::GAUSS_POINTS.len();
    let tab = |k: usize| if k < n_gp { pyr13::GAUSS_POINTS[k] } else { pyr13::CENTROID };
    let (mut nearest, mut off) = (0usize, f64::INFINITY);
    for k in 0..=n_gp {
        let q = tab(k);
        let d = ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt();
        if d < off {
            off = d;
            nearest = k;
        }
    }
    if off > PYR_TOL {
        return Err(ShapeError::PyramidOffTable { n: n_gp, point: p, off, nearest: tab(nearest) });
    }
    if nearest < n_gp {
        Ok((pyr13::N_AT_GAUSS[nearest].to_vec(), pyr13::DN_AT_GAUSS[nearest].to_vec()))
    } else {
        Ok((pyr13::N_AT_CENTROID.to_vec(), pyr13::DN_AT_CENTROID.to_vec()))
    }
}

/// Port of `shapefun.eval_shape_and_grad` for one point: `N` (num_nodes) and
/// `dN/dlocal` (num_nodes x 3). pyramid13 is looked up (see module docs).
pub fn eval_shape_and_grad(
    element_type: &str,
    xi: f64,
    eta: f64,
    zeta: f64,
) -> Result<(Vec<f64>, Vec<[f64; 3]>), ShapeError> {
    match element_type {
        "pyramid13" => pyramid13_lookup([xi, eta, zeta]),
        "tet10" => {
            let r = tet10_shape(Dual::var(xi, 0), Dual::var(eta, 1), Dual::var(zeta, 2));
            Ok((r.iter().map(|x| x.v).collect(), r.iter().map(|x| x.d).collect()))
        }
        other => match sif_quad::ElementType::from_name(other) {
            Some(et) => Ok(sif_shape::eval_shape_and_grad(et, xi, eta, zeta)),
            None => Err(ShapeError::UnknownElement(other.to_string())),
        },
    }
}

/// Port of `shapefun.tet10_shape` as a plain evaluation (values only).
pub fn tet10_values(xi: f64, eta: f64, zeta: f64) -> Vec<f64> {
    tet10_shape(Dual::c(xi), Dual::c(eta), Dual::c(zeta)).iter().map(|x| x.v).collect()
}

// Keast 4-point degree-2 tet rule (shapefun._TET10_*).
const TET10_A: f64 = 0.5854101966249685;
const TET10_B: f64 = 0.1381966011250105;

/// Port of `shapefun.gauss_points`: (points (n x 3), weights (n)) in local
/// (xi, eta, zeta). `None` for element types without a rule (tet4).
pub fn gauss_points(element_type: &str) -> Option<(Vec<[f64; 3]>, Vec<f64>)> {
    match element_type {
        "tet10" => Some((
            vec![
                [TET10_B, TET10_B, TET10_B],
                [TET10_A, TET10_B, TET10_B],
                [TET10_B, TET10_A, TET10_B],
                [TET10_B, TET10_B, TET10_A],
            ],
            vec![1.0 / 24.0; 4],
        )),
        "pyramid13" => Some((pyr13::GAUSS_POINTS.to_vec(), pyr13::GAUSS_WEIGHTS.to_vec())),
        other => sif_quad::ElementType::from_name(other).map(sif_quad::gauss_points),
    }
}
