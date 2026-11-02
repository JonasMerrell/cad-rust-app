//! 3D-printed (FDM) plastics in the Simulation tab: a printed part is a solid shell (its walls and its
//! top and bottom layers) around a sparse infill core, and it is weaker between layers than along them.
//!
//! - Solid printed material: the makers' data sheets give modulus and tensile strength measured on
//!   100 % infill specimens printed flat (X-Y) and upright (Z). Those are the shell's properties.
//! - Infill: Gibson–Ashby scaling of the solid's properties with the infill's relative density ρ
//!   (k ρⁿ, with n = 1 where the infill's walls stretch and 2-3 where they bend), per pattern.
//! - Layered model: transversely isotropic about the build direction (in-plane modulus E_xy,
//!   through-layer E_z), judged by von Mises against the in-plane strength and by a quadratic
//!   interlayer criterion (tension across the layers and shear along them).
//! - Isotropic model: one modulus (the average over directions) and the weakest (Z) strength.
//!
//! Elements are given a shell share by sampling the distance to the part's surface inside them, so
//! the walls stiffen the part in bending as they do in the real print, as long as the mesh is fine
//! enough to resolve them (a warning says when it isn't). Sources: docs/fea-printed-plastics.md.

use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::fea_mesh::{Mesh, Surface};
use crate::fea_solve::{Elastic, Layer};

/// How a printed part's stiffness and strength depend on direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrintModel {
    /// Stiffer and stronger along the layers than across them (transversely isotropic about the
    /// build direction).
    #[default]
    Layered,
    /// The same in every direction: the average modulus and the weakest (between-layer) strength.
    Isotropic,
}

impl PrintModel {
    pub fn label(self) -> &'static str {
        match self {
            PrintModel::Layered => "Layered (weaker in Z)",
            PrintModel::Isotropic => "Isotropic (effective)",
        }
    }
}

/// Sparse infill patterns, as slicers name them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Infill {
    /// Grid, rectilinear, lines, crosshatch: straight walls in two directions.
    #[default]
    Grid,
    /// Triangles (and triangle-based patterns such as stars or tri-hexagon).
    Triangles,
    /// Hexagonal honeycomb.
    Honeycomb,
    /// Gyroid (and other curved 3D patterns: cubic, adaptive cubic, lightning is weaker still).
    Gyroid,
}

impl Infill {
    pub const ALL: [Infill; 4] = [Infill::Grid, Infill::Triangles, Infill::Honeycomb, Infill::Gyroid];

    pub fn label(self) -> &'static str {
        match self {
            Infill::Grid => "Grid / rectilinear",
            Infill::Triangles => "Triangles",
            Infill::Honeycomb => "Honeycomb",
            Infill::Gyroid => "Gyroid / cubic",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Infill::Grid => "grid",
            Infill::Triangles => "triangles",
            Infill::Honeycomb => "honeycomb",
            Infill::Gyroid => "gyroid",
        }
    }
}

/// Slicer settings of the printed parts of a study.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Print {
    pub model: PrintModel,
    /// Sparse infill density (%).
    pub infill: f64,
    pub pattern: Infill,
    /// Wall (perimeter) loops.
    pub walls: u32,
    /// Extrusion line width (mm).
    pub line_width: f64,
    /// Solid top and bottom layers.
    pub top_bottom: u32,
    /// Layer height (mm).
    pub layer_height: f64,
    /// Build direction (the printer's Z) in the Part Studio's axes.
    pub build_dir: [f64; 3],
}

impl Default for Print {
    fn default() -> Self {
        Print { model: PrintModel::Layered, infill: 20.0, pattern: Infill::Grid, walls: 2, line_width: 0.45, top_bottom: 4, layer_height: 0.2, build_dir: [0.0, 0.0, 1.0] }
    }
}

impl Print {
    /// Wall thickness (mm).
    pub fn wall(&self) -> f64 {
        self.walls as f64 * self.line_width.max(0.0)
    }

    /// Top and bottom skin thickness (mm).
    pub fn skin(&self) -> f64 {
        self.top_bottom as f64 * self.layer_height.max(0.0)
    }

    pub fn axis(&self) -> DVec3 {
        DVec3::from(self.build_dir).normalize_or(DVec3::Z)
    }

