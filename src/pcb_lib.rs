//! The PCB library: schematic symbols, footprints and the stroke font used for silkscreen.
//!
//! Symbols and footprints are generated from their names (`SOIC-14`, `PinHeader_2x05`), so a
//! part's pin count follows its footprint. Coordinates are millimetres with y up; a symbol's pin
//! numbers match its footprint's pad numbers.

pub type P = [f64; 2];

/// Schematic grid (100 mil).
pub const GRID: f64 = 2.54;

/// A drawing primitive of a symbol or a footprint's silkscreen.
#[derive(Clone, Debug, PartialEq)]
pub enum G {
    Line(P, P),
    Rect(P, P),
    Circle(P, f64),
    /// Polyline; closed and filled when the flag is set.
    Poly(Vec<P>, bool),
}

#[derive(Clone, Debug)]
pub struct SymPin {
    pub num: String,
    pub name: String,
    /// The connection point (the pin's outer end).
    pub at: P,
    /// Unit direction from the connection point towards the body.
    pub dir: P,
    pub len: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Symbol {
    pub pins: Vec<SymPin>,
    pub g: Vec<G>,
    pub pin_numbers: bool,
    pub pin_names: bool,
    /// Where the reference and value go (left-anchored), in symbol coordinates.
    pub ref_at: P,
    pub value_at: P,
}

/// A symbol in the library.
pub struct SymbolKind {
    pub key: &'static str,
    pub name: &'static str,
    pub prefix: &'static str,
    pub value: &'static str,
    pub footprint: &'static str,
    /// Footprints that suit it, for the picker (others can be typed).
    pub footprints: &'static [&'static str],
    /// Words for the search box.
    pub tags: &'static str,
}

const CHIP: &[&str] = &["0402", "0603", "0805", "1206"];

pub const SYMBOLS: &[SymbolKind] = &[
    SymbolKind { key: "R", name: "Resistor", prefix: "R", value: "10k", footprint: "R_0805", footprints: &["R_0402", "R_0603", "R_0805", "R_1206", "R_Axial"], tags: "resistor ohm" },
    SymbolKind { key: "C", name: "Capacitor", prefix: "C", value: "100n", footprint: "C_0805", footprints: &["C_0402", "C_0603", "C_0805", "C_1206"], tags: "capacitor ceramic decoupling" },
    SymbolKind { key: "CP", name: "Polarised capacitor", prefix: "C", value: "10u", footprint: "CP_D5", footprints: &["CP_D5", "CP_D8", "C_1206"], tags: "electrolytic capacitor polarized" },
    SymbolKind { key: "L", name: "Inductor", prefix: "L", value: "10u", footprint: "L_1206", footprints: &["L_0805", "L_1206"], tags: "inductor coil choke" },
    SymbolKind { key: "D", name: "Diode", prefix: "D", value: "1N4148", footprint: "D_SOD-123", footprints: &["D_SOD-123", "D_DO-41"], tags: "diode rectifier schottky" },
    SymbolKind { key: "LED", name: "LED", prefix: "D", value: "Red", footprint: "LED_0805", footprints: &["LED_0603", "LED_0805", "LED_D5"], tags: "led light" },
    SymbolKind { key: "Q_NPN", name: "NPN transistor", prefix: "Q", value: "BC847", footprint: "SOT-23", footprints: &["SOT-23"], tags: "bjt transistor npn" },
    SymbolKind { key: "Q_PNP", name: "PNP transistor", prefix: "Q", value: "BC857", footprint: "SOT-23", footprints: &["SOT-23"], tags: "bjt transistor pnp" },
    SymbolKind { key: "Q_NMOS", name: "N-channel MOSFET", prefix: "Q", value: "2N7002", footprint: "SOT-23", footprints: &["SOT-23"], tags: "mosfet fet nmos transistor" },
    SymbolKind { key: "Q_PMOS", name: "P-channel MOSFET", prefix: "Q", value: "AO3401", footprint: "SOT-23", footprints: &["SOT-23"], tags: "mosfet fet pmos transistor" },
    SymbolKind {
        key: "U",
        name: "IC",
        prefix: "U",
        value: "IC",
        footprint: "SOIC-8",
        footprints: &["SOIC-8", "SOIC-14", "SOIC-16", "TSSOP-14", "TSSOP-16", "TSSOP-20", "DIP-8", "DIP-14", "DIP-16", "DIP-28", "SOT-23-5", "SOT-223", "TO-220"],
        tags: "ic chip microcontroller op amp regulator timer",
    },
    SymbolKind {
        key: "J",
        name: "Connector",
        prefix: "J",
        value: "Conn",
        footprint: "PinHeader_1x04",
        footprints: &["PinHeader_1x02", "PinHeader_1x03", "PinHeader_1x04", "PinHeader_1x06", "PinHeader_1x08", "PinHeader_2x03", "PinHeader_2x05", "Terminal_2P", "Terminal_3P"],
        tags: "connector header pin socket terminal",
    },
    SymbolKind { key: "SW", name: "Push button", prefix: "SW", value: "SW", footprint: "SW_6mm", footprints: &["SW_6mm", "Terminal_2P", "PinHeader_1x02"], tags: "switch button tactile limit microswitch endstop" },
    SymbolKind { key: "RV", name: "Potentiometer", prefix: "RV", value: "10k", footprint: "PinHeader_1x03", footprints: &["PinHeader_1x03", "Terminal_3P"], tags: "potentiometer pot trimmer variable resistor knob position sensor" },
    SymbolKind { key: "M", name: "DC motor", prefix: "M", value: "6V 10000rpm 1A", footprint: "Terminal_2P", footprints: &["Terminal_2P", "PinHeader_1x02"], tags: "motor dc gear actuator" },
    SymbolKind {
        key: "STEPPER",
        name: "Stepper motor",
        prefix: "M",
        value: "NEMA17 0.4Nm 1.7A",
        footprint: "PinHeader_1x04",
        footprints: &["PinHeader_1x04", "Terminal_2P"],
        tags: "stepper motor bipolar nema17 nema23 actuator",
    },
    SymbolKind {
        key: "STEPDRV",
        name: "Stepper driver module",
        prefix: "U",
        value: "A4988 1A",
        footprint: "StepStick",
        footprints: &["StepStick", "DIP-16"],
        tags: "stepper driver a4988 drv8825 tmc2209 stepstick pololu step dir",
    },
    SymbolKind { key: "SERVO", name: "Servo", prefix: "SV", value: "SG90", footprint: "PinHeader_1x03", footprints: &["PinHeader_1x03"], tags: "servo rc hobby actuator pwm" },
    SymbolKind { key: "ENC", name: "Encoder", prefix: "ENC", value: "Encoder", footprint: "PinHeader_1x04", footprints: &["PinHeader_1x04"], tags: "encoder quadrature rotary position sensor hall" },
    SymbolKind { key: "Y", name: "Crystal", prefix: "Y", value: "16MHz", footprint: "Crystal_HC49", footprints: &["Crystal_HC49"], tags: "crystal oscillator quartz" },
    SymbolKind {
        key: "PICO",
        name: "Raspberry Pi Pico",
        prefix: "U",
        value: "Pico",
        footprint: "RPi_Pico",
        footprints: &["RPi_Pico", "DIP-40"],
        tags: "raspberry pi pico rp2040 microcontroller mcu board module firmware",
    },
];

