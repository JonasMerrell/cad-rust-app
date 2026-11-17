//! A physical model of a milling cut, for the stock simulation's load check: from how wide and deep the
//! tool is engaged (measured in the stock model) and the programmed feed and speed, the chip it takes,
//! the cutting force, the spindle's power and torque, how far the tool bends and how hard it is
//! stressed. Every effect, formula, constant and source is written down in docs/cutting-model.md.

use std::f64::consts::PI;

/// Specific cutting force by material (Kienzle): kc = kc1 · h^-mc, N/mm², with h the chip thickness in mm.
/// Values from the Sandvik Coromant material classes (CMC / ISO groups), rounded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Material {
    pub kc1: f64,
    pub mc: f64,
    /// Radial (thrust) force as a share of the tangential force.
    pub radial: f64,
}

pub fn material(name: &str) -> Material {
    let n = name.to_ascii_lowercase();
    let m = |kc1, mc, radial| Material { kc1, mc, radial };
    if n.contains("7075") || n.contains("2024") {
        m(800.0, 0.25, 0.35)
    } else if n.contains("6061") || n.contains("6082") || n.contains("5083") || n.contains("alu") {
        m(700.0, 0.25, 0.35)
    } else if n.contains("brass") || n.contains("bronze") {
        m(780.0, 0.18, 0.4)
    } else if n.contains("stainless") || n.contains("304") || n.contains("316") {
        m(2000.0, 0.21, 0.5)
    } else if n.contains("steel") || n.contains("1018") || n.contains("1045") {
        m(1700.0, 0.25, 0.45)
    } else if n.contains("delrin") || n.contains("pom") || n.contains("acetal") || n.contains("plastic") || n.contains("hdpe") || n.contains("acrylic") {
        m(150.0, 0.25, 0.3)
    } else if n.contains("wood") || n.contains("mdf") {
        m(80.0, 0.25, 0.3)
    } else {
        // unknown: aluminium, the commonest on a small mill
        m(700.0, 0.25, 0.35)
    }
}

/// Young's modulus, N/mm², and a bending stress (MPa) a tool may see at the holder without risk of
/// breaking: carbide is stiff but brittle, HSS bends more but is tougher.
pub fn tool_steel(material: &str) -> (f64, f64) {
    if material.eq_ignore_ascii_case("hss") || material.eq_ignore_ascii_case("cobalt") {
        (210_000.0, 900.0)
    } else {
        (600_000.0, 800.0)
    }
}

/// The tool's side of a cut.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToolGeom {
    pub diameter: f64,
    pub ball: bool,
    pub flutes: u32,
    /// Out of the holder, flute length, shank diameter, mm.
    pub stickout: f64,
    pub flute_length: f64,
    pub shank: f64,
    pub modulus: f64,
    pub stress_limit: f64,
}

/// What the cut does to the tool, machine and work.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize)]
pub struct Cut {
    /// Effective diameter in the cut (a ball's diameter at the depth it is engaged), mm.
    pub d_eff: f64,
    /// Surface speed at d_eff, m/min.
    pub vc: f64,
    /// Feed per tooth, mm; engagement angle, degrees; largest and mean chip thickness, mm.
    pub fz: f64,
    pub angle: f64,
    pub h_max: f64,
    pub h_mean: f64,
    /// Material removal rate, cm³/min.
    pub mrr: f64,
    /// Tangential force at a straight tooth's peak (an upper bound) and on average over a turn, and the
    /// sideways force on the tool at its high point with a helix (tangential and radial together), N.
    pub ft_peak: f64,
    pub ft_mean: f64,
    pub force: f64,
    /// Spindle power, W, and torque, N·m, on average.
    pub power: f64,
    pub torque: f64,
    /// How far the tip bends under the peak force, mm, and the bending stress at the holder, MPa.
    pub deflection: f64,
    pub stress: f64,
}

