//! Circuit parts that move the design: DC motors and hobby servos turn or slide the mates of an
//! assembly, and potentiometers, limit switches and encoders read those mates back into the
//! circuit, so firmware on a Pico can run a mechanism in closed loop.
//!
//! Each linked part names an assembly mate. During a transient the mechanics run alongside the
//! circuit (`MechExt`, an `Ext`): a motor's current gives torque, its speed gives back EMF; a
//! servo times the pulses on its signal pin. The mate values spread through the assembly's
//! gear, rack and screw relations, and stop at mate limits. What every mate did is kept, so the
//! assembly can play the motion back.

use serde::{Deserialize, Serialize};

use crate::assembly::{self, Assembly, MateKind, RelationKind};
use crate::circuit_solve::Ext;

/// A schematic part tied to an assembly mate.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Link {
    /// The part's reference (M1, SV1, RV1, SW1, ENC1).
    pub part: String,
    /// Assembly tab (index) and mate id.
    pub assembly: usize,
    pub mate: u64,
    /// Motor on a revolute mate: gear ratio (motor turns per mate turn, on top of a gearbox in
    /// its value); on a slider: lead, mm per turn. Encoder: counts per turn (or per mm).
    #[serde(default = "one")]
    pub ratio: f64,
    /// Servo: the mate angle at 1 ms and 2 ms pulses. Potentiometer: the mate's travel from one
    /// end of the track to the other. Limit switch: closed while the mate is in this range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 2]>,
    /// Motor load at the mate: inertia (kg·m², or kg on a slider) …
    #[serde(default)]
    pub inertia: f64,
    /// … and friction (N·m, or N on a slider) that holds it until the motor beats it.
    #[serde(default)]
    pub load: f64,
    /// Turn the mate the other way.
    #[serde(default)]
    pub reverse: bool,
}

fn one() -> f64 {
    1.0
}

impl Default for Link {
    fn default() -> Self {
        Link { part: String::new(), assembly: 0, mate: 0, ratio: 1.0, range: None, inertia: 0.0, load: 0.0, reverse: false }
    }
}

/// What a part does to (or reads from) its mate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Motor,
    Servo,
    Pot,
    Switch,
    Encoder,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Motor => "DC motor",
            Role::Servo => "servo",
            Role::Pot => "potentiometer",
            Role::Switch => "limit switch",
            Role::Encoder => "encoder",
        }
    }
    /// Drives its mate (rather than reading it).
    pub fn drives(self) -> bool {
        matches!(self, Role::Motor | Role::Servo)
    }
}

/// The role of a library symbol that can be linked to a mate.
pub fn role(symbol: &str) -> Option<Role> {
    match symbol {
        "M" => Some(Role::Motor),
        "SERVO" => Some(Role::Servo),
        "RV" => Some(Role::Pot),
        "SW" => Some(Role::Switch),
        "ENC" => Some(Role::Encoder),
        _ => None,
    }
}

/// A new link with defaults for the role and the mate (`slide`: the mate is a slider).
pub fn new_link(part: &str, role: Role, assembly: usize, mate: u64, slide: bool, limits: Option<[f64; 2]>) -> Link {
    let mut l = Link { part: part.into(), assembly, mate, ..Default::default() };
    match role {
        Role::Motor if slide => l.ratio = 8.0,
        Role::Encoder => l.ratio = if slide { 1.0 } else { 20.0 },
        Role::Servo => l.range = Some([-90.0, 90.0]),
        Role::Pot => l.range = Some(limits.unwrap_or(if slide { [0.0, 50.0] } else { [-135.0, 135.0] })),
        Role::Switch => {
            l.range = Some(match limits {
                Some([lo, hi]) => {
                    let w = if slide { 0.5 } else { 2.0 };
                    [lo.max(hi) - w, lo.max(hi) + w]
                }
                None => [if slide { 10.0 } else { 45.0 }, if slide { 1e3 } else { 360.0 }],
            })
        }
        _ => {}
    }
    l
}

// ---------------------------------------------------------------- part models

/// A permanent-magnet DC motor, from its rated voltage, no-load speed and stall current.
#[derive(Clone, Debug, PartialEq)]
pub struct Motor {
    pub volts: f64,
    pub rpm: f64,
    pub stall: f64,
    /// Gearbox ratio (motor turns per output turn).
    pub gear: f64,
    pub name: String,
}

impl Motor {
    /// Winding resistance (Ω).
    pub fn r(&self) -> f64 {
        self.volts / self.stall.max(1e-6)
    }
    /// Torque and back-EMF constant (N·m/A = V·s/rad), allowing for the no-load current.
    pub fn k(&self) -> f64 {
        let w0 = self.rpm.max(1.0) * std::f64::consts::TAU / 60.0;
        (self.volts - self.i0() * self.r()) / w0
    }
    /// No-load current: about 5 % of the stall current.
    pub fn i0(&self) -> f64 {
        0.05 * self.stall
    }
    /// Viscous friction (N·m·s/rad) that takes the no-load current at no-load speed.
    pub fn b(&self) -> f64 {
        let w0 = self.rpm.max(1.0) * std::f64::consts::TAU / 60.0;
        self.k() * self.i0() / w0
    }
    /// Rotor inertia (kg·m²) for a 20 ms mechanical time constant.
    pub fn j(&self) -> f64 {
        0.02 * self.k().powi(2) / self.r()
    }
    /// Winding inductance (H): a 0.3 ms electrical time constant.
    pub fn l(&self) -> f64 {
        self.r() * 3e-4
    }
}