/// The Raspberry Pi Pico's header pins, 1 to 40.
pub const PICO_PINS: [&str; 40] = [
    "GP0", "GP1", "GND", "GP2", "GP3", "GP4", "GP5", "GND", "GP6", "GP7", "GP8", "GP9", "GND", "GP10", "GP11", "GP12", "GP13", "GND", "GP14", "GP15", "GP16", "GP17", "GND", "GP18", "GP19", "GP20", "GP21", "GND", "GP22", "RUN", "GP26", "GP27", "AGND",
    "GP28", "ADC_VREF", "3V3", "3V3_EN", "GND", "VSYS", "VBUS",
];

/// Default pin names for a library symbol (the Pico's pinout, a servo's lead, a stepper driver
/// module's pinout, which differs between the A4988 and the DRV8825).
pub fn default_pin_names(key: &str, value: &str) -> Vec<String> {
    match key {
        "PICO" => PICO_PINS.iter().map(|s| s.to_string()).collect(),
        "STEPPER" => ["A+", "A-", "B+", "B-"].iter().map(|s| s.to_string()).collect(),
        "STEPDRV" if value.contains("8825") => {
            ["EN", "M0", "M1", "M2", "RST", "SLP", "STEP", "DIR", "GND", "FAULT", "A2", "A1", "B1", "B2", "GND", "VMOT"].iter().map(|s| s.to_string()).collect()
        }
        "STEPDRV" => ["EN", "MS1", "MS2", "MS3", "RST", "SLP", "STEP", "DIR", "GND", "VDD", "1B", "1A", "2A", "2B", "GND", "VMOT"].iter().map(|s| s.to_string()).collect(),
        "SERVO" => ["GND", "V+", "SIG"].iter().map(|s| s.to_string()).collect(),
        "ENC" => ["A", "B", "VCC", "GND"].iter().map(|s| s.to_string()).collect(),
        _ => vec![],
    }
}

pub fn symbol_kind(key: &str) -> Option<&'static SymbolKind> {
    SYMBOLS.iter().find(|s| s.key.eq_ignore_ascii_case(key))
}

/// Footprints listed in the picker.
pub fn footprint_names() -> Vec<String> {
    let mut v: Vec<String> = vec![];
    for p in ["R", "C", "L", "LED"] {
        for s in CHIP {
            if !(p == "L" && (*s == "0402" || *s == "0603")) {
                v.push(format!("{p}_{s}"));
            }
        }
    }
    for s in ["R_Axial", "CP_D5", "CP_D8", "LED_D5", "D_SOD-123", "D_DO-41", "SOT-23", "SOT-23-5", "SOT-223", "TO-92", "TO-220"] {
        v.push(s.into());
    }
    for n in [8, 14, 16] {
        v.push(format!("SOIC-{n}"));
    }
    for n in [8, 14, 16, 20] {
        v.push(format!("TSSOP-{n}"));
    }
    for n in [8, 14, 16, 20, 28, 40] {
        v.push(format!("DIP-{n}"));
    }
    for n in 1..=10 {
        v.push(format!("PinHeader_1x{n:02}"));
    }
    for n in [2, 3, 4, 5, 8, 10, 20] {
        v.push(format!("PinHeader_2x{n:02}"));
    }
    for s in ["Terminal_2P", "Terminal_3P", "SW_6mm", "Crystal_HC49", "RPi_Pico", "StepStick"] {
        v.push(s.into());
    }
    v
}

// ---------------------------------------------------------------- footprints

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadShape {
    Rect,
    Round,
    Oval,
    RoundRect,
}

#[derive(Clone, Debug)]
pub struct Pad {
    pub num: String,
    pub at: P,
    pub size: P,
    pub shape: PadShape,
    /// Plated hole diameter; 0 for surface mount pads.
    pub drill: f64,
}

/// A 3D body: a box or an upright cylinder, in footprint coordinates (z up from the board).
#[derive(Clone, Debug)]
pub enum Solid {
    Box { lo: [f64; 3], hi: [f64; 3] },
    Cyl { c: P, r: f64, z0: f64, h: f64 },
}

#[derive(Clone, Debug)]
pub struct Footprint {
    pub name: String,
    pub pads: Vec<Pad>,
    pub silk: Vec<G>,
    /// Courtyard rectangle (the area other parts keep out of).
    pub court: (P, P),
    pub body: Vec<Solid>,
    pub color: [u8; 3],
}

impl Footprint {
    /// Pad numbers in order, each once.
    pub fn pin_numbers(&self) -> Vec<String> {
        let mut v: Vec<String> = vec![];
        for p in &self.pads {
            if !v.contains(&p.num) {
                v.push(p.num.clone());
            }
        }
        v
    }
    pub fn tht(&self) -> bool {
        self.pads.iter().any(|p| p.drill > 0.0)
    }
}

fn smd(num: &str, at: P, size: P) -> Pad {
    Pad { num: num.into(), at, size, shape: PadShape::RoundRect, drill: 0.0 }
}

fn tht(num: usize, at: P, d: f64, drill: f64) -> Pad {
    let shape = if num == 1 { PadShape::Rect } else { PadShape::Round };
    Pad { num: num.to_string(), at, size: [d, d], shape, drill }
}

fn rect_silk(w: f64, h: f64) -> Vec<G> {
    vec![G::Rect([-w / 2.0, -h / 2.0], [w / 2.0, h / 2.0])]
}

fn court_of(pads: &[Pad], body: (P, P)) -> (P, P) {
    let mut lo = body.0;
    let mut hi = body.1;
    for p in pads {
        lo[0] = lo[0].min(p.at[0] - p.size[0] / 2.0);
        lo[1] = lo[1].min(p.at[1] - p.size[1] / 2.0);
        hi[0] = hi[0].max(p.at[0] + p.size[0] / 2.0);
        hi[1] = hi[1].max(p.at[1] + p.size[1] / 2.0);
    }
    ([lo[0] - 0.25, lo[1] - 0.25], [hi[0] + 0.25, hi[1] + 0.25])
}