    /// Infill relative density (0-1).
    pub fn density(&self) -> f64 {
        (self.infill / 100.0).clamp(0.0, 1.0)
    }
}

/// A printable plastic as printed solid (100 % infill), from the maker's data sheet: modulus (MPa)
/// and tensile strength (MPa) in the layer plane (X-Y) and across the layers (Z).
#[derive(Clone, Copy, Debug)]
pub struct Printed {
    pub name: &'static str,
    pub e_xy: f64,
    pub e_z: f64,
    pub s_xy: f64,
    pub s_z: f64,
    pub source: &'static str,
}

/// Printed-solid properties. Polymaker values are dry, 100 % infill, 2 walls, ISO 527; Bambu Lab
/// values are 100 % infill, annealed and dried, ISO 527.
pub const PRINTED: &[Printed] = &[
    Printed { name: "PLA", e_xy: 3427.0, e_z: 3065.0, s_xy: 52.3, s_z: 40.5, source: "Polymaker PolyLite PLA TDS" },
    Printed { name: "PETG", e_xy: 2117.0, e_z: 1899.0, s_xy: 50.8, s_z: 42.8, source: "Polymaker PolyLite PETG TDS V5.4" },
    Printed { name: "ABS", e_xy: 2247.0, e_z: 2081.0, s_xy: 33.4, s_z: 29.7, source: "Polymaker PolyLite ABS TDS" },
    Printed { name: "ASA", e_xy: 2379.0, e_z: 1965.0, s_xy: 43.8, s_z: 32.0, source: "Polymaker ASA TDS" },
    Printed { name: "PC", e_xy: 2497.0, e_z: 2371.0, s_xy: 69.1, s_z: 52.8, source: "Polymaker PolyLite PC TDS" },
    Printed { name: "Nylon", e_xy: 2703.0, e_z: 2209.0, s_xy: 78.0, s_z: 45.8, source: "Polymaker PolyMide CoPA TDS (dry)" },
    Printed { name: "Nylon (humid)", e_xy: 724.0, e_z: 660.0, s_xy: 34.3, s_z: 13.7, source: "Polymaker PolyMide CoPA TDS (wet, 6 % water)" },
    Printed { name: "PLA-CF", e_xy: 3281.0, e_z: 2213.0, s_xy: 31.2, s_z: 15.1, source: "Polymaker PolyLite PLA-CF TDS" },
    Printed { name: "PETG-CF", e_xy: 2460.0, e_z: 1340.0, s_xy: 35.0, s_z: 29.0, source: "Bambu Lab PETG-CF TDS V3" },
    Printed { name: "ASA-CF", e_xy: 4200.0, e_z: 2290.0, s_xy: 34.0, s_z: 30.0, source: "Bambu Lab ASA-CF TDS V1" },
    Printed { name: "PET-CF", e_xy: 4730.0, e_z: 2160.0, s_xy: 74.0, s_z: 35.0, source: "Bambu Lab PET-CF TDS V3" },
];

/// The settings in a few words: "20% grid, 2 walls, layered".
pub fn summary(p: &Print) -> String {
    let dir = if (p.axis() - DVec3::Z).length() < 1e-9 { String::new() } else { format!(", build {}", fmt_dir(p.axis())) };
    format!("{}% {}, {} walls, {}{dir}", crate::expr::trim(p.infill, 1), p.pattern.key(), p.walls, if p.model == PrintModel::Layered { "layered" } else { "isotropic" })
}

/// A direction like +X or (0.71, 0, 0.71).
pub fn fmt_dir(d: DVec3) -> String {
    for (v, n) in [(DVec3::X, "+X"), (DVec3::NEG_X, "-X"), (DVec3::Y, "+Y"), (DVec3::NEG_Y, "-Y"), (DVec3::Z, "+Z"), (DVec3::NEG_Z, "-Z")] {
        if (d - v).length() < 1e-9 {
            return n.into();
        }
    }
    format!("({}, {}, {})", crate::expr::trim(d.x, 2), crate::expr::trim(d.y, 2), crate::expr::trim(d.z, 2))
}

/// Printed-solid data of a material, when it is a printable plastic.
pub fn printed(name: &str) -> Option<&'static Printed> {
    PRINTED.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