/// The cut a tool takes `width` mm sideways and `depth` mm deep at `feed` mm/min and `rpm`.
pub fn cut(t: &ToolGeom, m: &Material, width: f64, depth: f64, feed: f64, rpm: f64, area: f64) -> Cut {
    if !(width > 0.0 && depth > 0.0 && rpm > 0.0 && feed > 0.0 && t.diameter > 0.0) {
        return Cut::default();
    }
    let z = t.flutes.max(1) as f64;
    // a ball only cuts with the part of it below `depth`
    let d = if t.ball && depth < t.diameter / 2.0 { 2.0 * (depth * (t.diameter - depth)).sqrt() } else { t.diameter };
    let ae = width.min(d);
    let fz = feed / (rpm * z);
    // the angle of the turn the edge spends in the material (side milling: from where it enters to
    // where it leaves); a slot is half a turn
    let phi = (1.0 - 2.0 * ae / d).clamp(-1.0, 1.0).acos();
    let h_max = if phi < PI / 2.0 { fz * phi.sin() } else { fz };
    let h_mean = if phi > 1e-9 { fz * (1.0 - phi.cos()) / phi } else { 0.0 };
    let kc = |h: f64| m.kc1 * h.max(0.005).powf(-m.mc);
    // teeth in the cut at once: on average z·φ/2π, at the peak the next whole number (at least one)
    let teeth = z * phi / (2.0 * PI);
    let ft_peak = kc(h_max) * depth * h_max * teeth.ceil().max(1.0);
    let ft_mean = kc(h_mean) * depth * h_mean * teeth;
    // the sideways force on the tool: the helix spreads each tooth's cut along the flute, so the force
    // swings about 30% above its mean rather than to the straight-tooth peak (`ft_peak`, kept as an upper bound)
    let force = ft_mean.max(0.25 * ft_peak) * 1.3 * (1.0 + m.radial * m.radial).sqrt();
    let vc = PI * d * rpm / 1000.0;
    let power = ft_mean * vc / 60.0;
    let torque = ft_mean * d / 2000.0;
    // a cantilever from the holder: the shank, then the thinner fluted part (about 80% of the diameter
    // in bending), loaded at the middle of the cut
    let l = (t.stickout - depth / 2.0).max(1.0);
    let lf = t.flute_length.min(l).max(0.0);
    let i = |dia: f64| PI * dia.powi(4) / 64.0;
    let (i_f, i_s) = (i(0.8 * t.diameter), i(t.shank.max(0.1)));
    let deflection = force / (3.0 * t.modulus) * ((l.powi(3) - lf.powi(3)) / i_s + lf.powi(3) / i_f);
    // bending at the holder, or where the flutes start if they are the weaker section
    let stress = (force * l * (t.shank / 2.0) / i_s).max(force * lf * (0.4 * t.diameter) / i_f);
    let mrr = area * feed / 1000.0;
    Cut { d_eff: d, vc, fz, angle: phi.to_degrees(), h_max, h_mean, mrr, ft_peak, ft_mean, force, power, torque, deflection, stress }
}

/// What the machine and tool may take: each limit's use as a share (1 = at the limit).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// Spindle power at the shaft at its rated speed, W (a VFD spindle's rating is its electrical input:
    /// times its efficiency), and that speed (below it the torque stays constant, so the power available
    /// falls with the speed).
    pub power: f64,
    pub rated_rpm: f64,
    /// Share of `power` a cut may never go over (a problem past it) and the share past which it's a caution:
    /// with `power` the spindle's measured pull-out (it stalls there), the planning limits sit under it.
    pub never_exceed: f64,
    pub caution: f64,
    /// The most sideways force the machine takes at the tool, N.
    pub force: f64,
    /// Bend allowed at the tool's tip, mm (roughing; finishing passes report it as a wall error).
    pub deflection: f64,
}

