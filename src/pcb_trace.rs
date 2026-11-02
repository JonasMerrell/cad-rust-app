//! Trace solver: how much current a copper track carries and how hot it gets, its resistance and
//! voltage drop, and its characteristic impedance as a microstrip, stripline or differential pair.
//!
//! The formulas are pure functions of the track and the stackup so the calculator, the per-net
//! report and the design rule check all share them. Units: millimetres, amps, ohms, °C.
//!
//! - Current and temperature rise: IPC-2221 (`I = k·ΔT^0.44·A^0.725`, A in mil², k = 0.048 for
//!   outer layers and 0.024 for inner ones). It is the conservative closed form that most
//!   calculators use; IPC-2152's charts usually allow somewhat more current on outer layers.
//! - Resistance: copper at 1.724 µΩ·cm and 20 °C with a 0.393 %/°C temperature coefficient.
//! - Microstrip: Hammerstad and Jensen (1980) with their copper thickness correction, good to
//!   about 1 % for 0.01 < w/h < 100.
//! - Stripline (centred between two planes): Wheeler (1978) with thickness correction.
//! - Edge-coupled pairs: the IPC-2141 coupling factors on top of the single-ended impedance.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::pcb::{self, Layer, Netlist, Pcb, WPad};
use crate::pcb_lib::P;

/// Copper thickness of one ounce per square foot, in mm.
pub const OZ_MM: f64 = 0.03479;
/// Copper resistivity at 20 °C, Ω·mm.
pub const RHO: f64 = 1.724e-5;
/// Resistance temperature coefficient of copper, per °C.
pub const ALPHA: f64 = 0.00393;
const MIL2_PER_MM2: f64 = 1550.0031;
/// Light in vacuum, mm per picosecond.
const C_MM_PS: f64 = 0.299_792_458;