/// A motor from its value: "6V 10000rpm 1A", "12V 300rpm 2A 1:30", or a type (TT, N20, 130, 775).
pub fn motor_model(value: &str) -> Motor {
    let u = value.to_ascii_uppercase();
    let mut m = Motor { volts: 6.0, rpm: 10_000.0, stall: 1.0, gear: 1.0, name: "6 V DC motor".into() };
    let preset = |m: &mut Motor, v: f64, rpm: f64, a: f64, g: f64, name: &str| {
        (m.volts, m.rpm, m.stall, m.gear, m.name) = (v, rpm, a, g, name.to_string());
    };
    if u.contains("TT") {
        preset(&mut m, 6.0, 9600.0, 1.5, 48.0, "TT gear motor");
    } else if u.contains("N20") {
        preset(&mut m, 6.0, 15_000.0, 0.7, 100.0, "N20 gear motor");
    } else if u.contains("775") {
        preset(&mut m, 12.0, 12_000.0, 30.0, 1.0, "775 motor");
    } else if u.contains("130") {
        preset(&mut m, 3.0, 9000.0, 2.0, 1.0, "130 motor");
    }
    let mut named = false;
    for tok in u.split(|c: char| c.is_whitespace() || c == ',') {
        if let Some((a, b)) = tok.split_once(':') {
            if let (Ok(a), Ok(b)) = (a.parse::<f64>(), b.parse::<f64>()) {
                if a > 0.0 && b > 0.0 {
                    m.gear = a.max(b) / a.min(b);
                    named = true;
                }
            }
        } else if let Some(n) = tok.strip_suffix("RPM") {
            if let Some(v) = crate::circuit::parse_si(&n.to_ascii_lowercase()).filter(|v| *v > 0.0) {
                m.rpm = v;
                named = true;
            }
        } else if let Some(n) = tok.strip_suffix('V') {
            if let Some(v) = crate::circuit::parse_si(n).filter(|v| *v > 0.0) {
                m.volts = v;
                named = true;
            }
        } else if let Some(n) = tok.strip_suffix('A') {
            if let Some(v) = crate::circuit::parse_si(n).filter(|v| *v > 0.0) {
                m.stall = v;
                named = true;
            }
        }
    }
    if named {
        m.name = format!("{} V motor, {} rpm, {} stall", crate::circuit::si(m.volts, "").trim(), m.rpm.round(), crate::circuit::si(m.stall, "A"));
        if m.gear != 1.0 {
            m.name.push_str(&format!(", 1:{}", m.gear));
        }
    }
    m
}

/// A hobby servo: how fast it turns (degrees per second).
#[derive(Clone, Debug, PartialEq)]
pub struct Servo {
    pub speed: f64,
    pub name: String,
}

pub fn servo_model(value: &str) -> Servo {
    let u = value.to_ascii_uppercase();
    if u.contains("MG996") || u.contains("MG995") {
        Servo { speed: 350.0, name: "MG996R servo".into() }
    } else if u.contains("MG90") {
        Servo { speed: 600.0, name: "MG90S servo".into() }
    } else {
        Servo { speed: 600.0, name: "SG90 servo".into() }
    }
}

/// How a linked part was built into the circuit.
#[derive(Clone, Debug)]
pub enum MechPart {
    /// `br`: the winding current's index in the solution; `val`: the back-EMF value.
    Motor {
        motor: Motor,
        br: usize,
        val: usize,
    },
    /// Signal and ground nodes; `val`: the conductance that draws its motor current.
    Servo {
        servo: Servo,
        sig: usize,
        gnd: usize,
        val: usize,
    },
    /// Track resistance; the conductances from end 1 to the wiper and the wiper to end 3.
    Pot {
        r: f64,
        vals: [usize; 2],
    },
    Switch {
        val: usize,
    },
    /// The A and B outputs' `Dev::Pin`s.
    Encoder {
        pins: [usize; 2],
    },
}

#[derive(Clone, Debug)]
pub struct MechSim {
    pub reference: String,
    pub part: MechPart,
}

impl MechSim {
    pub fn role(&self) -> Role {
        match self.part {
            MechPart::Motor { .. } => Role::Motor,
            MechPart::Servo { .. } => Role::Servo,
            MechPart::Pot { .. } => Role::Pot,
            MechPart::Switch { .. } => Role::Switch,
            MechPart::Encoder { .. } => Role::Encoder,
        }
    }
}

// ---------------------------------------------------------------- the mechanism

/// One mate the mechanism knows: where it started and where it is (radians / mm).
#[derive(Clone, Debug)]
struct MateState {
    asm: usize,
    id: u64,
    name: String,
    kind: MateKind,
    angle0: f64,
    slide0: f64,
    angle: Option<f64>,
    slide: Option<f64>,
    /// Degrees for revolute mates, mm for sliders and cylindrical mates.
    limits: Option<[f64; 2]>,
}

impl MateState {
    /// The value parts read: degrees (revolute, cylindrical) or mm (slider).
    fn value(&self) -> f64 {
        if self.kind == MateKind::Slider { self.slide.unwrap_or(self.slide0) } else { self.angle.unwrap_or(self.angle0).to_degrees() }
    }
    fn slide_unit(&self) -> bool {
        self.kind == MateKind::Slider
    }
    /// The value its limits apply to: degrees (revolute) or mm.
    fn lim_val(&self) -> Option<f64> {
        match self.kind {
            MateKind::Revolute => self.angle.map(|x| x.to_degrees()),
            _ => self.slide,
        }
    }
    /// How far past a limit it is (0 inside).
    fn over(&self) -> f64 {
        let Some([a, b]) = self.limits else { return 0.0 };
        let (lo, hi) = (a.min(b), a.max(b));
        match self.lim_val() {
            Some(v) if v > hi + 1e-9 => v - hi,
            Some(v) if v < lo - 1e-9 => v - lo,
            _ => 0.0,
        }
    }
}

#[derive(Clone, Debug)]
struct Rel {
    kind: RelationKind,
    a: usize,
    b: Option<usize>,
    k: f64,
    phase: f64,
}

/// A motor's or servo's running state.
#[derive(Clone, Debug)]
enum Act {
    Motor {
        motor: Motor,
        br: usize,
        val: usize,
        /// Motor turns per mate turn (or per radian of lead on a slider: see `out`).
        n: f64,
        /// mm per output turn on a slider (0 on a revolute mate).
        lead: f64,
        j: f64,
        tc: f64,
        sign: f64,
        theta: f64,
        omega: f64,
        /// The mate it turns, and the mate's value at the start.
        mate: Option<usize>,
        base: f64,
    },
    Servo {
        servo: Servo,
        sig: usize,
        gnd: usize,
        val: usize,
        mate: Option<usize>,
        range: [f64; 2],
        /// Degrees (mate angle).
        angle: f64,
        target: Option<f64>,
        rise: Option<f64>,
        last: (f64, f64),
        moving: bool,
    },
}

