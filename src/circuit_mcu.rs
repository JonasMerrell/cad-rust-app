//! Microcontroller boards in the circuit simulation: the Raspberry Pi Pico's header pins and its
//! on-board parts (power path, regulator, LED, sense dividers) as circuit elements, and its
//! RP2040 running the board's firmware alongside a transient, reading the circuit's voltages and
//! driving its pins.

use serde::{Deserialize, Serialize};

use crate::circuit::{PinSim, diode_model, led_model};
use crate::circuit_solve::{Circuit, Dev, Ext, Reg, Wave};
use crate::mcu_rp2040::{Drive, NPIN, Rp2040, load_firmware};

/// The firmware a microcontroller part runs.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct McuSetup {
    /// The part's reference (U1).
    pub part: String,
    /// A UF2, ELF, HEX or BIN file.
    pub firmware: String,
    /// Powered from USB: 5 V on VBUS.
    pub usb: bool,
    /// Text that arrives on UART0 (GP1) at the start.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub uart_in: String,
}

impl Default for McuSetup {
    fn default() -> Self {
        McuSetup { part: String::new(), firmware: String::new(), usb: true, uart_in: String::new() }
    }
}

/// Whether a part is a Pico board.
pub fn is_pico(symbol: &str, value: &str, pins: usize) -> bool {
    let u = value.to_ascii_uppercase();
    symbol == "PICO" || (pins == 40 && (u.contains("PICO") || u.contains("RP2040")))
}

/// A Pico in the built circuit.
#[derive(Clone, Debug)]
pub struct PicoSim {
    pub reference: String,
    /// The first of its 30 `Dev::Pin` indices (GPIO n is `base + n`).
    pub base: usize,
    /// Each GPIO's node.
    pub gp: [usize; NPIN],
    pub v3: usize,
    pub gnd: usize,
    pub vref: usize,
    pub run: usize,
    pub setup: McuSetup,
}

/// The header pin number of each GPIO on the board (None: on the board only).
pub fn header_pin(g: usize) -> Option<usize> {
    let name = format!("GP{g}");
    crate::pcb_lib::PICO_PINS.iter().position(|p| *p == name).map(|k| k + 1)
}

fn ix(pins: &[PinSim], num: usize) -> Option<usize> {
    pins.iter().position(|p| p.num == num.to_string())
}

