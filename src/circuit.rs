//! Circuit simulation of a PCB tab's schematic: the setup saved with the PCB (sources, ground,
//! pressed switches, the analysis), models for the library parts by their value (1N4148, BC847,
//! NE555, LM358, AMS1117-3.3, …), the circuit they make for the solver, and the results.

use serde::{Deserialize, Serialize};

use crate::circuit_mech::{MechExt, MechPart, MechSim};
use crate::circuit_solve::{Bjt, Circuit, Cx, Dev, Diode, Ext, Mos, OpAmp, Reg, Sim, Timer, VT, Wave};
use crate::pcb::{LabelKind, Part, Pcb};

// ---------------------------------------------------------------- setup

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// DC operating point.
    Op,
    #[default]
    Tran,
    Ac,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Op => "DC operating point",
            Kind::Tran => "Transient",
            Kind::Ac => "AC sweep",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Analysis {
    pub kind: Kind,
    /// Transient stop time, s (0: worked out from the circuit's time constants).
    pub stop: f64,
    /// Longest transient step, s (0: stop / 500).
    pub step: f64,
    /// Start the transient from the DC operating point instead of from rest.
    pub from_op: bool,
    pub f_start: f64,
    pub f_stop: f64,
    pub per_decade: u32,
}

impl Default for Analysis {
    fn default() -> Self {
        Analysis { kind: Kind::Tran, stop: 0.0, step: 0.0, from_op: false, f_start: 10.0, f_stop: 1e6, per_decade: 20 }
    }
}

/// A voltage or current source between two nets.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Source {
    pub name: String,
    pub plus: String,
    /// Empty: ground.
    pub minus: String,
    /// `5`, `DC 5`, `PULSE(0 5 1m 1u 1u 4m 10m)`, `SIN(0 1 1k)`, `PWL(0 0 1m 5)`.
    pub wave: String,
    /// A current source (out of plus into the circuit) instead of a voltage source.
    pub current: bool,
    /// Small-signal magnitude for AC sweeps.
    pub ac: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SimSetup {
    /// Empty: a DC supply on every power net, by its name (VCC 5 V, +3V3 3.3 V, 12V …).
    pub sources: Vec<Source>,
    /// Ground net (empty: the ground ports, or GND).
    pub ground: String,
    /// Switches that are pressed (closed).
    pub closed: Vec<String>,
    /// Signals to plot: net names or V(NET), V(A,B), I(R1), I(U1.OUT), I(VCC).
    pub probes: Vec<String>,
    pub analysis: Analysis,
    /// Firmware for the microcontroller boards (Raspberry Pi Pico).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mcus: Vec<crate::circuit_mcu::McuSetup>,
    /// Motors, servos and sensors tied to assembly mates.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<crate::circuit_mech::Link>,
}

impl SimSetup {
    pub fn is_empty(&self) -> bool {
        *self == SimSetup::default()
    }
}

// ---------------------------------------------------------------- numbers

