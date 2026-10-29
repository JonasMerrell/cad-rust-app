//! Small-strain elastic-plastic materials for the cracked-body solve (the
//! J-integral's stresses and strain energy density), isotropic elasticity
//! `E`, `nu` throughout:
//!
//! * [`Plasticity::J2`]: von Mises flow theory with isotropic hardening
//!   `sigma_y(p)` (linear, Shih's piecewise power law, or a table), backward
//!   Euler radial return and its consistent (symmetric) tangent. The strain
//!   energy density the J-integral uses is the stress work
//!   `W = 1/2 sigma : C^-1 : sigma + sum sigma : d eps_p`.
//! * [`Plasticity::Deformation`]: Ramberg-Osgood deformation plasticity
//!   (nonlinear elasticity; Abaqus' `*DEFORMATION PLASTICITY`),
//!   `E eps = (1 + nu) s - (1 - 2 nu) p_h 1 + 3/2 alpha (q / sigma0)^(n-1) s`.
//!   `W` is a potential of the strain, so J is path independent exactly; the
//!   form HRR / EPRI fully plastic J solutions are written for.
//!
//! Tensors are 6-vectors in crackh3d's Voigt order `[11, 22, 33, 12, 13, 23]`
//! with tensor (not engineering) shear components, so a contraction `a : b`
//! is [`ddot`]. Tangents map the engineering strain [`super::assemble`]'s `B`
//! gives to the stress (the usual Voigt stiffness).

use serde::{Deserialize, Serialize};

pub type V6 = [f64; 6];
pub type M6 = [[f64; 6]; 6];

/// `a : b` of two symmetric tensors as Voigt 6-vectors (tensor shear).
pub fn ddot(a: &V6, b: &V6) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + 2.0 * (a[3] * b[3] + a[4] * b[4] + a[5] * b[5])
}

fn dev(a: &V6) -> (V6, f64) {
    let m = (a[0] + a[1] + a[2]) / 3.0;
    ([a[0] - m, a[1] - m, a[2] - m, a[3], a[4], a[5]], m)
}

/// The fourth-order identities in Voigt (tensor-shear) components acting on
/// engineering strain: `I_dev` and `1 x 1`.
fn idev() -> M6 {
    let mut d = [[0.0; 6]; 6];
    for i in 0..3 {
        for j in 0..3 {
            d[i][j] = if i == j { 2.0 / 3.0 } else { -1.0 / 3.0 };
        }
        d[i + 3][i + 3] = 0.5;
    }
    d
}

/// Hardening curve of the J2 model: the flow stress at equivalent plastic
/// strain `p`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Hardening {
    /// `sigma0 + h p`
    Linear { sigma0: f64, h: f64 },
    /// Shih's piecewise power law on the uniaxial curve: `eps = sigma / E`
    /// up to `sigma0`, then `eps / eps0 = (sigma / sigma0)^n` with
    /// `eps0 = sigma0 / E` (so `p = eps0 (sigma/sigma0)^n - sigma / E`).
    Power { sigma0: f64, n: f64 },
    /// Piecewise linear `stress(eqps)`, held flat past the last point
    /// (`eqps` ascending from 0).
    Table { eqps: Vec<f64>, stress: Vec<f64> },
}

/// The plastic part of the material.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "model", rename_all = "snake_case")]
pub enum Plasticity {
    J2 { hardening: Hardening },
    /// Ramberg-Osgood deformation plasticity (`sigma0`, `alpha`, `n`).
    Deformation { sigma0: f64, alpha: f64, n: f64 },
}

/// The elastic-plastic material: isotropic elasticity and a [`Plasticity`].
#[derive(Debug, Clone)]
pub struct Material {
    pub e: f64,
    pub nu: f64,
    pub g: f64,
    pub k: f64,
    pub plasticity: Plasticity,
}

/// State at one Gauss point.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GpState {
    /// stress (tensor components)
    pub sig: V6,
    /// total strain (tensor components)
    pub eps: V6,
    /// plastic strain (J2; zero for deformation plasticity)
    pub ep: V6,
    /// equivalent plastic strain (J2: accumulated; deformation: the
    /// Ramberg-Osgood plastic strain at the current stress)
    pub p: f64,
    /// strain energy density (stress work per unit volume)
    pub w: f64,
}