/// Shares of each limit a cut uses, and the worst.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize)]
pub struct Use {
    pub power: f64,
    pub torque: f64,
    pub force: f64,
    pub deflection: f64,
    pub stress: f64,
    /// The thickest chip against what the tool's edge takes (2.5% of its diameter).
    pub chip: f64,
    pub worst: f64,
}

/// `finishing`: the bend is a wall error to report, not a load to hold the cut to.
pub fn use_of(c: &Cut, t: &ToolGeom, l: &Limits, rpm: f64, finishing: bool) -> Use {
    let available = l.power * (rpm / l.rated_rpm.max(1.0)).min(1.0);
    let rated_torque = l.power / (2.0 * PI * l.rated_rpm.max(1.0) / 60.0);
    let power = c.power / available.max(1.0);
    let torque = c.torque / rated_torque.max(1e-6);
    let force = c.force / l.force.max(1.0);
    let deflection = c.deflection / l.deflection.max(1e-6);
    let stress = c.stress / t.stress_limit.max(1.0);
    let chip = c.h_max / (0.025 * t.diameter);
    // power and torque count against the never-exceed share, so 1 is where a cut becomes a problem
    let ne = l.never_exceed.clamp(0.1, 1.0);
    let worst = (power / ne).max(torque / ne).max(force).max(if finishing { 0.0 } else { deflection }).max(stress).max(chip);
    Use { power, torque, force, deflection, stress, chip, worst }
}

/// Chip evacuation, a risk index (not chip physics): chips that stay in the cut lie in the cavity around
/// the tool until the coolant washes them out; packed tight they get re-cut, weld on and break tools.
///
/// Per tool position: the chips made (volume removed × `BULK`), the share that stays (the more walled-in
/// the tool is, and the thinner and lighter the chips, the less is thrown clear), spread over the coarse
/// cells within two diameters; each cell drains at a rate set by the machine's chip clearing, slower the
/// deeper the cavity. Fill = chips in a cell ÷ its free volume (cell area × cavity depth). The constants
/// (`BULK`, `THROW_CHIP`, the clearing rates) are for tuning from what Jonas sees on the machine.
pub struct ChipMap {
    lo: glam::DVec2,
    cell: f64,
    nx: usize,
    ny: usize,
    vol: Vec<f64>,
    depth: Vec<f64>,
    at: Vec<f64>,
    clearing: String,
}

/// Loose chips take this many times the volume of the metal they came from.
pub const BULK: f64 = 12.0;
/// A chip this thick (mean, mm) or thicker is thrown clear of the tool; thinner ones less and less.
pub const THROW_CHIP: f64 = 0.02;
/// Scales the index so that the phone stand's op 1 (ran on the DMC2 with no chip trouble) reads about 0.4.
pub const RISK_SCALE: f64 = 0.635;

impl ChipMap {
    pub fn new(lo: glam::DVec2, hi: glam::DVec2, cell: f64, clearing: String) -> ChipMap {
        let nx = ((hi.x - lo.x) / cell).ceil().max(1.0) as usize + 1;
        let ny = ((hi.y - lo.y) / cell).ceil().max(1.0) as usize + 1;
        ChipMap { lo, cell, nx, ny, vol: vec![0.0; nx * ny], depth: vec![0.0; nx * ny], at: vec![0.0; nx * ny], clearing }
    }

    /// How fast chips leave a cavity `depth` deep for a tool of diameter `d`, share per second.
    fn drain(&self, depth: f64, d: f64, coolant: bool) -> f64 {
        let base = match self.clearing.as_str() {
            "none" => 0.05,
            "air" | "air blast" => 0.6,
            "flood" | "high-pressure flood" => 1.0,
            // low-pressure flood (the DMC2), and the default
            _ => 0.4,
        };
        let base = if coolant || self.clearing == "air" { base } else { 0.05 };
        base * (-depth / (1.5 * d.max(1.0))).exp()
    }