fn bx(w: f64, l: f64, h: f64) -> Solid {
    Solid::Box { lo: [-w / 2.0, -l / 2.0, 0.0], hi: [w / 2.0, l / 2.0, h] }
}

/// Build a footprint from its name, or None for an unknown name.
pub fn footprint(name: &str) -> Option<Footprint> {
    let mut f = Footprint { name: name.into(), pads: vec![], silk: vec![], court: ([0.0; 2], [0.0; 2]), body: vec![], color: [40, 40, 44] };
    let body_rect;
    // two-terminal chips: pads along x, pad 1 on the left (the cathode for diodes and LEDs)
    let chip = |s: &str| -> Option<(f64, f64, f64, f64, f64, f64)> {
        // (pad x centre, pad w, pad h, body l, body w, body h)
        Some(match s {
            "0402" => (0.51, 0.54, 0.64, 1.0, 0.5, 0.35),
            "0603" => (0.825, 0.8, 0.95, 1.6, 0.8, 0.45),
            "0805" => (0.9125, 1.025, 1.4, 2.0, 1.25, 0.5),
            "1206" => (1.4625, 1.125, 1.75, 3.2, 1.6, 0.55),
            _ => return None,
        })
    };
    let (prefix, rest) = name.split_once('_').unwrap_or((name, ""));
    if let (true, Some((x, pw, ph, bl, bw, bh))) = (matches!(prefix, "R" | "C" | "L" | "LED"), chip(rest)) {
        f.pads = vec![smd("1", [-x, 0.0], [pw, ph]), smd("2", [x, 0.0], [pw, ph])];
        f.body = vec![bx(bl, bw, bh)];
        f.color = match prefix {
            "R" => [30, 30, 32],
            "C" => [176, 140, 96],
            "L" => [70, 70, 74],
            _ => [235, 235, 225],
        };
        let y = ph / 2.0 + 0.2;
        if prefix == "LED" {
            // cathode bar
            f.silk = vec![G::Line([-x - pw / 2.0 - 0.2, y], [x, y]), G::Line([-x - pw / 2.0 - 0.2, -y], [x, -y]), G::Line([-x - pw / 2.0 - 0.2, -y], [-x - pw / 2.0 - 0.2, y])];
        } else if bl > 1.2 {
            f.silk = vec![G::Line([-bl / 2.0 + 0.5, y], [bl / 2.0 - 0.5, y]), G::Line([-bl / 2.0 + 0.5, -y], [bl / 2.0 - 0.5, -y])];
        }
        body_rect = ([-bl / 2.0, -bw / 2.0], [bl / 2.0, bw / 2.0]);
    } else if let Some(n) = name.strip_prefix("SOIC-").and_then(|n| n.parse::<usize>().ok()) {
        body_rect = dual_row(&mut f, n, 1.27, 2.7, [1.55, 0.6], 3.9, 1.5)?;
    } else if let Some(n) = name.strip_prefix("TSSOP-").and_then(|n| n.parse::<usize>().ok()) {
        body_rect = dual_row(&mut f, n, 0.65, 2.9, [1.45, 0.4], 4.4, 1.1)?;
    } else if let Some(n) = name.strip_prefix("DIP-").and_then(|n| n.parse::<usize>().ok()) {
        if n < 4 || n % 2 != 0 || n > 64 {
            return None;
        }
        let rows = n / 2;
        let h = (rows - 1) as f64 * 2.54;
        for i in 0..rows {
            f.pads.push(tht(i + 1, [-3.81, h / 2.0 - i as f64 * 2.54], 1.6, 0.8));
        }
        for i in 0..rows {
            f.pads.push(tht(rows + i + 1, [3.81, -h / 2.0 + i as f64 * 2.54], 1.6, 0.8));
        }
        let (w, l) = (6.35, h + 2.54);
        f.silk = notch_outline(w, l);
        f.body = vec![Solid::Box { lo: [-w / 2.0, -l / 2.0, 0.5], hi: [w / 2.0, l / 2.0, 3.8] }];
        for p in &f.pads {
            f.body.push(Solid::Box { lo: [p.at[0] - 0.25, p.at[1] - 0.25, -3.0], hi: [p.at[0] + 0.25, p.at[1] + 0.25, 0.6] });
        }
        body_rect = ([-w / 2.0, -l / 2.0], [w / 2.0, l / 2.0]);
    } else if let Some(spec) = name.strip_prefix("PinHeader_") {
        let (cols, n) = spec.split_once('x')?;
        let (cols, n): (usize, usize) = (cols.parse().ok()?, n.parse().ok()?);
        if !(1..=2).contains(&cols) || n == 0 || n > 40 {
            return None;
        }
        let h = (n - 1) as f64 * 2.54;
        let w = (cols - 1) as f64 * 2.54;
        for i in 0..n {
            for c in 0..cols {
                let k = i * cols + c + 1;
                f.pads.push(tht(k, [-w / 2.0 + c as f64 * 2.54, h / 2.0 - i as f64 * 2.54], 1.7, 1.0));
            }
        }
        let (bw, bl) = (w + 2.54, h + 2.54);
        f.silk = rect_silk(bw, bl);
        f.body = vec![Solid::Box { lo: [-bw / 2.0, -bl / 2.0, 0.0], hi: [bw / 2.0, bl / 2.0, 2.5] }];
        for p in &f.pads {
            f.body.push(Solid::Box { lo: [p.at[0] - 0.32, p.at[1] - 0.32, -3.0], hi: [p.at[0] + 0.32, p.at[1] + 0.32, 8.5] });
        }
        body_rect = ([-bw / 2.0, -bl / 2.0], [bw / 2.0, bl / 2.0]);
    } else {
        match name {
            "SOT-23" => {
                f.pads = vec![smd("1", [-0.95, -1.1], [0.6, 1.0]), smd("2", [0.95, -1.1], [0.6, 1.0]), smd("3", [0.0, 1.1], [0.6, 1.0])];
                f.body = vec![bx(2.9, 1.3, 1.0)];
                f.silk = vec![G::Line([-1.45, 0.65], [-0.6, 0.65]), G::Line([0.6, 0.65], [1.45, 0.65]), G::Line([-0.3, -0.65], [0.3, -0.65])];
                body_rect = ([-1.45, -0.65], [1.45, 0.65]);
            }
            "SOT-23-5" => {
                f.pads = vec![
                    smd("1", [-0.95, -1.1], [0.6, 1.0]),
                    smd("2", [0.0, -1.1], [0.6, 1.0]),
                    smd("3", [0.95, -1.1], [0.6, 1.0]),
                    smd("4", [0.95, 1.1], [0.6, 1.0]),
                    smd("5", [-0.95, 1.1], [0.6, 1.0]),
                ];
                f.body = vec![bx(2.9, 1.6, 1.1)];
                f.silk = vec![G::Line([-1.45, 0.8], [-1.45, -0.8]), G::Line([1.45, 0.8], [1.45, -0.8]), G::Circle([-1.6, -1.6], 0.1)];
                body_rect = ([-1.45, -0.8], [1.45, 0.8]);
            }
            "SOT-223" => {
                for i in 0..3 {
                    f.pads.push(smd(&(i + 1).to_string(), [(i as f64 - 1.0) * 2.3, -3.15], [1.0, 2.0]));
                }
                f.pads.push(smd("4", [0.0, 3.15], [3.6, 2.0]));
                f.body = vec![bx(6.5, 3.5, 1.7)];
                f.silk = rect_silk(6.7, 3.7);
                body_rect = ([-3.25, -1.75], [3.25, 1.75]);
            }
            "D_SOD-123" => {
                f.pads = vec![smd("1", [-1.65, 0.0], [0.9, 1.2]), smd("2", [1.65, 0.0], [0.9, 1.2])];
                f.body = vec![bx(2.7, 1.6, 1.1)];
                f.silk = vec![G::Line([-2.4, 0.9], [1.5, 0.9]), G::Line([-2.4, -0.9], [1.5, -0.9]), G::Line([-2.4, 0.9], [-2.4, -0.9])];
                body_rect = ([-1.35, -0.8], [1.35, 0.8]);
            }
            "D_DO-41" | "R_Axial" => {
                let (pitch, bl, r, drill) = if name == "R_Axial" { (10.16, 6.3, 1.25, 0.8) } else { (10.16, 5.2, 1.35, 1.0) };
                f.pads = vec![tht(1, [-pitch / 2.0, 0.0], drill + 0.8, drill), tht(2, [pitch / 2.0, 0.0], drill + 0.8, drill)];
                if name == "R_Axial" {
                    f.pads[0].shape = PadShape::Round;
                }
                f.silk = rect_silk(bl, 2.0 * r + 0.2);
                if name == "D_DO-41" {
                    f.silk.push(G::Line([-bl / 2.0 + 0.8, -r], [-bl / 2.0 + 0.8, r]));
                }
                f.body = vec![Solid::Box { lo: [-bl / 2.0, -r, 0.0], hi: [bl / 2.0, r, 2.0 * r] }];
                f.color = if name == "R_Axial" { [210, 190, 150] } else { [30, 30, 32] };
                body_rect = ([-bl / 2.0, -r], [bl / 2.0, r]);
            }
            "CP_D5" | "CP_D8" | "LED_D5" => {
                let (d, pitch, h) = match name {
                    "CP_D5" => (5.0, 2.0, 11.0),
                    "CP_D8" => (8.0, 3.5, 11.5),
                    _ => (5.0, 2.54, 8.6),
                };
                if name == "LED_D5" {
                    // cathode (flat side) on pad 1
                    f.pads = vec![tht(1, [-pitch / 2.0, 0.0], 1.8, 0.9), tht(2, [pitch / 2.0, 0.0], 1.8, 0.9)];
                    f.color = [220, 40, 40];
                } else {
                    // pad 1 is + on electrolytics
                    f.pads = vec![tht(1, [-pitch / 2.0, 0.0], 1.6, 0.8), tht(2, [pitch / 2.0, 0.0], 1.6, 0.8)];
                    f.color = [40, 60, 120];
                    f.silk.push(G::Line([-d / 2.0 - 0.8, 1.5], [-d / 2.0 - 0.8, 0.5]));
                    f.silk.push(G::Line([-d / 2.0 - 1.3, 1.0], [-d / 2.0 - 0.3, 1.0]));
                }
                f.silk.push(G::Circle([0.0, 0.0], d / 2.0 + 0.1));
                f.body = vec![Solid::Cyl { c: [0.0, 0.0], r: d / 2.0, z0: 0.0, h }];
                body_rect = ([-d / 2.0, -d / 2.0], [d / 2.0, d / 2.0]);
            }
            "TO-92" => {
                f.pads = (0..3).map(|i| Pad { num: (i + 1).to_string(), at: [(i as f64 - 1.0) * 1.27, 0.0], size: [1.05, 1.5], shape: PadShape::Oval, drill: 0.75 }).collect();
                f.pads[0].shape = PadShape::Rect;
                f.silk = vec![G::Line([-2.4, -1.6], [2.4, -1.6])];
                f.body = vec![Solid::Cyl { c: [0.0, 0.0], r: 2.4, z0: 0.0, h: 4.8 }];
                body_rect = ([-2.4, -2.4], [2.4, 2.4]);
            }
            "TO-220" => {
                f.pads = (0..3).map(|i| Pad { num: (i + 1).to_string(), at: [(i as f64 - 1.0) * 2.54, 0.0], size: [1.9, 2.5], shape: PadShape::Oval, drill: 1.1 }).collect();
                f.pads[0].shape = PadShape::Rect;
                f.silk = vec![G::Rect([-5.2, -1.5], [5.2, 3.3]), G::Line([-5.2, 2.0], [5.2, 2.0])];
                f.body = vec![Solid::Box { lo: [-5.0, -1.0, 0.0], hi: [5.0, 3.5, 15.0] }];
                f.color = [50, 50, 54];
                body_rect = ([-5.2, -1.5], [5.2, 3.3]);
            }
            "Terminal_2P" | "Terminal_3P" => {
                let n = if name == "Terminal_2P" { 2 } else { 3 };
                let w = (n - 1) as f64 * 5.08;
                for i in 0..n {
                    f.pads.push(tht(i + 1, [-w / 2.0 + i as f64 * 5.08, 0.0], 2.6, 1.3));
                }
                let bw = w + 5.0;
                f.silk = vec![G::Rect([-bw / 2.0, -4.0], [bw / 2.0, 4.2])];
                f.body = vec![Solid::Box { lo: [-bw / 2.0, -4.0, 0.0], hi: [bw / 2.0, 4.2, 10.0] }];
                f.color = [40, 130, 70];
                body_rect = ([-bw / 2.0, -4.0], [bw / 2.0, 4.2]);
            }
            "SW_6mm" => {
                f.pads = vec![tht(1, [-3.25, 2.25], 2.0, 1.1), tht(2, [3.25, 2.25], 2.0, 1.1), tht(1, [-3.25, -2.25], 2.0, 1.1), tht(2, [3.25, -2.25], 2.0, 1.1)];
                f.pads[2].shape = PadShape::Round;
                f.silk = vec![G::Rect([-3.0, -3.0], [3.0, 3.0]), G::Circle([0.0, 0.0], 1.75)];
                f.body = vec![Solid::Box { lo: [-3.0, -3.0, 0.0], hi: [3.0, 3.0, 3.5] }, Solid::Cyl { c: [0.0, 0.0], r: 1.75, z0: 3.5, h: 1.5 }];
                body_rect = ([-3.0, -3.0], [3.0, 3.0]);
            }
            "RPi_Pico" => {
                // the module's castellated edge holes: 1..20 down the left, 21..40 up the right
                for i in 0..20 {
                    f.pads.push(tht(i + 1, [-8.89, 24.13 - i as f64 * 2.54], 1.7, 1.0));
                }
                for i in 0..20 {
                    f.pads.push(tht(21 + i, [8.89, -24.13 + i as f64 * 2.54], 1.7, 1.0));
                }
                f.silk = vec![G::Rect([-10.5, -25.5], [10.5, 25.5]), G::Rect([-4.0, 23.0], [4.0, 26.5])];
                f.body = vec![
                    Solid::Box { lo: [-10.5, -25.5, 0.0], hi: [10.5, 25.5, 1.0] },
                    Solid::Box { lo: [-3.5, -3.5, 1.0], hi: [3.5, 3.5, 1.9] },
                    Solid::Box { lo: [-4.0, 22.0, 1.0], hi: [4.0, 26.5, 3.8] },
                    Solid::Box { lo: [-2.0, -13.0, 1.0], hi: [2.0, -9.0, 2.0] },
                ];
                f.color = [30, 120, 60];
                body_rect = ([-10.5, -25.5], [10.5, 26.5]);
            }
            "StepStick" => {
                // a Pololu-style driver module on two 1 × 8 sockets 12.7 mm apart: 1..8 down the
                // left (EN … DIR), 9..16 up the right (GND … VMOT)
                for i in 0..8 {
                    f.pads.push(tht(i + 1, [-6.35, 8.89 - i as f64 * 2.54], 1.7, 1.0));
                }
                for i in 0..8 {
                    f.pads.push(tht(9 + i, [6.35, -8.89 + i as f64 * 2.54], 1.7, 1.0));
                }
                f.silk = vec![G::Rect([-7.62, -10.16], [7.62, 10.16]), G::Line([-7.62, 8.0], [-5.0, 10.16])];
                f.body = vec![
                    Solid::Box { lo: [-7.62, -10.16, 0.0], hi: [-5.08, 10.16, 8.5] },
                    Solid::Box { lo: [5.08, -10.16, 0.0], hi: [7.62, 10.16, 8.5] },
                    Solid::Box { lo: [-7.62, -10.16, 8.5], hi: [7.62, 10.16, 10.1] },
                    // the chip's heatsink
                    Solid::Box { lo: [-4.5, -4.5, 10.1], hi: [4.5, 4.5, 19.0] },
                ];
                for p in &f.pads {
                    f.body.push(Solid::Box { lo: [p.at[0] - 0.32, p.at[1] - 0.32, -3.0], hi: [p.at[0] + 0.32, p.at[1] + 0.32, 0.0] });
                }
                f.color = [30, 70, 150];
                body_rect = ([-7.62, -10.16], [7.62, 10.16]);
            }
            "Crystal_HC49" => {
                f.pads = vec![tht(1, [-2.44, 0.0], 1.5, 0.8), tht(2, [2.44, 0.0], 1.5, 0.8)];
                f.pads[0].shape = PadShape::Round;
                f.silk = rect_silk(11.0, 4.6);
                f.body = vec![Solid::Box { lo: [-5.4, -2.25, 0.0], hi: [5.4, 2.25, 3.5] }];
                f.color = [190, 190, 196];
                body_rect = ([-5.5, -2.3], [5.5, 2.3]);
            }
            _ => return None,
        }
    }
    f.court = court_of(&f.pads, body_rect);
    Some(f)
}

