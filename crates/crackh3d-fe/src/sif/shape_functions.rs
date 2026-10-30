//! Port of `crackh3d/sif/shape_functions.py`: isoparametric shape functions
//! for wedge15 / hex20 in the project's node ordering.
//!
//! The Python takes local derivatives by complex-step differentiation. Here
//! the same polynomials are evaluated on forward-mode [`Dual`] numbers, which
//! gives the identical exact derivative (no truncation error) without
//! complex arithmetic. [`Scalar`] / [`Dual`] are also used by
//! `isotropic_fields` for the auxiliary-field gradient.

use std::ops::{Add, Mul, Neg, Sub};

pub use super::quadrature::ElementType;

/// Minimal arithmetic needed by the shape / near-tip formulas, implemented for
/// `f64` and [`Dual`].
pub trait Scalar:
    Copy + Add<Output = Self> + Sub<Output = Self> + Mul<Output = Self> + Neg<Output = Self> + From<f64>
{
    fn sqrt(self) -> Self;
    /// Division by a real constant.
    fn div_f64(self, d: f64) -> Self;
    fn sin(self) -> Self;
    fn cos(self) -> Self;
}

impl Scalar for f64 {
    fn div_f64(self, d: f64) -> Self {
        self / d
    }
    fn sqrt(self) -> Self {
        f64::sqrt(self)
    }
    fn sin(self) -> Self {
        f64::sin(self)
    }
    fn cos(self) -> Self {
        f64::cos(self)
    }
}

/// Forward-mode dual number `v + d*eps` (one directional derivative).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dual {
    pub v: f64,
    pub d: f64,
}

impl Dual {
    pub fn new(v: f64, d: f64) -> Self {
        Self { v, d }
    }
}
impl From<f64> for Dual {
    fn from(v: f64) -> Self {
        Self { v, d: 0.0 }
    }
}
impl Add for Dual {
    type Output = Dual;
    fn add(self, o: Dual) -> Dual {
        Dual::new(self.v + o.v, self.d + o.d)
    }
}
impl Sub for Dual {
    type Output = Dual;
    fn sub(self, o: Dual) -> Dual {
        Dual::new(self.v - o.v, self.d - o.d)
    }
}
impl Mul for Dual {
    type Output = Dual;
    fn mul(self, o: Dual) -> Dual {
        Dual::new(self.v * o.v, self.v * o.d + self.d * o.v)
    }
}
impl Neg for Dual {
    type Output = Dual;
    fn neg(self) -> Dual {
        Dual::new(-self.v, -self.d)
    }
}
impl Scalar for Dual {
    fn div_f64(self, d: f64) -> Self {
        Dual::new(self.v / d, self.d / d)
    }
    fn sqrt(self) -> Self {
        let s = self.v.sqrt();
        Dual::new(s, self.d / (2.0 * s))
    }
    fn sin(self) -> Self {
        Dual::new(self.v.sin(), self.d * self.v.cos())
    }
    fn cos(self) -> Self {
        Dual::new(self.v.cos(), -self.d * self.v.sin())
    }
}

#[inline]
fn c<T: Scalar>(x: f64) -> T {
    T::from(x)
}

/// Port of `wedge15_shape`, generic over [`Scalar`]. Node order: 0..2 bottom
/// corners (zeta=-1), 3..5 top corners, 6=mid(0,1), 7=mid(0,2), 8=mid(0,3),
/// 9=mid(1,2), 10=mid(1,4), 11=mid(2,5), 12=mid(3,4), 13=mid(3,5), 14=mid(4,5).
pub fn wedge15_shape_t<T: Scalar>(xi: T, eta: T, zeta: T) -> [T; 15] {
    let one = c::<T>(1.0);
    let l1 = one - xi - eta;
    let l2 = xi;
    let l3 = eta;
    let pb = (one - zeta) * c(0.5);
    let pt = (one + zeta) * c(0.5);
    let omz2 = one - zeta * zeta;
    let two = c::<T>(2.0);
    let four = c::<T>(4.0);
    let half = c::<T>(0.5);
    [
        l1 * (two * l1 - one) * pb - half * l1 * omz2,
        l2 * (two * l2 - one) * pb - half * l2 * omz2,
        l3 * (two * l3 - one) * pb - half * l3 * omz2,
        l1 * (two * l1 - one) * pt - half * l1 * omz2,
        l2 * (two * l2 - one) * pt - half * l2 * omz2,
        l3 * (two * l3 - one) * pt - half * l3 * omz2,
        four * l1 * l2 * pb,
        four * l1 * l3 * pb,
        l1 * omz2,
        four * l2 * l3 * pb,
        l2 * omz2,
        l3 * omz2,
        four * l1 * l2 * pt,
        four * l1 * l3 * pt,
        four * l2 * l3 * pt,
    ]
}