    /// A tool position at `p` removing `removed` mm³, walled in on `conf` of its sides by walls `depth`
    /// high, chips `chip` mm thick: add the chips that stay, drain, and give the fill where the tool is.
    #[allow(clippy::too_many_arguments)]
    pub fn cut(&mut self, p: glam::DVec3, removed: f64, conf: f64, depth: f64, chip: f64, d: f64, coolant: bool, now: f64) -> f64 {
        let throw = (chip / THROW_CHIP).clamp(0.15, 1.0);
        let stay = conf * conf * (1.0 - 0.6 * throw);
        let made = removed * BULK * stay;
        let ci = ((p.x - self.lo.x) / self.cell).round() as i64;
        let cj = ((p.y - self.lo.y) / self.cell).round() as i64;
        let n = (d / self.cell).ceil() as i64;
        let mut cells = vec![];
        for j in cj - n..=cj + n {
            for i in ci - n..=ci + n {
                if i < 0 || j < 0 || i >= self.nx as i64 || j >= self.ny as i64 || (((i - ci).pow(2) + (j - cj).pow(2)) as f64).sqrt() * self.cell > d {
                    continue;
                }
                cells.push(j as usize * self.nx + i as usize);
            }
        }
        if cells.is_empty() {
            return 0.0;
        }
        let share = made / cells.len() as f64;
        let mut here = 0.0f64;
        for k in cells {
            // bring the cell up to date, then add
            let rate = self.drain(self.depth[k].max(depth), d, coolant);
            self.vol[k] *= (-rate * (now - self.at[k]).max(0.0)).exp();
            self.at[k] = now;
            self.vol[k] += share;
            self.depth[k] = if made > 0.0 { depth } else { self.depth[k] };
            // chips in a shallow cavity spill over the edge rather than pack: weight by depth
            let free = self.cell * self.cell * self.depth[k].max(1.0);
            let deep = (self.depth[k] / (1.5 * d.max(1.0))).clamp(0.1, 2.0);
            here = here.max(RISK_SCALE * self.vol[k] / free * deep);
        }
        here
    }
}

/// The tool's first bending mode at its tip, in series with the holder and spindle: stiffness N/mm,
/// natural frequency Hz, damping ratio. Measured values (a tap test) override the computed ones.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Mode {
    pub k: f64,
    pub hz: f64,
    pub zeta: f64,
}

/// The mode from the tool's geometry: a stepped cantilever (shank, then flutes at about 80% of the
/// diameter in bending) of the tool's material, its mass lumped as a uniform cantilever's (0.24 of it at
/// the tip), on a holder/spindle of stiffness `holder_k` N/mm (in series).
pub fn tool_mode(t: &ToolGeom, holder_k: f64, zeta: f64) -> Mode {
    let l = t.stickout.max(1.0);
    let lf = t.flute_length.min(l).max(0.0);
    let i = |dia: f64| PI * dia.powi(4) / 64.0;
    let (i_f, i_s) = (i(0.8 * t.diameter), i(t.shank.max(0.1)));
    let compliance = 1.0 / (3.0 * t.modulus) * ((l.powi(3) - lf.powi(3)) / i_s + lf.powi(3) / i_f);
    let k_tool = 1.0 / compliance.max(1e-12);
    // density, tonnes/mm³ (N·s²/mm per mm³): carbide 14.5 g/cm³, steel 8.0
    let rho = if t.modulus > 400_000.0 { 1.45e-8 } else { 8.0e-9 };
    let area = |dia: f64| PI * dia * dia / 4.0;
    let mass = rho * (area(t.shank) * (l - lf) + area(0.85 * t.diameter) * lf);
    let hz_tool = (k_tool / (0.243 * mass)).sqrt() / (2.0 * PI);
    // the holder in series lowers the stiffness; its mass hardly moves at the tool's mode
    let k = 1.0 / (1.0 / k_tool + 1.0 / holder_k.max(1.0));
    let hz = hz_tool * (k / k_tool).sqrt();
    Mode { k, hz, zeta }
}