// ---------------------------------------------------------------- stackup

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum StackKind {
    Copper,
    Core,
    Prepreg,
    Mask,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StackLayer {
    pub name: String,
    pub kind: StackKind,
    /// mm
    pub thickness: f64,
    /// Relative permittivity of a dielectric layer.
    pub er: f64,
    /// Copper weight of a copper layer, oz/ft².
    pub copper_oz: f64,
}

impl Default for StackLayer {
    fn default() -> Self {
        StackLayer { name: String::new(), kind: StackKind::Core, thickness: 0.2, er: 4.5, copper_oz: 0.0 }
    }
}

/// Round to a micrometre's thousandth so thicknesses survive a save.
fn um(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// The board's layers, listed top to bottom.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Stackup {
    pub layers: Vec<StackLayer>,
}

impl Default for Stackup {
    fn default() -> Self {
        Stackup::two_layer(1.6, 1.0)
    }
}

impl Stackup {
    /// Two copper layers of `oz` on an FR4 core, `total` mm thick over the copper.
    pub fn two_layer(total: f64, oz: f64) -> Stackup {
        let cu = oz * OZ_MM;
        let mask = |n: &str| StackLayer { name: n.into(), kind: StackKind::Mask, thickness: 0.02, er: 3.5, copper_oz: 0.0 };
        let copper = |n: &str| StackLayer { name: n.into(), kind: StackKind::Copper, thickness: cu, er: 1.0, copper_oz: oz };
        Stackup {
            layers: vec![
                mask("Top mask"),
                copper("Top"),
                StackLayer { name: "Core".into(), kind: StackKind::Core, thickness: um((total - 2.0 * cu).max(0.05)), er: 4.5, copper_oz: 0.0 },
                copper("Bottom"),
                mask("Bottom mask"),
            ],
        }
    }

    /// Indices of the copper layers, top first.
    pub fn coppers(&self) -> Vec<usize> {
        self.layers.iter().enumerate().filter(|(_, l)| l.kind == StackKind::Copper).map(|(i, _)| i).collect()
    }

    /// The stack index of a board copper layer.
    pub fn copper_index(&self, l: Layer) -> Option<usize> {
        let c = self.coppers();
        match l {
            Layer::Top => c.first().copied(),
            Layer::Bottom => c.last().copied(),
        }
    }

    /// Copper thickness of a board layer, mm.
    pub fn copper_mm(&self, l: Layer) -> f64 {
        self.copper_index(l).map(|i| self.layers[i].copper_oz * OZ_MM).filter(|t| *t > 0.0).unwrap_or(OZ_MM)
    }

    /// Copper weight of a board layer, oz.
    pub fn copper_oz(&self, l: Layer) -> f64 {
        self.copper_index(l).map(|i| self.layers[i].copper_oz).filter(|t| *t > 0.0).unwrap_or(1.0)
    }

    fn dielectric(&self, idx: usize, down: bool) -> Option<(f64, f64)> {
        // thickness and thickness-weighted er of the dielectrics between this copper and the next
        let mut h = 0.0;
        let mut her = 0.0;
        let mut i = idx as isize;
        loop {
            i += if down { 1 } else { -1 };
            let l = self.layers.get(usize::try_from(i).ok()?)?;
            match l.kind {
                StackKind::Copper => break,
                StackKind::Mask => return None,
                _ => {
                    h += l.thickness;
                    her += l.thickness * l.er;
                }
            }
        }
        (h > 0.0).then(|| (h, her / h))
    }

    /// Dielectric thickness and permittivity between a copper layer and the next copper below it.
    pub fn dielectric_below(&self, idx: usize) -> Option<(f64, f64)> {
        self.dielectric(idx, true)
    }

    /// Dielectric thickness and permittivity between a copper layer and the next copper above it.
    pub fn dielectric_above(&self, idx: usize) -> Option<(f64, f64)> {
        self.dielectric(idx, false)
    }

    /// Whether a copper layer is an outer one (a microstrip over the next plane).
    pub fn is_outer(&self, idx: usize) -> bool {
        let c = self.coppers();
        c.first() == Some(&idx) || c.last() == Some(&idx)
    }

    /// The transmission line geometry of a track on a board layer, with the reference plane
    /// taken as the nearest copper layer.
    pub fn line(&self, l: Layer) -> Geometry {
        let Some(i) = self.copper_index(l) else { return Geometry::default() };
        let t = self.copper_mm(l);
        let (h, er) = if l == Layer::Top { self.dielectric_below(i) } else { self.dielectric_above(i) }.unwrap_or((1.51, 4.5));
        Geometry { kind: LineKind::Microstrip, h, h2: 0.0, er, t }
    }

    /// The total thickness, mm.
    pub fn total(&self) -> f64 {
        self.layers.iter().filter(|l| l.kind != StackKind::Mask).map(|l| l.thickness).sum()
    }

    /// Change the board thickness by growing or shrinking the dielectrics.
    pub fn set_total(&mut self, total: f64) {
        let cu: f64 = self.layers.iter().filter(|l| l.kind == StackKind::Copper).map(|l| l.thickness).sum();
        let d: f64 = self.layers.iter().filter(|l| matches!(l.kind, StackKind::Core | StackKind::Prepreg)).map(|l| l.thickness).sum();
        let want = (total - cu).max(0.05);
        if d <= 0.0 {
            return;
        }
        for l in self.layers.iter_mut().filter(|l| matches!(l.kind, StackKind::Core | StackKind::Prepreg)) {
            l.thickness = um(l.thickness * want / d);
        }
    }

    /// Set the copper weight of every copper layer (outer: the first and last).
    pub fn set_copper(&mut self, outer: Option<f64>, inner: Option<f64>) {
        let c = self.coppers();
        for (k, &i) in c.iter().enumerate() {
            let out = k == 0 || k + 1 == c.len();
            if let Some(oz) = if out { outer } else { inner } {
                self.layers[i].copper_oz = oz;
                self.layers[i].thickness = um(oz * OZ_MM);
            }
        }
    }
}

/// The board's stackup, scaled to its thickness if the two disagree (the thickness is set on
/// its own in the board settings).
pub fn stack(b: &pcb::Board) -> Stackup {
    let mut s = b.stackup.clone();
    if s.coppers().len() < 2 {
        s = Stackup::two_layer(b.thickness, 1.0);
    }
    if (s.total() - b.thickness).abs() > 1e-3 {
        s.set_total(b.thickness);
    }
    s
}

// ---------------------------------------------------------------- current and heating

/// The IPC-2221 constant: outer layers shed heat into the air, inner ones through the board.
fn k(outer: bool) -> f64 {
    if outer { 0.048 } else { 0.024 }
}

/// Current (A) a track of `w` × `t` mm carries for a `dt` °C rise.
pub fn current_for(w: f64, t: f64, dt: f64, outer: bool) -> f64 {
    let a = (w * t * MIL2_PER_MM2).max(0.0);
    k(outer) * dt.max(0.0).powf(0.44) * a.powf(0.725)
}

/// Temperature rise (°C) of a track of `w` × `t` mm carrying `i` A.
pub fn temp_rise(w: f64, t: f64, i: f64, outer: bool) -> f64 {
    let a = (w * t * MIL2_PER_MM2).max(1e-12);
    (i.abs() / (k(outer) * a.powf(0.725))).powf(1.0 / 0.44)
}

/// Width (mm) a track of `t` mm copper needs to carry `i` A with at most `dt` °C rise.
pub fn width_for_current(i: f64, t: f64, dt: f64, outer: bool) -> f64 {
    if i <= 0.0 || t <= 0.0 || dt <= 0.0 {
        return 0.0;
    }
    let a = (i / (k(outer) * dt.powf(0.44))).powf(1.0 / 0.725);
    a / MIL2_PER_MM2 / t
}

/// Resistance (Ω) of `len` mm of `w` × `t` mm copper at `temp` °C.
pub fn resistance(len: f64, w: f64, t: f64, temp: f64) -> f64 {
    if w <= 0.0 || t <= 0.0 {
        return f64::INFINITY;
    }
    RHO * len / (w * t) * (1.0 + ALPHA * (temp - 20.0))
}

/// Resistance (Ω) of a plated via barrel: `drill` mm hole, `plating` mm wall, `len` mm long.
pub fn via_resistance(drill: f64, plating: f64, len: f64, temp: f64) -> f64 {
    let ro = drill / 2.0;
    let ri = (ro - plating).max(0.0);
    let a = std::f64::consts::PI * (ro * ro - ri * ri);
    if a <= 0.0 {
        return f64::INFINITY;
    }
    RHO * len / a * (1.0 + ALPHA * (temp - 20.0))
}

/// Typical via plating thickness, mm (25 µm, IPC class 2 average).
pub const VIA_PLATING: f64 = 0.025;

// ---------------------------------------------------------------- impedance

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum LineKind {
    /// On an outer layer over a plane.
    #[default]
    Microstrip,
    /// Centred between two planes.
    Stripline,
}

/// Where a track runs: the dielectric to its plane (h; for a stripline the distance to each
/// plane is h and h2, or h on both sides when h2 is 0), the permittivity and copper thickness.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Geometry {
    pub kind: LineKind,
    pub h: f64,
    pub h2: f64,
    pub er: f64,
    pub t: f64,
}