/// A sensor reading a mate.
#[derive(Clone, Debug)]
enum Sense {
    Pot { r: f64, vals: [usize; 2], range: [f64; 2] },
    Switch { val: usize, range: [f64; 2] },
    Encoder { pins: [usize; 2], cpr: f64 },
}

/// What one part did over the run.
#[derive(Clone, Debug, Default)]
pub struct PartTrace {
    pub reference: String,
    pub role: Option<Role>,
    /// The mate value it drove or read (degrees or mm), at every time point.
    pub pos: Vec<f64>,
    pub unit: &'static str,
    /// Motors: output shaft speed (rpm).
    pub rpm: Vec<f64>,
}

/// What one mate did over the run: degrees and mm at every time point (None: not moved).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MateTrace {
    pub assembly: usize,
    pub mate: u64,
    pub name: String,
    pub angle: Option<Vec<f64>>,
    pub slide: Option<Vec<f64>>,
}

impl MateTrace {
    /// The drive for time point `k` (or between `k` and `k + 1`, `f` of the way).
    pub fn drive(&self, k: usize, f: f64) -> assembly::Drive {
        let at = |v: &Vec<f64>| {
            let a = v[k.min(v.len() - 1)];
            let b = v[(k + 1).min(v.len() - 1)];
            a + (b - a) * f
        };
        assembly::Drive { mate: self.mate, angle: self.angle.as_ref().map(at), slide: self.slide.as_ref().map(at) }
    }
}

pub struct MechExt {
    nn: usize,
    t: f64,
    mates: Vec<MateState>,
    rels: Vec<Rel>,
    acts: Vec<(String, Act)>,
    senses: Vec<(String, Sense, Option<usize>)>,
    vals: Vec<f64>,
    pins: Vec<(usize, (f64, f64))>,
    npins: usize,
    horizon: f64,
    traces: Vec<PartTrace>,
    mate_tr: Vec<(Vec<f64>, Vec<f64>)>,
    /// The mate that limits the step: its speed (units per second) and the most it may move
    /// in a step.
    speed: f64,
    fine: f64,
}

const ENC_G: f64 = 1.0 / 100.0;

impl MechExt {
    /// The mechanism for the built parts and the links to `asms`; problems go in `notes`.
    pub fn new(parts: &[MechSim], links: &[Link], asms: &[Assembly], nn: usize, nvals: usize, npins: usize, notes: &mut Vec<String>) -> MechExt {
        let mut ext = MechExt {
            nn,
            t: 0.0,
            mates: vec![],
            rels: vec![],
            acts: vec![],
            senses: vec![],
            vals: vec![0.0; nvals],
            pins: vec![],
            npins,
            horizon: 1e-3,
            traces: vec![],
            mate_tr: vec![],
            speed: 0.0,
            fine: 0.5,
        };
        // the mates the links name, and every mate related to them
        let mut link_mate = vec![None; parts.len()];
        for (pi, p) in parts.iter().enumerate() {
            let Some(l) = links.iter().find(|l| l.part.eq_ignore_ascii_case(&p.reference)) else { continue };
            let Some(a) = asms.get(l.assembly) else {
                notes.push(format!("{}: its assembly is gone; it is not linked", p.reference));
                continue;
            };
            let Some(m) = a.mates.iter().find(|m| m.id == l.mate) else {
                notes.push(format!("{}: its mate is gone from {}; it is not linked", p.reference, a.name));
                continue;
            };
            if !matches!(m.kind, MateKind::Revolute | MateKind::Slider | MateKind::Cylindrical) {
                notes.push(format!("{}: {} is a {} mate; link it to a revolute, slider or cylindrical mate", p.reference, m.name, m.kind.label().to_lowercase()));
                continue;
            }
            if m.suppressed || m.a.is_none() || m.b.is_none() {
                notes.push(format!("{}: {} is suppressed or unfinished; it is not linked", p.reference, m.name));
                continue;
            }
            link_mate[pi] = Some(ext.mate_ix(asms, l.assembly, l.mate));
        }
        // gather related mates until none is added
        loop {
            let before = ext.mates.len();
            let known: Vec<(usize, u64)> = ext.mates.iter().map(|m| (m.asm, m.id)).collect();
            for (ai, id) in known {
                for r in &asms[ai].relations {
                    if r.a == id || r.b == Some(id) {
                        ext.mate_ix(asms, ai, r.a);
                        if let Some(b) = r.b {
                            ext.mate_ix(asms, ai, b);
                        }
                    }
                }
            }
            if ext.mates.len() == before {
                break;
            }
        }
        let mut seen = std::collections::HashSet::new();
        for (ai, a) in asms.iter().enumerate() {
            for r in &a.relations {
                let ix = |id: u64| ext.mates.iter().position(|m| m.asm == ai && m.id == id);
                let (Some(ia), ib) = (ix(r.a), r.b.and_then(ix)) else { continue };
                if r.b.is_some() && ib.is_none() || !seen.insert((ai, r.id)) {
                    continue;
                }
                let sign = if r.reverse { -1.0 } else { 1.0 };
                ext.rels.push(Rel { kind: r.kind, a: ia, b: ib, k: sign * r.ratio, phase: r.phase });
            }
        }
        let mut tau: f64 = f64::INFINITY;
        for (pi, p) in parts.iter().enumerate() {
            let l = links.iter().find(|l| l.part.eq_ignore_ascii_case(&p.reference)).cloned().unwrap_or_default();
            let mate = link_mate[pi];
            let slide = mate.map(|m| ext.mates[m].slide_unit()).unwrap_or(false);
            let base = mate.map(|m| if slide { ext.mates[m].slide0 } else { ext.mates[m].angle0 }).unwrap_or(0.0);
            let unit = if slide { "mm" } else { "deg" };
            ext.traces.push(PartTrace { reference: p.reference.clone(), role: Some(p.role()), unit, ..Default::default() });
            match &p.part {
                MechPart::Motor { motor, br, val } => {
                    let ratio = if l.ratio > 0.0 { l.ratio } else { 1.0 };
                    let (n, lead) = if slide { (motor.gear, ratio) } else { (motor.gear * ratio, 0.0) };
                    // the load seen at the motor shaft
                    let reff = if slide { lead / 1000.0 / std::f64::consts::TAU / n } else { 1.0 / n };
                    let (j, tc) = if mate.is_some() { (motor.j() + l.inertia.max(0.0) * reff * reff, l.load.max(0.0) * reff) } else { (motor.j(), 0.0) };
                    tau = tau.min(j * motor.r() / motor.k().powi(2));
                    let sign = if l.reverse { -1.0 } else { 1.0 };
                    ext.acts.push((p.reference.clone(), Act::Motor { motor: motor.clone(), br: *br, val: *val, n, lead, j, tc, sign, theta: 0.0, omega: 0.0, mate, base }));
                }
                MechPart::Servo { servo, sig, gnd, val } => {
                    let range = l.range.unwrap_or([-90.0, 90.0]);
                    let angle = mate.map(|m| ext.mates[m].angle0.to_degrees()).unwrap_or((range[0] + range[1]) / 2.0);
                    if mate.is_some() && slide {
                        notes.push(format!("{}: a servo turns; on a slider its angle is taken as mm", p.reference));
                    }
                    ext.acts.push((
                        p.reference.clone(),
                        Act::Servo { servo: servo.clone(), sig: *sig, gnd: *gnd, val: *val, mate, range, angle, target: None, rise: None, last: (0.0, 0.0), moving: false },
                    ));
                }
                MechPart::Pot { r, vals } => ext.senses.push((p.reference.clone(), Sense::Pot { r: *r, vals: *vals, range: l.range.unwrap_or([-135.0, 135.0]) }, mate)),
                MechPart::Switch { val } => ext.senses.push((p.reference.clone(), Sense::Switch { val: *val, range: l.range.unwrap_or([45.0, 1e6]) }, mate)),
                MechPart::Encoder { pins } => ext.senses.push((p.reference.clone(), Sense::Encoder { pins: *pins, cpr: if l.ratio > 0.0 { l.ratio } else { 20.0 } }, mate)),
            }
        }
        if tau.is_finite() {
            ext.horizon = (tau / 20.0).clamp(20e-6, 1e-3);
        }
        ext.mate_tr = vec![(vec![], vec![]); ext.mates.len()];
        ext.propagate();
        ext.sense();
        ext.record();
        ext
    }