/// Add a Pico's elements; `pins` are its header pins (numbers 1..40) with their nodes.
pub fn build_pico(c: &mut Circuit, pins: &mut [PinSim], reference: &str, setup: McuSetup, notes: &mut Vec<String>) -> PicoSim {
    let base = c.elems.iter().filter(|e| matches!(e.dev, Dev::Pin(_))).count();
    let prefix = format!("{reference}.");
    let internal = |c: &Circuit, n: usize| n != 0 && c.nodes[n].starts_with(&prefix);
    let node = |pins: &[PinSim], num: usize| ix(pins, num).map(|k| pins[k].node);
    // the ground: the first ground pin wired to the circuit
    let gnd_pins = [3, 8, 13, 18, 23, 28, 33, 38];
    let wired = gnd_pins.iter().filter_map(|n| node(pins, *n)).find(|n| !internal(c, *n));
    let gnd = match wired {
        Some(n) => n,
        None => {
            notes.push(format!("{reference}: no GND pin is wired; the board shares the circuit's ground"));
            0
        }
    };
    enum T {
        P(usize),
        N(usize),
    }
    let add = |c: &mut Circuit, pins: &mut [PinSim], dev: Dev, ts: &[T]| {
        let nodes: Vec<usize> = ts
            .iter()
            .map(|t| match t {
                T::P(num) => ix(pins, *num).map(|k| pins[k].node).unwrap_or(0),
                T::N(n) => *n,
            })
            .collect();
        let e = c.add(dev, &nodes);
        for (k, t) in ts.iter().enumerate() {
            if let T::P(num) = t {
                if let Some(pk) = ix(pins, *num) {
                    pins[pk].terms.push((e, k));
                }
            }
        }
    };
    let g = |ohms: f64| Dev::R(1.0 / ohms);
    for n in gnd_pins {
        if node(pins, n).is_some_and(|k| k != gnd) {
            add(c, pins, g(1e-3), &[T::P(n), T::N(gnd)]);
        }
    }
    let (vbus, vsys, v3, en, vref, run) = (40, 39, 36, 37, 35, 30);
    if setup.usb {
        let usb = c.node(format!("{reference}.usb"));
        c.add(Dev::V(Wave::Dc(5.0), 0.0), &[usb, gnd]);
        add(c, pins, g(0.1), &[T::N(usb), T::P(vbus)]);
    }
    // VBUS → Schottky → VSYS → buck-boost → 3V3
    let (d, rs, cj, _) = diode_model("SCHOTTKY");
    let mid = c.node(format!("{reference}.d1"));
    add(c, pins, g(rs), &[T::P(vbus), T::N(mid)]);
    add(c, pins, Dev::D(d), &[T::N(mid), T::P(vsys)]);
    add(c, pins, Dev::C(cj), &[T::N(mid), T::P(vsys)]);
    add(c, pins, Dev::C(10e-6), &[T::P(vsys), T::N(gnd)]);
    add(c, pins, Dev::Reg(Reg { vset: 3.3, vdo: 0.1, iq: 20e-6, ro: 0.02 }), &[T::P(vsys), T::N(gnd), T::P(v3)]);
    add(c, pins, Dev::C(10e-6), &[T::P(v3), T::N(gnd)]);
    // the RP2040 and its flash draw about 20 mA
    add(c, pins, g(165.0), &[T::P(v3), T::N(gnd)]);
    add(c, pins, g(200.0), &[T::P(v3), T::P(vref)]);
    add(c, pins, Dev::C(2.2e-6), &[T::P(vref), T::N(gnd)]);
    add(c, pins, g(100e3), &[T::P(vsys), T::P(en)]);
    add(c, pins, g(50e3), &[T::P(v3), T::P(run)]);
    let v3n = node(pins, v3).unwrap_or(0);
    let mut gp = [0usize; NPIN];
    for (k, slot) in gp.iter_mut().enumerate() {
        let n = match header_pin(k).and_then(|h| node(pins, h)) {
            Some(n) => n,
            None => c.node(format!("{reference}.GP{k}")),
        };
        *slot = n;
        match header_pin(k) {
            Some(h) => add(c, pins, Dev::Pin(base + k), &[T::P(h), T::N(v3n), T::N(gnd)]),
            None => {
                c.add(Dev::Pin(base + k), &[n, v3n, gnd]);
            }
        }
        c.add(Dev::C(5e-12), &[n, gnd]);
    }
    // on the board: the LED on GP25, VBUS sense on GP24, VSYS / 3 on GP29 (ADC 3)
    let (d, rs, cj, _) = led_model("green");
    let a = c.node(format!("{reference}.led"));
    c.add(Dev::R(1.0 / (470.0 + rs)), &[gp[25], a]);
    c.add(Dev::D(d), &[a, gnd]);
    c.add(Dev::C(cj), &[a, gnd]);
    let vbus_n = node(pins, vbus).unwrap_or(0);
    let vsys_n = node(pins, vsys).unwrap_or(0);
    c.add(Dev::R(1.0 / 5.6e3), &[vbus_n, gp[24]]);
    c.add(Dev::R(1.0 / 10e3), &[gp[24], gnd]);
    c.add(Dev::R(1.0 / 200e3), &[vsys_n, gp[29]]);
    c.add(Dev::R(1.0 / 100e3), &[gp[29], gnd]);
    PicoSim { reference: reference.into(), base, gp, v3: v3n, gnd, vref: node(pins, vref).unwrap_or(0), run: node(pins, run).unwrap_or(0), setup }
}

/// What a microcontroller did in a run.
#[derive(Clone, Debug, Default)]
pub struct McuOut {
    pub reference: String,
    pub firmware: String,
    /// Lines each UART sent: (time it started, UART, text).
    pub uart: Vec<(f64, usize, String)>,
    pub notes: Vec<String>,
    pub instructions: u64,
    /// Each GPIO's state at the end (pin label, state).
    pub pins: Vec<(String, String)>,
}

struct McuRun {
    sim: PicoSim,
    mcu: Option<Rp2040>,
    /// The drive the circuit sees now (the chip may have run ahead and changed it later).
    applied: [Drive; NPIN],
    level: u32,
    held: bool,
}

/// The microcontrollers of a circuit, as the transient's pin driver.
pub struct McuExt {
    runs: Vec<McuRun>,
    npins: usize,
    horizon: f64,
}