impl Default for Geometry {
    fn default() -> Self {
        Geometry { kind: LineKind::Microstrip, h: 1.51, h2: 0.0, er: 4.5, t: OZ_MM }
    }
}

/// Characteristic impedance and how fast a signal travels on a line.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq)]
pub struct Line {
    pub z0: f64,
    pub er_eff: f64,
    /// ps per mm
    pub delay: f64,
}

fn hj_z01(u: f64) -> f64 {
    let f = 6.0 + (2.0 * std::f64::consts::PI - 6.0) * (-(30.666 / u).powf(0.7528)).exp();
    60.0 * (f / u + (1.0 + 4.0 / (u * u)).sqrt()).ln()
}

fn hj_eeff(u: f64, er: f64) -> f64 {
    let a = 1.0 + ((u.powi(4) + (u / 52.0).powi(2)) / (u.powi(4) + 0.432)).ln() / 49.0 + (1.0 + (u / 18.1).powi(3)).ln() / 18.7;
    let b = 0.564 * ((er - 0.9) / (er + 3.0)).powf(0.053);
    (er + 1.0) / 2.0 + (er - 1.0) / 2.0 * (1.0 + 10.0 / u).powf(-a * b)
}

/// A microstrip of width `w` on `h` of dielectric `er`, copper `t` thick (Hammerstad-Jensen).
pub fn microstrip(w: f64, h: f64, er: f64, t: f64) -> Line {
    let u = (w / h).max(1e-6);
    let (u1, ur) = if t > 0.0 {
        let tn = t / h;
        let coth = 1.0 / (6.517 * u).sqrt().tanh();
        let du1 = tn / std::f64::consts::PI * (1.0 + 4.0 * std::f64::consts::E / (tn * coth * coth)).ln();
        let dur = 0.5 * (1.0 + 1.0 / (er - 1.0).max(0.0).sqrt().cosh()) * du1;
        (u + du1, u + dur)
    } else {
        (u, u)
    };
    let ee_r = hj_eeff(ur, er);
    let z0 = hj_z01(ur) / ee_r.sqrt();
    let er_eff = ee_r * (hj_z01(u1) / hj_z01(ur)).powi(2);
    Line { z0, er_eff, delay: er_eff.sqrt() / C_MM_PS }
}

/// A symmetric stripline of width `w` between planes `b` apart, copper `t` thick (Wheeler).
fn stripline_sym(w: f64, b: f64, er: f64, t: f64) -> f64 {
    let pi = std::f64::consts::PI;
    let x = (t / b).clamp(0.0, 0.9);
    let wp = if x > 0.0 {
        let m = 2.0 / (1.0 + 2.0 * x / (3.0 * (1.0 - x)));
        let dw = (b - t) * x / (pi * (1.0 - x)) * (1.0 - 0.5 * ((x / (2.0 - x)).powi(2) + (0.0796 * x / (w / b + 1.1 * x)).powf(m)).ln());
        w + dw
    } else {
        w
    };
    let k = 4.0 * (b - t) / (pi * wp.max(1e-9));
    30.0 / er.sqrt() * (1.0 + k * (2.0 * k + (4.0 * k * k + 6.27).sqrt())).ln()
}

/// A stripline `h` and `h2` from its two planes (h2 = 0: centred).
pub fn stripline(w: f64, h: f64, h2: f64, er: f64, t: f64) -> Line {
    let h2 = if h2 > 0.0 { h2 } else { h };
    let z0 = if (h - h2).abs() < 1e-9 {
        stripline_sym(w, 2.0 * h + t, er, t)
    } else {
        // off centre: the parallel combination of the two symmetric lines (Cohn's approximation)
        let a = stripline_sym(w, 2.0 * h + t, er, t);
        let b = stripline_sym(w, 2.0 * h2 + t, er, t);
        2.0 * a * b / (a + b)
    };
    Line { z0, er_eff: er, delay: er.sqrt() / C_MM_PS }
}