/// Infill properties over the printed solid's, at relative density ρ: in-plane and through-layer
/// modulus, through-layer shear modulus, in-plane and through-layer strength.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Factors {
    pub e_p: f64,
    pub e_z: f64,
    pub g_z: f64,
    pub s_p: f64,
    pub s_z: f64,
}

/// Gibson–Ashby scaling of each pattern's properties with its relative density ρ (Gibson & Ashby,
/// Cellular Solids, 1997): prismatic patterns (grid, triangles, honeycomb) have vertical walls, so
/// along Z they are stretch-dominated (ρ); in the layer plane triangles stretch (ρ/3), a grid stretches
/// along its ribs but bends at 45° (about ρ/4 on average) and a honeycomb bends (1.5 ρ³ stiffness,
/// 0.5 ρ² strength). Gyroid walls bend in every direction like an open-cell foam (stiffness about
/// 0.8 ρ², strength 0.3 ρ^1.5, which matches PLA gyroid tests at ρ = 0.2). Each law is lifted by
/// (1 − law(1)) ρ³ so a 100 % infill comes out as the solid.
pub fn infill_factors(pattern: Infill, rho: f64) -> Factors {
    let r = rho.clamp(0.0, 1.0);
    let lift = |law: &dyn Fn(f64) -> f64| law(r) + (1.0 - law(1.0)) * r.powi(3);
    let (e_p, e_z, g_z, s_p, s_z): (&dyn Fn(f64) -> f64, &dyn Fn(f64) -> f64, &dyn Fn(f64) -> f64, &dyn Fn(f64) -> f64, &dyn Fn(f64) -> f64) = match pattern {
        Infill::Grid => (&|r| r / 4.0, &|r| r, &|r| r / 2.0, &|r| r / 4.0, &|r| r),
        Infill::Triangles => (&|r| r / 3.0, &|r| r, &|r| r / 2.0, &|r| r / 3.0, &|r| r),
        Infill::Honeycomb => (&|r| 1.5 * r.powi(3), &|r| r, &|r| r / 2.0, &|r| 0.5 * r * r, &|r| r),
        Infill::Gyroid => (&|r| 0.8 * r * r, &|r| 0.8 * r * r, &|r| 0.8 * r * r, &|r| 0.3 * r.powf(1.5), &|r| 0.3 * r.powf(1.5)),
    };
    Factors { e_p: lift(e_p), e_z: lift(e_z), g_z: lift(g_z), s_p: lift(s_p), s_z: lift(s_z) }
}

/// Transversely isotropic stiffness about local z (Voigt: xx, yy, zz, xy, yz, zx, engineering shear):
/// in-plane modulus and Poisson's ratio, through-layer modulus, the Poisson's ratio of in-plane
/// contraction under a z stress, and the through-layer shear modulus.
pub fn transverse(ep: f64, nup: f64, ez: f64, nuzp: f64, gz: f64) -> [[f64; 6]; 6] {
    // compliance of the normal block, inverted in closed form
    let s11 = 1.0 / ep;
    let s12 = -nup / ep;
    let s13 = -nuzp / ez;
    let s33 = 1.0 / ez;
    let m = [[s11, s12, s13], [s12, s11, s13], [s13, s13, s33]];
    let inv = glam::DMat3::from_cols_array_2d(&m).inverse().to_cols_array_2d();
    let mut d = [[0.0; 6]; 6];
    for i in 0..3 {
        for j in 0..3 {
            d[i][j] = inv[j][i];
        }
    }
    d[3][3] = ep / (2.0 * (1.0 + nup));
    d[4][4] = gz;
    d[5][5] = gz;
    d
}

const VOIGT: [[usize; 3]; 3] = [[0, 3, 5], [3, 1, 4], [5, 4, 2]];