impl Hardening {
    fn check(&self) -> Result<(), String> {
        match self {
            Hardening::Linear { sigma0, h } => {
                if !(*sigma0 > 0.0) || !(*h >= 0.0) {
                    return Err(format!("hardening linear: sigma0 {sigma0} must be > 0 and h {h} >= 0"));
                }
            }
            Hardening::Power { sigma0, n } => {
                if !(*sigma0 > 0.0) || !(*n >= 1.0) {
                    return Err(format!("hardening power: sigma0 {sigma0} must be > 0 and n {n} >= 1"));
                }
            }
            Hardening::Table { eqps, stress } => {
                if eqps.len() < 2 || eqps.len() != stress.len() {
                    return Err("hardening table: eqps and stress must have the same length >= 2".into());
                }
                if eqps[0] != 0.0 || eqps.windows(2).any(|w| w[1] <= w[0]) {
                    return Err("hardening table: eqps must start at 0 and ascend".into());
                }
                if stress.iter().any(|&s| !(s > 0.0)) || stress.windows(2).any(|w| w[1] < w[0]) {
                    return Err("hardening table: stress must be positive and non-decreasing".into());
                }
            }
        }
        Ok(())
    }

    /// The initial yield stress.
    pub fn sigma_y0(&self) -> f64 {
        match self {
            Hardening::Linear { sigma0, .. } | Hardening::Power { sigma0, .. } => *sigma0,
            Hardening::Table { stress, .. } => stress[0],
        }
    }

    /// `(sigma_y(p), d sigma_y / dp)`.
    pub fn flow(&self, p: f64, e: f64) -> (f64, f64) {
        match self {
            Hardening::Linear { sigma0, h } => (sigma0 + h * p, *h),
            Hardening::Power { sigma0, n } => {
                // p = eps0 (s/s0)^n - s/E, s >= s0: Newton on s from the larger root bound
                let eps0 = sigma0 / e;
                if p <= 0.0 {
                    // slope at s0: dp/ds = n eps0 / s0 - 1/E = (n - 1) / E
                    return (*sigma0, if *n > 1.0 { e / (n - 1.0) } else { f64::INFINITY });
                }
                let mut s = (sigma0 * ((p + eps0) / eps0).powf(1.0 / n)).max(*sigma0);
                for _ in 0..50 {
                    let r = eps0 * (s / sigma0).powf(*n) - s / e - p;
                    let d = n * eps0 / sigma0 * (s / sigma0).powf(n - 1.0) - 1.0 / e;
                    let ds = r / d;
                    s -= ds;
                    if ds.abs() <= 1e-14 * s {
                        break;
                    }
                }
                let d = n * eps0 / sigma0 * (s / sigma0).powf(n - 1.0) - 1.0 / e;
                (s, 1.0 / d)
            }
            Hardening::Table { eqps, stress } => {
                let m = eqps.len();
                if p >= eqps[m - 1] {
                    return (stress[m - 1], 0.0);
                }
                let k = eqps.partition_point(|&v| v <= p).max(1) - 1;
                let sl = (stress[k + 1] - stress[k]) / (eqps[k + 1] - eqps[k]);
                (stress[k] + sl * (p - eqps[k]), sl)
            }
        }
    }
}

impl Material {
    pub fn new(e: f64, nu: f64, plasticity: Plasticity) -> Result<Self, String> {
        match &plasticity {
            Plasticity::J2 { hardening } => hardening.check()?,
            Plasticity::Deformation { sigma0, alpha, n } => {
                if !(*sigma0 > 0.0) || !(*alpha >= 0.0) || !(*n >= 1.0) {
                    return Err(format!("deformation plasticity: sigma0 {sigma0} > 0, alpha {alpha} >= 0, n {n} >= 1"));
                }
            }
        }
        let g = e / (2.0 * (1.0 + nu));
        let k = e / (3.0 * (1.0 - 2.0 * nu));
        Ok(Material { e, nu, g, k, plasticity })
    }