impl Geometry {
    pub fn line(&self, w: f64) -> Line {
        match self.kind {
            LineKind::Microstrip => microstrip(w, self.h, self.er, self.t),
            LineKind::Stripline => stripline(w, self.h, self.h2, self.er, self.t),
        }
    }

    /// Differential impedance of an edge-coupled pair `s` mm apart (IPC-2141 coupling).
    pub fn diff(&self, w: f64, s: f64) -> f64 {
        let z = self.line(w).z0;
        match self.kind {
            LineKind::Microstrip => 2.0 * z * (1.0 - 0.48 * (-0.96 * s / self.h).exp()),
            LineKind::Stripline => {
                let b = self.h + if self.h2 > 0.0 { self.h2 } else { self.h } + self.t;
                2.0 * z * (1.0 - 0.347 * (-2.9 * s / b).exp())
            }
        }
    }

    /// The width that gives `z` Ω single ended, if one between 1 µm and 50 mm does.
    pub fn width_for(&self, z: f64) -> Option<f64> {
        solve_down(|w| self.line(w).z0, z)
    }

    /// The width at spacing `s` that gives `z` Ω differential.
    pub fn diff_width_for(&self, z: f64, s: f64) -> Option<f64> {
        solve_down(|w| self.diff(w, s), z)
    }

    /// The spacing at width `w` that gives `z` Ω differential.
    pub fn diff_gap_for(&self, z: f64, w: f64) -> Option<f64> {
        // impedance rises with the gap, so it falls with 1 / gap
        solve_down(|x| self.diff(w, 1.0 / x), z).map(|x| 1.0 / x)
    }
}

/// Solve f(x) = target for x in [0.001, 50] where f falls as x grows, by bisection on log x.
fn solve_down(f: impl Fn(f64) -> f64, target: f64) -> Option<f64> {
    let (mut lo, mut hi) = (0.001f64.ln(), 50f64.ln());
    if !(f(lo.exp()) >= target && f(hi.exp()) <= target) {
        return None;
    }
    for _ in 0..80 {
        let m = 0.5 * (lo + hi);
        if f(m.exp()) > target {
            lo = m;
        } else {
            hi = m;
        }
    }
    Some((0.5 * (lo + hi)).exp())
}

// ---------------------------------------------------------------- the calculator

/// Everything the trace calculator works from.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Calc {
    pub width: f64,
    pub copper_oz: f64,
    /// Outer layer (in air) or inner (in the board), for heating.
    pub outer: bool,
    pub current: f64,
    pub length: f64,
    /// Allowed temperature rise, °C.
    pub temp_rise: f64,
    pub ambient: f64,
    pub geometry: Geometry,
    /// Gap of a differential pair, mm (0: single ended only).
    pub gap: f64,
}