/// `(xi_i, eta_i, zeta_i)` of hex20's nodes (`_HEX20_NODES`).
const HEX20_NODES: [[i32; 3]; 20] = [
    [-1, -1, -1], [1, -1, -1], [1, 1, -1], [-1, 1, -1],
    [-1, -1, 1], [1, -1, 1], [1, 1, 1], [-1, 1, 1],
    [0, -1, -1], [-1, 0, -1], [-1, -1, 0], [1, 0, -1],
    [1, -1, 0], [0, 1, -1], [1, 1, 0], [-1, 1, 0],
    [0, -1, 1], [-1, 0, 1], [1, 0, 1], [0, 1, 1],
];

/// Port of `hex20_shape`, generic over [`Scalar`].
pub fn hex20_shape_t<T: Scalar>(xi: T, eta: T, zeta: T) -> [T; 20] {
    let one = c::<T>(1.0);
    let mut out = [c::<T>(0.0); 20];
    for (n, nd) in HEX20_NODES.iter().enumerate() {
        let (xi_i, eta_i, zeta_i) = (c::<T>(nd[0] as f64), c::<T>(nd[1] as f64), c::<T>(nd[2] as f64));
        let n_zero = nd.iter().filter(|&&v| v == 0).count();
        let fx = one + xi * xi_i;
        let fy = one + eta * eta_i;
        let fz = one + zeta * zeta_i;
        out[n] = if n_zero == 0 {
            c::<T>(0.125) * fx * fy * fz * (xi * xi_i + eta * eta_i + zeta * zeta_i - c(2.0))
        } else if nd[0] == 0 {
            c::<T>(0.25) * (one - xi * xi) * fy * fz
        } else if nd[1] == 0 {
            c::<T>(0.25) * fx * (one - eta * eta) * fz
        } else {
            c::<T>(0.25) * fx * fy * (one - zeta * zeta)
        };
    }
    out
}

/// `wedge15_shape` at real coordinates.
pub fn wedge15_shape(xi: f64, eta: f64, zeta: f64) -> [f64; 15] {
    wedge15_shape_t(xi, eta, zeta)
}

/// `hex20_shape` at real coordinates.
pub fn hex20_shape(xi: f64, eta: f64, zeta: f64) -> [f64; 20] {
    hex20_shape_t(xi, eta, zeta)
}

/// Port of `eval_shape_and_grad` for one point: `N (num_nodes)` and
/// `dN_dlocal (num_nodes, 3)` with columns `(d/dxi, d/deta, d/dzeta)`.
pub fn eval_shape_and_grad(et: ElementType, xi: f64, eta: f64, zeta: f64) -> (Vec<f64>, Vec<[f64; 3]>) {
    let run = |seed: usize| -> Vec<Dual> {
        let a = [xi, eta, zeta];
        let mk = |i: usize| Dual::new(a[i], if i == seed { 1.0 } else { 0.0 });
        match et {
            ElementType::Wedge15 => wedge15_shape_t(mk(0), mk(1), mk(2)).to_vec(),
            ElementType::Hex20 => hex20_shape_t(mk(0), mk(1), mk(2)).to_vec(),
        }
    };
    let (dx, dy, dz) = (run(0), run(1), run(2));
    let n = dx.iter().map(|d| d.v).collect();
    let g = (0..dx.len()).map(|i| [dx[i].d, dy[i].d, dz[i].d]).collect();
    (n, g)
}