/// The deepest stable axial depth (mm) at `rpm` for a cut engaged `width` of a `d` mm tool with `z` teeth,
/// from the zero-order (average-coefficient) milling stability solution (Altintas & Budak 1995), the tool
/// equally flexible in X and Y. `kt`: tangential cutting coefficient, N/mm² (kc at the mean chip);
/// `kr`: radial to tangential. Also the lowest the lobes go at any speed (the unconditionally stable depth).
pub fn stable_depth(m: &Mode, d: f64, width: f64, z: u32, kt: f64, kr: f64, rpm: f64, climb: bool) -> (f64, f64) {
    let n_teeth = z.max(1) as f64;
    let ae = width.clamp(0.0, d);
    // entry and exit angles (climb: the tooth enters at the cut's edge and leaves at the wall)
    let a = (1.0 - 2.0 * ae / d).clamp(-1.0, 1.0).acos();
    let (st, ex) = if climb { (PI - a, PI) } else { (0.0, a) };
    let f = |ph: f64| {
        let (c2, s2) = ((2.0 * ph).cos(), (2.0 * ph).sin());
        [0.5 * (c2 - 2.0 * kr * ph + kr * s2), 0.5 * (-s2 - 2.0 * ph + kr * c2), 0.5 * (-s2 + 2.0 * ph + kr * c2), 0.5 * (-c2 - 2.0 * kr * ph - kr * s2)]
    };
    let (fe, fs) = (f(ex), f(st));
    let [axx, axy, ayx, ayy] = [fe[0] - fs[0], fe[1] - fs[1], fe[2] - fs[2], fe[3] - fs[3]];
    let det = axx * ayy - axy * ayx;
    let tr = axx + ayy;
    if det.abs() < 1e-12 && tr.abs() < 1e-12 {
        return (f64::INFINITY, f64::INFINITY);
    }
    let wn = 2.0 * PI * m.hz;
    let mut best = f64::INFINITY;
    let mut floor = f64::INFINITY;
    // the eigenvalue of the oriented transfer function: Λ = -λ0 / G, λ0 = (tr ± sqrt(tr² - 4 det)) / (2 det)
    let disc = num_complex_sqrt(tr * tr - 4.0 * det);
    for sign in [1.0, -1.0] {
        let lam0 = if det.abs() > 1e-12 { ((tr + sign * disc.0) / (2.0 * det), sign * disc.1 / (2.0 * det)) } else { (1.0 / tr, 0.0) };
        // previous point on each lobe, to interpolate where it crosses `rpm`
        let mut prev: Vec<Option<(f64, f64)>> = vec![None; 40];
        let steps = 1500;
        for i in 0..=steps {
            let r = 1.0 + 1.5 * i as f64 / steps as f64;
            let wc = r * wn;
            // 1/G = k (1 - r² + 2iζr)
            let (gr, gi) = (m.k * (1.0 - r * r), m.k * 2.0 * m.zeta * r);
            let (lr, li) = (-(lam0.0 * gr - lam0.1 * gi), -(lam0.0 * gi + lam0.1 * gr));
            if lr >= 0.0 {
                continue;
            }
            let kappa = li / lr;
            let ap = -2.0 * PI * lr / (n_teeth * kt) * (1.0 + kappa * kappa);
            floor = floor.min(ap);
            let eps = PI - 2.0 * kappa.atan();
            for (k, p) in prev.iter_mut().enumerate() {
                let n = 60.0 * wc / (n_teeth * (eps + 2.0 * PI * k as f64));
                if let Some((n0, a0)) = *p {
                    if (n0 - rpm) * (n - rpm) <= 0.0 && (n - n0).abs() > 1e-9 {
                        best = best.min(a0 + (ap - a0) * (rpm - n0) / (n - n0));
                    }
                }
                *p = Some((n, ap));
            }
        }
    }
    // between lobes (no lobe crosses this speed): stable up to the lobes' tops, take the floor's few times
    (if best.is_finite() { best } else { floor * 4.0 }, floor)
}