impl Default for Calc {
    fn default() -> Self {
        Calc { width: 0.25, copper_oz: 1.0, outer: true, current: 1.0, length: 50.0, temp_rise: 10.0, ambient: 25.0, geometry: Geometry::default(), gap: 0.2 }
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct CalcOut {
    /// Current for the allowed rise, A.
    pub max_current: f64,
    /// Temperature rise at the given current, °C.
    pub rise: f64,
    /// Width for the current at the allowed rise, mm.
    pub width_for_current: f64,
    /// Ω, at the track's running temperature.
    pub resistance: f64,
    pub drop: f64,
    /// W
    pub power: f64,
    pub z0: f64,
    pub er_eff: f64,
    /// ps/mm
    pub delay: f64,
    pub zdiff: f64,
}

impl Calc {
    pub fn t(&self) -> f64 {
        self.copper_oz * OZ_MM
    }

    pub fn solve(&self) -> CalcOut {
        let t = self.t();
        let rise = temp_rise(self.width, t, self.current, self.outer);
        let r = resistance(self.length, self.width, t, self.ambient + rise);
        let g = Geometry { t, ..self.geometry };
        let line = g.line(self.width);
        CalcOut {
            max_current: current_for(self.width, t, self.temp_rise, self.outer),
            rise,
            width_for_current: width_for_current(self.current, t, self.temp_rise, self.outer),
            resistance: r,
            drop: r * self.current,
            power: r * self.current * self.current,
            z0: line.z0,
            er_eff: line.er_eff,
            delay: line.delay,
            zdiff: if self.gap > 0.0 { g.diff(self.width, self.gap) } else { 0.0 },
        }
    }
}

// ---------------------------------------------------------------- the board's nets

/// What each net asks of its tracks: the current it carries and its impedance (0: none).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Target {
    pub current: f64,
    pub impedance: f64,
}

/// A net's current and impedance targets, from its net class.
pub fn target(pcb: &Pcb, nl: &Netlist, net: &str) -> Target {
    let _ = nl;
    pcb.board.rules.class_of(net).map(|c| Target { current: c.current.max(0.0), impedance: c.impedance.max(0.0) }).unwrap_or_default()
}

/// One net's copper, solved.
#[derive(Clone, Debug, Default, Serialize)]
pub struct NetReport {
    pub net: String,
    pub current: f64,
    pub impedance: f64,
    pub tracks: usize,
    pub vias: usize,
    /// Total track length, mm.
    pub length: f64,
    pub min_width: f64,
    pub max_width: f64,
    /// The worst pad-to-pad resistance along the tracks and vias, Ω (pours not counted).
    pub resistance: f64,
    /// Between these two pads ("R1.2").
    pub worst: (String, String),
    /// Voltage drop at the net's current over that path, V.
    pub drop: f64,
    /// Hottest track's rise at the net's current, °C.
    pub rise: f64,
    /// Width the net's current needs at the allowed rise, mm (0: no current set).
    pub need_width: f64,
    /// Impedance range of its tracks (single ended), Ω.
    pub z_min: f64,
    pub z_max: f64,
    /// Width its impedance target needs on the top layer, mm.
    pub z_width: f64,
    /// Some pads are joined by a pour, so the resistance leaves those paths out.
    pub poured: bool,
    /// Some of its pads are not joined by tracks yet.
    pub open: bool,
}

/// Running temperature for resistances, °C.
pub const AMBIENT: f64 = 25.0;

/// Solve every net that has tracks or a target. `target` gives each net's current and impedance.
pub fn nets(pcb: &Pcb, nl: &Netlist, pads: &[WPad], target: &dyn Fn(&str) -> Target) -> Vec<NetReport> {
    let b = &pcb.board;
    let st = &stack(b);
    let dt = b.rules.temp_rise;
    let mut out = vec![];
    for (ni, n) in nl.nets.iter().enumerate() {
        let tg = target(&n.name);
        let tracks: Vec<&pcb::Track> = b.tracks.iter().filter(|t| t.net == n.name).collect();
        if tracks.is_empty() && tg.current <= 0.0 && tg.impedance <= 0.0 {
            continue;
        }
        let vias: Vec<&pcb::Via> = b.vias.iter().filter(|v| v.net == n.name).collect();
        let mut r = NetReport { net: n.name.clone(), current: tg.current, impedance: tg.impedance, tracks: tracks.len(), vias: vias.len(), ..Default::default() };
        r.min_width = tracks.iter().map(|t| t.width).fold(f64::INFINITY, f64::min);
        r.max_width = tracks.iter().map(|t| t.width).fold(0.0, f64::max);
        if tracks.is_empty() {
            r.min_width = 0.0;
        }
        r.length = tracks.iter().map(|t| pcb::dist(t.a, t.b)).sum();
        r.z_min = f64::INFINITY;
        for t in &tracks {
            let th = st.copper_mm(t.layer);
            r.rise = r.rise.max(temp_rise(t.width, th, tg.current, true));
            let z = st.line(t.layer).line(t.width).z0;
            r.z_min = r.z_min.min(z);
            r.z_max = r.z_max.max(z);
        }
        if tracks.is_empty() {
            r.z_min = 0.0;
        }
        if tg.current > 0.0 {
            let th = st.copper_mm(Layer::Top).min(st.copper_mm(Layer::Bottom));
            r.need_width = width_for_current(tg.current, th, dt, true);
        }
        if tg.impedance > 0.0 {
            r.z_width = st.line(Layer::Top).width_for(tg.impedance).unwrap_or(0.0);
        }
        r.poured = b.zones.iter().any(|z| z.net == n.name);
        let my_pads: Vec<usize> = pads.iter().enumerate().filter(|(_, p)| p.net == Some(ni)).map(|(i, _)| i).collect();
        let (res, worst, open) = worst_path(pcb, pads, &my_pads, &tracks, &vias, AMBIENT + r.rise);
        r.resistance = res;
        r.worst = worst;
        r.open = open;
        r.drop = res * tg.current;
        out.push(r);
    }
    out
}

/// The resistance network of a net: nodes are track ends, pads and vias; the worst pad-to-pad
/// resistance and whether some pads are not reached.
fn worst_path(pcb: &Pcb, pads: &[WPad], my: &[usize], tracks: &[&pcb::Track], vias: &[&pcb::Via], temp: f64) -> (f64, (String, String), bool) {
    let st = &stack(&pcb.board);
    // nodes: each pad, each via, then the track ends (merged when they meet)
    let mut nodes: Vec<(P, Option<Layer>)> = vec![];
    let mut adj: Vec<Vec<(usize, f64)>> = vec![];
    let add = |nodes: &mut Vec<(P, Option<Layer>)>, adj: &mut Vec<Vec<(usize, f64)>>, p: P, l: Option<Layer>| {
        nodes.push((p, l));
        adj.push(vec![]);
        nodes.len() - 1
    };
    let pad_nodes: Vec<usize> = my.iter().map(|&i| add(&mut nodes, &mut adj, pads[i].at, None)).collect();
    let via_nodes: Vec<usize> = vias.iter().map(|v| add(&mut nodes, &mut adj, v.at, None)).collect();
    let mut ends: HashMap<(i64, i64, u8), usize> = HashMap::new();
    let key = |p: P, l: Layer| ((p[0] * 1000.0).round() as i64, (p[1] * 1000.0).round() as i64, l as u8);
    let mut edges: Vec<(usize, usize, f64)> = vec![];
    for t in tracks {
        let mut node = |p: P| -> usize {
            *ends.entry(key(p, t.layer)).or_insert_with(|| {
                nodes.push((p, Some(t.layer)));
                adj.push(vec![]);
                nodes.len() - 1
            })
        };
        let (a, b) = (node(t.a), node(t.b));
        let th = st.copper_mm(t.layer);
        edges.push((a, b, resistance(pcb::dist(t.a, t.b), t.width, th, temp)));
    }
    // track ends join pads and vias they land in, and other tracks' middles they touch
    let thick = pcb.board.thickness;
    for (k, v) in vias.iter().enumerate() {
        edges.push((via_nodes[k], via_nodes[k], 0.0));
        let barrel = via_resistance(v.drill, VIA_PLATING, thick, temp);
        for (ni, (p, l)) in nodes.iter().enumerate() {
            if l.is_some() && pcb::dist(*p, v.at) <= v.d / 2.0 + 1e-6 {
                // half the barrel to each layer: a top-to-bottom hop costs the whole barrel
                edges.push((via_nodes[k], ni, barrel / 2.0));
            }
        }
    }
    for (k, &pi) in my.iter().enumerate() {
        let pad = &pads[pi];
        let cu = pad.cu();
        for (ni, (p, l)) in nodes.iter().enumerate() {
            if let Some(l) = l {
                if pad.on(*l) && (cu.contains(*p) || pcb::dist(*p, pad.at) < 1e-6) {
                    edges.push((pad_nodes[k], ni, 0.0));
                }
            }
        }
    }
    // a track end on another track's middle (T junction)
    let n_nodes = nodes.len();
    for ni in 0..n_nodes {
        let (p, Some(l)) = nodes[ni] else { continue };
        for t in tracks.iter().filter(|t| t.layer == l) {
            if pcb::seg_point(t.a, t.b, p) <= t.width / 2.0 + 1e-6 && pcb::dist(p, t.a) > 1e-6 && pcb::dist(p, t.b) > 1e-6 {
                // split: from the junction to each end
                let th = st.copper_mm(l);
                let ja = ends.get(&key(t.a, l)).copied();
                let jb = ends.get(&key(t.b, l)).copied();
                if let (Some(a), Some(b)) = (ja, jb) {
                    edges.push((ni, a, resistance(pcb::dist(p, t.a), t.width, th, temp)));
                    edges.push((ni, b, resistance(pcb::dist(p, t.b), t.width, th, temp)));
                }
            }
        }
    }
    for (a, b, r) in edges {
        if a != b {
            adj[a].push((b, r));
            adj[b].push((a, r));
        }
    }
    let label = |k: usize| {
        let p = &pads[my[k]];
        format!("{}.{}", pcb.parts[p.part].reference, p.num)
    };
    let mut worst = 0.0;
    let mut pair = (String::new(), String::new());
    let mut open = false;
    for (i, &s) in pad_nodes.iter().enumerate() {
        let d = dijkstra(&adj, s);
        for (j, &e) in pad_nodes.iter().enumerate().skip(i + 1) {
            if d[e].is_finite() {
                if d[e] > worst {
                    worst = d[e];
                    pair = (label(i), label(j));
                }
            } else {
                open = true;
            }
        }
    }
    (worst, pair, open)
}

fn dijkstra(adj: &[Vec<(usize, f64)>], s: usize) -> Vec<f64> {
    use std::cmp::Ordering;
    use std::collections::BinaryHeap;
    #[derive(PartialEq)]
    struct E(f64, usize);
    impl Eq for E {}
    impl PartialOrd for E {
        fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
            Some(self.cmp(o))
        }
    }
    impl Ord for E {
        fn cmp(&self, o: &Self) -> Ordering {
            o.0.total_cmp(&self.0)
        }
    }
    let mut d = vec![f64::INFINITY; adj.len()];
    let mut h = BinaryHeap::new();
    d[s] = 0.0;
    h.push(E(0.0, s));
    while let Some(E(c, u)) = h.pop() {
        if c > d[u] {
            continue;
        }
        for &(v, w) in &adj[u] {
            let n = c + w;
            if n < d[v] {
                d[v] = n;
                h.push(E(n, v));
            }
        }
    }
    d
}