/// SOIC/TSSOP: pin 1 top left, numbered down the left and up the right.
fn dual_row(f: &mut Footprint, n: usize, pitch: f64, x: f64, pad: P, bw: f64, h: f64) -> Option<(P, P)> {
    if n < 4 || n % 2 != 0 || n > 64 {
        return None;
    }
    let rows = n / 2;
    let span = (rows - 1) as f64 * pitch;
    for i in 0..rows {
        f.pads.push(smd(&(i + 1).to_string(), [-x, span / 2.0 - i as f64 * pitch], pad));
    }
    for i in 0..rows {
        f.pads.push(smd(&(rows + i + 1).to_string(), [x, -span / 2.0 + i as f64 * pitch], pad));
    }
    let l = span + pitch + 0.6;
    f.silk = vec![G::Line([-bw / 2.0, l / 2.0], [bw / 2.0, l / 2.0]), G::Line([-bw / 2.0, -l / 2.0], [bw / 2.0, -l / 2.0]), G::Circle([-x - pad[0] / 2.0 - 0.1, span / 2.0 + 0.9], 0.15)];
    f.body = vec![Solid::Box { lo: [-bw / 2.0, -l / 2.0, 0.1], hi: [bw / 2.0, l / 2.0, h] }];
    for p in &f.pads {
        let s = p.at[0].signum();
        f.body.push(Solid::Box { lo: [p.at[0].min(s * bw / 2.0) - 0.0, p.at[1] - pad[1] / 3.0, 0.0], hi: [p.at[0].max(s * bw / 2.0), p.at[1] + pad[1] / 3.0, 0.2] });
    }
    Some(([-bw / 2.0, -l / 2.0], [bw / 2.0, l / 2.0]))
}