/// A stiffness whose local z axis is `axis`, in global axes (C'ᵢⱼₖₗ = Rᵢₐ Rⱼᵦ Rₖ꜀ Rₗ_d Cₐᵦ꜀_d).
pub fn rotate(d: &[[f64; 6]; 6], axis: DVec3) -> [[f64; 6]; 6] {
    let b = axis.normalize_or(DVec3::Z);
    if (b - DVec3::Z).length() < 1e-12 {
        return *d;
    }
    let (e1, e2) = b.any_orthonormal_pair();
    // columns: local x, y, z in global axes
    let r = [[e1.x, e2.x, b.x], [e1.y, e2.y, b.y], [e1.z, e2.z, b.z]];
    let c = |i: usize, j: usize, k: usize, l: usize| d[VOIGT[i][j]][VOIGT[k][l]];
    let mut out = [[0.0; 6]; 6];
    for (i, j) in [(0, 0), (1, 1), (2, 2), (0, 1), (1, 2), (2, 0)] {
        for (k, l) in [(0, 0), (1, 1), (2, 2), (0, 1), (1, 2), (2, 0)] {
            let mut s = 0.0;
            for a in 0..3 {
                for bb in 0..3 {
                    let rab = r[i][a] * r[j][bb];
                    if rab == 0.0 {
                        continue;
                    }
                    for cc in 0..3 {
                        for dd in 0..3 {
                            s += rab * r[k][cc] * r[l][dd] * c(a, bb, cc, dd);
                        }
                    }
                }
            }
            out[VOIGT[i][j]][VOIGT[k][l]] = s;
        }
    }
    out
}

/// The material of printed plastic `p` (its library entry `base`: Poisson's ratio, density, loss
/// factor) where a share `shell` (0-1) of the volume is solid shell and the rest infill.
pub fn material(p: &Printed, base: &Elastic, print: &Print, shell: f64) -> Elastic {
    let f = shell.clamp(0.0, 1.0);
    let nu = base.nu;
    let k = infill_factors(print.pattern, print.density());
    let rho = base.rho * (f + (1.0 - f) * print.density());
    let lerp = |a: f64, b: f64| f * a + (1.0 - f) * b;
    match print.model {
        PrintModel::Isotropic => {
            let es = (2.0 * p.e_xy + p.e_z) / 3.0;
            let ec = (2.0 * k.e_p * p.e_xy + k.e_z * p.e_z) / 3.0;
            let ys = p.s_xy.min(p.s_z);
            let yc = (k.s_p * p.s_xy).min(k.s_z * p.s_z);
            Elastic { e: lerp(es, ec), nu, rho, yield_mpa: lerp(ys, yc), eta: base.eta, d: None, layer: None }
        }
        PrintModel::Layered => {
            let gz = p.e_z / (2.0 * (1.0 + nu));
            let shell_d = transverse(p.e_xy, nu, p.e_z, nu * (p.e_z / p.e_xy).sqrt(), gz);
            let (ep, ez) = (k.e_p * p.e_xy, k.e_z * p.e_z);
            // a lattice's Poisson's ratio is about 0.3 whatever its material; full infill is the solid's
            let r = print.density();
            let nuc = 0.3 + (nu - 0.3) * r * r * r;
            let core_d = transverse(ep.max(1e-6), nuc, ez.max(1e-6), nuc * (ez / ep.max(1e-6)).sqrt().min(1.0), (k.g_z * gz).max(1e-6));
            let mut d = [[0.0; 6]; 6];
            for i in 0..6 {
                for j in 0..6 {
                    d[i][j] = lerp(shell_d[i][j], core_d[i][j]);
                }
            }
            let x = lerp(p.s_xy, k.s_p * p.s_xy);
            let z = lerp(p.s_z, k.s_z * p.s_z);
            // interlayer shear strength: the bond's tensile strength over √3, as von Mises would give
            let s = lerp(p.s_z, k.s_z.min(k.s_p) * p.s_z) / 3f64.sqrt();
            Elastic { e: lerp(p.e_xy, ep), nu, rho, yield_mpa: x, eta: base.eta, d: Some(rotate(&d, print.axis())), layer: Some(Layer { axis: print.axis().to_array(), x, z, s }) }
        }
    }
}