/// Design rule findings for net currents and impedances: tracks too thin for their current, and
/// tracks off their impedance by more than 10 %.
pub fn check(pcb: &Pcb, target: &dyn Fn(&str) -> Target) -> Vec<pcb::Issue> {
    let b = &pcb.board;
    let st = &stack(b);
    let dt = b.rules.temp_rise;
    let mut out = vec![];
    // one finding per net and layer, at its thinnest track
    let mut seen: HashMap<(String, u8, bool), ()> = HashMap::new();
    let mut tracks: Vec<&pcb::Track> = b.tracks.iter().collect();
    tracks.sort_by(|a, b| a.width.total_cmp(&b.width));
    for t in tracks {
        if t.net.is_empty() {
            continue;
        }
        let tg = target(&t.net);
        let th = st.copper_mm(t.layer);
        let mid = [(t.a[0] + t.b[0]) / 2.0, (t.a[1] + t.b[1]) / 2.0];
        if tg.current > 0.0 {
            let need = width_for_current(tg.current, th, dt, true);
            if t.width < need - 1e-6 && seen.insert((t.net.clone(), t.layer as u8, true), ()).is_none() {
                let rise = temp_rise(t.width, th, tg.current, true);
                out.push(pcb::Issue::new("current", mid, true, format!("{} carries {} A: a {} mm track on {} heats {:.0} °C (rule {} °C), it needs {:.2} mm", t.net, tg.current, t.width, t.layer.label().to_lowercase(), rise, dt, need)));
            }
        }
        if tg.impedance > 0.0 {
            let z = st.line(t.layer).line(t.width).z0;
            if (z - tg.impedance).abs() > 0.1 * tg.impedance && seen.insert((t.net.clone(), t.layer as u8, false), ()).is_none() {
                let w = st.line(t.layer).width_for(tg.impedance).map(|w| format!(", {w:.3} mm gives {} Ω", tg.impedance)).unwrap_or_default();
                out.push(pcb::Issue::new("impedance", mid, false, format!("{} should be {} Ω: a {} mm track on {} is {z:.0} Ω{w}", t.net, tg.impedance, t.width, t.layer.label().to_lowercase())));
            }
        }
    }
    out
}