/// A number with an SI prefix and an optional unit: 10k, 4k7, 2.2uF, 100n, 1R5, 1meg, 5V, 1kHz.
/// `m` is milli and `M` mega.
pub fn parse_si(s: &str) -> Option<f64> {
    let s = s.trim();
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let start_digits = i;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if i == start_digits {
        return None;
    }
    // exponent
    if i + 1 < b.len() && (b[i] == b'e' || b[i] == b'E') && (b[i + 1].is_ascii_digit() || ((b[i + 1] == b'-' || b[i + 1] == b'+') && i + 2 < b.len() && b[i + 2].is_ascii_digit()))
    {
        i += 2;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    let mut v: f64 = s[..i].parse().ok()?;
    let rest = &s[i..];
    let lower = rest.to_ascii_lowercase();
    let (mult, plen) = if lower.starts_with("meg") {
        (1e6, 3)
    } else {
        match rest.chars().next() {
            Some('T') => (1e12, 1),
            Some('G') => (1e9, 1),
            Some('M') => (1e6, 1),
            Some('k') | Some('K') => (1e3, 1),
            Some('m') => (1e-3, 1),
            Some('u') | Some('µ') | Some('μ') => (1e-6, rest.chars().next().unwrap().len_utf8()),
            Some('n') | Some('N') => (1e-9, 1),
            Some('p') | Some('P') => (1e-12, 1),
            Some('f') => (1e-15, 1),
            Some('R') | Some('r') => (1.0, 1),
            _ => (1.0, 0),
        }
    };
    // 4k7 style: digits after the prefix are the decimals
    let tail = &rest[plen..];
    if plen > 0 {
        let d: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !d.is_empty() && !s[..i].contains('.') {
            v = format!("{}.{}", &s[..i], d).parse().ok()?;
        }
    }
    Some(v * mult)
}

/// Leakage through the solver's tiny conductances (fA, fW) as 0, for display.
pub fn tidy(v: f64, unit: &str) -> f64 {
    let floor = match unit {
        "A" => 1e-11,
        "W" => 1e-12,
        _ => 0.0,
    };
    if v.abs() < floor { 0.0 } else { v }
}

/// A value with three significant digits and an SI prefix: 4.70 kΩ → "4.7 kΩ".
pub fn si(v: f64, unit: &str) -> String {
    if !v.is_finite() {
        return format!("{v} {unit}");
    }
    if v == 0.0 {
        return format!("0 {unit}");
    }
    let a = v.abs();
    let (m, p) = [(1e9, "G"), (1e6, "M"), (1e3, "k"), (1.0, ""), (1e-3, "m"), (1e-6, "µ"), (1e-9, "n"), (1e-12, "p"), (1e-15, "f")]
        .iter()
        .copied()
        .find(|(m, _)| a >= *m * 0.9995)
        .unwrap_or((1e-15, "f"));
    let x = v / m;
    let s = if x.abs() >= 99.95 {
        format!("{x:.0}")
    } else if x.abs() >= 9.995 {
        format!("{x:.1}")
    } else {
        format!("{x:.2}")
    };
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    format!("{s} {p}{unit}")
}

/// A supply voltage named in text: 3V3, +3.3V, -3.3, 5V0, 12V.
fn volts_in(s: &str) -> Option<f64> {
    let u = s.to_ascii_uppercase();
    let b = u.as_bytes();
    for i in 0..b.len() {
        if !b[i].is_ascii_digit() || (i > 0 && (b[i - 1].is_ascii_digit() || b[i - 1] == b'.')) {
            continue;
        }
        let mut j = i;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        let int = &u[i..j];
        if j < b.len() && b[j] == b'V' {
            let mut k = j + 1;
            while k < b.len() && b[k].is_ascii_digit() {
                k += 1;
            }
            let v = if k > j + 1 { format!("{int}.{}", &u[j + 1..k]) } else { int.to_string() };
            return v.parse().ok();
        }
        if j < b.len() && b[j] == b'.' {
            let mut k = j + 1;
            while k < b.len() && b[k].is_ascii_digit() {
                k += 1;
            }
            if k < b.len() && b[k] == b'V' || (i > 0 && (b[i - 1] == b'-' || b[i - 1] == b'+')) {
                return u[i..k].parse().ok();
            }
        }
    }
    None
}

impl Wave {
    /// `5`, `DC 5`, `PULSE(v1 v2 delay rise fall width period)`, `SIN(offset amp freq delay damping)`,
    /// `PWL(t1 v1 t2 v2 …)`; numbers take SI prefixes.
    pub fn parse(s: &str) -> Result<Wave, String> {
        let t = s.trim();
        let up = t.to_ascii_uppercase();
        let args = |name: &str| -> Result<Vec<f64>, String> {
            let inner = t[name.len()..].trim().trim_start_matches('(').trim_end_matches(')');
            inner.split(|c: char| c.is_whitespace() || c == ',').filter(|x| !x.is_empty()).map(|x| parse_si(x).ok_or_else(|| format!("{x} is not a number"))).collect()
        };
        if up.starts_with("PULSE") {
            let a = args("PULSE")?;
            if a.len() < 2 {
                return Err("PULSE(v1 v2 delay rise fall width period)".into());
            }
            let g = |k: usize, d: f64| a.get(k).copied().unwrap_or(d);
            let (tr, tf) = (g(3, 0.0), g(4, 0.0));
            let tr = if tr > 0.0 { tr } else { 1e-9 };
            let tf = if tf > 0.0 { tf } else { 1e-9 };
            let per = g(6, 0.0);
            let pw = g(5, if per > 0.0 { (per - tr - tf) / 2.0 } else { f64::INFINITY });
            return Ok(Wave::Pulse { v1: a[0], v2: a[1], td: g(2, 0.0), tr, tf, pw, per });
        }
        if up.starts_with("SIN") {
            let a = args("SIN")?;
            if a.len() < 3 {
                return Err("SIN(offset amplitude frequency delay damping)".into());
            }
            return Ok(Wave::Sin { vo: a[0], va: a[1], f: a[2], td: a.get(3).copied().unwrap_or(0.0), theta: a.get(4).copied().unwrap_or(0.0) });
        }
        if up.starts_with("PWL") {
            let a = args("PWL")?;
            if a.len() < 2 || a.len() % 2 != 0 {
                return Err("PWL(t1 v1 t2 v2 …): pairs of time and value".into());
            }
            let pts: Vec<(f64, f64)> = a.chunks(2).map(|c| (c[0], c[1])).collect();
            if pts.windows(2).any(|w| w[1].0 < w[0].0) {
                return Err("PWL times must increase".into());
            }
            return Ok(Wave::Pwl(pts));
        }
        let v = if up.starts_with("DC") { &t[2..] } else { t };
        parse_si(v).map(Wave::Dc).ok_or_else(|| format!("{t}: give a number (5, 3.3, 10m), DC 5, PULSE(…), SIN(…) or PWL(…)"))
    }
}

// ---------------------------------------------------------------- models

/// What a part is simulated as.
#[derive(Clone, Debug, PartialEq)]
pub enum Model {
    R(f64),
    C(f64),
    L(f64),
    D(Diode, f64, f64, String),
    Q(Bjt, bool, [f64; 2], String),
    M(Mos, bool, [f64; 2], String),
    Timer(Timer, Vec<String>),
    OpAmp(OpAmp, Vec<[String; 3]>, [String; 2], String),
    Reg(Reg, [String; 3], String),
    Switch(bool),
    /// A Raspberry Pi Pico board.
    Pico,
    Motor(crate::circuit_mech::Motor),
    Servo(crate::circuit_mech::Servo, [String; 3]),
    /// Track resistance.
    Pot(f64),
    /// Pins A, B, VCC, GND.
    Encoder([String; 4]),
    /// Left out of the circuit, with the reason.
    Open(String),
}

fn has(v: &str, keys: &[&str]) -> bool {
    keys.iter().any(|k| v.contains(k))
}

/// A diode: model, series resistance, junction capacitance and the model name.
pub(crate) fn diode_model(value: &str) -> (Diode, f64, f64, String) {
    let u = value.to_ascii_uppercase();
    let d = |is: f64, n: f64, bv: f64| Diode { is, n, bv, ibv: 1e-3 };
    // zeners: BZX84C5V1, 1N4733A, "5V1 zener"
    let zener_v = if let Some(k) = u.find("1N47") {
        u[k + 4..].chars().take(2).collect::<String>().parse::<usize>().ok().and_then(|n| {
            const V: [f64; 25] = [3.3, 3.6, 3.9, 4.3, 4.7, 5.1, 5.6, 6.2, 6.8, 7.5, 8.2, 9.1, 10.0, 11.0, 12.0, 13.0, 15.0, 16.0, 18.0, 20.0, 22.0, 24.0, 27.0, 30.0, 33.0];
            n.checked_sub(28).and_then(|i| V.get(i).copied())
        })
    } else if has(&u, &["BZX", "BZT", "ZENER", "MMSZ", "ZMM"]) {
        volts_in(u.split_once('C').map(|x| x.1).unwrap_or(&u)).or_else(|| volts_in(&u))
    } else {
        None
    };
    if let Some(bv) = zener_v {
        return (Diode { is: 1e-14, n: 1.0, bv, ibv: 5e-3 }, 1.0, 100e-12, format!("{} V zener", si(bv, "").trim()));
    }
    if has(&u, &["1N400", "1N540", "M7", "S1M", "RECT"]) {
        return (d(7.0e-9, 1.81, 1000.0), 0.034, 15e-12, "1N4007 rectifier".into());
    }
    if has(&u, &["5817", "5818", "5819", "SS1", "SS3", "SCHOTTKY", "B5819"]) {
        return (d(3.2e-5, 1.37, 40.0), 0.05, 110e-12, "1N5819 Schottky".into());
    }
    if has(&u, &["BAT", "BAS40", "SD103"]) {
        return (d(1e-7, 1.05, 30.0), 1.5, 10e-12, "BAT54 Schottky".into());
    }
    (d(2.52e-9, 1.752, 100.0), 0.568, 4e-12, "1N4148".into())
}

/// An LED from its colour, or a forward voltage in its value (Red, Blue, "2.1V").
pub(crate) fn led_model(value: &str) -> (Diode, f64, f64, String) {
    let u = value.to_ascii_uppercase();
    let (vf, name) = if let Some(v) = volts_in(&u) {
        (v, "LED")
    } else if has(&u, &["IR", "INFRA"]) {
        (1.25, "infrared LED")
    } else if has(&u, &["BLUE", "WHITE", "UV", "VIOLET", "PURE GREEN", "TRUE GREEN", "RGB"]) {
        (3.1, "blue/white LED")
    } else if u.contains("GREEN") {
        (2.15, "green LED")
    } else if u.contains("YELLOW") || u.contains("AMBER") {
        (2.05, "yellow LED")
    } else if u.contains("ORANGE") {
        (2.0, "orange LED")
    } else {
        (1.9, "red LED")
    };
    // Vf at 20 mA with n = 2 and 5 Ω
    let (n, rs, i) = (2.0, 5.0, 0.02);
    let is = i / ((vf - i * rs) / (n * VT)).exp();
    (Diode { is, n, bv: 5.0, ibv: 1e-6 }, rs, 30e-12, format!("{name}, Vf {} at 20 mA", si(vf, "V")))
}

fn bjt_model(value: &str, pnp: bool) -> (Bjt, [f64; 2], String) {
    let u = value.to_ascii_uppercase();
    let m = |is: f64, bf: f64, br: f64, vaf: f64| Bjt { is, bf, br, vaf, nf: 1.0 };
    let table: &[(&[&str], bool, Bjt, [f64; 2], &str)] = &[
        (&["3904"], false, m(6.734e-15, 416.0, 0.737, 74.0), [4.5e-12, 3.6e-12], "2N3904"),
        (&["3906"], true, m(1.41e-15, 180.7, 4.98, 18.7), [8.1e-12, 7.3e-12], "2N3906"),
        (&["2222", "MMBT2222"], false, m(14.3e-15, 256.0, 6.09, 74.0), [22e-12, 7.3e-12], "2N2222"),
        (&["2907"], true, m(650e-15, 231.0, 3.56, 116.0), [20e-12, 14.8e-12], "2N2907"),
        (&["BC847", "BC547", "BC846", "BC848", "BC548"], false, m(1.8e-14, 290.0, 6.0, 80.0), [12e-12, 5e-12], "BC847"),
        (&["BC857", "BC557", "BC856", "BC858", "BC558"], true, m(2.0e-14, 250.0, 6.0, 40.0), [12e-12, 7e-12], "BC857"),
        (&["S8050", "SS8050"], false, m(2e-13, 200.0, 5.0, 100.0), [30e-12, 10e-12], "S8050"),
        (&["S8550", "SS8550"], true, m(2e-13, 200.0, 5.0, 100.0), [30e-12, 12e-12], "S8550"),
        (&["TIP31", "TIP41", "BD139"], false, m(1e-12, 80.0, 2.0, 100.0), [200e-12, 80e-12], "TIP31 power"),
        (&["TIP32", "TIP42", "BD140"], true, m(1e-12, 80.0, 2.0, 100.0), [200e-12, 80e-12], "TIP32 power"),
    ];
    for (keys, p, b, caps, name) in table {
        if *p == pnp && has(&u, keys) {
            return (b.clone(), *caps, (*name).into());
        }
    }
    (m(1e-14, 200.0, 5.0, 80.0), [10e-12, 5e-12], format!("generic {}", if pnp { "PNP" } else { "NPN" }))
}

fn mos_model(value: &str, pmos: bool) -> (Mos, [f64; 2], String) {
    let u = value.to_ascii_uppercase();
    let m = |vto: f64, kp: f64| Mos { vto, kp, lambda: 0.01 };
    let table: &[(&[&str], bool, Mos, [f64; 2], &str)] = &[
        (&["2N7002", "2N7000"], false, m(2.1, 0.11), [40e-12, 5e-12], "2N7002"),
        (&["BSS138"], false, m(1.3, 0.09), [25e-12, 5e-12], "BSS138"),
        (&["AO3400", "SI2302"], false, m(1.05, 4.0), [600e-12, 60e-12], "AO3400"),
        (&["IRLZ44", "IRLB8721", "IRL540"], false, m(1.8, 30.0), [3000e-12, 200e-12], "IRLZ44N"),
        (&["IRF540", "IRFZ44", "IRF3205"], false, m(3.0, 15.0), [2000e-12, 150e-12], "IRF540"),
        (&["AO3401", "SI2301", "SI2305"], true, m(0.9, 2.5), [650e-12, 70e-12], "AO3401"),
        (&["BSS84"], true, m(1.6, 0.05), [25e-12, 5e-12], "BSS84"),
        (&["IRF9540", "IRF9Z34", "IRF4905"], true, m(3.0, 8.0), [1500e-12, 200e-12], "IRF9540"),
    ];
    for (keys, p, mm, caps, name) in table {
        if *p == pmos && has(&u, keys) {
            return (mm.clone(), *caps, (*name).into());
        }
    }
    (m(if pmos { 1.0 } else { 1.5 }, 1.0), [100e-12, 20e-12], format!("generic {}", if pmos { "P-MOSFET" } else { "N-MOSFET" }))
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, ' ' | '_' | '/' | '~')).collect::<String>().to_ascii_uppercase()
}

/// Pin numbers for each role: by the part's pin names when they name every role, else the defaults.
fn roles(part: &Part, pins: &[String], roles: &[&[&str]], default: &[&str]) -> Option<Vec<String>> {
    if !part.pin_names.is_empty() {
        let names: Vec<String> = part.pin_names.iter().map(|n| norm(n)).collect();
        let found: Vec<Option<String>> = roles.iter().map(|aliases| names.iter().position(|n| aliases.iter().any(|a| n == a)).and_then(|k| pins.get(k).cloned())).collect();
        if found.iter().all(|f| f.is_some()) {
            return Some(found.into_iter().map(|f| f.unwrap()).collect());
        }
    }
    if default.iter().all(|d| pins.iter().any(|p| p == d)) { Some(default.iter().map(|s| s.to_string()).collect()) } else { None }
}

struct OpSpec {
    keys: &'static [&'static str],
    units: usize,
    gbw: f64,
    sr: f64,
    hr: [f64; 2],
    name: &'static str,
}