/// A DIP outline with the pin-1 notch at the top.
fn notch_outline(w: f64, l: f64) -> Vec<G> {
    let (x, y) = (w / 2.0, l / 2.0);
    vec![G::Poly(vec![[-0.8, y], [-x, y], [-x, -y], [x, -y], [x, y], [0.8, y]], false), G::Poly((0..=8).map(|i| {
        let a = std::f64::consts::PI * (1.0 + i as f64 / 8.0);
        [0.8 * a.cos(), y + 0.8 * a.sin()]
    }).collect(), false)]
}

// ---------------------------------------------------------------- symbols

fn pin(num: impl ToString, name: &str, at: P, dir: P, len: f64) -> SymPin {
    SymPin { num: num.to_string(), name: name.into(), at, dir, len }
}

fn arc(c: P, r: f64, a0: f64, a1: f64) -> G {
    let n = 10;
    G::Poly((0..=n).map(|i| {
        let a = (a0 + (a1 - a0) * i as f64 / n as f64).to_radians();
        [c[0] + r * a.cos(), c[1] + r * a.sin()]
    }).collect(), false)
}

/// The symbol for a library key; `pins` are the footprint's pad numbers (for ICs and connectors)
/// and `names` optional pin names by position.
pub fn symbol(key: &str, pins: &[String], names: &[String]) -> Symbol {
    if matches!(key, "PICO" | "SERVO" | "ENC" | "STEPPER" | "STEPDRV") {
        let names = if names.is_empty() { default_pin_names(key, "") } else { names.to_vec() };
        return symbol("U", pins, &names);
    }
    let up = [0.0, -1.0];
    let down = [0.0, 1.0];
    let two = |a: &str, b: &str| vec![pin(1, a, [0.0, 5.08], up, 2.54), pin(2, b, [0.0, -5.08], down, 2.54)];
    let mut s = Symbol { ref_at: [2.0, 0.6], value_at: [2.0, -1.8], ..Default::default() };
    match key {
        "R" => {
            s.pins = two("", "");
            s.g = vec![G::Rect([-0.9, -2.54], [0.9, 2.54])];
        }
        "C" | "CP" => {
            s.pins = two("", "");
            s.g = vec![G::Line([0.0, 2.54], [0.0, 0.5]), G::Line([0.0, -2.54], [0.0, -0.5]), G::Line([-1.8, 0.5], [1.8, 0.5])];
            if key == "CP" {
                s.g.push(G::Poly(vec![[-1.8, -0.5], [-1.8, -0.9], [1.8, -0.9], [1.8, -0.5]], false));
                s.g.push(G::Line([-1.4, 1.6], [-0.6, 1.6]));
                s.g.push(G::Line([-1.0, 1.2], [-1.0, 2.0]));
            } else {
                s.g.push(G::Line([-1.8, -0.5], [1.8, -0.5]));
            }
        }
        "L" => {
            s.pins = two("", "");
            for i in 0..4 {
                s.g.push(arc([0.0, 1.905 - i as f64 * 1.27], 0.635, -90.0, 90.0));
            }
        }
        "D" | "LED" => {
            // pin 1 cathode at the bottom, anode on top: current flows down
            s.pins = vec![pin(1, "K", [0.0, -5.08], down, 2.54), pin(2, "A", [0.0, 5.08], up, 2.54)];
            s.g = vec![G::Poly(vec![[-1.27, 1.1], [1.27, 1.1], [0.0, -1.1]], true), G::Line([-1.27, -1.1], [1.27, -1.1]), G::Line([0.0, 2.54], [0.0, -2.54])];
            if key == "LED" {
                for dy in [0.0, -1.0] {
                    s.g.push(G::Line([1.6, 0.6 + dy], [2.6, -0.2 + dy]));
                    s.g.push(G::Poly(vec![[2.6, -0.2 + dy], [2.0, -0.1 + dy], [2.4, 0.3 + dy]], true));
                }
                s.ref_at = [3.0, 0.6];
                s.value_at = [3.0, -1.8];
            }
        }
        "Q_NPN" | "Q_PNP" => {
            // SOT-23 BJTs: 1 B, 2 E, 3 C
            s.pins = vec![pin(1, "B", [-5.08, 0.0], [1.0, 0.0], 2.54), pin(3, "C", [2.54, 5.08], down, 2.54), pin(2, "E", [2.54, -5.08], up, 2.54)];
            s.g = vec![G::Circle([0.6, 0.0], 2.8), G::Line([-1.0, 1.6], [-1.0, -1.6]), G::Line([-1.0, 0.7], [2.54, 2.54]), G::Line([-1.0, -0.7], [2.54, -2.54])];
            s.g.push(if key == "Q_NPN" { G::Poly(vec![[2.54, -2.54], [1.3, -2.4], [1.9, -1.5]], true) } else { G::Poly(vec![[-1.0, -0.7], [0.2, -0.85], [-0.4, -1.75]], true) });
            s.ref_at = [4.0, 0.6];
            s.value_at = [4.0, -1.8];
        }
        "Q_NMOS" | "Q_PMOS" => {
            // SOT-23 MOSFETs: 1 G, 2 S, 3 D
            s.pins = vec![pin(1, "G", [-5.08, 0.0], [1.0, 0.0], 3.81), pin(3, "D", [2.54, 5.08], down, 2.54), pin(2, "S", [2.54, -5.08], up, 2.54)];
            s.g = vec![
                G::Circle([0.6, 0.0], 2.8),
                G::Line([-1.27, 1.6], [-1.27, -1.27]),
                G::Line([-0.6, 1.8], [-0.6, 0.9]),
                G::Line([-0.6, 0.5], [-0.6, -0.5]),
                G::Line([-0.6, -0.9], [-0.6, -1.8]),
                G::Line([-0.6, 1.27], [2.54, 1.27]),
                G::Line([2.54, 1.27], [2.54, 2.54]),
                G::Line([-0.6, -1.27], [2.54, -1.27]),
                G::Line([2.54, -1.27], [2.54, -2.54]),
                G::Line([-0.6, 0.0], [1.4, 0.0]),
                G::Line([1.4, 0.0], [1.4, -1.27]),
            ];
            s.g.push(if key == "Q_NMOS" { G::Poly(vec![[-0.6, 0.0], [0.3, 0.4], [0.3, -0.4]], true) } else { G::Poly(vec![[1.2, 0.0], [0.3, 0.4], [0.3, -0.4]], true) });
            s.ref_at = [4.0, 0.6];
            s.value_at = [4.0, -1.8];
        }
        "RV" => {
            // the track top to bottom, the wiper from the right
            s.pins = vec![pin(1, "", [0.0, 5.08], up, 2.54), pin(3, "", [0.0, -5.08], down, 2.54), pin(2, "", [5.08, 0.0], [-1.0, 0.0], 2.54)];
            s.g = vec![G::Rect([-0.9, -2.54], [0.9, 2.54]), G::Line([2.54, 0.0], [1.5, 0.0]), G::Poly(vec![[0.9, 0.0], [1.7, 0.45], [1.7, -0.45]], true)];
            s.ref_at = [-4.5, 0.6];
            s.value_at = [-4.5, -1.8];
        }
        "M" => {
            s.pins = two("+", "");
            s.g = vec![G::Circle([0.0, 0.0], 2.54), G::Poly(vec![[-1.1, -1.0], [-1.1, 1.0], [0.0, -0.1], [1.1, 1.0], [1.1, -1.0]], false), G::Line([-1.6, 3.6], [-0.6, 3.6]), G::Line([-1.1, 3.1], [-1.1, 4.1])];
            s.ref_at = [3.4, 0.6];
            s.value_at = [3.4, -1.8];
        }
        "SW" => {
            s.pins = vec![pin(1, "", [-5.08, 0.0], [1.0, 0.0], 2.54), pin(2, "", [5.08, 0.0], [-1.0, 0.0], 2.54)];
            s.g = vec![G::Circle([-2.2, 0.0], 0.35), G::Circle([2.2, 0.0], 0.35), G::Line([-2.4, 1.0], [2.4, 1.0]), G::Line([0.0, 1.0], [0.0, 2.2]), G::Line([-1.0, 2.2], [1.0, 2.2])];
            s.ref_at = [-2.5, 3.0];
            s.value_at = [-2.5, -2.2];
        }
        "Y" => {
            s.pins = vec![pin(1, "", [-5.08, 0.0], [1.0, 0.0], 3.3), pin(2, "", [5.08, 0.0], [-1.0, 0.0], 3.3)];
            s.g = vec![G::Rect([-0.7, -1.5], [0.7, 1.5]), G::Line([-1.6, -1.8], [-1.6, 1.8]), G::Line([1.6, -1.8], [1.6, 1.8])];
            s.ref_at = [-2.5, 2.6];
            s.value_at = [-2.5, -3.6];
        }
        "J" => {
            // one column of pins on the left, in pad order
            let n = pins.len().max(1);
            let h = n as f64 * GRID;
            let top = ((n - 1) as f64 / 2.0).floor() * GRID;
            for (i, num) in pins.iter().enumerate() {
                let y = top - i as f64 * GRID;
                s.pins.push(pin(num, names.get(i).map(|x| x.as_str()).unwrap_or(""), [-5.08, y], [1.0, 0.0], 2.54));
                s.g.push(G::Rect([-2.54, y - 0.3], [-1.6, y + 0.3]));
            }
            s.g.push(G::Rect([-2.54, top - h + GRID / 2.0], [0.0, top + GRID / 2.0]));
            s.pin_numbers = true;
            s.pin_names = !names.is_empty();
            s.ref_at = [-2.54, top + GRID];
            s.value_at = [0.8, top - h + GRID / 2.0 + 0.3];
        }
        _ => {
            // a box with pins 1..n/2 down the left and the rest up the right (as on a DIP/SOIC)
            let n = pins.len().max(1);
            let left = n.div_ceil(2);
            let right = n - left;
            let label_w = names.iter().map(|x| x.chars().count()).max().unwrap_or(0) as f64 * 1.0;
            let half = ((label_w * 2.0 + 3.0) / 2.0 / GRID).ceil().max(2.0) * GRID;
            let rows = left.max(right);
            let top = ((rows - 1) as f64 / 2.0).floor() * GRID;
            for i in 0..n {
                let (x, y, dir) = if i < left { (-half - GRID, top - i as f64 * GRID, [1.0, 0.0]) } else { (half + GRID, top - (rows - 1 - (i - left)) as f64 * GRID, [-1.0, 0.0]) };
                s.pins.push(pin(&pins[i], names.get(i).map(|x| x.as_str()).unwrap_or(""), [x, y], dir, GRID));
            }
            let bottom = top - (rows - 1) as f64 * GRID - GRID;
            s.g.push(G::Rect([-half, bottom], [half, top + GRID]));
            s.pin_numbers = true;
            s.pin_names = true;
            s.ref_at = [-half, top + GRID + 0.8];
            s.value_at = [-half, bottom - 2.2];
        }
    }
    s
}