/// The width a new track on a net gets for its current (at least the rule width).
pub fn routed_width(pcb: &Pcb, base: f64, tg: Target) -> f64 {
    let st = &stack(&pcb.board);
    let mut w = base;
    if tg.current > 0.0 {
        let th = st.copper_mm(Layer::Top).min(st.copper_mm(Layer::Bottom));
        let need = width_for_current(tg.current, th, pcb.board.rules.temp_rise, true);
        // round up to 0.05 mm
        w = w.max((need / 0.05).ceil() * 0.05);
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol * b.abs().max(1e-12)
    }

    #[test]
    fn ipc2221_matches_published_values() {
        // 10 mil × 1 oz outer at 10 °C rise is 0.88 A in IPC-2221 calculators
        let w = 0.254;
        let i = current_for(w, OZ_MM, 10.0, true);
        assert!(near(i, 0.88, 0.02), "{i}");
        // inner layers carry about half
        assert!(near(current_for(w, OZ_MM, 10.0, false), i / 2.0, 1e-9));
        // round trip
        let need = width_for_current(i, OZ_MM, 10.0, true);
        assert!(near(need, w, 1e-9));
        assert!(near(temp_rise(w, OZ_MM, i, true), 10.0, 1e-9));
    }

    #[test]
    fn resistance_of_copper() {
        // 1 oz copper is about 0.5 mΩ per square
        let sq = resistance(1.0, 1.0, OZ_MM, 20.0);
        assert!(near(sq, 0.000496, 0.02), "{sq}");
        assert!(resistance(1.0, 1.0, OZ_MM, 120.0) > 1.39 * sq);
    }

    #[test]
    fn microstrip_matches_references() {
        // 50 Ω on 1.6 mm FR4 (er 4.5, h 1.51) is close to 2.85 mm wide (Saturn / KiCad give ~2.8-2.9)
        let w = Geometry { kind: LineKind::Microstrip, h: 1.51, h2: 0.0, er: 4.5, t: OZ_MM }.width_for(50.0).unwrap();
        assert!((2.6..3.1).contains(&w), "{w}");
        // w = h with no copper, er 1: 126.5 Ω air microstrip (Hammerstad)
        let z = microstrip(1.0, 1.0, 1.0, 0.0).z0;
        assert!(near(z, 126.5, 0.01), "{z}");
        // er_eff sits between 1 and er
        let l = microstrip(0.3, 0.2, 4.3, OZ_MM);
        assert!(l.er_eff > 2.5 && l.er_eff < 4.3 && l.z0 > 40.0 && l.z0 < 60.0, "{l:?}");
    }

    #[test]
    fn stripline_matches_references() {
        // b = 2 mm, w/b = 0.5, t = 0, er = 1: 100.2 Ω by Pozar's design formula (30π·b/(w + 0.441b))
        let z = stripline(1.0, 1.0, 0.0, 1.0, 0.0).z0;
        assert!(near(z, 100.2, 0.01), "{z}");
        // narrower is higher
        assert!(stripline(0.5, 1.0, 0.0, 4.0, 0.035).z0 > stripline(1.0, 1.0, 0.0, 4.0, 0.035).z0);
    }

    #[test]
    fn diff_pairs_couple() {
        let g = Geometry { kind: LineKind::Microstrip, h: 0.2, h2: 0.0, er: 4.3, t: OZ_MM };
        let z = g.line(0.2).z0;
        let near_pair = g.diff(0.2, 0.1);
        let far_pair = g.diff(0.2, 5.0);
        assert!(near_pair < far_pair && near(far_pair, 2.0 * z, 0.01));
        let s = g.diff_gap_for(90.0, 0.2).unwrap();
        assert!(near(g.diff(0.2, s), 90.0, 1e-4), "{s}");
        let w = g.diff_width_for(100.0, 0.15).unwrap();
        assert!(near(g.diff(w, 0.15), 100.0, 1e-4));
    }

    #[test]
    fn stackup_geometry() {
        let s = Stackup::default();
        assert!(near(s.total(), 1.6, 1e-9));
        let g = s.line(Layer::Bottom);
        assert!(near(g.h, 1.6 - 2.0 * OZ_MM, 1e-9) && g.er == 4.5);
        let mut s2 = s.clone();
        s2.set_total(0.8);
        assert!(near(s2.total(), 0.8, 1e-9));
        s2.set_copper(Some(2.0), None);
        assert!(near(s2.copper_mm(Layer::Top), 2.0 * OZ_MM, 1e-9));
        let back: Stackup = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn net_report_and_check() {
        let mut p = crate::pcb::tests::led_board();
        crate::pcb_route::autoroute(&mut p, None);
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let tg = |n: &str| if n == "VCC" { Target { current: 3.0, impedance: 0.0 } } else if n == "GND" { Target { current: 0.0, impedance: 50.0 } } else { Target::default() };
        let rep = nets(&p, &nl, &pads, &tg);
        let vcc = rep.iter().find(|r| r.net == "VCC").unwrap();
        assert!(!vcc.open, "{vcc:?}");
        assert!(vcc.resistance > 0.0 && vcc.resistance < 0.1, "{vcc:?}");
        assert!(near(vcc.drop, vcc.resistance * 3.0, 1e-9));
        // the path resistance is no more than all the net's copper in series
        let series: f64 = p.board.tracks.iter().filter(|t| t.net == "VCC").map(|t| resistance(pcb::dist(t.a, t.b), t.width, OZ_MM, AMBIENT + vcc.rise)).sum::<f64>()
            + p.board.vias.iter().filter(|v| v.net == "VCC").map(|v| via_resistance(v.drill, VIA_PLATING, p.board.thickness, AMBIENT + vcc.rise)).sum::<f64>();
        assert!(vcc.resistance <= series + 1e-12, "{} > {series}", vcc.resistance);
        let issues = check(&p, &tg);
        assert!(issues.iter().any(|i| i.error && i.message.starts_with("VCC carries 3 A")), "{issues:?}");
        assert!(issues.iter().any(|i| !i.error && i.message.starts_with("GND should be 50 Ω")), "{issues:?}");
        // widening the VCC tracks clears it
        let w = routed_width(&p, 0.25, tg("VCC"));
        for t in p.board.tracks.iter_mut().filter(|t| t.net == "VCC") {
            t.width = w;
        }
        assert!(!check(&p, &tg).iter().any(|i| i.message.starts_with("VCC")));
    }

    #[test]
    fn net_classes_drive_the_check_and_router() {
        let mut p = crate::pcb::tests::led_board();
        p.board.rules.classes.push(crate::pcb::NetClass { name: "Power".into(), nets: vec!["VCC".into()], current: 2.0, ..Default::default() });
        let nl = p.netlist();
        assert_eq!(target(&p, &nl, "VCC").current, 2.0);
        // new VCC tracks come out wide enough for 2 A
        let w = p.track_width(&nl, "VCC");
        assert!(w >= width_for_current(2.0, OZ_MM, 10.0, true), "{w}");
        crate::pcb_route::autoroute(&mut p, None);
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let fills = crate::pcb::fill_zones(&p, &nl, &pads);
        let issues = crate::pcb::drc(&p, &nl, &pads, &fills);
        assert!(!issues.iter().any(|i| i.rule == "current"), "{:?}", issues.iter().map(|i| &i.message).collect::<Vec<_>>());
        // thin them by hand and the check catches it
        for t in p.board.tracks.iter_mut().filter(|t| t.net == "VCC") {
            t.width = 0.25;
        }
        let issues = crate::pcb::drc(&p, &nl, &pads, &fills);
        assert!(issues.iter().any(|i| i.rule == "current" && i.error));
        p.board.rules.off.push("current".into());
        assert!(!crate::pcb::drc(&p, &nl, &pads, &fills).iter().any(|i| i.rule == "current"));
    }
}