/// Depth of each node into the part, over the shell thickness there (below 1: in the shell). The
/// thickness is the top/bottom skin where the nearest surface faces up or down the build direction
/// (within 45°), the walls elsewhere.
pub fn node_depth(mesh: &Mesh, surf: &Surface, tris: &[[DVec3; 3]], print: &Print) -> Vec<f64> {
    let (wall, skin, b) = (print.wall(), print.skin(), print.axis());
    let depth = |p: DVec3| -> f64 {
        let Some((q, ti)) = surf.closest(p) else { return f64::INFINITY };
        let t = &tris[ti];
        let n = (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
        let th = if n.dot(b).abs() > std::f64::consts::FRAC_1_SQRT_2 { skin } else { wall };
        if th <= 0.0 {
            f64::INFINITY
        } else {
            q.distance(p) / th
        }
    };
    let n = mesh.nodes.len();
    let threads = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(1).clamp(1, 16);
    let chunk = n.div_ceil(threads).max(1);
    let mut out = vec![0.0; n];
    std::thread::scope(|sc| {
        for (k, part) in out.chunks_mut(chunk).enumerate() {
            let depth = &depth;
            sc.spawn(move || {
                for (i, v) in part.iter_mut().enumerate() {
                    *v = depth(mesh.nodes[k * chunk + i]);
                }
            });
        }
    });
    out
}

/// Share of each element's volume in the shell: the nodal depths interpolated (quadratically) at 35
/// points spread through the element.
pub fn shell_shares(mesh: &Mesh, depth: &[f64]) -> Vec<f64> {
    const N: usize = 8;
    let mut pts: Vec<[f64; 4]> = vec![];
    for i in 1..N {
        for j in 1..N {
            for k in 1..N {
                let l = N as i64 - (i + j + k) as i64;
                if l >= 1 {
                    pts.push([i as f64 / N as f64, j as f64 / N as f64, k as f64 / N as f64, l as f64 / N as f64]);
                }
            }
        }
    }
    let shape = |l: &[f64; 4]| -> [f64; 10] {
        let mut n = [0.0; 10];
        for i in 0..4 {
            n[i] = l[i] * (2.0 * l[i] - 1.0);
        }
        for (k, (a, b)) in crate::fea_mesh::TET_EDGES.iter().enumerate() {
            n[4 + k] = 4.0 * l[*a] * l[*b];
        }
        n
    };
    let shapes: Vec<[f64; 10]> = pts.iter().map(shape).collect();
    mesh.tets
        .iter()
        .map(|e| {
            let q: [f64; 10] = e.map(|n| depth[n as usize].min(1e6));
            if q.iter().all(|v| *v < 1.0) {
                return 1.0;
            }
            let inside = shapes.iter().filter(|n| n.iter().zip(&q).map(|(a, b)| a * b).sum::<f64>() < 1.0).count();
            inside as f64 / shapes.len() as f64
        })
        .collect()
}

/// Shell share steps per printed part (its materials are 0, 1/STEPS, …, 1 shell).
pub const STEPS: usize = 8;

/// The printed parts' materials laid out element by element.
pub struct Assigned {
    /// The mesh with `body` renumbered to material slots.
    pub mesh: Mesh,
    /// Material of each slot.
    pub mats: Vec<Elastic>,
    /// Shell share of each element (0 for parts that aren't printed).
    pub shell: Vec<f32>,
    pub warnings: Vec<String>,
}

/// Give each element of the printed parts its shell share's material. `parts`: each part's material
/// name and library material (`mesh.body` numbering; one entry when the mesh is one part).
pub fn assign(mesh: &Mesh, surf: &Surface, tris: &[[DVec3; 3]], parts: &[(String, Elastic)], print: &Print) -> Option<Assigned> {
    let kinds: Vec<Option<&Printed>> = parts.iter().map(|p| printed(&p.0)).collect();
    if kinds.iter().all(|k| k.is_none()) {
        return None;
    }
    let part_of = |ei: usize| mesh.body.get(ei).map(|b| *b as usize).unwrap_or(0).min(parts.len() - 1);
    // slots
    let mut base = vec![];
    let mut mats = vec![];
    for (p, k) in parts.iter().zip(&kinds) {
        base.push(mats.len());
        match k {
            Some(pr) => mats.extend((0..=STEPS).map(|s| material(pr, &p.1, print, s as f64 / STEPS as f64))),
            None => mats.push(p.1),
        }
    }
    let depth = node_depth(mesh, surf, tris, print);
    let shares = shell_shares(mesh, &depth);
    let shell = (0..mesh.tets.len()).map(|ei| if kinds[part_of(ei)].is_some() { shares[ei] as f32 } else { 1.0 }).collect();
    let body = (0..mesh.tets.len())
        .map(|ei| {
            let p = part_of(ei);
            (base[p] + if kinds[p].is_some() { (shares[ei] * STEPS as f64).round() as usize } else { 0 }) as u32
        })
        .collect();
    let mut warnings = vec![];
    let thin = print.wall().min(print.skin());
    if thin > 0.0 && mesh.size > 2.0 * thin {
        warnings.push(format!(
            "Elements ({} mm) are coarser than the printed walls ({} mm) and top/bottom ({} mm), so they blend into the infill: use a smaller mesh size, or a mesh control where the stress matters",
            crate::expr::trim(mesh.size, 3),
            crate::expr::trim(print.wall(), 3),
            crate::expr::trim(print.skin(), 3)
        ));
    }
    let mut out = mesh.clone();
    out.body = body;
    Some(Assigned { mesh: out, mats, shell, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_infill_is_solid() {
        for p in Infill::ALL {
            let k = infill_factors(p, 1.0);
            for v in [k.e_p, k.e_z, k.g_z, k.s_p, k.s_z] {
                assert!((v - 1.0).abs() < 1e-12, "{p:?}: {k:?}");
            }
            let k0 = infill_factors(p, 0.0);
            assert_eq!(k0.e_p, 0.0);
        }
        // honeycomb bends in the plane: far softer than triangles at 20 %
        assert!(infill_factors(Infill::Honeycomb, 0.2).e_p < 0.2 * infill_factors(Infill::Triangles, 0.2).e_p);
    }

    #[test]
    fn transverse_matrix_gives_the_moduli() {
        let d = transverse(3000.0, 0.35, 2000.0, 0.3, 700.0);
        // invert numerically: apply σ = (0, 0, 1, 0, 0, 0) by solving the normal block
        let m = glam::DMat3::from_cols_array_2d(&[[d[0][0], d[1][0], d[2][0]], [d[0][1], d[1][1], d[2][1]], [d[0][2], d[1][2], d[2][2]]]);
        let eps = m.inverse() * DVec3::Z;
        assert!((1.0 / eps.z - 2000.0).abs() < 1e-6);
        let eps = m.inverse() * DVec3::X;
        assert!((1.0 / eps.x - 3000.0).abs() < 1e-6);
        assert!((-eps.y / eps.x - 0.35).abs() < 1e-9);
    }

    #[test]
    fn rotating_to_x_swaps_the_axes() {
        let d = transverse(3000.0, 0.35, 1000.0, 0.2, 400.0);
        let r = rotate(&d, DVec3::X);
        // the soft axis is now x
        assert!((r[0][0] - d[2][2]).abs() < 1e-6, "{} vs {}", r[0][0], d[2][2]);
        assert!((r[1][1] - d[0][0]).abs() < 1e-6);
        assert!((r[2][2] - d[0][0]).abs() < 1e-6);
        // through-layer shears are the planes containing x
        assert!((r[3][3] - 400.0).abs() < 1e-6 && (r[5][5] - 400.0).abs() < 1e-6);
        assert!((r[4][4] - d[3][3]).abs() < 1e-6);
    }

    #[test]
    fn layers_fail_first_in_z_tension() {
        let pla = printed("PLA").unwrap();
        let base = Elastic::iso(3500.0, 0.36, 1.24e-9, 50.0, 0.02);
        let m = material(pla, &base, &Print::default(), 1.0);
        let along = m.failure_index(&[30.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let across = m.failure_index(&[0.0, 0.0, 30.0, 0.0, 0.0, 0.0]);
        let squeezed = m.failure_index(&[0.0, 0.0, -30.0, 0.0, 0.0, 0.0]);
        assert!((along - 30.0 / pla.s_xy).abs() < 1e-6);
        assert!((across - 30.0 / pla.s_z).abs() < 1e-6);
        assert!(squeezed < across);
        // the isotropic model uses the weakest strength everywhere
        let iso = material(pla, &base, &Print { model: PrintModel::Isotropic, ..Print::default() }, 1.0);
        assert!((iso.failure_index(&[30.0, 0.0, 0.0, 0.0, 0.0, 0.0]) - 30.0 / pla.s_z).abs() < 1e-6);
        // infill is lighter and softer
        let core = material(pla, &base, &Print::default(), 0.0);
        assert!((core.rho - 0.2 * base.rho).abs() < 1e-15);
        assert!(core.d.unwrap()[0][0] < 0.1 * m.d.unwrap()[0][0]);
    }
}