    /// The elastic Voigt stiffness (engineering shear).
    pub fn c_elastic(&self) -> M6 {
        let id = idev();
        let mut c = [[0.0; 6]; 6];
        for i in 0..6 {
            for j in 0..6 {
                c[i][j] = 2.0 * self.g * id[i][j] + if i < 3 && j < 3 { self.k } else { 0.0 };
            }
        }
        c
    }

    /// The equivalent (von Mises) stress.
    pub fn mises(sig: &V6) -> f64 {
        let (s, _) = dev(sig);
        (1.5 * ddot(&s, &s)).sqrt()
    }

    /// The stress at which plasticity starts (J2: the initial yield stress;
    /// deformation: `sigma0`, a reference only -- that model is nonlinear from
    /// zero load).
    pub fn sigma_ref(&self) -> f64 {
        match &self.plasticity {
            Plasticity::J2 { hardening } => hardening.sigma_y0(),
            Plasticity::Deformation { sigma0, .. } => *sigma0,
        }
    }

    /// Stress update from the total strain `eps` (tensor shear) and the last
    /// converged state: the new state and the tangent `d sigma / d eps_eng`.
    pub fn update(&self, eps: &V6, old: &GpState) -> Result<(GpState, M6), String> {
        match &self.plasticity {
            Plasticity::J2 { hardening } => self.update_j2(hardening, eps, old),
            Plasticity::Deformation { sigma0, alpha, n } => Ok(self.update_deformation(*sigma0, *alpha, *n, eps)),
        }
    }

    fn update_j2(&self, h: &Hardening, eps: &V6, old: &GpState) -> Result<(GpState, M6), String> {
        let (g, k) = (self.g, self.k);
        let ee: V6 = std::array::from_fn(|i| eps[i] - old.ep[i]);
        let (e_dev, e_m) = dev(&ee);
        let s_tr: V6 = std::array::from_fn(|i| 2.0 * g * e_dev[i]);
        let pr = 3.0 * k * e_m;
        let q_tr = (1.5 * ddot(&s_tr, &s_tr)).sqrt();
        let (sy, _) = h.flow(old.p, self.e);
        let we_of = |sig: &V6| {
            let (s, m) = dev(sig);
            ddot(&s, &s) / (4.0 * g) + 0.5 * m * m / k
        };
        if q_tr <= sy * (1.0 + 1e-12) {
            let sig: V6 = std::array::from_fn(|i| s_tr[i] + if i < 3 { pr } else { 0.0 });
            let wp = old.w - we_of(&old.sig);
            let st = GpState { sig, eps: *eps, ep: old.ep, p: old.p, w: we_of(&sig) + wp };
            return Ok((st, self.c_elastic()));
        }
        // q_tr - 3 G dp - sigma_y(p + dp) = 0
        let mut dp = (q_tr - sy) / (3.0 * g);
        let mut hh = 0.0;
        let mut ok = false;
        for _ in 0..60 {
            let (s, hp) = h.flow(old.p + dp, self.e);
            hh = hp;
            let r = q_tr - 3.0 * g * dp - s;
            let d = 3.0 * g + hp;
            let step = r / d;
            dp = (dp + step).max(0.0);
            if step.abs() <= 1e-13 * (dp.abs() + 1e-30) || r.abs() <= 1e-12 * sy {
                ok = true;
                break;
            }
        }
        if !ok {
            return Err(format!("J2 return map: no convergence (q_trial {q_tr:.4e}, p {:.3e})", old.p));
        }
        let nhat: V6 = std::array::from_fn(|i| 1.5 * s_tr[i] / q_tr); // d q / d sigma
        let fac = 1.0 - 3.0 * g * dp / q_tr;
        let s: V6 = std::array::from_fn(|i| fac * s_tr[i]);
        let sig: V6 = std::array::from_fn(|i| s[i] + if i < 3 { pr } else { 0.0 });
        let dep: V6 = std::array::from_fn(|i| dp * nhat[i]);
        let ep: V6 = std::array::from_fn(|i| old.ep[i] + dep[i]);
        let wp_old = old.w - we_of(&old.sig);
        let w = we_of(&sig) + wp_old + ddot(&sig, &dep);
        // consistent tangent (Simo & Hughes): K 1x1 + 2G theta I_dev - 2G thetabar n x n
        let theta = fac;
        let thetabar = 1.0 / (1.0 + hh / (3.0 * g)) - (1.0 - theta);
        let nrm = ddot(&s_tr, &s_tr).sqrt();
        let nn: V6 = std::array::from_fn(|i| s_tr[i] / nrm);
        let id = idev();
        let mut c = [[0.0; 6]; 6];
        for i in 0..6 {
            for j in 0..6 {
                c[i][j] = 2.0 * g * theta * id[i][j] - 2.0 * g * thetabar * nn[i] * nn[j]
                    + if i < 3 && j < 3 { k } else { 0.0 };
            }
        }
        Ok((GpState { sig, eps: *eps, ep, p: old.p + dp, w }, c))
    }

