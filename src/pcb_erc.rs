//! Electrical rule check for PCB schematics, like Altium's ERC: every pin has an electrical type
//! (from the library, its name, or set by hand), and the connection matrix flags outputs driving
//! each other, power pins with no supply, inputs nothing drives, pins left open without a
//! no-connect mark, labels and wires that join nothing, duplicate references and unknown
//! footprints.

use serde::{Deserialize, Serialize};

use crate::pcb::{self, Issue, LabelKind, Netlist, Pcb, Part};
use crate::pcb_lib::{self, P};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PinType {
    #[default]
    Passive,
    Input,
    Output,
    /// Input or output (a GPIO).
    Bidir,
    /// A supply pin the part takes power from (VCC, GND).
    PowerIn,
    /// A pin that supplies power (a regulator's output).
    PowerOut,
    /// Pulls low only (open collector or drain).
    OpenCollector,
    /// Not connected inside the part.
    NoConnect,
}

fn is_ground(n: &str) -> bool {
    matches!(n, "GND" | "AGND" | "DGND" | "PGND" | "VSS" | "VEE" | "V-" | "GROUND" | "0V") || n.starts_with("GND")
}

fn is_supply(n: &str) -> bool {
    matches!(n, "VCC" | "VDD" | "V+" | "VS" | "VBAT" | "AVCC" | "AVDD" | "DVDD" | "VMOT" | "VM" | "VIO" | "VDDIO") || n.starts_with("VCC") || n.starts_with("VDD")
}

/// A regulator by its value (7805, AMS1117, LDO …): its OUT pin supplies power.
fn regulator(value: &str) -> bool {
    let v = value.to_ascii_uppercase();
    ["78", "79", "1117", "LDO", "LM317", "LM2596", "MCP1700", "AP2112", "REG", "XC6206", "HT73"].iter().any(|k| v.contains(k)) && !v.contains("555")
}

/// A pin's type from its part and name, when nobody set it.
pub fn library_type(part: &Part, name: &str) -> PinType {
    let n = name.trim().to_ascii_uppercase();
    let n = n.as_str();
    match part.symbol.to_ascii_uppercase().as_str() {
        "R" | "C" | "CP" | "L" | "D" | "LED" | "Y" | "SW" | "RV" | "M" | "STEPPER" | "J" => PinType::Passive,
        s if s.starts_with("Q_") => PinType::Passive,
        "SERVO" => match n {
            "SIG" => PinType::Input,
            _ => PinType::PowerIn,
        },
        "ENC" => match n {
            "A" | "B" | "Z" | "I" => PinType::Output,
            _ => PinType::PowerIn,
        },
        "PICO" => match n {
            "GND" | "AGND" => PinType::PowerIn,
            "3V3" => PinType::PowerOut,
            "RUN" | "3V3_EN" => PinType::Input,
            _ if n.starts_with("GP") => PinType::Bidir,
            // VSYS and VBUS take power or give it (USB), the reference is either
            _ => PinType::Passive,
        },
        "STEPDRV" => match n {
            "GND" | "VDD" | "VMOT" => PinType::PowerIn,
            "FAULT" => PinType::OpenCollector,
            "1A" | "1B" | "2A" | "2B" | "A1" | "A2" | "B1" | "B2" => PinType::Output,
            _ => PinType::Input,
        },
        _ => ic_type(part, n),
    }
}