const OPAMPS: &[OpSpec] = &[
    OpSpec { keys: &["LM358", "LM2904", "LM158"], units: 2, gbw: 1e6, sr: 0.3e6, hr: [1.5, 0.02], name: "LM358" },
    OpSpec { keys: &["LM324", "LM2902"], units: 4, gbw: 1e6, sr: 0.3e6, hr: [1.5, 0.02], name: "LM324" },
    OpSpec { keys: &["TL072", "TL082", "TL062"], units: 2, gbw: 3e6, sr: 13e6, hr: [1.5, 1.5], name: "TL072" },
    OpSpec { keys: &["TL074", "TL084", "TL064"], units: 4, gbw: 3e6, sr: 13e6, hr: [1.5, 1.5], name: "TL074" },
    OpSpec { keys: &["TL071", "TL081", "TL061"], units: 1, gbw: 3e6, sr: 13e6, hr: [1.5, 1.5], name: "TL071" },
    OpSpec { keys: &["NE5532", "SA5532"], units: 2, gbw: 10e6, sr: 9e6, hr: [1.5, 1.5], name: "NE5532" },
    OpSpec { keys: &["NE5534"], units: 1, gbw: 10e6, sr: 13e6, hr: [1.5, 1.5], name: "NE5534" },
    OpSpec { keys: &["741", "OP07"], units: 1, gbw: 1e6, sr: 0.5e6, hr: [2.0, 2.0], name: "LM741" },
    OpSpec { keys: &["MCP6001", "MCP6L01", "MCP6271"], units: 1, gbw: 1e6, sr: 0.6e6, hr: [0.025, 0.025], name: "MCP6001" },
    OpSpec { keys: &["MCP6002", "MCP6L02", "MCP6272"], units: 2, gbw: 1e6, sr: 0.6e6, hr: [0.025, 0.025], name: "MCP6002" },
    OpSpec { keys: &["MCP6004", "MCP6L04", "MCP6274"], units: 4, gbw: 1e6, sr: 0.6e6, hr: [0.025, 0.025], name: "MCP6004" },
    OpSpec { keys: &["LMV321", "TLV9001", "OPA333", "OPA340"], units: 1, gbw: 1e6, sr: 1e6, hr: [0.02, 0.02], name: "LMV321" },
    OpSpec { keys: &["LMV358", "TLV9002", "OPA2333", "OPA2340"], units: 2, gbw: 1e6, sr: 1e6, hr: [0.02, 0.02], name: "LMV358" },
    OpSpec { keys: &["LMV324", "TLV9004"], units: 4, gbw: 1e6, sr: 1e6, hr: [0.02, 0.02], name: "LMV324" },
    OpSpec { keys: &["OPA2134", "OPA1612", "OPA2604"], units: 2, gbw: 8e6, sr: 20e6, hr: [1.0, 1.0], name: "OPA2134" },
    OpSpec { keys: &["OPA134", "OPA1611"], units: 1, gbw: 8e6, sr: 20e6, hr: [1.0, 1.0], name: "OPA134" },
];