    fn update_deformation(&self, s0: f64, alpha: f64, n: f64, eps: &V6) -> (GpState, M6) {
        let (g, k, e) = (self.g, self.k, self.e);
        let (ed, em) = (dev(eps).0, dev(eps).1);
        let tr = 3.0 * em;
        let e_eq = (2.0 / 3.0 * ddot(&ed, &ed)).sqrt();
        // q / (3G) + alpha s0 / E (q / s0)^n = e_eq
        let f = |q: f64| q / (3.0 * g) + alpha * s0 / e * (q / s0).powf(n) - e_eq;
        let fp = |q: f64| 1.0 / (3.0 * g) + alpha * n / e * (q / s0).powf(n - 1.0);
        let mut q = 0.0;
        if e_eq > 0.0 {
            // start from the larger of the elastic and fully plastic estimates' minimum (f convex)
            let qe = 3.0 * g * e_eq;
            let qp = s0 * (e_eq * e / (alpha * s0).max(1e-300)).powf(1.0 / n);
            q = qe.min(qp);
            for _ in 0..100 {
                let step = f(q) / fp(q);
                q -= step;
                if step.abs() <= 1e-14 * q {
                    break;
                }
            }
        }
        // phi(q): e = phi s
        let phi = 1.0 / (2.0 * g) + 1.5 * alpha / e * (q / s0).powf(n - 1.0);
        let s: V6 = std::array::from_fn(|i| ed[i] / phi);
        let sig: V6 = std::array::from_fn(|i| s[i] + if i < 3 { k * tr } else { 0.0 });
        let w = 0.5 * k * tr * tr + q * q / (6.0 * g) + alpha * n / ((n + 1.0) * e) * s0 * s0 * (q / s0).powf(n + 1.0);
        let id = idev();
        let mut c = [[0.0; 6]; 6];
        // ds = de / phi - e phi'(q) q'(e_eq) (2/3 e : de / e_eq) / phi^2
        let corr = if e_eq > 0.0 && n > 1.0 && q > 0.0 {
            let dphi = 1.5 * alpha * (n - 1.0) / (e * s0) * (q / s0).powf(n - 2.0);
            dphi / fp(q) * (2.0 / 3.0) / (e_eq * phi * phi)
        } else {
            0.0
        };
        for i in 0..6 {
            for j in 0..6 {
                c[i][j] = id[i][j] / phi - corr * ed[i] * ed[j] + if i < 3 && j < 3 { k } else { 0.0 };
            }
        }
        let p = alpha * s0 / e * (q / s0).powf(n);
        (GpState { sig, eps: *eps, ep: [0.0; 6], p, w }, c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eng(e: &V6) -> V6 {
        [e[0], e[1], e[2], 2.0 * e[3], 2.0 * e[4], 2.0 * e[5]]
    }

    fn fd_check(m: &Material, eps: V6, old: GpState) {
        let (_, c) = m.update(&eps, &old).unwrap();
        let h = 1e-7;
        for j in 0..6 {
            let mut ep = eng(&eps);
            let mut em = eng(&eps);
            ep[j] += h;
            em[j] -= h;
            let t = |v: &V6| -> V6 { [v[0], v[1], v[2], v[3] / 2.0, v[4] / 2.0, v[5] / 2.0] };
            let (sp, _) = m.update(&t(&ep), &old).unwrap();
            let (sm, _) = m.update(&t(&em), &old).unwrap();
            for i in 0..6 {
                let fd = (sp.sig[i] - sm.sig[i]) / (2.0 * h);
                assert!((fd - c[i][j]).abs() <= 1e-4 * m.e, "C[{i}][{j}] {} vs fd {fd}", c[i][j]);
            }
        }
    }

    fn mats() -> Vec<Material> {
        vec![
            Material::new(200e3, 0.3, Plasticity::J2 { hardening: Hardening::Linear { sigma0: 300.0, h: 2000.0 } }).unwrap(),
            Material::new(200e3, 0.3, Plasticity::J2 { hardening: Hardening::Power { sigma0: 300.0, n: 5.0 } }).unwrap(),
            Material::new(200e3, 0.3, Plasticity::Deformation { sigma0: 300.0, alpha: 1.0, n: 5.0 }).unwrap(),
        ]
    }

    #[test]
    fn tangents_match_finite_differences() {
        let eps = [0.004, -0.001, 0.0005, 0.0012, -0.0004, 0.0007];
        for m in mats() {
            fd_check(&m, eps, GpState::default());
            fd_check(&m, [1e-5, 0.0, 0.0, 0.0, 0.0, 0.0], GpState::default());
        }
    }

    /// Uniaxial stress by driving eps_11 and solving the lateral strains:
    /// the curve the hardening laws are written for.
    fn uniaxial(m: &Material, e11: f64, steps: usize) -> (f64, f64) {
        let mut st = GpState::default();
        let mut lat = -m.nu * e11 / steps as f64;
        for k in 1..=steps {
            let e = e11 * k as f64 / steps as f64;
            for _ in 0..50 {
                let eps = [e, lat, lat, 0.0, 0.0, 0.0];
                let (s, c) = m.update(&eps, &st).unwrap();
                let r = s.sig[1];
                if r.abs() < 1e-10 * m.e {
                    break;
                }
                lat -= r / (c[1][1] + c[1][2]);
            }
            st = m.update(&[e, lat, lat, 0.0, 0.0, 0.0], &st).unwrap().0;
        }
        (st.sig[0], st.w)
    }

    #[test]
    fn uniaxial_curves() {
        let ms = mats();
        // power: eps = eps0 (s/s0)^n at eps = 0.01: s = 300 (0.01/0.0015)^(1/5)
        let (s, _) = uniaxial(&ms[1], 0.01, 50);
        let want = 300.0 * (0.01f64 / 0.0015).powf(0.2);
        assert!((s - want).abs() < 1e-6 * want, "{s} vs {want}");
        // Ramberg-Osgood: eps = s/E + alpha s0/E (s/s0)^n
        let (s, w) = uniaxial(&ms[2], 0.01, 1);
        let eps = s / 200e3 + 300.0 / 200e3 * (s / 300.0f64).powi(5);
        assert!((eps - 0.01).abs() < 1e-10, "{eps}");
        // W = s^2/2E + alpha n/(n+1) s0/E (s/s0)^(n+1) s0... = int s de
        let wr = s * s / (2.0 * 200e3) + 5.0 / 6.0 * 300.0 * 300.0 / 200e3 * (s / 300.0f64).powi(6);
        assert!((w - wr).abs() < 1e-8 * wr, "{w} vs {wr}");
        // J2 stress work = int s de (trapezoid of a fine path)
        let (_, w1) = uniaxial(&ms[0], 0.01, 400);
        let sy = |e: f64| {
            let ey = 300.0 / 200e3;
            if e <= ey { 200e3 * e } else { 300.0 + 200e3 * 2000.0 / (200e3 + 2000.0) * (e - ey) }
        };
        let n = 20000;
        let wr: f64 = (0..n).map(|i| sy(0.01 * (i as f64 + 0.5) / n as f64) * 0.01 / n as f64).sum();
        assert!((w1 - wr).abs() < 1e-4 * wr, "{w1} vs {wr}");
    }
}