// ---------------------------------------------------------------- stroke font

/// A small vector font (caps, digits, a few signs) on a 4 × 6 grid: polylines separated by `;`.
fn glyph(c: char) -> Option<&'static str> {
    let lower = match c {
        'a' => "4,4 4,0;4,3 3,4 1,4 0,3 0,1 1,0 3,0 4,1",
        'b' => "0,6 0,0;0,3 1,4 3,4 4,3 4,1 3,0 1,0 0,1",
        'c' => "4,3 3,4 1,4 0,3 0,1 1,0 3,0 4,1",
        'd' => "4,6 4,0;4,3 3,4 1,4 0,3 0,1 1,0 3,0 4,1",
        'e' => "0,2 4,2 4,3 3,4 1,4 0,3 0,1 1,0 3,0 4,1",
        'f' => "3.5,6 2.5,6 1.5,5 1.5,0;0,4 3,4",
        'g' => "4,4 4,-1 3,-2 1,-2 0,-1;4,3 3,4 1,4 0,3 0,1 1,0 3,0 4,1",
        'h' => "0,6 0,0;0,3 1,4 3,4 4,3 4,0",
        'i' => "2,4 2,0;2,5.5 2,6",
        'j' => "3,4 3,-1 2,-2 1,-2;3,5.5 3,6",
        'k' => "0,6 0,0;4,4 0,1.5;1.3,2.3 4,0",
        'l' => "1,6 2,6 2,1 3,0",
        'm' => "0,4 0,0;0,3 1,4 2,3 2,0;2,3 3,4 4,3 4,0",
        'n' => "0,4 0,0;0,3 1,4 3,4 4,3 4,0",
        'o' => "1,0 3,0 4,1 4,3 3,4 1,4 0,3 0,1 1,0",
        'p' => "0,4 0,-2;0,3 1,4 3,4 4,3 4,1 3,0 1,0 0,1",
        'q' => "4,4 4,-2;4,3 3,4 1,4 0,3 0,1 1,0 3,0 4,1",
        'r' => "0,4 0,0;0,2.5 1.5,4 3,4 4,3.5",
        's' => "4,3.5 3,4 1,4 0,3.2 1,2 3,2 4,0.8 3,0 1,0 0,0.5",
        't' => "1.5,6 1.5,1 2.5,0 3.5,0;0,4 3.5,4",
        'u' => "0,4 0,1 1,0 3,0 4,1;4,4 4,0",
        'v' => "0,4 2,0 4,4",
        'w' => "0,4 1,0 2,2.5 3,0 4,4",
        'x' => "0,0 4,4;0,4 4,0",
        'y' => "0,4 2,0;4,4 1,-2",
        'z' => "0,4 4,4 0,0 4,0",
        _ => "",
    };
    if !lower.is_empty() {
        return Some(lower);
    }
    Some(match c.to_ascii_uppercase() {
        '0' => "1,0 3,0 4,1 4,5 3,6 1,6 0,5 0,1 1,0;0.5,1 3.5,5",
        '1' => "1,5 2,6 2,0;1,0 3,0",
        '2' => "0,5 1,6 3,6 4,5 4,4 0,0 4,0",
        '3' => "0,5 1,6 3,6 4,5 4,4 3,3 4,2 4,1 3,0 1,0 0,1;1.5,3 3,3",
        '4' => "3,0 3,6 0,2 4,2",
        '5' => "4,6 0,6 0,3 3,3 4,2 4,1 3,0 1,0 0,1",
        '6' => "3,6 1,6 0,5 0,1 1,0 3,0 4,1 4,2 3,3 0,3",
        '7' => "0,6 4,6 1,0",
        '8' => "1,3 0,4 0,5 1,6 3,6 4,5 4,4 3,3 1,3 0,2 0,1 1,0 3,0 4,1 4,2 3,3",
        '9' => "4,3 1,3 0,4 0,5 1,6 3,6 4,5 4,1 3,0 1,0",
        'A' => "0,0 0,4 2,6 4,4 4,0;0,3 4,3",
        'B' => "0,0 0,6 3,6 4,5 4,4 3,3 0,3;3,3 4,2 4,1 3,0 0,0",
        'C' => "4,1 3,0 1,0 0,1 0,5 1,6 3,6 4,5",
        'D' => "0,0 0,6 2,6 4,4 4,2 2,0 0,0",
        'E' => "4,0 0,0 0,6 4,6;0,3 3,3",
        'F' => "0,0 0,6 4,6;0,3 3,3",
        'G' => "4,5 3,6 1,6 0,5 0,1 1,0 3,0 4,1 4,3 2,3",
        'H' => "0,0 0,6;4,0 4,6;0,3 4,3",
        'I' => "1,0 3,0;2,0 2,6;1,6 3,6",
        'J' => "0,1 1,0 2,0 3,1 3,6;2,6 4,6",
        'K' => "0,0 0,6;4,6 0,2;1.3,3.3 4,0",
        'L' => "0,6 0,0 4,0",
        'M' => "0,0 0,6 2,3 4,6 4,0",
        'N' => "0,0 0,6 4,0 4,6",
        'O' => "1,0 3,0 4,1 4,5 3,6 1,6 0,5 0,1 1,0",
        'P' => "0,0 0,6 3,6 4,5 4,4 3,3 0,3",
        'Q' => "1,0 3,0 4,1 4,5 3,6 1,6 0,5 0,1 1,0;2.5,1.5 4,0",
        'R' => "0,0 0,6 3,6 4,5 4,4 3,3 0,3;2,3 4,0",
        'S' => "4,5 3,6 1,6 0,5 0,4 1,3 3,3 4,2 4,1 3,0 1,0 0,1",
        'T' => "0,6 4,6;2,6 2,0",
        'U' => "0,6 0,1 1,0 3,0 4,1 4,6",
        'V' => "0,6 2,0 4,6",
        'W' => "0,6 1,0 2,3 3,0 4,6",
        'X' => "0,0 4,6;0,6 4,0",
        'Y' => "0,6 2,3 4,6;2,3 2,0",
        'Z' => "0,6 4,6 0,0 4,0",
        '-' => "1,3 3,3",
        '+' => "0,3 4,3;2,1 2,5",
        '.' => "2,0 2,0.5",
        ',' => "2,0.5 1.5,-1",
        '/' => "0,0 4,6",
        '(' => "3,6 2,5 2,1 3,0",
        ')' => "1,6 2,5 2,1 1,0",
        '[' => "3,6 2,6 2,0 3,0",
        ']' => "1,6 2,6 2,0 1,0",
        '_' => "0,0 4,0",
        ':' => "2,1 2,1.5;2,4 2,4.5",
        '=' => "0,2 4,2;0,4 4,4",
        '#' => "1,0 1,6;3,0 3,6;0,2 4,2;0,4 4,4",
        '*' => "2,1 2,5;0.5,2 3.5,4;0.5,4 3.5,2",
        '?' => "0,5 1,6 3,6 4,5 4,4 2,3 2,2;2,0 2,0.5",
        '!' => "2,6 2,2;2,0 2,0.5",
        '%' => "0,0 4,6;0,6 0,5;4,0 4,1",
        '<' => "4,6 0,3 4,0",
        '>' => "0,6 4,3 0,0",
        '\'' => "2,6 2,4.5",
        'Μ' | 'µ' => "0,-1 0,4;0,1 1,0 3,0 4,1;4,0 4,4",
        'Ω' => "0,0 1,0 1,1 0,3 0,5 1,6 3,6 4,5 4,3 3,1 3,0 4,0",
        _ => return None,
    })
}