impl McuExt {
    pub fn new(picos: &[PicoSim], horizon: f64, notes: &mut Vec<String>) -> McuExt {
        let mut runs = vec![];
        for s in picos {
            let path = s.setup.firmware.trim();
            let mcu = if path.is_empty() {
                notes.push(format!("{}: no firmware chosen; its pins stay inputs", s.reference));
                None
            } else {
                match std::fs::read(path).map_err(|e| format!("can't read {path}: {e}")).and_then(|b| load_firmware(&b)) {
                    Ok(fw) => {
                        let mut m = Rp2040::new(fw);
                        if !s.setup.uart_in.is_empty() {
                            let text = s.setup.uart_in.replace("\\n", "\n").replace("\\r", "\r");
                            m.uart_input(0, text.as_bytes());
                        }
                        Some(m)
                    }
                    Err(e) => {
                        notes.push(format!("{}: {e}", s.reference));
                        None
                    }
                }
            };
            runs.push(McuRun { sim: s.clone(), mcu, applied: [Drive::default(); NPIN], level: 0, held: false });
        }
        let npins = picos.iter().map(|s| s.base + NPIN).max().unwrap_or(0);
        McuExt { runs, npins, horizon }
    }

    pub fn active(&self) -> bool {
        self.runs.iter().any(|r| r.mcu.is_some())
    }

    pub fn finish(self) -> Vec<McuOut> {
        self.runs
            .into_iter()
            .map(|r| {
                let mut out = McuOut { reference: r.sim.reference.clone(), firmware: r.sim.setup.firmware.clone(), ..Default::default() };
                if let Some(m) = &r.mcu {
                    for u in 0..2 {
                        let mut line = String::new();
                        let mut start = 0.0;
                        for (t, b) in &m.chip.uart_log[u] {
                            if line.is_empty() {
                                start = *t as f64 * 1e-12;
                            }
                            match b {
                                b'\n' => out.uart.push((start, u, std::mem::take(&mut line))),
                                b'\r' => {}
                                b => line.push(*b as char),
                            }
                        }
                        if !line.is_empty() {
                            out.uart.push((start, u, line));
                        }
                    }
                    out.uart.sort_by(|a, b| a.0.total_cmp(&b.0));
                    out.notes = m.chip.notes.clone();
                    out.instructions = m.cpu[0].instructions + m.cpu[1].instructions;
                    let d = m.drives();
                    for g in 0..NPIN {
                        let label = match (g, header_pin(g)) {
                            (25, _) => "GP25 (LED)".to_string(),
                            (_, Some(h)) => format!("GP{g} (pin {h})"),
                            _ => continue,
                        };
                        let fsel = m.chip.funcsel(g);
                        let state = if fsel == "SIO" || fsel == "none" { d[g].describe().to_string() } else { format!("{fsel}, {}", d[g].describe()) };
                        out.pins.push((label, state));
                    }
                }
                out
            })
            .collect()
    }
}

impl Ext for McuExt {
    fn advance(&mut self, t: f64, x: &[f64], until: f64) -> f64 {
        let v = |n: usize| if n == 0 { 0.0 } else { x[n - 1] };
        let mut reached = until;
        for r in &mut self.runs {
            let Some(m) = r.mcu.as_mut() else { continue };
            if m.time() <= t + 1e-13 {
                r.applied = m.drives();
                let vg = v(r.sim.gnd);
                let vd = v(r.sim.v3) - vg;
                // Schmitt inputs, about 0.45 and 0.55 of the supply
                let mut lv = 0u32;
                for g in 0..NPIN {
                    let u = v(r.sim.gp[g]) - vg;
                    let was = r.level >> g & 1 != 0;
                    let hi = vd > 1.0 && if was { u > 0.45 * vd } else { u > 0.55 * vd };
                    if hi {
                        lv |= 1 << g;
                    }
                }
                r.level = lv;
                let analog = std::array::from_fn(|k| v(r.sim.gp[26 + k]) - vg);
                let vref = if r.sim.vref != 0 { v(r.sim.vref) - vg } else { vd };
                m.set_inputs((1 << NPIN) - 1, lv, analog, vref);
                // RUN low holds the chip in reset; releasing it restarts the firmware
                let run_low = r.sim.run != 0 && v(r.sim.run) - vg < 0.4 * vd.max(1.0);
                if run_low {
                    r.held = true;
                    m.hold(until);
                    r.applied = m.drives();
                } else {
                    if r.held {
                        r.held = false;
                        m.restart();
                        r.applied = m.drives();
                    }
                    m.run(until, true);
                }
            }
            reached = reached.min(m.time());
        }
        reached
    }