fn opamp_model(part: &Part, pins: &[String]) -> Option<Result<Model, String>> {
    let u = part.value.to_ascii_uppercase();
    let spec = OPAMPS.iter().find(|s| has(&u, s.keys));
    let generic = has(&u, &["OPAMP", "OP AMP", "OP-AMP"]);
    if spec.is_none() && !generic {
        return None;
    }
    let units = spec.map(|s| s.units).unwrap_or(match pins.len() {
        5 => 1,
        14 => 4,
        _ => 2,
    });
    let (gbw, sr, hr, name) = spec.map(|s| (s.gbw, s.sr, s.hr, s.name.to_string())).unwrap_or((1e6, 1e6, [0.05, 0.05], "rail-to-rail op amp".into()));
    let m = OpAmp { a0: 1e5, gbw, sr, hr_hi: hr[0], hr_lo: hr[1], ro: 50.0 };
    let unit_roles = |k: usize| -> [Vec<String>; 3] {
        let (n, l) = (k + 1, (b'A' + k as u8) as char);
        let mut out = vec![format!("OUT{n}"), format!("OUT{l}"), format!("{l}OUT"), format!("VOUT{l}"), format!("VOUT{n}")];
        let mut inn = vec![format!("-IN{n}"), format!("IN{n}-"), format!("-IN{l}"), format!("IN{l}-"), format!("{l}-"), format!("VIN{l}-"), format!("VIN{n}-")];
        let mut inp = vec![format!("+IN{n}"), format!("IN{n}+"), format!("+IN{l}"), format!("IN{l}+"), format!("{l}+"), format!("VIN{l}+"), format!("VIN{n}+")];
        if units == 1 {
            out.extend(["OUT".into(), "VOUT".into(), "OUTPUT".into()]);
            inn.extend(["-IN".into(), "IN-".into(), "INV".into(), "VIN-".into(), "-".into()]);
            inp.extend(["+IN".into(), "IN+".into(), "NONINV".into(), "VIN+".into(), "+".into()]);
        }
        [out, inn, inp]
    };
    let supply: [&[&str]; 2] = [&["V+", "VCC", "VDD", "VS+", "+V", "VCC+"], &["V-", "VEE", "VSS", "GND", "VS-", "-V", "VCC-"]];
    // names first
    let mut by_name = None;
    if !part.pin_names.is_empty() {
        let names: Vec<String> = part.pin_names.iter().map(|n| norm(n)).collect();
        let find = |al: &[String]| names.iter().position(|n| al.iter().any(|a| a == n)).and_then(|k| pins.get(k).cloned());
        let mut us = vec![];
        for k in 0..units {
            let [o, n, p] = unit_roles(k);
            match (find(&o), find(&n), find(&p)) {
                (Some(o), Some(n), Some(p)) => us.push([p, n, o]),
                _ => break,
            }
        }
        let vp = find(&supply[0].iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let vn = find(&supply[1].iter().map(|s| s.to_string()).collect::<Vec<_>>());
        if let (false, Some(vp), Some(vn)) = (us.is_empty(), vp, vn) {
            by_name = Some((us, [vp, vn]));
        }
    }
    let defaults: Option<(Vec<[&str; 3]>, [&str; 2])> = match (units, pins.len()) {
        (1, 5) => Some((vec![["3", "4", "1"]], ["5", "2"])),
        (1, 8) => Some((vec![["3", "2", "6"]], ["7", "4"])),
        (2, 8) => Some((vec![["3", "2", "1"], ["5", "6", "7"]], ["8", "4"])),
        (4, 14) => Some((vec![["3", "2", "1"], ["5", "6", "7"], ["10", "9", "8"], ["12", "13", "14"]], ["4", "11"])),
        _ => None,
    };
    let (us, sup) = match (by_name, defaults) {
        (Some(x), _) => x,
        (None, Some((us, sup))) => (us.into_iter().map(|u| u.map(String::from)).collect(), sup.map(String::from)),
        (None, None) => {
            return Some(Err(format!("{} {}: give its pin names (OUT, IN-, IN+, V+, V-), its {} pins don't match a {}-amp package", part.reference, part.value, pins.len(), units)));
        }
    };
    Some(Ok(Model::OpAmp(m, us, sup, format!("{name} op amp ×{units}, GBW {}, slew {}", si(gbw, "Hz"), si(sr * 1e-6, "V/µs")))))
}

fn reg_model(part: &Part, pins: &[String]) -> Option<Result<Model, String>> {
    let u = part.value.to_ascii_uppercase();
    let after_dash = |s: &str| s.split_once('-').map(|x| x.1.to_string());
    // (vset, dropout, ground current, adjustable, default pins [in, ref, out], name)
    let spec: Option<(f64, f64, f64, bool, [&str; 3], String)> = if let Some(k) = u.find("78") {
        let rest: String = u[k + 2..].trim_start_matches(['L', 'M']).chars().take_while(|c| c.is_ascii_digit()).collect();
        rest.parse::<f64>().ok().filter(|v| *v >= 2.0 && rest.len() == 2).map(|v| (v, 2.0, 5e-3, false, ["1", "2", "3"], format!("78{rest}")))
    } else if u.contains("317") {
        Some((1.25, 2.0, -50e-6, true, ["3", "1", "2"], "LM317".into()))
    } else if u.contains("1117") {
        match after_dash(&u).and_then(|s| volts_in(&s).or_else(|| s.parse().ok())) {
            Some(v) => Some((v, 1.1, 5e-3, false, ["3", "1", "2"], format!("AMS1117-{v}"))),
            None => Some((1.25, 1.1, -60e-6, true, ["3", "1", "2"], "AMS1117-ADJ".into())),
        }
    } else if u.contains("AP2112") {
        let v = after_dash(&u).and_then(|s| volts_in(&s).or_else(|| s.parse().ok())).unwrap_or(3.3);
        Some((v, 0.25, 55e-6, false, ["1", "2", "5"], format!("AP2112-{v}")))
    } else if u.contains("MCP1700") {
        let v = after_dash(&u).and_then(|s| s.get(..2).and_then(|d| d.parse::<f64>().ok())).map(|d| d / 10.0).unwrap_or(3.3);
        Some((v, 0.18, 1.6e-6, false, ["3", "1", "2"], format!("MCP1700-{v}")))
    } else if has(&u, &["LDO", "REG"]) {
        volts_in(&u).map(|v| (v, 0.3, 50e-6, false, ["1", "2", "3"], format!("{v} V LDO")))
    } else {
        None
    };
    let (vset, vdo, iq, adj, def, name) = spec?;
    let ref_names: &[&str] = if adj { &["ADJ", "ADJUST", "GND/ADJ", "ADJ/GND"] } else { &["GND", "VSS", "GND/ADJ", "ADJ/GND", "0V"] };
    let r = roles(part, pins, &[&["IN", "VIN", "INPUT", "VI"], ref_names, &["OUT", "VOUT", "OUTPUT", "VO"]], &def);
    let Some(r) = r else {
        return Some(Err(format!("{} {}: give its pin names (IN, GND or ADJ, OUT)", part.reference, part.value)));
    };
    let desc = if adj {
        format!("{name} adjustable regulator, 1.25 V reference, dropout {}", si(vdo, "V"))
    } else {
        format!("{name} regulator, {} out, dropout {}", si(vset, "V"), si(vdo, "V"))
    };
    Some(Ok(Model::Reg(Reg { vset, vdo, iq, ro: 0.02 }, [r[0].clone(), r[1].clone(), r[2].clone()], desc)))
}

/// The model for a part, from its symbol and value.
pub fn model(part: &Part, setup: &SimSetup) -> Result<Model, String> {
    let val = || parse_si(&part.value).ok_or_else(|| format!("{}: {} is not a value (e.g. 10k, 100n)", part.reference, part.value));
    let pins = part.footprint().pin_numbers();
    Ok(match part.symbol.as_str() {
        "R" => {
            let r = val()?;
            if r <= 0.0 {
                return Err(format!("{}: resistance must be positive", part.reference));
            }
            Model::R(r)
        }
        "C" | "CP" => Model::C(val()?),
        "L" => Model::L(val()?),
        "D" => {
            let (d, rs, cj, name) = diode_model(&part.value);
            Model::D(d, rs, cj, name)
        }
        "LED" => {
            let (d, rs, cj, name) = led_model(&part.value);
            Model::D(d, rs, cj, name)
        }
        "Q_NPN" | "Q_PNP" => {
            let pnp = part.symbol == "Q_PNP";
            let (b, caps, name) = bjt_model(&part.value, pnp);
            Model::Q(b, pnp, caps, name)
        }
        "Q_NMOS" | "Q_PMOS" => {
            let p = part.symbol == "Q_PMOS";
            let (m, caps, name) = mos_model(&part.value, p);
            Model::M(m, p, caps, name)
        }
        "SW" => Model::Switch(setup.closed.iter().any(|s| s.eq_ignore_ascii_case(&part.reference))),
        "M" => Model::Motor(crate::circuit_mech::motor_model(&part.value)),
        "SERVO" => {
            let r = roles(part, &pins, &[&["GND", "-", "VSS"], &["V+", "VCC", "+", "5V", "PWR", "VDD"], &["SIG", "PWM", "S", "IN", "CTRL"]], &["1", "2", "3"]);
            let Some(r) = r else { return Err(format!("{}: a servo needs 3 pins (GND, V+, SIG)", part.reference)) };
            Model::Servo(crate::circuit_mech::servo_model(&part.value), [r[0].clone(), r[1].clone(), r[2].clone()])
        }
        "RV" => {
            let r = parse_si(&part.value).unwrap_or(10e3);
            if r <= 0.0 {
                return Err(format!("{}: resistance must be positive", part.reference));
            }
            Model::Pot(r)
        }
        "ENC" => {
            let r = roles(part, &pins, &[&["A", "OUTA", "CHA"], &["B", "OUTB", "CHB"], &["VCC", "V+", "VDD", "+"], &["GND", "-", "VSS"]], &["1", "2", "3", "4"]);
            let Some(r) = r else { return Err(format!("{}: an encoder needs 4 pins (A, B, VCC, GND)", part.reference)) };
            Model::Encoder([r[0].clone(), r[1].clone(), r[2].clone(), r[3].clone()])
        }
        "J" => Model::Open("connector (power comes from the simulation sources)".into()),
        "Y" => Model::Open("crystal (not simulated)".into()),
        _ if crate::circuit_mcu::is_pico(&part.symbol, &part.value, pins.len()) => Model::Pico,
        _ => {
            let u = part.value.to_ascii_uppercase();
            if u.contains("555") {
                let cmos = has(&u, &["TLC", "LMC", "7555", "ICM"]);
                let r = roles(
                    part,
                    &pins,
                    &[
                        &["GND", "VSS"],
                        &["TRIG", "TR", "TRIGGER"],
                        &["OUT", "Q", "OUTPUT"],
                        &["RESET", "RST", "R"],
                        &["CTRL", "CV", "CONT", "CONTROL"],
                        &["THR", "THRES", "THRESHOLD", "TH"],
                        &["DIS", "DISCH", "DISCHARGE"],
                        &["VCC", "VDD", "V+"],
                    ],
                    &["1", "2", "3", "4", "5", "6", "7", "8"],
                );
                let Some(r) = r else { return Err(format!("{} {}: a 555 needs 8 pins (or pin names GND, TRIG, OUT, RESET, CTRL, THR, DIS, VCC)", part.reference, part.value)) };
                return Ok(Model::Timer(Timer { cmos }, r));
            }
            if let Some(m) = opamp_model(part, &pins) {
                return m;
            }
            if let Some(m) = reg_model(part, &pins) {
                return m;
            }
            Model::Open(format!("no model for {} (left open)", if part.value.is_empty() { "this IC" } else { &part.value }))
        }
    })
}

pub fn describe(m: &Model) -> String {
    match m {
        Model::R(r) => format!("resistor {}", si(*r, "Ω")),
        Model::C(c) => format!("capacitor {}", si(*c, "F")),
        Model::L(l) => format!("inductor {}", si(*l, "H")),
        Model::D(_, _, _, n) => n.clone(),
        Model::Q(_, pnp, _, n) => format!("{n} {}", if *pnp { "PNP" } else { "NPN" }),
        Model::M(_, p, _, n) => format!("{n} {}", if *p { "P-channel" } else { "N-channel" }),
        Model::Timer(t, _) => format!("555 timer ({})", if t.cmos { "CMOS" } else { "bipolar" }),
        Model::OpAmp(_, _, _, d) | Model::Reg(_, _, d) => d.clone(),
        Model::Switch(c) => format!("switch, {}", if *c { "pressed" } else { "open" }),
        Model::Pico => "Raspberry Pi Pico (RP2040)".into(),
        Model::Motor(m) => m.name.clone(),
        Model::Servo(s, _) => s.name.clone(),
        Model::Pot(r) => format!("potentiometer {}", si(*r, "Ω")),
        Model::Encoder(_) => "quadrature encoder".into(),
        Model::Open(why) => why.clone(),
    }
}

// ---------------------------------------------------------------- building the circuit

#[derive(Clone, Debug)]
pub struct PinSim {
    pub num: String,
    pub label: String,
    pub node: usize,
    /// The element terminals on this pin: current into the pin is their sum.
    pub terms: Vec<(usize, usize)>,
}

#[derive(Clone, Debug)]
pub struct PartSim {
    pub reference: String,
    pub desc: String,
    pub modelled: bool,
    pub pins: Vec<PinSim>,
    /// For two-terminal parts, the pin whose current is the part's current (R1: pin 1, D1: anode).
    pub main: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct SrcSim {
    pub name: String,
    pub elem: usize,
}

#[derive(Clone, Debug)]
pub enum SigOf {
    Node(usize),
    /// Sum of terminal currents, times the sign.
    Terms(Vec<(usize, usize)>, f64),
}

#[derive(Clone, Debug)]
pub struct SigDef {
    pub name: String,
    pub unit: &'static str,
    pub of: SigOf,
}

pub struct Built {
    pub c: Circuit,
    pub parts: Vec<PartSim>,
    pub sources: Vec<SrcSim>,
    pub sigs: Vec<SigDef>,
    pub ground: String,
    /// Named nets and their nodes.
    pub nets: Vec<(String, usize)>,
    pub notes: Vec<String>,
    pub mcus: Vec<crate::circuit_mcu::PicoSim>,
    /// Motors, servos and sensors.
    pub mech: Vec<crate::circuit_mech::MechSim>,
}

/// A supply voltage guessed from a power net's name: +5V 5, 3V3 3.3, -12V -12, VCC/VDD 5.
pub fn supply_guess(net: &str) -> f64 {
    let u = net.to_ascii_uppercase();
    let neg = u.starts_with('-') || u.starts_with("VEE") || u.starts_with("VSS-");
    match volts_in(&u) {
        Some(v) => {
            if neg {
                -v
            } else {
                v
            }
        }
        None if u.contains("BAT") => 3.7,
        None if neg => -5.0,
        None => 5.0,
    }
}

fn ground_names(p: &Pcb) -> Vec<String> {
    let mut g: Vec<String> = p.labels.iter().filter(|l| l.kind == LabelKind::Ground).map(|l| l.text.trim().to_string()).collect();
    g.push("GND".into());
    g
}

/// The DC supplies used when the setup names no sources: one per power net.
pub fn default_sources(p: &Pcb) -> Vec<Source> {
    let nl = p.netlist();
    let gnd = ground_names(p);
    let ground = if p.sim.ground.is_empty() {
        nl.nets.iter().find(|n| gnd.iter().any(|g| g.eq_ignore_ascii_case(&n.name))).map(|n| n.name.clone()).unwrap_or_default()
    } else {
        p.sim.ground.clone()
    };
    nl.nets
        .iter()
        .filter(|n| n.power && !n.name.eq_ignore_ascii_case(&ground) && !gnd.iter().any(|g| g.eq_ignore_ascii_case(&n.name)))
        .map(|n| Source { name: n.name.clone(), plus: n.name.clone(), minus: String::new(), wave: format!("{}", supply_guess(&n.name)), current: false, ac: 0.0 })
        .collect()
}

pub fn build(p: &Pcb) -> Result<Built, String> {
    let setup = &p.sim;
    let nl = p.netlist();
    let gnd = ground_names(p);
    let ground = if !setup.ground.trim().is_empty() {
        let g = setup.ground.trim();
        nl.by_name(g).map(|i| nl.nets[i].name.clone()).ok_or_else(|| format!("no net {g} for the ground"))?
    } else {
        nl.nets
            .iter()
            .find(|n| gnd.iter().any(|g| g.eq_ignore_ascii_case(&n.name)))
            .map(|n| n.name.clone())
            .ok_or("the schematic has no ground: add a ground port (G) or choose the ground net")?
    };
    let mut c = Circuit::new();
    let mut notes = vec![];
    let mut net_node = vec![0usize; nl.nets.len()];
    let mut nets = vec![];
    for (i, n) in nl.nets.iter().enumerate() {
        if n.name == ground {
            net_node[i] = 0;
        } else {
            net_node[i] = c.node(n.name.clone());
            nets.push((n.name.clone(), net_node[i]));
        }
    }
    let mut parts = vec![];
    let mut mcus = vec![];
    let mut mech = vec![];
    // the next `Dev::Emf`/`Dev::G` value and `Dev::Pin` indices
    let nval = |c: &Circuit| c.elems.iter().filter(|e| matches!(e.dev, Dev::Emf(_) | Dev::G(_))).count();
    let npin = |c: &Circuit| c.elems.iter().filter(|e| matches!(e.dev, Dev::Pin(_))).count();
    for (pi, part) in p.parts.iter().enumerate() {
        let sym = part.symbol();
        let nums = part.footprint().pin_numbers();
        let mut pins: Vec<PinSim> = nums
            .iter()
            .map(|num| {
                let label = sym.pins.iter().find(|x| &x.num == num).map(|x| x.name.clone()).filter(|x| !x.is_empty()).unwrap_or_else(|| num.clone());
                let node = match nl.net_of(pi, num) {
                    Some(ni) => net_node[ni],
                    None => c.node(format!("{}.{}", part.reference, num)),
                };
                PinSim { num: num.clone(), label, node, terms: vec![] }
            })
            .collect();
        let m = match model(part, setup) {
            Ok(m) => m,
            Err(e) => {
                notes.push(e.clone());
                Model::Open(e)
            }
        };
        let desc = describe(&m);
        let pin_ix = |pins: &[PinSim], num: &str| pins.iter().position(|x| x.num == num);
        let mut main = None;
        // terminals: a pin (by index) or an internal node
        enum T {
            P(usize),
            N(usize),
        }
        let add = |c: &mut Circuit, pins: &mut Vec<PinSim>, dev: Dev, ts: &[T]| {
            let nodes: Vec<usize> = ts
                .iter()
                .map(|t| match t {
                    T::P(k) => pins[*k].node,
                    T::N(n) => *n,
                })
                .collect();
            let e = c.add(dev, &nodes);
            for (k, t) in ts.iter().enumerate() {
                if let T::P(pk) = t {
                    pins[*pk].terms.push((e, k));
                }
            }
            e
        };
        let modelled = !matches!(m, Model::Open(_));
        let two = |pins: &[PinSim]| -> Option<(usize, usize)> { Some((pin_ix(pins, "1")?, pin_ix(pins, "2")?)) };
        match &m {
            Model::R(r) => {
                if let Some((a, b)) = two(&pins) {
                    add(&mut c, &mut pins, Dev::R(1.0 / r), &[T::P(a), T::P(b)]);
                    main = Some(a);
                }
            }
            Model::C(cap) => {
                if let Some((a, b)) = two(&pins) {
                    add(&mut c, &mut pins, Dev::C(*cap), &[T::P(a), T::P(b)]);
                    main = Some(a);
                }
            }
            Model::L(l) => {
                if let Some((a, b)) = two(&pins) {
                    add(&mut c, &mut pins, Dev::L(*l, 1e-3), &[T::P(a), T::P(b)]);
                    main = Some(a);
                }
            }
            Model::Switch(closed) => {
                if let Some((a, b)) = two(&pins) {
                    if setup.links.iter().any(|l| l.part.eq_ignore_ascii_case(&part.reference)) {
                        // a limit switch: its mate opens and closes it
                        let val = nval(&c);
                        add(&mut c, &mut pins, Dev::G(val), &[T::P(a), T::P(b)]);
                        mech.push(MechSim { reference: part.reference.clone(), part: MechPart::Switch { val } });
                    } else {
                        add(&mut c, &mut pins, Dev::R(if *closed { 1.0 / 0.05 } else { 1e-12 }), &[T::P(a), T::P(b)]);
                    }
                    main = Some(a);
                }
            }
            Model::Motor(m) => {
                if let Some((a, b)) = two(&pins) {
                    let mid = c.node(format!("{}.emf", part.reference));
                    let e = add(&mut c, &mut pins, Dev::L(m.l(), m.r()), &[T::P(a), T::N(mid)]);
                    let val = nval(&c);
                    add(&mut c, &mut pins, Dev::Emf(val), &[T::N(mid), T::P(b)]);
                    let br = c.elems[e].br;
                    mech.push(MechSim { reference: part.reference.clone(), part: MechPart::Motor { motor: m.clone(), br, val } });
                    main = Some(a);
                }
            }
            Model::Servo(sv, r) => {
                if let (Some(g), Some(vp), Some(sg)) = (pin_ix(&pins, &r[0]), pin_ix(&pins, &r[1]), pin_ix(&pins, &r[2])) {
                    // a high-impedance signal input, the electronics' idle current, and the motor's
                    add(&mut c, &mut pins, Dev::R(1.0 / 100e3), &[T::P(sg), T::P(g)]);
                    add(&mut c, &mut pins, Dev::R(1.0 / 1000.0), &[T::P(vp), T::P(g)]);
                    let val = nval(&c);
                    add(&mut c, &mut pins, Dev::G(val), &[T::P(vp), T::P(g)]);
                    mech.push(MechSim { reference: part.reference.clone(), part: MechPart::Servo { servo: sv.clone(), sig: pins[sg].node, gnd: pins[g].node, val } });
                }
            }
            Model::Pot(r) => {
                if let (Some(a), Some(w), Some(b)) = (pin_ix(&pins, "1"), pin_ix(&pins, "2"), pin_ix(&pins, "3")) {
                    let v0 = nval(&c);
                    add(&mut c, &mut pins, Dev::G(v0), &[T::P(a), T::P(w)]);
                    add(&mut c, &mut pins, Dev::G(v0 + 1), &[T::P(w), T::P(b)]);
                    mech.push(MechSim { reference: part.reference.clone(), part: MechPart::Pot { r: *r, vals: [v0, v0 + 1] } });
                }
            }
            Model::Encoder(r) => {
                let ix: Vec<usize> = r.iter().filter_map(|n| pin_ix(&pins, n)).collect();
                if let [a, b, vcc, gnd] = ix[..] {
                    let k = npin(&c);
                    add(&mut c, &mut pins, Dev::Pin(k), &[T::P(a), T::P(vcc), T::P(gnd)]);
                    add(&mut c, &mut pins, Dev::Pin(k + 1), &[T::P(b), T::P(vcc), T::P(gnd)]);
                    add(&mut c, &mut pins, Dev::R(1.0 / 5000.0), &[T::P(vcc), T::P(gnd)]);
                    mech.push(MechSim { reference: part.reference.clone(), part: MechPart::Encoder { pins: [k, k + 1] } });
                }
            }
            Model::D(d, rs, cj, _) => {
                // pin 2 anode, pin 1 cathode
                if let Some((k, a)) = two(&pins) {
                    let mid = c.node(format!("{}.a'", part.reference));
                    add(&mut c, &mut pins, Dev::R(1.0 / rs.max(1e-3)), &[T::P(a), T::N(mid)]);
                    add(&mut c, &mut pins, Dev::D(d.clone()), &[T::N(mid), T::P(k)]);
                    add(&mut c, &mut pins, Dev::C(*cj), &[T::N(mid), T::P(k)]);
                    main = Some(a);
                }
            }
            Model::Q(b, pnp, caps, _) => {
                let by = |name: &str| sym.pins.iter().find(|x| x.name == name).and_then(|x| pin_ix(&pins, &x.num));
                if let (Some(cc), Some(bb), Some(ee)) = (by("C"), by("B"), by("E")) {
                    add(&mut c, &mut pins, Dev::Q(b.clone(), *pnp), &[T::P(cc), T::P(bb), T::P(ee)]);
                    add(&mut c, &mut pins, Dev::C(caps[0]), &[T::P(bb), T::P(ee)]);
                    add(&mut c, &mut pins, Dev::C(caps[1]), &[T::P(bb), T::P(cc)]);
                }
            }
            Model::M(mm, pm, caps, _) => {
                let by = |name: &str| sym.pins.iter().find(|x| x.name == name).and_then(|x| pin_ix(&pins, &x.num));
                if let (Some(d), Some(g), Some(s)) = (by("D"), by("G"), by("S")) {
                    add(&mut c, &mut pins, Dev::M(mm.clone(), *pm), &[T::P(d), T::P(g), T::P(s)]);
                    add(&mut c, &mut pins, Dev::C(caps[0]), &[T::P(g), T::P(s)]);
                    add(&mut c, &mut pins, Dev::C(caps[1]), &[T::P(g), T::P(d)]);
                    // body diode
                    let bd = Diode { is: 1e-12, n: 1.0, bv: f64::INFINITY, ibv: 0.0 };
                    if *pm {
                        add(&mut c, &mut pins, Dev::D(bd), &[T::P(d), T::P(s)]);
                    } else {
                        add(&mut c, &mut pins, Dev::D(bd), &[T::P(s), T::P(d)]);
                    }
                }
            }
            Model::Timer(t, r) => {
                let ix: Vec<usize> = r.iter().filter_map(|n| pin_ix(&pins, n)).collect();
                if ix.len() == 8 {
                    let (gnd, ctrl, vcc) = (ix[0], ix[4], ix[7]);
                    let m1 = c.node(format!("{}.div", part.reference));
                    let g5k = 1.0 / 5000.0;
                    add(&mut c, &mut pins, Dev::R(g5k), &[T::P(vcc), T::P(ctrl)]);
                    add(&mut c, &mut pins, Dev::R(g5k), &[T::P(ctrl), T::N(m1)]);
                    add(&mut c, &mut pins, Dev::R(g5k), &[T::N(m1), T::P(gnd)]);
                    // supply current: about 3 mA (bipolar) or 0.1 mA (CMOS) at 5 V
                    add(&mut c, &mut pins, Dev::R(if t.cmos { 1.0 / 50e3 } else { 1.0 / 1900.0 }), &[T::P(vcc), T::P(gnd)]);
                    let ts: Vec<T> = ix.iter().map(|k| T::P(*k)).collect();
                    add(&mut c, &mut pins, Dev::T555(t.clone()), &ts);
                }
            }
            Model::OpAmp(om, units, sup, _) => {
                let (vp, vn) = (pin_ix(&pins, &sup[0]), pin_ix(&pins, &sup[1]));
                for (k, [ip, inn, out]) in units.iter().enumerate() {
                    let (Some(vp), Some(vn), Some(ip), Some(inn), Some(out)) = (vp, vn, pin_ix(&pins, ip), pin_ix(&pins, inn), pin_ix(&pins, out)) else { continue };
                    let int = c.node(format!("{}.int{}", part.reference, k + 1));
                    add(&mut c, &mut pins, Dev::Op(om.clone()), &[T::P(ip), T::P(inn), T::P(out), T::P(vp), T::P(vn), T::N(int)]);
                    add(&mut c, &mut pins, Dev::C(om.cint()), &[T::N(int), T::N(0)]);
                }
            }
            Model::Reg(rm, r, _) => {
                if let (Some(i), Some(g), Some(o)) = (pin_ix(&pins, &r[0]), pin_ix(&pins, &r[1]), pin_ix(&pins, &r[2])) {
                    add(&mut c, &mut pins, Dev::Reg(rm.clone()), &[T::P(i), T::P(g), T::P(o)]);
                }
            }
            Model::Pico => {
                let ms = setup.mcus.iter().find(|m| m.part.eq_ignore_ascii_case(&part.reference)).cloned().unwrap_or_else(|| crate::circuit_mcu::McuSetup { part: part.reference.clone(), ..Default::default() });
                mcus.push(crate::circuit_mcu::build_pico(&mut c, &mut pins, &part.reference, ms, &mut notes));
            }
            Model::Open(_) => {}
        }
        parts.push(PartSim { reference: part.reference.clone(), desc, modelled, pins, main });
    }
    // sources
    let list = if setup.sources.is_empty() { default_sources(p) } else { setup.sources.clone() };
    if list.is_empty() {
        notes.push("nothing powers the circuit: add a source, or a power port such as VCC or +5V".into());
    }
    let node_of = |name: &str| -> Result<usize, String> {
        if name.trim().is_empty() || name.trim().eq_ignore_ascii_case(&ground) {
            return Ok(0);
        }
        nets.iter().find(|(n, _)| n.eq_ignore_ascii_case(name.trim())).map(|x| x.1).ok_or_else(|| format!("no net {name}"))
    };
    let mut sources = vec![];
    for (k, s) in list.iter().enumerate() {
        let name = if s.name.is_empty() { format!("V{}", k + 1) } else { s.name.clone() };
        let plus = node_of(&s.plus).map_err(|e| format!("source {name}: {e}"))?;
        let minus = node_of(&s.minus).map_err(|e| format!("source {name}: {e}"))?;
        let w = Wave::parse(if s.wave.trim().is_empty() { "0" } else { &s.wave }).map_err(|e| format!("source {name}: {e}"))?;
        let dev = if s.current { Dev::I(w, s.ac) } else { Dev::V(w, s.ac) };
        let e = c.add(dev, &[plus, minus]);
        sources.push(SrcSim { name, elem: e });
    }
    // signals
    let mut sigs: Vec<SigDef> = nets.iter().map(|(n, k)| SigDef { name: format!("V({n})"), unit: "V", of: SigOf::Node(*k) }).collect();
    for s in &sources {
        // the current a source delivers out of its plus terminal
        sigs.push(SigDef { name: format!("I({})", s.name), unit: "A", of: SigOf::Terms(vec![(s.elem, 0)], -1.0) });
    }
    for ps in &parts {
        if !ps.modelled {
            continue;
        }
        if let Some(m) = ps.main {
            sigs.push(SigDef { name: format!("I({})", ps.reference), unit: "A", of: SigOf::Terms(ps.pins[m].terms.clone(), 1.0) });
        } else {
            for pin in &ps.pins {
                let name = format!("I({}.{})", ps.reference, pin.label);
                // pins sharing a name (a board's GND pins) report once
                if !pin.terms.is_empty() && !sigs.iter().any(|s| s.name == name) {
                    sigs.push(SigDef { name, unit: "A", of: SigOf::Terms(pin.terms.clone(), 1.0) });
                }
            }
        }
    }
    for m in &mcus {
        for (k, n) in m.gp.iter().enumerate() {
            sigs.push(SigDef { name: format!("V({}.GP{k})", m.reference), unit: "V", of: SigOf::Node(*n) });
        }
    }
    let open: Vec<String> = parts.iter().filter(|x| !x.modelled && !x.desc.starts_with("connector")).map(|x| format!("{} ({})", x.reference, x.desc)).collect();
    if !open.is_empty() {
        notes.push(format!("not simulated: {}", open.join(", ")));
    }
    for l in &setup.links {
        if !mech.iter().any(|m| m.reference.eq_ignore_ascii_case(&l.part)) {
            notes.push(format!("{} is linked to a mate but is not a motor, servo, potentiometer, switch or encoder", l.part));
        }
    }
    Ok(Built { c, parts, sources, sigs, ground, nets, notes, mcus, mech })
}

/// A transient stop time from the circuit: five of its longest RC time constants, or periods.
pub fn auto_stop(b: &Built) -> f64 {
    let mut rmax: f64 = 0.0;
    let mut cmax: f64 = 0.0;
    let mut per: f64 = 0.0;
    for e in &b.c.elems {
        match &e.dev {
            Dev::R(g) if *g > 1e-9 => rmax = rmax.max(1.0 / g),
            Dev::C(c) if *c < 1e-2 => cmax = cmax.max(*c),
            Dev::V(w, _) | Dev::I(w, _) => match w {
                Wave::Pulse { per: p, td, tr, pw, tf, .. } => per = per.max(if *p > 0.0 { *p } else { td + tr + pw.min(1e3) + tf }),
                Wave::Sin { f, .. } if *f > 0.0 => per = per.max(1.0 / f),
                Wave::Pwl(pts) => per = per.max(pts.last().map(|x| x.0).unwrap_or(0.0)),
                _ => {}
            },
            _ => {}
        }
    }
    let tau = (rmax * cmax * 5.0).max(per * 5.0);
    if tau > 0.0 { nice(tau.clamp(1e-9, 100.0)) } else { 1e-3 }
}

/// Round up to 1, 2 or 5 × 10ⁿ.
pub fn nice(v: f64) -> f64 {
    let e = v.log10().floor();
    let m = v / 10f64.powf(e);
    let n = if m <= 1.0 + 1e-9 {
        1.0
    } else if m <= 2.0 {
        2.0
    } else if m <= 5.0 {
        5.0
    } else {
        10.0
    };
    n * 10f64.powf(e)
}

// ---------------------------------------------------------------- results

#[derive(Clone, Debug, Default)]
pub struct Sig {
    pub name: String,
    pub unit: &'static str,
    pub re: Vec<f64>,
    /// AC only.
    pub im: Vec<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct PartOp {
    pub reference: String,
    pub desc: String,
    /// (pin label, voltage, current into the pin)
    pub pins: Vec<(String, f64, f64)>,
    /// Power taken by the part (negative: delivered).
    pub power: f64,
}

#[derive(Clone, Debug, Default)]
pub struct SimOut {
    pub kind: Kind,
    /// Time (s) or frequency (Hz); one point for an operating point.
    pub x: Vec<f64>,
    pub sigs: Vec<Sig>,
    /// Operating point: net voltages, part pins, and sources (name, V, I delivered).
    pub nets: Vec<(String, f64)>,
    pub parts: Vec<PartOp>,
    pub sources: Vec<(String, f64, f64)>,
    pub ground: String,
    pub notes: Vec<String>,
    /// The transient's stop time when it was worked out.
    pub stop: f64,
    /// What each microcontroller did (transients).
    pub mcus: Vec<crate::circuit_mcu::McuOut>,
    /// What the motors, servos and sensors did, and the assembly mates they moved (transients).
    pub mech: Vec<crate::circuit_mech::PartTrace>,
    pub mates: Vec<crate::circuit_mech::MateTrace>,
}

impl SimOut {
    /// A signal by probe: NET, V(NET), V(A,B), I(R1), I(U1.OUT), I(VCC), R1 (its current).
    pub fn probe(&self, q: &str) -> Option<Sig> {
        let q = q.trim();
        let find = |n: &str| self.sigs.iter().find(|s| s.name.eq_ignore_ascii_case(n)).cloned();
        if let Some(s) = find(q) {
            return Some(s);
        }
        let up = q.to_ascii_uppercase();
        if up.starts_with("V(") && q.ends_with(')') {
            let inner = &q[2..q.len() - 1];
            if let Some((a, b)) = inner.split_once(',') {
                let zero = || Sig { name: String::new(), unit: "V", re: vec![0.0; self.x.len()], im: if self.kind == Kind::Ac { vec![0.0; self.x.len()] } else { vec![] } };
                let get = |n: &str| if n.trim().eq_ignore_ascii_case(&self.ground) { Some(zero()) } else { find(&format!("V({})", n.trim())) };
                let (sa, sb) = (get(a)?, get(b)?);
                return Some(Sig {
                    name: format!("V({},{})", a.trim(), b.trim()),
                    unit: "V",
                    re: sa.re.iter().zip(&sb.re).map(|(x, y)| x - y).collect(),
                    im: sa.im.iter().zip(&sb.im).map(|(x, y)| x - y).collect(),
                });
            }
            if inner.trim().eq_ignore_ascii_case(&self.ground) {
                let mut z = Sig { name: format!("V({})", inner.trim()), unit: "V", re: vec![0.0; self.x.len()], im: vec![] };
                if self.kind == Kind::Ac {
                    z.im = z.re.clone();
                }
                return Some(z);
            }
            return None;
        }
        find(&format!("V({q})")).or_else(|| find(&format!("I({q})")))
    }

    /// Signal names that probes can use.
    pub fn names(&self) -> Vec<String> {
        self.sigs.iter().map(|s| s.name.clone()).collect()
    }
}

fn eval_sig(def: &SigDef, x: &[f64], i: &[f64], off: &[usize]) -> f64 {
    match &def.of {
        SigOf::Node(k) => {
            if *k == 0 {
                0.0
            } else {
                x[k - 1]
            }
        }
        SigOf::Terms(ts, s) => s * ts.iter().map(|(e, t)| i[off[*e] + t]).sum::<f64>(),
    }
}

fn eval_sig_c(def: &SigDef, x: &[Cx], i: &[Cx], off: &[usize]) -> Cx {
    match &def.of {
        SigOf::Node(k) => {
            if *k == 0 {
                Cx::default()
            } else {
                x[k - 1]
            }
        }
        SigOf::Terms(ts, s) => {
            let mut acc = Cx::default();
            for (e, t) in ts {
                acc = acc + i[off[*e] + t];
            }
            Cx::new(acc.re * s, acc.im * s)
        }
    }
}

fn op_tables(b: &Built, x: &[f64], i: &[f64], off: &[usize], out: &mut SimOut) {
    let v = |k: usize| if k == 0 { 0.0 } else { x[k - 1] };
    out.nets = b.nets.iter().map(|(n, k)| (n.clone(), v(*k))).collect();
    out.parts = b
        .parts
        .iter()
        .map(|ps| {
            let pins: Vec<(String, f64, f64)> = ps.pins.iter().map(|p| (p.label.clone(), v(p.node), p.terms.iter().map(|(e, t)| i[off[*e] + t]).sum::<f64>())).collect();
            let power = pins.iter().map(|(_, v, i)| v * i).sum();
            PartOp { reference: ps.reference.clone(), desc: ps.desc.clone(), pins, power }
        })
        .collect();
    out.sources = b
        .sources
        .iter()
        .map(|s| {
            let e = &b.c.elems[s.elem];
            (s.name.clone(), v(e.n[0]) - v(e.n[1]), -i[off[s.elem]])
        })
        .collect();
}

/// How far a microcontroller runs ahead of the circuit it reads (its inputs are this stale at most).
const MCU_HORIZON: f64 = 50e-6;

/// The microcontrollers and the mechanism together, driving the circuit.
struct Cosim<'a> {
    mcu: Option<&'a mut crate::circuit_mcu::McuExt>,
    mech: &'a mut MechExt,
}

impl Ext for Cosim<'_> {
    fn advance(&mut self, t: f64, x: &[f64], until: f64) -> f64 {
        let r = match self.mcu.as_deref_mut() {
            Some(m) => m.advance(t, x, until),
            None => until,
        };
        r.min(self.mech.advance(t, until))
    }
    fn pins(&self) -> Vec<(f64, f64)> {
        let mut p = self.mcu.as_deref().map(|m| m.pins()).unwrap_or_default();
        self.mech.fill_pins(&mut p);
        p
    }
    fn values(&self) -> Vec<f64> {
        self.mech.values()
    }
    fn step(&mut self, t: f64, x: &[f64]) {
        self.mech.step(t, x)
    }
    fn horizon(&self) -> f64 {
        let h = self.mcu.as_deref().map(|m| m.horizon()).unwrap_or(f64::INFINITY);
        h.min(self.mech.horizon())
    }
}

/// Run the setup's analysis on a PCB's schematic.
pub fn run(p: &Pcb, a: &Analysis) -> Result<SimOut, String> {
    run_with(p, a, &[])
}

/// `run`, with the workspace's assemblies for the parts linked to mates.
pub fn run_with(p: &Pcb, a: &Analysis, asms: &[crate::assembly::Assembly]) -> Result<SimOut, String> {
    let b = build(p)?;
    if b.c.size() == 0 {
        return Err("the circuit is empty".into());
    }
    let mut sim = Sim::new(&b.c);
    let mut out = SimOut { kind: a.kind, ground: b.ground.clone(), notes: b.notes.clone(), ..Default::default() };
    let mut mech = MechExt::new(&b.mech, &p.sim.links, asms, b.c.nodes.len() - 1, sim.vals.len(), sim.pins.len(), &mut out.notes);
    // sensors read where the mechanism stands
    sim.vals = mech.values();
    mech.fill_pins(&mut sim.pins);
    match a.kind {
        Kind::Op => {
            let x = sim.op()?;
            let i = sim.currents(&x, true);
            out.x = vec![0.0];
            out.sigs = b.sigs.iter().map(|d| Sig { name: d.name.clone(), unit: d.unit, re: vec![eval_sig(d, &x, &i, &sim.offsets)], im: vec![] }).collect();
            op_tables(&b, &x, &i, &sim.offsets, &mut out);
        }
        Kind::Tran => {
            let stop = if a.stop > 0.0 {
                a.stop
            } else if b.mcus.is_empty() {
                auto_stop(&b)
            } else {
                auto_stop(&b).max(1.0)
            };
            out.stop = stop;
            let mut ext = crate::circuit_mcu::McuExt::new(&b.mcus, MCU_HORIZON, &mut out.notes);
            let tr = if ext.active() || mech.active() {
                let mut co = Cosim { mcu: if ext.active() { Some(&mut ext) } else { None }, mech: &mut mech };
                sim.tran_ext(stop, a.step, a.from_op, Some(&mut co))?
            } else {
                sim.tran(stop, a.step, a.from_op)?
            };
            out.mcus = ext.finish();
            let mut extra = vec![];
            if !b.mech.is_empty() {
                let (parts, mates) = mech.finish();
                for pt in &parts {
                    if pt.pos.len() == tr.t.len() {
                        extra.push(Sig { name: format!("POS({})", pt.reference), unit: pt.unit, re: pt.pos.clone(), im: vec![] });
                    }
                    if pt.rpm.len() == tr.t.len() {
                        extra.push(Sig { name: format!("RPM({})", pt.reference), unit: "rpm", re: pt.rpm.clone(), im: vec![] });
                    }
                }
                out.mech = parts;
                out.mates = mates.into_iter().filter(|m| m.angle.as_ref().or(m.slide.as_ref()).map(|v| v.len() == tr.t.len()).unwrap_or(false)).collect();
            }
            out.sigs = b
                .sigs
                .iter()
                .map(|d| Sig { name: d.name.clone(), unit: d.unit, re: tr.x.iter().zip(&tr.i).map(|(x, i)| eval_sig(d, x, i, &sim.offsets)).collect(), im: vec![] })
                .collect();
            out.sigs.extend(extra);
            // the nets at the end
            if let (Some(x), Some(i)) = (tr.x.last(), tr.i.last()) {
                op_tables(&b, x, i, &sim.offsets, &mut out);
            }
            out.x = tr.t;
        }
        Kind::Ac => {
            let (f0, f1) = (a.f_start.max(1e-3), a.f_stop.max(a.f_start.max(1e-3) * 1.001));
            let n = ((f1 / f0).log10() * a.per_decade.clamp(1, 200) as f64).ceil().max(1.0) as usize;
            let freqs: Vec<f64> = (0..=n).map(|k| f0 * (f1 / f0).powf(k as f64 / n as f64)).collect();
            let x = sim.op()?;
            let i = sim.currents(&x, true);
            op_tables(&b, &x, &i, &sim.offsets, &mut out);
            let (xs, is) = sim.ac(&x, &freqs)?;
            out.sigs = b
                .sigs
                .iter()
                .map(|d| {
                    let v: Vec<Cx> = xs.iter().zip(&is).map(|(x, i)| eval_sig_c(d, x, i, &sim.offsets)).collect();
                    Sig { name: d.name.clone(), unit: d.unit, re: v.iter().map(|c| c.re).collect(), im: v.iter().map(|c| c.im).collect() }
                })
                .collect();
            out.x = freqs;
        }
    }
    out.notes.extend(sim.notes.iter().cloned());
    Ok(out)
}

/// The probed signals as CSV (AC: magnitude in dB and phase in degrees).
pub fn csv(out: &SimOut, probes: &[Sig]) -> String {
    let mut s = String::new();
    s.push_str(if out.kind == Kind::Ac { "frequency_hz" } else { "time_s" });
    for p in probes {
        if out.kind == Kind::Ac {
            s.push_str(&format!(",{} dB,{} deg", p.name, p.name));
        } else {
            s.push_str(&format!(",{} {}", p.name, p.unit));
        }
    }
    s.push('\n');
    for (k, x) in out.x.iter().enumerate() {
        s.push_str(&format!("{x:e}"));
        for p in probes {
            if out.kind == Kind::Ac {
                s.push_str(&format!(",{:.4},{:.3}", db(p.re[k], p.im[k]), Cx::new(p.re[k], p.im[k]).arg().to_degrees()));
            } else {
                s.push_str(&format!(",{:e}", p.re[k]));
            }
        }
        s.push('\n');
    }
    s
}

// ---------------------------------------------------------------- measurements

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub rms: f64,
    pub last: f64,
    /// Repeating signals: the period and the fraction of it spent above the middle.
    pub period: Option<f64>,
    pub duty: Option<f64>,
}