fn ic_type(part: &Part, n: &str) -> PinType {
    if n.is_empty() {
        return PinType::Passive;
    }
    if n == "NC" || n == "N/C" || n == "DNC" {
        return PinType::NoConnect;
    }
    if is_ground(n) {
        return PinType::PowerIn;
    }
    let reg = regulator(&part.value);
    if reg {
        return match n {
            "VOUT" | "OUT" | "VO" => PinType::PowerOut,
            "VIN" | "IN" | "VI" => PinType::PowerIn,
            "EN" | "SHDN" | "ON" => PinType::Input,
            "ADJ" | "FB" => PinType::Input,
            _ => PinType::Passive,
        };
    }
    if is_supply(n) || n == "VIN" {
        return PinType::PowerIn;
    }
    if n == "DIS" || n == "DISCH" || n.contains("OPEN") || n == "OC" || n == "OD" || n == "FAULT" || n == "INT" || n == "ALERT" {
        return PinType::OpenCollector;
    }
    let out = n == "OUT" || n == "Q" || n == "Y" || n == "TX" || n == "TXD" || n == "DO" || n == "SDO" || n == "MISO" || n.starts_with("OUT") || n.ends_with("OUT") || n == "/Q" || n == "~Q";
    if out {
        return PinType::Output;
    }
    let inp = n == "IN" || n == "RX" || n == "RXD" || n == "DI" || n == "SDI" || n == "MOSI" || n == "SCK" || n == "SCLK" || n == "CS" || n == "TRIG" || n == "THR" || n == "RESET" || n == "RST" || n == "CTRL" || n == "CV" || n == "EN" || n.starts_with("IN") || n.ends_with('+') || n.ends_with('-') || n.starts_with("/") || n.starts_with('~');
    if inp {
        return PinType::Input;
    }
    if n.starts_with("GP") || n.starts_with("IO") || n.starts_with("P") && n[1..].chars().next().is_some_and(|c| c.is_ascii_alphabetic()) || n == "SDA" || n == "SCL" {
        return PinType::Bidir;
    }
    PinType::Passive
}

/// A pin's type: set by hand in `pin_types` ("U1.3"), or from the library.
pub fn pin_type(p: &Pcb, part: usize, num: &str, name: &str) -> PinType {
    let pt = &p.parts[part];
    if let Some(t) = p.pin_types.get(&format!("{}.{}", pt.reference, num)) {
        return *t;
    }
    if let Some(l) = crate::pcb_lib::lib_symbol(&pt.symbol) {
        if let Some(k) = l.pins.iter().find(|x| x.num == num).map(|x| x.kind.as_str()) {
            return kicad_type(k);
        }
    }
    library_type(pt, name)
}

/// A KiCad pin electrical type.
pub fn kicad_type(k: &str) -> PinType {
    match k {
        "input" => PinType::Input,
        "output" => PinType::Output,
        "bidirectional" | "tri_state" => PinType::Bidir,
        "power_in" => PinType::PowerIn,
        "power_out" => PinType::PowerOut,
        "open_collector" | "open_emitter" => PinType::OpenCollector,
        "no_connect" | "unconnected" => PinType::NoConnect,
        _ => PinType::Passive,
    }
}

/// Every schematic pin with its name.
fn pin_names(p: &Pcb, part: usize) -> Vec<(String, String)> {
    p.parts[part].symbol().pins.iter().map(|s| (s.num.clone(), s.name.clone())).collect()
}

const EPS: f64 = 1e-3;