    fn pins(&self) -> Vec<(f64, f64)> {
        let mut out = vec![(0.0, 0.0); self.npins];
        for r in &self.runs {
            for g in 0..NPIN {
                out[r.sim.base + g] = r.applied[g].conductances();
            }
        }
        out
    }

    fn horizon(&self) -> f64 {
        self.horizon
    }
}

#[cfg(test)]
mod tests {
    use crate::circuit::{Analysis, Kind, run};
    use crate::pcb::Pcb;

    fn firmware(name: &str) -> String {
        format!("{}/examples/firmware/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    fn board(parts: &[(&str, &str)], nets: &[(&str, &[(&str, &str)])], fw: String) -> Pcb {
        let mut p = Pcb::new("t");
        p.add_part("PICO", None, None, [0.0, 0.0]).unwrap();
        for (k, (s, v)) in parts.iter().enumerate() {
            p.add_part(s, None, Some(v), [(k + 2) as f64 * 30.48, 0.0]).unwrap();
        }
        for (net, pins) in nets {
            let pins: Vec<(usize, String)> = pins.iter().map(|(r, n)| (p.part_index(r).unwrap(), n.to_string())).collect();
            crate::agent::pcb_connect(&mut p, net, &pins);
        }
        p.sim.mcus = vec![super::McuSetup { part: "U1".into(), firmware: fw, ..Default::default() }];
        p
    }

    fn tran(stop: f64) -> Analysis {
        Analysis { kind: Kind::Tran, stop, ..Default::default() }
    }

    #[test]
    fn pico_button_lights_an_led() {
        let fw = firmware("button.uf2");
        let nets: &[(&str, &[(&str, &str)])] = &[
            ("GND", &[("U1", "3"), ("SW1", "2"), ("D1", "1")]),
            ("BTN", &[("U1", "20"), ("SW1", "1")]),
            ("DRV", &[("U1", "21"), ("R1", "1")]),
            ("LED_A", &[("R1", "2"), ("D1", "2")]),
        ];
        let mut p = board(&[("SW", "SW"), ("R", "330"), ("LED", "Red")], nets, fw);
        let led = |p: &Pcb| {
            let out = run(p, &tran(0.02)).unwrap();
            let i = out.probe("I(D1)").unwrap();
            (*i.re.last().unwrap(), out)
        };
        let (off, out) = led(&p);
        assert!(off.abs() < 1e-5, "{off} {:?}", out.notes);
        assert!(out.mcus[0].pins.iter().any(|(l, s)| l.starts_with("GP16") && s == "low"), "{:?}", out.mcus[0].pins);
        p.sim.closed = vec!["SW1".into()];
        let (on, out) = led(&p);
        // (3.3 V - 1.9 V) / 330 Ω, less the pin's drop
        assert!(on > 3e-3 && on < 4.5e-3, "{on} {:?} {:?}", out.notes, out.mcus[0].notes);
        // the button reads low through its pull-up
        let btn = out.probe("V(BTN)").unwrap();
        assert!(btn.re.last().unwrap().abs() < 0.01);
    }

    #[test]
    fn pico_adc_drives_pwm_into_an_rc_filter() {
        let fw = firmware("adc.uf2");
        // a divider from 3V3 into GP26, and GP16's PWM through 10 kΩ into 1 µF
        let nets: &[(&str, &[(&str, &str)])] = &[
            ("GND", &[("U1", "3"), ("R2", "2"), ("C1", "2")]),
            ("V3", &[("U1", "36"), ("R1", "1")]),
            ("MID", &[("R1", "2"), ("R2", "1"), ("U1", "31")]),
            ("PWM", &[("U1", "21"), ("R3", "1")]),
            ("AVG", &[("R3", "2"), ("C1", "1")]),
        ];
        let p = board(&[("R", "10k"), ("R", "10k"), ("R", "10k"), ("C", "1u")], nets, fw);
        let out = run(&p, &tran(0.08)).unwrap();
        let m = &out.mcus[0];
        let line = m.uart.iter().rev().find(|l| l.2.starts_with("adc ")).unwrap_or_else(|| panic!("{:?} {:?}", m.uart, out.notes));
        let code: u32 = line.2.split(' ').nth(1).unwrap().parse().unwrap();
        assert!((2040..=2056).contains(&code), "{line:?}");
        let avg = *out.probe("V(AVG)").unwrap().re.last().unwrap();
        assert!((avg - 1.65).abs() < 0.08, "{avg}");
    }
}