pub fn tran_stats(t: &[f64], y: &[f64]) -> Stats {
    let n = t.len().min(y.len());
    if n == 0 {
        return Stats::default();
    }
    let min = y[..n].iter().copied().fold(f64::INFINITY, f64::min);
    let max = y[..n].iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = t[n - 1] - t[0];
    let (mut s1, mut s2) = (0.0, 0.0);
    for k in 1..n {
        let dt = t[k] - t[k - 1];
        s1 += (y[k] + y[k - 1]) / 2.0 * dt;
        s2 += (y[k] * y[k] + y[k - 1] * y[k - 1]) / 2.0 * dt;
    }
    let (avg, rms) = if span > 0.0 { (s1 / span, (s2 / span).sqrt()) } else { (y[0], y[0].abs()) };
    let mut st = Stats { min, max, avg, rms, last: y[n - 1], period: None, duty: None };
    let swing = max - min;
    if swing <= 1e-9 * max.abs().max(min.abs()).max(1e-6) {
        return st;
    }
    // rising crossings of the middle, with 10 % hysteresis
    let mid = (max + min) / 2.0;
    let lo = mid - 0.1 * swing;
    let mut armed = y[0] < lo;
    let mut rises = vec![];
    for k in 1..n {
        if y[k] < lo {
            armed = true;
        }
        if armed && y[k - 1] < mid && y[k] >= mid {
            let f = (mid - y[k - 1]) / (y[k] - y[k - 1]);
            rises.push(t[k - 1] + f * (t[k] - t[k - 1]));
            armed = false;
        }
    }
    if rises.len() >= 3 {
        // leave out the first cycle, which starts from rest
        let per: Vec<f64> = rises.windows(2).skip(1).map(|w| w[1] - w[0]).collect();
        let p = per.iter().sum::<f64>() / per.len() as f64;
        let (a, b) = (rises[1], *rises.last().unwrap());
        let mut high = 0.0;
        for k in 1..n {
            let (t0, t1) = (t[k - 1].max(a), t[k].min(b));
            if t1 > t0 && (y[k] + y[k - 1]) / 2.0 > mid {
                high += t1 - t0;
            }
        }
        st.period = Some(p);
        st.duty = Some(high / (b - a));
    }
    st
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AcStats {
    pub start_db: f64,
    pub peak_db: f64,
    pub peak_f: f64,
    /// Where the gain falls 3 dB below the peak, below and above it.
    pub f_low: Option<f64>,
    pub f_high: Option<f64>,
}

pub fn db(re: f64, im: f64) -> f64 {
    20.0 * Cx::new(re, im).abs().max(1e-300).log10()
}

pub fn ac_stats(f: &[f64], re: &[f64], im: &[f64]) -> AcStats {
    let g: Vec<f64> = re.iter().zip(im).map(|(a, b)| db(*a, *b)).collect();
    if g.is_empty() {
        return AcStats::default();
    }
    let (k, peak) = g.iter().copied().enumerate().fold((0, f64::NEG_INFINITY), |a, b| if b.1 > a.1 { b } else { a });
    let lvl = peak - 3.0103;
    let cross = |a: usize, b: usize| -> f64 {
        let t = (lvl - g[a]) / (g[b] - g[a]);
        (f[a].ln() + t * (f[b].ln() - f[a].ln())).exp()
    };
    let f_high = (k + 1..g.len()).find(|&j| g[j] < lvl).map(|j| cross(j - 1, j));
    let f_low = (0..k).rev().find(|&j| g[j] < lvl).map(|j| cross(j + 1, j));
    AcStats { start_db: g[0], peak_db: peak, peak_f: f[k], f_low, f_high }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_parse() {
        let close = |a: Option<f64>, b: f64| (a.unwrap() - b).abs() <= b.abs() * 1e-9;
        assert!(close(parse_si("10k"), 1e4));
        assert!(close(parse_si("4k7"), 4700.0));
        assert!(close(parse_si("2.2uF"), 2.2e-6));
        assert!(close(parse_si("100n"), 1e-7));
        assert!(close(parse_si("1R5"), 1.5));
        assert!(close(parse_si("1M"), 1e6));
        assert!(close(parse_si("1meg"), 1e6));
        assert!(close(parse_si("10m"), 1e-2));
        assert!(close(parse_si("470"), 470.0));
        assert!(close(parse_si("10µF"), 1e-5));
        assert!(close(parse_si("1e-3"), 1e-3));
        assert!(parse_si("Red").is_none());
        assert_eq!(volts_in("+3V3"), Some(3.3));
        assert_eq!(volts_in("AMS1117-3.3"), Some(3.3));
        assert_eq!(volts_in("12V"), Some(12.0));
        assert_eq!(supply_guess("VCC"), 5.0);
        assert_eq!(supply_guess("-12V"), -12.0);
        assert_eq!(si(4700.0, "Ω"), "4.7 kΩ");
        assert_eq!(si(0.0123, "A"), "12.3 mA");
    }

    #[test]
    fn waves_parse() {
        assert_eq!(Wave::parse("5").unwrap(), Wave::Dc(5.0));
        assert_eq!(Wave::parse("DC 3.3").unwrap(), Wave::Dc(3.3));
        let p = Wave::parse("PULSE(0 5 1m 1u 1u 4m 10m)").unwrap();
        assert!((p.at(3e-3) - 5.0).abs() < 1e-12 && p.at(0.5e-3) == 0.0 && p.at(8e-3) == 0.0);
        assert!((p.next_break(0.0) - 1e-3).abs() < 1e-15);
        let s = Wave::parse("SIN(0 1 1k)").unwrap();
        assert!((s.at(0.25e-3) - 1.0).abs() < 1e-9);
        let w = Wave::parse("PWL(0 0 1m 5)").unwrap();
        assert!((w.at(0.5e-3) - 2.5).abs() < 1e-12);
        assert!(Wave::parse("PULSE(1)").is_err());
    }

    fn blinker() -> Pcb {
        let mut p = Pcb::new("t");
        let parts = [
            ("U", "NE555", Some("SOIC-8")),
            ("R", "1k", None),
            ("R", "10k", None),
            ("R", "470", None),
            ("CP", "10u", None),
            ("C", "10n", None),
            ("C", "100n", None),
            ("LED", "Red", None),
        ];
        for (k, (s, v, fp)) in parts.iter().enumerate() {
            p.add_part(s, *fp, Some(v), [k as f64 * 30.48, 0.0]).unwrap();
        }
        p.parts[0].pin_names = ["GND", "TRIG", "OUT", "RESET", "CTRL", "THR", "DIS", "VCC"].iter().map(|s| s.to_string()).collect();
        let nets: &[(&str, &[(&str, &str)])] = &[
            ("VCC", &[("U1", "8"), ("U1", "4"), ("R1", "1"), ("C3", "1")]),
            ("GND", &[("U1", "1"), ("C1", "2"), ("C2", "2"), ("C3", "2"), ("D1", "1")]),
            ("DIS", &[("U1", "7"), ("R1", "2"), ("R2", "1")]),
            ("TIMING", &[("U1", "6"), ("U1", "2"), ("R2", "2"), ("C1", "1")]),
            ("CTRL", &[("U1", "5"), ("C2", "1")]),
            ("OUT", &[("U1", "3"), ("R3", "1")]),
            ("LED_A", &[("R3", "2"), ("D1", "2")]),
        ];
        for (net, pins) in nets {
            let pins: Vec<(usize, String)> = pins.iter().map(|(r, n)| (p.part_index(r).unwrap(), n.to_string())).collect();
            crate::agent::pcb_connect(&mut p, net, &pins);
        }
        p
    }

    #[test]
    fn blinker_blinks_at_the_555_formula() {
        let p = blinker();
        assert_eq!(default_sources(&p).len(), 1);
        let a = Analysis { kind: Kind::Tran, stop: 1.0, ..Default::default() };
        let out = run(&p, &a).unwrap();
        let v = out.probe("OUT").unwrap();
        let st = tran_stats(&out.x, &v.re);
        // f = 1.44 / ((R1 + 2 R2) C), duty (R1 + R2) / (R1 + 2 R2)
        let period = 0.693 * (1e3 + 2.0 * 10e3) * 10e-6;
        let got = st.period.unwrap();
        assert!((got - period).abs() / period < 0.03, "period {got} vs {period}");
        let duty = 11.0 / 21.0;
        assert!((st.duty.unwrap() - duty).abs() < 0.03, "duty {:?}", st.duty);
        // output high about VCC − 1.7 V
        assert!((st.max - 3.3).abs() < 0.3, "high {}", st.max);
        // the timing capacitor swings between a third and two thirds of VCC
        let t = tran_stats(&out.x, &out.probe("TIMING").unwrap().re);
        assert!((t.max - 10.0 / 3.0).abs() < 0.1 && t.min < 0.1, "{t:?}");
        let tail: Vec<f64> = out.x.iter().zip(&out.probe("TIMING").unwrap().re).filter(|(x, _)| **x > 0.3).map(|(_, v)| *v).collect();
        let lo = tail.iter().copied().fold(f64::INFINITY, f64::min);
        assert!((lo - 5.0 / 3.0).abs() < 0.1, "{lo}");
        // LED current (5 − 1.7 − ~1.8) / 470 ≈ 3 mA while on
        let led = tran_stats(&out.x, &out.probe("I(D1)").unwrap().re);
        assert!(led.max > 2e-3 && led.max < 4e-3, "{}", led.max);
    }

    #[test]
    fn op_reports_parts() {
        let mut p = Pcb::new("t");
        p.add_part("R", None, Some("1k"), [0.0, 0.0]).unwrap();
        p.add_part("LED", None, Some("Red"), [30.0, 0.0]).unwrap();
        crate::agent::pcb_connect(&mut p, "+5V", &[(0, "1".into())]);
        crate::agent::pcb_connect(&mut p, "A", &[(0, "2".into()), (1, "2".into())]);
        crate::agent::pcb_connect(&mut p, "GND", &[(1, "1".into())]);
        let out = run(&p, &Analysis { kind: Kind::Op, ..Default::default() }).unwrap();
        let a = out.nets.iter().find(|n| n.0 == "A").unwrap().1;
        assert!(a > 1.7 && a < 1.95, "{a}");
        let i = out.probe("I(R1)").unwrap().re[0];
        assert!((i - (5.0 - a) / 1000.0).abs() < 1e-6);
        let r = out.parts.iter().find(|x| x.reference == "R1").unwrap();
        assert!((r.power - i * i * 1000.0).abs() < 1e-6);
        assert!((out.sources[0].2 - i).abs() < 1e-6);
    }

    #[test]
    fn regulator_and_opamp_from_values() {
        let mut p = Pcb::new("t");
        p.add_part("U", Some("SOT-223"), Some("AMS1117-3.3"), [0.0, 0.0]).unwrap();
        p.add_part("R", None, Some("100"), [30.0, 0.0]).unwrap();
        // SOT-223: 1 GND, 2 OUT, 3 IN
        crate::agent::pcb_connect(&mut p, "+5V", &[(0, "3".into())]);
        crate::agent::pcb_connect(&mut p, "3V3", &[(0, "2".into()), (1, "1".into())]);
        crate::agent::pcb_connect(&mut p, "GND", &[(0, "1".into()), (1, "2".into())]);
        // with no sources the power nets would both be supplies: drive only +5V
        p.sim.sources = vec![Source { name: "VIN".into(), plus: "+5V".into(), wave: "5".into(), ..Default::default() }];
        let out = run(&p, &Analysis { kind: Kind::Op, ..Default::default() }).unwrap();
        let v = out.nets.iter().find(|n| n.0 == "3V3").unwrap().1;
        assert!((v - 3.3).abs() < 0.01, "{v}");
        // dropout: at 4 V in the output is about 2.9 V
        p.sim.sources[0].wave = "4".into();
        let out = run(&p, &Analysis { kind: Kind::Op, ..Default::default() }).unwrap();
        let v = out.nets.iter().find(|n| n.0 == "3V3").unwrap().1;
        assert!((v - 2.9).abs() < 0.08, "{v}");
    }

    #[test]
    fn ac_sweep_finds_the_corner() {
        let mut p = Pcb::new("t");
        p.add_part("R", None, Some("1k"), [0.0, 0.0]).unwrap();
        p.add_part("C", None, Some("100n"), [30.0, 0.0]).unwrap();
        crate::agent::pcb_connect(&mut p, "IN", &[(0, "1".into())]);
        crate::agent::pcb_connect(&mut p, "OUT", &[(0, "2".into()), (1, "1".into())]);
        crate::agent::pcb_connect(&mut p, "GND", &[(1, "2".into())]);
        p.sim.sources = vec![Source { name: "VIN".into(), plus: "IN".into(), wave: "0".into(), ac: 1.0, ..Default::default() }];
        let out = run(&p, &Analysis { kind: Kind::Ac, f_start: 10.0, f_stop: 1e6, per_decade: 50, ..Default::default() }).unwrap();
        let s = out.probe("V(OUT)").unwrap();
        let st = ac_stats(&out.x, &s.re, &s.im);
        let fc = 1.0 / (2.0 * std::f64::consts::PI * 1e3 * 100e-9);
        assert!((st.f_high.unwrap() - fc).abs() / fc < 0.01, "{:?}", st.f_high);
        assert!(st.start_db.abs() < 0.01);
    }
}