/// Run the electrical rule check; checks in `board.rules.off` are skipped.
pub fn erc(p: &Pcb, nl: &Netlist) -> Vec<Issue> {
    let mut out = vec![];
    let pins = p.sch_pins();
    let at_of = |part: usize, num: &str| pins.iter().find(|s| s.part == part && s.num == num).map(|s| s.at).unwrap_or(p.parts[part].sch);
    let name_of = |part: usize, num: &str| pin_names(p, part).into_iter().find(|x| x.0 == num).map(|x| x.1).unwrap_or_default();
    let nc_at = |at: P| p.no_connects.iter().any(|q| pcb::dist(*q, at) < EPS);
    let who = |part: usize, num: &str| {
        let n = name_of(part, num);
        if n.is_empty() || n == num { format!("{} pin {num}", p.parts[part].reference) } else { format!("{} pin {num} ({n})", p.parts[part].reference) }
    };
    // pins on no net
    for s in &pins {
        let connected = nl.net_of(s.part, &s.num).is_some();
        let nc = nc_at(s.at);
        let ty = pin_type(p, s.part, &s.num, &name_of(s.part, &s.num));
        if !connected && !nc && ty != PinType::NoConnect {
            out.push(Issue::new("erc_unconnected", s.at, false, format!("{} is not connected", who(s.part, &s.num))));
        }
        if connected && (nc || ty == PinType::NoConnect) {
            let why = if nc { "has a no-connect mark" } else { "is a no-connect pin" };
            out.push(Issue::new("erc_nc", s.at, false, format!("{} {why} but is wired to {}", who(s.part, &s.num), nl.nets[nl.net_of(s.part, &s.num).unwrap()].name)));
        }
    }
    for net in &nl.nets {
        let first = net.pins.first().map(|(a, n)| at_of(*a, n)).unwrap_or([0.0; 2]);
        if net.pins.len() == 1 {
            out.push(Issue::new("erc_single", first, false, format!("Net {} has only one pin ({})", net.name, who(net.pins[0].0, &net.pins[0].1))));
        }
        let types: Vec<(PinType, usize, &str)> = net.pins.iter().map(|(a, n)| (pin_type(p, *a, n, &name_of(*a, n)), *a, n.as_str())).collect();
        let count = |t: PinType| types.iter().filter(|x| x.0 == t).count();
        let outs: Vec<String> = types.iter().filter(|x| x.0 == PinType::Output).map(|x| who(x.1, x.2)).collect();
        let pouts: Vec<String> = types.iter().filter(|x| x.0 == PinType::PowerOut).map(|x| who(x.1, x.2)).collect();
        if outs.len() > 1 {
            out.push(Issue::new("erc_drivers", first, true, format!("Net {}: outputs drive each other ({})", net.name, outs.join(", "))));
        }
        if !outs.is_empty() && !pouts.is_empty() {
            out.push(Issue::new("erc_drivers", first, true, format!("Net {}: {} drives a supply ({})", net.name, outs[0], pouts.join(", "))));
        }
        if !outs.is_empty() && count(PinType::OpenCollector) > 0 {
            out.push(Issue::new("erc_drivers", first, true, format!("Net {}: {} fights an open-collector pin", net.name, outs[0])));
        }
        if pouts.len() > 1 {
            out.push(Issue::new("erc_drivers", first, false, format!("Net {}: several supplies tied together ({})", net.name, pouts.join(", "))));
        }
        let sources = count(PinType::PowerOut) + count(PinType::Passive) + count(PinType::Bidir);
        if count(PinType::PowerIn) > 0 && !net.power && sources == 0 {
            let (_, a, n) = types.iter().find(|x| x.0 == PinType::PowerIn).unwrap();
            out.push(Issue::new("erc_power", at_of(*a, n), true, format!("{} needs power, but net {} has no supply (add a power port or a regulator)", who(*a, n), net.name)));
        }
        let drivers = count(PinType::Output) + count(PinType::Bidir) + count(PinType::Passive) + count(PinType::OpenCollector) + count(PinType::PowerOut);
        if count(PinType::Input) > 0 && drivers == 0 && !net.power {
            out.push(Issue::new("erc_undriven", first, false, format!("Net {}: nothing drives the inputs ({})", net.name, types.iter().filter(|x| x.0 == PinType::Input).map(|x| who(x.1, x.2)).collect::<Vec<_>>().join(", "))));
        }
    }
    // labels and wire ends that join nothing
    let on_wire = |at: P, skip: Option<usize>| p.wires.iter().enumerate().any(|(i, w)| Some(i) != skip && pcb::seg_point(w.a, w.b, at) < EPS);
    let on_pin = |at: P| pins.iter().any(|s| pcb::dist(s.at, at) < EPS);
    for l in &p.labels {
        if !on_wire(l.at, None) && !on_pin(l.at) {
            let what = match l.kind {
                LabelKind::Net => "Label",
                LabelKind::Power => "Power port",
                LabelKind::Ground => "Ground port",
            };
            out.push(Issue::new("erc_labels", l.at, false, format!("{what} {} is not on a wire or pin", l.text.trim())));
        }
    }
    for (i, w) in p.wires.iter().enumerate() {
        for end in [w.a, w.b] {
            let joined = on_wire(end, Some(i)) || on_pin(end) || p.labels.iter().any(|l| pcb::dist(l.at, end) < EPS) || nc_at(end);
            if !joined {
                out.push(Issue::new("erc_labels", end, false, "A wire ends on nothing".into()));
            }
        }
    }
    let mut seen: Vec<(String, String)> = vec![];
    for l in p.labels.iter().filter(|l| l.kind == LabelKind::Net) {
        let t = l.text.trim().to_string();
        if let Some((_, o)) = seen.iter().find(|(u, o)| u.eq_ignore_ascii_case(&t) && *o != t) {
            out.push(Issue::new("erc_labels", l.at, false, format!("Labels {t} and {o} differ only in case, so they are different nets")));
        } else {
            seen.push((t.to_ascii_uppercase(), t));
        }
    }
    // references and footprints
    for (i, a) in p.parts.iter().enumerate() {
        if p.parts[..i].iter().any(|b| b.reference.eq_ignore_ascii_case(&a.reference)) {
            out.push(Issue::new("erc_refs", a.sch, true, format!("Reference {} is used more than once", a.reference)));
        }
        if pcb_lib::footprint(&a.footprint).is_none() {
            out.push(Issue::new("erc_footprint", a.sch, true, format!("{}: footprint {} is not in the library", a.reference, a.footprint)));
        }
    }
    out.retain(|i| p.board.rules.checks(i.rule));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcb::{Label, Wire};

    fn port(p: &mut Pcb, at: P, text: &str, kind: LabelKind) {
        p.labels.push(Label { at, text: text.into(), kind, rot: 0 });
    }

    #[test]
    fn outputs_fight_and_supplies_are_missing() {
        let mut p = Pcb::new("e");
        let u1 = p.add_part("U", Some("SOIC-8"), Some("NE555"), [0.0, 0.0]).unwrap();
        let u2 = p.add_part("U", Some("SOIC-8"), Some("NE555"), [30.0, 0.0]).unwrap();
        for u in [u1, u2] {
            p.parts[u].pin_names = ["GND", "TRIG", "OUT", "RESET", "CTRL", "THR", "DIS", "VCC"].iter().map(|s| s.to_string()).collect();
        }
        let pins = p.sch_pins();
        let at = |part: usize, num: &str| pins.iter().find(|s| s.part == part && s.num == num).unwrap().at;
        // both OUT pins on one net, both VCC pins on a net with no supply
        port(&mut p, at(u1, "3"), "SIG", LabelKind::Net);
        port(&mut p, at(u2, "3"), "SIG", LabelKind::Net);
        p.wires.push(Wire { a: at(u1, "8"), b: at(u2, "8") });
        let nl = p.netlist();
        let issues = erc(&p, &nl);
        assert!(issues.iter().any(|i| i.rule == "erc_drivers" && i.message.contains("SIG")), "{issues:#?}");
        assert!(issues.iter().any(|i| i.rule == "erc_power"), "{issues:#?}");
        assert!(issues.iter().any(|i| i.rule == "erc_unconnected" && i.message.contains("U1 pin 2 (TRIG)")), "{issues:#?}");
        // a power port supplies VCC; a no-connect mark quiets an open pin
        port(&mut p, at(u1, "8"), "VCC", LabelKind::Power);
        p.no_connects.push(at(u1, "2"));
        let nl = p.netlist();
        let issues = erc(&p, &nl);
        assert!(!issues.iter().any(|i| i.rule == "erc_power"), "{issues:#?}");
        assert!(!issues.iter().any(|i| i.message.contains("U1 pin 2 ")), "{issues:#?}");
        // a pin type set by hand wins, and a check can be turned off
        p.pin_types.insert("U2.3".into(), PinType::Input);
        let nl = p.netlist();
        assert!(!erc(&p, &nl).iter().any(|i| i.rule == "erc_drivers"));
        p.board.rules.off.push("erc_unconnected".into());
        assert!(!erc(&p, &nl).iter().any(|i| i.rule == "erc_unconnected"));
    }

    #[test]
    fn pin_types_from_names() {
        let mut part = Part { symbol: "U".into(), value: "AMS1117-3.3".into(), ..Default::default() };
        assert_eq!(library_type(&part, "VOUT"), PinType::PowerOut);
        assert_eq!(library_type(&part, "GND"), PinType::PowerIn);
        part.value = "TL072".into();
        assert_eq!(library_type(&part, "OUT1"), PinType::Output);
        assert_eq!(library_type(&part, "IN1-"), PinType::Input);
        assert_eq!(library_type(&part, "V+"), PinType::PowerIn);
        part.symbol = "PICO".into();
        assert_eq!(library_type(&part, "GP15"), PinType::Bidir);
        assert_eq!(library_type(&part, "3V3"), PinType::PowerOut);
        part.symbol = "R".into();
        assert_eq!(library_type(&part, "1"), PinType::Passive);
    }
}
