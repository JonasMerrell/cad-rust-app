//! Port of `crackh3d/fe/surface.py`: six-node (quadratic) triangular surface
//! facets: area, and the consistent nodal force vector for a traction.
//!
//! Node ordering is gmsh's for a 6-node triangle: corners 0,1,2 then the
//! midside nodes of edges (0,1), (1,2), (2,0).

/// A tri6 facet: node ids into the mesh points.
pub type Facet = [usize; 6];

// 6-point, degree-4 symmetric rule on the reference triangle.
const A1: f64 = 0.445948490915965;
const W1: f64 = 0.223381589678011;
const A2: f64 = 0.091576213509771;
const W2: f64 = 0.109951743655322;

fn tri6_pts() -> [[f64; 2]; 6] {
    [[A1, A1], [1.0 - 2.0 * A1, A1], [A1, 1.0 - 2.0 * A1], [A2, A2], [1.0 - 2.0 * A2, A2], [A2, 1.0 - 2.0 * A2]]
}

/// The rule's weights sum to 1 and the reference triangle has area 1/2.
fn tri6_wts() -> [f64; 6] {
    [W1, W1, W1, W2, W2, W2].map(|w| 0.5 * w)
}

/// `tri6_shape_and_grad` at one point: `N` (6) and `dN` (6 x 2), in area
/// coordinates L1 = 1 - xi - eta, L2 = xi, L3 = eta.
pub fn tri6_shape_and_grad(xi: f64, eta: f64) -> ([f64; 6], [[f64; 2]; 6]) {
    let (l1, l2, l3) = (1.0 - xi - eta, xi, eta);
    let n = [
        l1 * (2.0 * l1 - 1.0),
        l2 * (2.0 * l2 - 1.0),
        l3 * (2.0 * l3 - 1.0),
        4.0 * l1 * l2,
        4.0 * l2 * l3,
        4.0 * l3 * l1,
    ];
    let dn = [
        [1.0 - 4.0 * l1, 1.0 - 4.0 * l1],
        [4.0 * l2 - 1.0, 0.0],
        [0.0, 4.0 * l3 - 1.0],
        [4.0 * (l1 - l2), -4.0 * l2],
        [4.0 * l3, 4.0 * l2],
        [-4.0 * l3, 4.0 * (l1 - l3)],
    ];
    (n, dn)
}

/// `(detJ, unit normal)` at every Gauss point of one facet. The normal's sign
/// follows the facet winding; it is NaN where `detJ` is 0.
fn jacobian_norm_and_normal(points: &[[f64; 3]], f: &Facet) -> ([f64; 6], [[f64; 3]; 6]) {
    let mut det = [0.0; 6];
    let mut nrm = [[0.0; 3]; 6];
    for (g, p) in tri6_pts().iter().enumerate() {
        let (_, dn) = tri6_shape_and_grad(p[0], p[1]);
        let mut t = [[0.0; 3]; 2]; // t[a][k]
        for (n, &id) in f.iter().enumerate() {
            for k in 0..3 {
                t[0][k] += points[id][k] * dn[n][0];
                t[1][k] += points[id][k] * dn[n][1];
            }
        }
        let c = [
            t[0][1] * t[1][2] - t[0][2] * t[1][1],
            t[0][2] * t[1][0] - t[0][0] * t[1][2],
            t[0][0] * t[1][1] - t[0][1] * t[1][0],
        ];
        let d = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
        det[g] = d;
        nrm[g] = [c[0] / d, c[1] / d, c[2] / d];
    }
    (det, nrm)
}

/// Area of each tri6 facet, integrated over its actual curved geometry.
pub fn facet_areas(points: &[[f64; 3]], facets: &[Facet]) -> Vec<f64> {
    let w = tri6_wts();
    facets
        .iter()
        .map(|f| {
            let (det, _) = jacobian_norm_and_normal(points, f);
            det.iter().zip(&w).map(|(d, w)| d * w).sum()
        })
        .collect()
}

/// Uniform traction: a fixed 3-vector, or a scalar pressure acting along
/// each facet's own normal (positive pushes into the body, i.e. along -n).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Traction {
    Vector([f64; 3]),
    Pressure(f64),
}

/// Consistent nodal force vector (length `ndof`) for a uniform traction over
/// `facets`: `integral(N_i t dA)`.
pub fn traction_load_vector(points: &[[f64; 3]], facets: &[Facet], traction: Traction, ndof: usize) -> Vec<f64> {
    let mut f = vec![0.0; ndof];
    let w = tri6_wts();
    let pts = tri6_pts();
    let shapes: Vec<[f64; 6]> = pts.iter().map(|p| tri6_shape_and_grad(p[0], p[1]).0).collect();
    for fc in facets {
        let (det, nrm) = jacobian_norm_and_normal(points, fc);
        let mut contrib = [[0.0; 3]; 6];
        for g in 0..6 {
            let t = match traction {
                Traction::Pressure(p) => [-p * nrm[g][0], -p * nrm[g][1], -p * nrm[g][2]],
                Traction::Vector(v) => v,
            };
            let wd = det[g] * w[g];
            for n in 0..6 {
                for k in 0..3 {
                    contrib[n][k] += shapes[g][n] * t[k] * wd;
                }
            }
        }
        for (n, &id) in fc.iter().enumerate() {
            for k in 0..3 {
                f[3 * id + k] += contrib[n][k];
            }
        }
    }
    f
}