    fn mate_ix(&mut self, asms: &[Assembly], ai: usize, id: u64) -> usize {
        if let Some(k) = self.mates.iter().position(|m| m.asm == ai && m.id == id) {
            return k;
        }
        let a = &asms[ai];
        let m = a.mates.iter().find(|m| m.id == id);
        let frames = m.and_then(|m| Some((a.world(m.a.as_ref()?)?, a.world(m.b.as_ref()?)?)));
        let (angle0, slide0) = match (m, frames) {
            (Some(m), Some((fa, fb))) => (assembly::mate_angle(m, fa, fb), assembly::mate_slide(m, fa, fb)),
            _ => (0.0, 0.0),
        };
        self.mates.push(MateState {
            asm: ai,
            id,
            name: m.map(|m| m.name.clone()).unwrap_or_default(),
            kind: m.map(|m| m.kind).unwrap_or(MateKind::Revolute),
            angle0,
            slide0,
            angle: None,
            slide: None,
            limits: m.and_then(|m| m.limits),
        });
        self.mates.len() - 1
    }

    pub fn active(&self) -> bool {
        !self.acts.is_empty() || !self.senses.is_empty()
    }

    /// Set the driven mates from the actuators and spread them through the relations.
    fn propagate(&mut self) {
        for m in &mut self.mates {
            m.angle = None;
            m.slide = None;
        }
        for (_, a) in &self.acts {
            match a {
                Act::Motor { mate: Some(m), base, theta, n, lead, sign, .. } => {
                    let out = sign * theta / n;
                    let ms = &mut self.mates[*m];
                    if ms.slide_unit() {
                        ms.slide = Some(base + out / std::f64::consts::TAU * lead);
                    } else {
                        ms.angle = Some(base + out);
                    }
                }
                Act::Servo { mate: Some(m), angle, .. } => {
                    let ms = &mut self.mates[*m];
                    if ms.slide_unit() {
                        ms.slide = Some(*angle);
                    } else {
                        ms.angle = Some(angle.to_radians());
                    }
                }
                _ => {}
            }
        }
        for _ in 0..self.mates.len() + 1 {
            let mut changed = false;
            for r in &self.rels {
                let (a, b) = (r.a, r.b);
                match (r.kind, b) {
                    (RelationKind::Gear, Some(b)) => {
                        // θb = phase − k θa
                        if let (Some(x), None) = (self.mates[a].angle, self.mates[b].angle) {
                            self.mates[b].angle = Some(r.phase - r.k * x);
                            changed = true;
                        } else if let (None, Some(y)) = (self.mates[a].angle, self.mates[b].angle) {
                            if r.k != 0.0 {
                                self.mates[a].angle = Some((r.phase - y) / r.k);
                                changed = true;
                            }
                        }
                    }
                    (RelationKind::Rack, Some(b)) => {
                        // travel b = k θa + phase
                        if let (Some(x), None) = (self.mates[a].angle, self.mates[b].slide) {
                            self.mates[b].slide = Some(r.k * x + r.phase);
                            changed = true;
                        } else if let (None, Some(y)) = (self.mates[a].angle, self.mates[b].slide) {
                            if r.k != 0.0 {
                                self.mates[a].angle = Some((y - r.phase) / r.k);
                                changed = true;
                            }
                        }
                    }
                    (RelationKind::Screw, _) => {
                        // travel = k θ / 2π + phase
                        let m = &mut self.mates[a];
                        if let (Some(x), None) = (m.angle, m.slide) {
                            m.slide = Some(r.k * x / std::f64::consts::TAU + r.phase);
                            changed = true;
                        } else if let (None, Some(y)) = (m.angle, m.slide) {
                            if r.k != 0.0 {
                                m.angle = Some((y - r.phase) * std::f64::consts::TAU / r.k);
                                changed = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// The mate furthest past a limit, and by how much.
    fn overrun(&self) -> Option<(usize, f64)> {
        self.mates.iter().enumerate().map(|(k, m)| (k, m.over())).filter(|x| x.1 != 0.0).max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
    }

    /// Put each sensor's reading into the circuit.
    fn sense(&mut self) {
        self.pins.clear();
        for (_, s, mate) in &self.senses {
            let v = mate.map(|m| self.mates[m].value());
            match s {
                Sense::Pot { r, vals, range } => {
                    let f = match v {
                        Some(v) if range[1] != range[0] => ((v - range[0]) / (range[1] - range[0])).clamp(0.0, 1.0),
                        _ => 0.5,
                    };
                    self.vals[vals[0]] = 1.0 / (r * f + 0.5);
                    self.vals[vals[1]] = 1.0 / (r * (1.0 - f) + 0.5);
                }
                Sense::Switch { val, range } => {
                    let (lo, hi) = (range[0].min(range[1]), range[0].max(range[1]));
                    let closed = v.map(|v| v >= lo && v <= hi).unwrap_or(false);
                    self.vals[*val] = if closed { 20.0 } else { 1e-12 };
                }
                Sense::Encoder { pins, cpr } => {
                    let units = match mate {
                        Some(m) if self.mates[*m].slide_unit() => v.unwrap_or(0.0),
                        _ => v.unwrap_or(0.0) / 360.0,
                    };
                    let phase = ((units * cpr * 4.0).floor() as i64).rem_euclid(4);
                    let a = phase == 1 || phase == 2;
                    let b = phase >= 2;
                    let drive = |hi: bool| if hi { (ENC_G, 0.0) } else { (0.0, ENC_G) };
                    self.pins.push((pins[0], drive(a)));
                    self.pins.push((pins[1], drive(b)));
                }
            }
        }
        for (_, a) in &self.acts {
            if let Act::Servo { val, moving, .. } = a {
                // about 200 mA at 5 V while it turns
                self.vals[*val] = if *moving { 0.04 } else { 0.0 };
            }
        }
    }

    fn record(&mut self) {
        for (k, m) in self.mates.iter().enumerate() {
            let (a, s) = &mut self.mate_tr[k];
            a.push(m.angle.unwrap_or(m.angle0).to_degrees());
            s.push(m.slide.unwrap_or(m.slide0));
        }
        // parts are traced in build order: actuators and sensors interleave, so look each up
        let vals: Vec<(String, f64, Option<f64>)> = self
            .acts
            .iter()
            .map(|(r, a)| match a {
                Act::Motor { mate, omega, n, theta, sign, .. } => {
                    let pos = mate.map(|m| self.mates[m].value()).unwrap_or((sign * theta / n).to_degrees());
                    (r.clone(), pos, Some(omega / n * 60.0 / std::f64::consts::TAU))
                }
                Act::Servo { mate, angle, .. } => (r.clone(), mate.map(|m| self.mates[m].value()).unwrap_or(*angle), None),
            })
            .chain(self.senses.iter().map(|(r, _, mate)| (r.clone(), mate.map(|m| self.mates[m].value()).unwrap_or(0.0), None)))
            .collect();
        for (r, pos, rpm) in vals {
            if let Some(t) = self.traces.iter_mut().find(|t| t.reference == r) {
                t.pos.push(pos);
                if let Some(w) = rpm {
                    t.rpm.push(w);
                }
            }
        }
    }

    /// Set the pins this drives in `out` (every `Dev::Pin`, sized to them all).
    pub fn fill_pins(&self, out: &mut Vec<(f64, f64)>) {
        out.resize(self.npins.max(out.len()), (0.0, 0.0));
        for (k, g) in &self.pins {
            out[*k] = *g;
        }
    }

    pub fn values(&self) -> Vec<f64> {
        self.vals.clone()
    }

    pub fn horizon(&self) -> f64 {
        self.horizon
    }

    /// Run on to `until`, or less while the mechanism moves fast (so encoders and switches
    /// change close to when they should).
    pub fn advance(&mut self, t: f64, until: f64) -> f64 {
        if self.speed > 0.0 { until.min(t + (self.fine / self.speed).max(1e-6)) } else { until }
    }

    /// The circuit reached `t` with solution `x`: move the mechanism to `t`.
    pub fn step(&mut self, t: f64, x: &[f64]) {
        let dt = t - self.t;
        let v = |n: usize| if n == 0 { 0.0 } else { x[n - 1] };
        let before: Vec<f64> = self.mates.iter().map(|m| m.value()).collect();
        for ai in 0..self.acts.len() {
            let prev = self.acts[ai].1.clone();
            match &mut self.acts[ai].1 {
                Act::Motor { motor, br, j, tc, theta, omega, .. } => {
                    let i = x[self.nn + *br];
                    let tm = motor.k() * i;
                    let b = motor.b();
                    if dt > 0.0 {
                        // friction holds a stopped motor until its torque beats it
                        if omega.abs() < 1e-9 && tm.abs() <= *tc {
                            *omega = 0.0;
                        } else {
                            let dir = if omega.abs() > 1e-9 { omega.signum() } else { tm.signum() };
                            let w = (*omega * *j + dt * (tm - *tc * dir)) / (*j + b * dt);
                            // friction stops it rather than turning it back
                            *omega = if w * dir < 0.0 && omega.abs() > 1e-9 { 0.0 } else { w };
                        }
                        *theta += *omega * dt;
                    }
                }
                Act::Servo { servo, sig, gnd, angle, target, rise, last, moving, range, .. } => {
                    let u = v(*sig) - v(*gnd);
                    let th = 1.4;
                    let (t0, u0) = *last;
                    let cross = |a: f64, b: f64| if (b - a).abs() > 1e-12 { t0 + (t - t0) * (th - a) / (b - a) } else { t };
                    if u0 < th && u >= th {
                        *rise = Some(cross(u0, u));
                    } else if u0 >= th && u < th {
                        if let Some(r) = rise.take() {
                            let pw = cross(u0, u) - r;
                            if (0.3e-3..3e-3).contains(&pw) {
                                let f = ((pw - 1e-3) / 1e-3).clamp(0.0, 1.0);
                                *target = Some(range[0] + (range[1] - range[0]) * f);
                            }
                        }
                    }
                    *last = (t, u);
                    *moving = false;
                    if let Some(g) = *target {
                        let step = servo.speed * dt;
                        if (g - *angle).abs() > 1e-9 {
                            *angle += (g - *angle).clamp(-step, step);
                            *moving = true;
                        }
                    }
                }
            }
            // stop at a mate limit
            self.propagate();
            if let Some((k, over)) = self.overrun() {
                let cur = self.acts[ai].1.clone();
                let now = self.mates[k].lim_val().unwrap_or(0.0);
                let lim = now - over;
                self.acts[ai].1 = prev.clone();
                self.propagate();
                let was = self.mates[k].lim_val().unwrap_or(now);
                // the values are linear in the actuator's position: stop it where the mate meets the limit
                let f = if (now - was).abs() > 1e-12 { ((lim - was) / (now - was)).clamp(0.0, 1.0) } else { 0.0 };
                self.acts[ai].1 = cur;
                match (&mut self.acts[ai].1, &prev) {
                    (Act::Motor { theta, omega, .. }, Act::Motor { theta: p, .. }) => {
                        *theta = p + (*theta - p) * f;
                        *omega = 0.0;
                    }
                    (Act::Servo { angle, moving, .. }, Act::Servo { angle: p, .. }) => {
                        *angle = p + (*angle - p) * f;
                        *moving = false;
                    }
                    _ => {}
                }
                self.propagate();
            }
        }
        // the EMFs for the next step
        for (_, a) in &self.acts {
            if let Act::Motor { val, omega, motor, .. } = a {
                self.vals[*val] = motor.k() * omega;
            }
        }
        if dt > 0.0 {
            self.speed = 0.0;
            self.fine = f64::INFINITY;
            for (k, m) in self.mates.iter().enumerate() {
                let s = (m.value() - before[k]).abs() / dt;
                let fine = self
                    .senses
                    .iter()
                    .filter(|x| x.2 == Some(k))
                    .map(|x| match x.1 {
                        Sense::Encoder { cpr, .. } => {
                            if m.slide_unit() {
                                0.125 / cpr
                            } else {
                                45.0 / cpr
                            }
                        }
                        _ => {
                            if m.slide_unit() {
                                0.05
                            } else {
                                0.5
                            }
                        }
                    })
                    .fold(if m.slide_unit() { 0.5 } else { 2.0 }, f64::min);
                if s > 0.0 && fine / s < self.fine / self.speed.max(1e-300) {
                    self.speed = s;
                    self.fine = fine;
                }
            }
            if self.speed == 0.0 {
                self.fine = 0.5;
            }
        }
        self.t = t;
        self.sense();
        self.record();
    }

    /// The parts' and mates' traces.
    pub fn finish(self) -> (Vec<PartTrace>, Vec<MateTrace>) {
        let mut mates = vec![];
        for (k, m) in self.mates.iter().enumerate() {
            let (a, s) = &self.mate_tr[k];
            let moved = |v: &Vec<f64>| v.iter().any(|x| (x - v[0]).abs() > 1e-9);
            let angle = (m.kind != MateKind::Slider && moved(a)).then(|| a.clone());
            let slide = (m.kind != MateKind::Revolute && moved(s)).then(|| s.clone());
            if angle.is_some() || slide.is_some() {
                mates.push(MateTrace { assembly: m.asm, mate: m.id, name: m.name.clone(), angle, slide });
            }
        }
        (self.traces, mates)
    }
}

/// `MechExt` alone as the circuit's `Ext` (no microcontroller).
impl Ext for MechExt {
    fn advance(&mut self, t: f64, _x: &[f64], until: f64) -> f64 {
        MechExt::advance(self, t, until)
    }
    fn pins(&self) -> Vec<(f64, f64)> {
        let mut out = vec![];
        self.fill_pins(&mut out);
        out
    }
    fn values(&self) -> Vec<f64> {
        MechExt::values(self)
    }
    fn step(&mut self, t: f64, x: &[f64]) {
        MechExt::step(self, t, x)
    }
    fn horizon(&self) -> f64 {
        self.horizon
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::{Connector, Instance, Mate, Relation};
    use crate::circuit::{Analysis, Kind, SimOut, Source, run_with};
    use crate::pcb::Pcb;

    fn conn(inst: u64, z: [f64; 3]) -> Connector {
        Connector { inst, origin: [0.0; 3], z, x: [1.0, 0.0, 0.0], ..Default::default() }
    }

    fn mate(id: u64, name: &str, kind: MateKind, limits: Option<[f64; 2]>) -> Mate {
        let (a, b) = (id * 10, id * 10 + 1);
        Mate {
            id,
            name: name.into(),
            kind,
            a: Some(conn(a, [0.0, 0.0, 1.0])),
            b: Some(conn(b, [0.0, 0.0, -1.0])),
            flip: false,
            offset: 0.0,
            offset_xy: [0.0; 2],
            rotation: 0.0,
            limits,
            suppressed: false,
        }
    }

    /// An assembly of the given mates, each between a fixed base and a free part of its own.
    fn asm(mates: Vec<Mate>, relations: Vec<Relation>) -> Assembly {
        let mut a = Assembly { name: "Mechanism".into(), ..Default::default() };
        for m in &mates {
            for (k, fixed) in [(m.id * 10, true), (m.id * 10 + 1, false)] {
                a.instances.push(Instance { id: k, name: format!("p{k}"), studio: 0, body: Default::default(), pose: Default::default(), fixed, hidden: false, sub: None });
            }
        }
        a.mates = mates;
        a.relations = relations;
        a.next_id = 1000;
        a
    }

    fn board(parts: &[(&str, &str)], nets: &[(&str, &[(&str, &str)])], sources: &[(&str, &str)]) -> Pcb {
        let mut p = Pcb::new("t");
        for (k, (s, v)) in parts.iter().enumerate() {
            p.add_part(s, None, Some(v), [k as f64 * 30.48, 0.0]).unwrap();
        }
        for (net, pins) in nets {
            let pins: Vec<(usize, String)> = pins.iter().map(|(r, n)| (p.part_index(r).unwrap(), n.to_string())).collect();
            crate::agent::pcb_connect(&mut p, net, &pins);
        }
        p.sim.ground = "GND".into();
        p.sim.sources = sources.iter().map(|(n, w)| Source { name: format!("V{n}"), plus: n.to_string(), wave: w.to_string(), ..Default::default() }).collect();
        p
    }

    fn tran(p: &Pcb, stop: f64, asms: &[Assembly]) -> SimOut {
        run_with(p, &Analysis { kind: Kind::Tran, stop, ..Default::default() }, asms).unwrap()
    }

    fn last(out: &SimOut, q: &str) -> f64 {
        *out.probe(q).unwrap_or_else(|| panic!("{q}: {:?}", out.names())).re.last().unwrap()
    }

    #[test]
    fn motor_models_from_values() {
        let m = motor_model("12V 300rpm 2A 1:30");
        assert_eq!((m.volts, m.rpm, m.stall, m.gear), (12.0, 300.0, 2.0, 30.0));
        assert_eq!(motor_model("TT").gear, 48.0);
        let d = motor_model("6V 10000rpm 1A");
        assert!((d.r() - 6.0).abs() < 1e-9);
        // no-load: the back EMF and the friction current balance the supply at the rated speed
        let w0 = 10_000.0 * std::f64::consts::TAU / 60.0;
        assert!((d.k() * w0 + d.r() * d.b() * w0 / d.k() - 6.0).abs() < 1e-9);
    }

    #[test]
    fn a_free_motor_runs_up_to_its_rated_speed() {
        let p = board(&[("M", "6V 10000rpm 1A")], &[("6V", &[("M1", "1")]), ("GND", &[("M1", "2")])], &[("6V", "6")]);
        let out = tran(&p, 0.2, &[]);
        let rpm = last(&out, "RPM(M1)");
        assert!((rpm - 10_000.0).abs() < 150.0, "{rpm}");
        // the no-load current, after the start-up surge
        let i = out.probe("I(M1)").unwrap();
        let peak = i.re.iter().cloned().fold(0.0, f64::max);
        assert!(peak > 0.5 && (last(&out, "I(M1)") - 0.05).abs() < 0.01, "{peak} {}", last(&out, "I(M1)"));
    }

    #[test]
    fn a_motor_turns_its_mate_to_the_limit_and_trips_the_switch() {
        let a = asm(vec![mate(1, "Arm", MateKind::Revolute, Some([-10.0, 90.0]))], vec![]);
        let mut p = board(
            &[("M", "6V 10000rpm 1A 1:100"), ("SW", "Limit"), ("R", "10k")],
            &[("6V", &[("M1", "1"), ("SW1", "1")]), ("GND", &[("M1", "2"), ("R1", "2")]), ("SENSE", &[("SW1", "2"), ("R1", "1")])],
            &[("6V", "6")],
        );
        p.sim.links = vec![new_link("M1", Role::Motor, 0, 1, false, None), new_link("SW1", Role::Switch, 0, 1, false, Some([-10.0, 90.0]))];
        let out = tran(&p, 0.5, &[a]);
        assert!(out.notes.iter().all(|n| !n.contains("link")), "{:?}", out.notes);
        // 100 rpm at the arm: 90° in 0.15 s, then stalled at the limit
        let pos = out.probe("POS(M1)").unwrap();
        assert!((last(&out, "POS(M1)") - 90.0).abs() < 1e-6, "{:?}", pos.re.last());
        let k = out.x.iter().position(|t| *t >= 0.1).unwrap();
        assert!(pos.re[k] > 45.0 && pos.re[k] < 70.0, "{}", pos.re[k]);
        assert!((last(&out, "I(M1)") - 1.0).abs() < 0.02, "stall current {}", last(&out, "I(M1)"));
        // the switch closed near the end of travel
        let sense = out.probe("V(SENSE)").unwrap();
        assert!(sense.re[k] < 0.01 && last(&out, "V(SENSE)") > 5.9);
        let tm = &out.mates[0];
        assert_eq!((tm.mate, tm.slide.is_none()), (1, true));
        assert!((tm.angle.as_ref().unwrap().last().unwrap() - 90.0).abs() < 1e-6);
    }

    #[test]
    fn a_servo_follows_its_pulses() {
        let a = asm(vec![mate(1, "Horn", MateKind::Revolute, None)], vec![]);
        let mut p =
            board(&[("SERVO", "SG90")], &[("5V", &[("SV1", "2")]), ("GND", &[("SV1", "1")]), ("PWM", &[("SV1", "3")])], &[("5V", "5"), ("PWM", "PULSE(0 3.3 1m 1u 1u 1.75m 20m)")]);
        p.sim.links = vec![new_link("SV1", Role::Servo, 0, 1, false, None)];
        let out = tran(&p, 0.3, &[a]);
        // 1.75 ms: three quarters of the way from −90° to 90°
        assert!((last(&out, "POS(SV1)") - 45.0).abs() < 0.5, "{}", last(&out, "POS(SV1)"));
        // it turns at 600°/s: 45° takes 75 ms after the first pulse ends
        let pos = out.probe("POS(SV1)").unwrap();
        let k = out.x.iter().position(|t| *t >= 0.04).unwrap();
        assert!(pos.re[k] > 15.0 && pos.re[k] < 30.0, "{}", pos.re[k]);
        // and draws more current while it turns
        let i = out.probe("I(SV1.V+)").unwrap();
        assert!(i.re[k] > 0.1 && last(&out, "I(SV1.V+)") < 0.01, "{} {}", i.re[k], last(&out, "I(SV1.V+)"));
    }

    #[test]
    fn a_pot_reads_a_slider_through_a_rack() {
        // a pinion (revolute) drives a rack (slider): 5 mm of travel per radian
        let rel = Relation { id: 99, name: "Rack".into(), kind: RelationKind::Rack, a: 1, b: Some(2), ratio: 5.0, reverse: false, phase: 0.0 };
        let a = asm(vec![mate(1, "Pinion", MateKind::Revolute, None), mate(2, "Rack", MateKind::Slider, Some([0.0, 40.0]))], vec![rel]);
        let mut p = board(
            &[("M", "6V 10000rpm 1A 1:200"), ("RV", "10k")],
            &[("6V", &[("M1", "1")]), ("GND", &[("M1", "2"), ("RV1", "3")]), ("3V3", &[("RV1", "1")]), ("WIPER", &[("RV1", "2")])],
            &[("6V", "6"), ("3V3", "3.3")],
        );
        let mut pot = new_link("RV1", Role::Pot, 0, 2, true, None);
        pot.range = Some([40.0, 0.0]);
        p.sim.links = vec![new_link("M1", Role::Motor, 0, 1, false, None), pot];
        let out = tran(&p, 0.5, &[a]);
        // 50 rpm = 5.24 rad/s = 26 mm/s at the rack
        let k = out.x.iter().position(|t| *t >= 0.5).unwrap();
        let slide = out.probe("POS(RV1)").unwrap().re[k];
        assert!(slide > 11.0 && slide < 14.0, "{slide}");
        let v = out.probe("V(WIPER)").unwrap().re[k];
        assert!((v - 3.3 * slide / 40.0).abs() < 0.01, "{v} {slide}");
        let rack = out.mates.iter().find(|m| m.mate == 2).unwrap();
        assert!((rack.slide.as_ref().unwrap()[k] - slide).abs() < 1e-9);
        assert!(out.mates.iter().find(|m| m.mate == 1).unwrap().angle.is_some());
    }

    #[test]
    fn an_encoder_counts_the_turns() {
        let a = asm(vec![mate(1, "Wheel", MateKind::Revolute, None)], vec![]);
        let mut p = board(
            &[("M", "6V 6000rpm 1A 1:10"), ("ENC", "Encoder")],
            &[("6V", &[("M1", "1")]), ("GND", &[("M1", "2"), ("ENC1", "4")]), ("3V3", &[("ENC1", "3")]), ("A", &[("ENC1", "1")]), ("B", &[("ENC1", "2")])],
            &[("6V", "6"), ("3V3", "3.3")],
        );
        let mut enc = new_link("ENC1", Role::Encoder, 0, 1, false, None);
        enc.ratio = 12.0;
        p.sim.links = vec![new_link("M1", Role::Motor, 0, 1, false, None), enc];
        let out = tran(&p, 0.5, &[a]);
        let turns = last(&out, "POS(M1)") / 360.0;
        let count = |q: &str| out.probe(q).unwrap().re.windows(2).filter(|w| w[0] < 1.65 && w[1] >= 1.65).count() as f64;
        // one rising edge of A and of B per count cycle
        for q in ["V(A)", "V(B)"] {
            assert!((count(q) - turns * 12.0).abs() <= 1.0, "{q}: {} edges for {turns} turns", count(q));
        }
        assert!(turns > 3.0);
    }

    /// The Pico arm example's circuit and mechanism.
    pub(crate) fn arm_board() -> (Pcb, Assembly) {
        let a = asm(vec![mate(1, "Arm", MateKind::Revolute, Some([0.0, 90.0])), mate(2, "Horn", MateKind::Revolute, None)], vec![]);
        let nets: &[(&str, &[(&str, &str)])] = &[
            ("GND", &[("U1", "3"), ("U1", "33"), ("Q1", "2"), ("SW1", "2"), ("RV1", "1"), ("SV1", "1")]),
            ("VMOT", &[("M1", "1"), ("D1", "1")]),
            ("DRAIN", &[("M1", "2"), ("Q1", "3"), ("D1", "2")]),
            ("GATE", &[("U1", "20"), ("R1", "1")]),
            ("G", &[("R1", "2"), ("Q1", "1")]),
            ("LIMIT", &[("U1", "19"), ("SW1", "1")]),
            ("3V3", &[("U1", "36"), ("RV1", "3")]),
            ("ANGLE", &[("U1", "31"), ("RV1", "2")]),
            ("VBUS", &[("U1", "40"), ("SV1", "2")]),
            ("SERVO", &[("U1", "21"), ("SV1", "3")]),
        ];
        let mut p =
            board(&[("PICO", "Pico"), ("M", "TT"), ("Q_NMOS", "AO3400"), ("D", "1N5819"), ("R", "100"), ("SW", "Limit"), ("RV", "10k"), ("SERVO", "SG90")], nets, &[("VMOT", "6")]);
        p.sim.mcus = vec![crate::circuit_mcu::McuSetup {
            part: "U1".into(),
            firmware: format!("{}/examples/firmware/arm.uf2", env!("CARGO_MANIFEST_DIR")),
            usb: true,
            ..Default::default()
        }];
        let mut m = new_link("M1", Role::Motor, 0, 1, false, None);
        m.ratio = 5.0;
        p.sim.links = vec![
            m,
            new_link("SW1", Role::Switch, 0, 1, false, Some([0.0, 90.0])),
            new_link("RV1", Role::Pot, 0, 1, false, Some([0.0, 90.0])),
            new_link("SV1", Role::Servo, 0, 2, false, None),
        ];
        (p, a)
    }

    #[test]
    fn a_pico_homes_an_arm_and_sweeps_a_servo() {
        let (p, a) = arm_board();
        let out = tran(&p, 1.0, &[a]);
        let uart: Vec<&str> = out.mcus[0].uart.iter().map(|x| x.2.as_str()).collect();
        assert_eq!(uart.first(), Some(&"homing from 0 deg"), "{uart:?} {:?} {:?}", out.notes, out.mcus[0].notes);
        let hit = uart.iter().find(|l| l.starts_with("limit switch at")).unwrap_or_else(|| panic!("{uart:?}"));
        // the switch closes 2° before the end stop
        let deg: i32 = hit.split_whitespace().nth(3).unwrap().parse().unwrap();
        assert!((86..=89).contains(&deg), "{hit}");
        assert!(uart.contains(&"servo at 2 ms"), "{uart:?}");
        // the arm coasts on to its end stop; the servo follows the sweep to 90°
        assert!((last(&out, "POS(M1)") - 90.0).abs() < 0.5, "{}", last(&out, "POS(M1)"));
        assert!((last(&out, "POS(SV1)") - 90.0).abs() < 1.0, "{}", last(&out, "POS(SV1)"));
        assert_eq!(out.mates.len(), 2);
    }

    #[test]
    fn links_to_missing_mates_are_noted() {
        let mut p = board(&[("M", "TT")], &[("6V", &[("M1", "1")]), ("GND", &[("M1", "2")])], &[("6V", "6")]);
        p.sim.links = vec![new_link("M1", Role::Motor, 3, 1, false, None)];
        let out = tran(&p, 0.01, &[]);
        assert!(out.notes.iter().any(|n| n.contains("assembly is gone")), "{:?}", out.notes);
        assert!(out.mates.is_empty());
    }
}