/// Text as stroke polylines: baseline from `at`, cap height `h`, anchor 0 left / 1 centre / 2 right,
/// turned `rot` degrees about `at`.
pub fn stroke_text(text: &str, at: P, h: f64, anchor: u8, rot: f64) -> Vec<Vec<P>> {
    let k = h / 6.0;
    let w = text_width(text, h);
    let x0 = match anchor {
        1 => -w / 2.0,
        2 => -w,
        _ => 0.0,
    };
    let (s, c) = rot.to_radians().sin_cos();
    let mut out = vec![];
    let mut x = x0;
    for ch in text.chars() {
        let kk = k;
        if let Some(g) = glyph(ch) {
            for line in g.split(';') {
                let pts: Vec<P> = line
                    .split_whitespace()
                    .filter_map(|p| {
                        let (a, b) = p.split_once(',')?;
                        let (u, v) = (x + a.parse::<f64>().ok()? * kk, b.parse::<f64>().ok()? * kk);
                        Some([at[0] + u * c - v * s, at[1] + u * s + v * c])
                    })
                    .collect();
                if pts.len() >= 2 {
                    out.push(pts);
                }
            }
        }
        x += 6.0 * kk;
    }
    out
}

pub fn text_width(text: &str, h: f64) -> f64 {
    let k = h / 6.0;
    let w: f64 = text.chars().count() as f64 * 6.0 * k;
    (w - 2.0 * k).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_footprint_builds() {
        for n in footprint_names() {
            let f = footprint(&n).unwrap_or_else(|| panic!("{n}"));
            assert!(!f.pads.is_empty(), "{n}");
            assert!(f.court.0[0] < f.court.1[0] && f.court.0[1] < f.court.1[1], "{n}");
        }
        for s in SYMBOLS {
            assert!(footprint(s.footprint).is_some(), "{}", s.key);
        }
    }

    #[test]
    fn symbol_pins_match_footprints() {
        for s in SYMBOLS {
            let f = footprint(s.footprint).unwrap();
            let pins = f.pin_numbers();
            let sym = symbol(s.key, &pins, &[]);
            for p in &sym.pins {
                assert!(pins.contains(&p.num), "{} pin {} not on {}", s.key, p.num, s.footprint);
            }
        }
        assert_eq!(footprint("SOIC-8").unwrap().pads[7].num, "8");
        assert!(footprint("SOIC-7").is_none());
    }

    #[test]
    fn pins_sit_on_the_grid() {
        for s in SYMBOLS {
            let f = footprint(s.footprint).unwrap();
            let sym = symbol(s.key, &f.pin_numbers(), &[]);
            for p in &sym.pins {
                for v in p.at {
                    assert!(((v / GRID).round() * GRID - v).abs() < 1e-6, "{} pin {} at {:?}", s.key, p.num, p.at);
                }
            }
        }
    }
}