/// The computed lobes, with the placeholder holder stiffness and damping, put every cut the DMC2 has made
/// cleanly (the phone stand's op 1 at worst 11–12× its lobes' stable depth) deep in chatter: the real
/// tool-point response is far better damped or stiffer than assumed. Until a tap test, the margin is
/// scaled so those clean cuts read about 0.9: an index relative to proven cuts, not an absolute prediction.
pub const CHATTER_SCALE: f64 = 0.078;

/// sqrt of a real number as a complex (re, im).
fn num_complex_sqrt(x: f64) -> (f64, f64) {
    if x >= 0.0 {
        (x.sqrt(), 0.0)
    } else {
        (0.0, (-x).sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The DMC2's 1/4" 3-flute carbide end mill, 24 out of the holder, at 24000 rpm and 2400 mm/min.
    fn t24() -> ToolGeom {
        ToolGeom { diameter: 6.35, ball: false, flutes: 3, stickout: 24.0, flute_length: 19.05, shank: 6.35, modulus: 600_000.0, stress_limit: 800.0 }
    }

    fn dmc2() -> Limits {
        // the measured pull-out at 24000 (the stick-out gauge's broken-tool run): 645 W by this model's watts
        Limits { power: 645.0, rated_rpm: 24000.0, never_exceed: 0.85, caution: 0.775, force: 700.0, deflection: 0.06 }
    }

    #[test]
    fn chip_thinning_and_a_slot() {
        let m = material("6061-T6");
        // a slot: half a turn in the cut, the chip as thick as the feed per tooth
        let c = cut(&t24(), &m, 6.35, 1.0, 2400.0, 24000.0, 6.35);
        assert!((c.angle - 180.0).abs() < 1e-6 && (c.h_max - 2400.0 / 72000.0).abs() < 1e-9, "{c:?}");
        // a light step: a much thinner chip than the feed per tooth
        let c = cut(&t24(), &m, 0.3, 10.0, 2400.0, 24000.0, 3.0);
        assert!(c.h_max < 0.5 * c.fz && c.angle < 30.0, "{c:?}");
        // power is force times speed: MRR × kc to within the mean-chip approximation
        let c = cut(&t24(), &m, 3.0, 3.0, 2400.0, 24000.0, 9.0);
        let p = c.mrr * 1000.0 / 60.0 * m.kc1 * c.h_mean.powf(-m.mc) / 1000.0;
        assert!((c.power - p).abs() / p < 0.05, "{} vs {p}", c.power);
    }

    /// The DMC2's spindle on the 1/4" 3-flute at 24000 rpm, 6061, from the stick-out gauge's broken-tool run
    /// (2026-11-17, the speed heard from the spindle head): 6 deep at F2400, 0.6 wide sagged 1.8% (steady),
    /// 1.0 wide 3.2% (steady), 1.4 wide never settled (7.5%: at pull-out), 1.8 wide ran away and broke the tool.
    /// Jonas: 0.6 x 10 at 2500 comfortable, 0.6 x 12 at 2500 on the edge. The model's 100% is that pull-out
    /// (645 W of its watts), 550 W (85%) is never to be exceeded, 500 W is a caution.
    #[test]
    fn calibrated_on_the_dmc2() {
        let m = material("6061-T6");
        let l = dmc2();
        let at = |w: f64, d: f64, f: f64| use_of(&cut(&t24(), &m, w, d, f, 24000.0, w * d), &t24(), &l, 24000.0, false);
        let (c1, c2, c3, c4) = (at(0.6, 6.0, 2400.0), at(1.0, 6.0, 2400.0), at(1.4, 6.0, 2400.0), at(1.8, 6.0, 2400.0));
        // steady cuts stay under the caution, the one at pull-out is a problem, the one that broke is far over
        assert!(c1.worst < 0.75 && c2.worst < 0.95, "{c1:?} {c2:?}");
        assert!(c3.power > 0.9 && c3.worst > 1.0, "{c3:?}");
        assert!(c4.power > 1.2, "{c4:?}");
        // Jonas's cuts: 0.6 x 10 comfortable (under the never-exceed), 0.6 x 12 on the edge (at pull-out)
        let (ten, twelve) = (at(0.6, 10.0, 2500.0), at(0.6, 12.0, 2500.0));
        assert!(ten.worst < 1.0 && twelve.power > 0.95, "{ten:?} {twelve:?}");
    }

    /// Chips pile up in a deep pocket, not on an open edge; thin chips stay more than thick ones; coolant
    /// washes them out.
    #[test]
    fn chips_pile_up_in_deep_pockets() {
        let run = |conf: f64, depth: f64, chip: f64, coolant: bool| {
            let mut m = ChipMap::new(glam::DVec2::ZERO, glam::DVec2::new(40.0, 40.0), 2.0, "flood-low".into());
            let mut fill = 0.0;
            // 30 s cutting 5 mm³ per 0.01 s along a 20 mm line, back and forth
            for k in 0..3000 {
                let x = 10.0 + (k % 200) as f64 * 0.1;
                fill = m.cut(glam::DVec3::new(x, 20.0, 0.0), 5.0, conf, depth, chip, 6.35, coolant, k as f64 * 0.01);
            }
            fill
        };
        let (open, pocket) = (run(0.2, 2.0, 0.02, true), run(0.9, 20.0, 0.02, true));
        assert!(pocket > 5.0 * open, "{open} {pocket}");
        assert!(run(0.9, 20.0, 0.005, true) > run(0.9, 20.0, 0.03, true));
        assert!(run(0.9, 5.0, 0.01, false) > run(0.9, 5.0, 0.01, true));
    }

    /// A 1/4" carbide end mill 24 out: a first mode in the low kHz, stiffness a few N/µm; lobes give a
    /// deeper stable cut for a light stepover than for a slot, and a shorter stick-out is stiffer.
    #[test]
    fn chatter_lobes() {
        let m = tool_mode(&t24(), 20_000.0, 0.03);
        assert!(m.hz > 1500.0 && m.hz < 8000.0 && m.k > 1000.0 && m.k < 20_000.0, "{m:?}");
        let kt = 700.0 * 0.02f64.powf(-0.25);
        let (slot, _) = stable_depth(&m, 6.35, 6.35, 3, kt, 0.35, 24000.0, true);
        let (light, _) = stable_depth(&m, 6.35, 0.6, 3, kt, 0.35, 24000.0, true);
        assert!(light > slot, "{light} {slot}");
        let short = tool_mode(&ToolGeom { stickout: 19.0, ..t24() }, 20_000.0, 0.03);
        assert!(short.k > m.k && short.hz > m.hz, "{short:?} {m:?}");
    }

    #[test]
    fn a_ball_cuts_with_a_smaller_diameter_near_its_tip() {
        let ball = ToolGeom { ball: true, diameter: 6.0, flutes: 2, modulus: 210_000.0, stress_limit: 900.0, stickout: 21.0, flute_length: 18.0, shank: 6.0 };
        let c = cut(&ball, &material("6061"), 0.5, 0.3, 800.0, 10000.0, 0.15);
        assert!(c.d_eff < 3.0 && c.vc < 0.5 * PI * 6.0 * 10.0, "{c:?}");
    }

    #[test]
    fn a_longer_tool_bends_more() {
        let m = material("6061");
        let short = cut(&t24(), &m, 1.0, 5.0, 2400.0, 24000.0, 5.0);
        let long = cut(&ToolGeom { stickout: 40.0, ..t24() }, &m, 1.0, 5.0, 2400.0, 24000.0, 5.0);
        assert!(long.deflection > 3.0 * short.deflection, "{short:?} {long:?}");
    }
}
