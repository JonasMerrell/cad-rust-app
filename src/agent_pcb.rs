//! Agent tools for PCB tabs: parts, nets, board layout, routing, checks, renders, Gerbers and
//! the 3D board.

use serde_json::{Value, json};

use super::{R, Session, round};
use crate::pcb::{self, Label, LabelKind, Layer, Pcb, Track, Via, Wire, Zone};
use crate::pcb_lib::{self, GRID, P};

fn pt(v: &Value) -> Option<P> {
    let a = v.as_array()?;
    Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?])
}

fn pts(v: &Value, what: &str) -> R<Vec<P>> {
    v.as_array().ok_or_else(|| format!("{what}: a list of [x, y] points"))?.iter().map(|p| pt(p).ok_or_else(|| format!("{what}: each point is [x, y] in mm"))).collect()
}

fn layer(v: Option<&Value>) -> R<Option<Layer>> {
    match v.and_then(|x| x.as_str()) {
        None => Ok(None),
        Some("top") => Ok(Some(Layer::Top)),
        Some("bottom") => Ok(Some(Layer::Bottom)),
        Some(o) => Err(format!("layer is top or bottom, not {o}")),
    }
}

fn r2(p: P) -> Value {
    json!([round(p[0]), round(p[1])])
}

/// "R1.2" → (part index, pin number).
fn pin_ref(p: &Pcb, s: &str) -> R<(usize, String)> {
    let (r, n) = s.rsplit_once('.').ok_or_else(|| format!("{s}: a pin is REF.PIN, e.g. R1.2 or U1.8"))?;
    let i = p.part_index(r).ok_or_else(|| format!("{s}: no part {r}"))?;
    let pins = p.parts[i].footprint().pin_numbers();
    // a pin may also be named (U1.VCC)
    let num = if pins.iter().any(|x| x == n) {
        n.to_string()
    } else if let Some(k) = p.parts[i].pin_names.iter().position(|x| x.eq_ignore_ascii_case(n)) {
        pins.get(k).cloned().ok_or_else(|| format!("{s}: pin name {n} has no pin"))?
    } else {
        return Err(format!("{s}: {r} has pins {}", pins.join(", ")));
    };
    Ok((i, num))
}

fn label_kind(net: &str) -> LabelKind {
    let u = net.to_ascii_uppercase();
    if u.starts_with("GND") || u.starts_with("VSS") || u == "0V" {
        LabelKind::Ground
    } else if u.starts_with('+') || u.starts_with("VCC") || u.starts_with("VDD") || u.starts_with("VBAT") || u.starts_with("VIN") || u.starts_with("3V3") || u.starts_with("5V") || u.starts_with("12V") {
        LabelKind::Power
    } else {
        LabelKind::Net
    }
}

/// Join pins to a net on the schematic: a short wire out of each pin and a label at its end.
pub(crate) fn connect(p: &mut Pcb, net: &str, pins: &[(usize, String)]) {
    let all = p.sch_pins();
    let kind = label_kind(net);
    for (pi, num) in pins {
        let Some(sp) = all.iter().find(|x| x.part == *pi && &x.num == num) else { continue };
        if p.labels.iter().any(|l| l.text == net && pcb::dist(l.at, sp.at) < 1e-6) {
            continue;
        }
        // outwards: away from the body
        let d = pcb::sub(sp.at, sp.inner);
        let l = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-9);
        let dir = [(d[0] / l).round(), (d[1] / l).round()];
        let end = pcb::add(sp.at, [dir[0] * GRID, dir[1] * GRID]);
        p.wires.push(Wire { a: sp.at, b: end });
        let rot = match (dir[0] as i32, dir[1] as i32) {
            (-1, 0) => 2,
            (0, 1) => 1,
            (0, -1) => 3,
            _ => 0,
        };
        // ports hang the right way: ground below, supplies above
        let kind = match kind {
            LabelKind::Ground if dir[1] < -0.5 => LabelKind::Ground,
            LabelKind::Power if dir[1] > 0.5 => LabelKind::Power,
            _ => LabelKind::Net,
        };
        p.labels.push(Label { at: end, text: net.to_string(), kind, rot });
    }
}

/// Lay new symbols out in columns on the schematic, clear of the others.
fn free_sch_spot(p: &Pcb) -> P {
    let n = p.parts.len();
    let (col, rowi) = (n % 6, n / 6);
    pcb::snap([col as f64 * 30.48, -(rowi as f64) * 35.56], GRID)
}

/// Move part `i` (just added) off any symbol it overlaps: the column slot if that is clear,
/// else the first clear spot in reading order. Big imported symbols need the room.
fn clear_sch_spot(p: &mut Pcb, i: usize) {
    let boxes: Vec<(P, P)> = p.parts.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, q)| crate::pcb_export::sch_bbox(q)).collect();
    let at = p.parts[i].sch;
    p.parts[i].sch = [0.0; 2];
    let (lo, hi) = crate::pcb_export::sch_bbox(&p.parts[i]);
    let hits = |c: P| {
        // room for the reference and value written right of each symbol
        let (a, b) = ([c[0] + lo[0] - 12.7, c[1] + lo[1] - 2.54], [c[0] + hi[0] + 12.7, c[1] + hi[1] + 2.54]);
        boxes.iter().any(|(l, h)| a[0] < h[0] && b[0] > l[0] && a[1] < h[1] && b[1] > l[1])
    };
    let mut spot = at;
    if hits(at) {
        'search: for row in 0..200 {
            for col in 0..80 {
                let c = [col as f64 * 5.08, -(row as f64) * 5.08];
                if !hits(c) {
                    spot = c;
                    break 'search;
                }
            }
        }
    }
    p.parts[i].sch = pcb::snap(spot, GRID);
}

impl Session {
    pub(super) fn pcb_idx(&self, a: &Value) -> R<usize> {
        let i = a.get("pcb").and_then(|v| v.as_u64()).ok_or("give pcb (the PCB tab's index from create_pcb)")? as usize;
        if i >= self.ws.pcbs.len() {
            return Err(format!("no PCB tab {i}"));
        }
        Ok(i)
    }

    fn pcb_summary(&self, pi: usize) -> Value {
        let p = &self.ws.pcbs[pi];
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let fills = pcb::fill_zones(p, &nl, &pads);
        let its = pcb::items(p, &nl, &pads, &fills);
        let air = pcb::ratsnest(&its, &pads, &fills);
        let size = p.board.size();
        json!({
            "pcb": pi,
            "name": p.name,
            "board": {"size": [round(size[0]), round(size[1])], "thickness": p.board.thickness, "tracks": p.board.tracks.len(), "vias": p.board.vias.len(), "zones": p.board.zones.len()},
            "parts": p.parts.len(),
            "nets": nl.nets.len(),
            "unrouted": air.len(),
        })
    }

    /// The trace solver: a calculator, and the board's nets solved for current, heating,
    /// resistance, voltage drop and impedance.
    fn pcb_traces(&mut self, a: &Value) -> R<Value> {
        use crate::pcb_trace::{self as tr, LineKind};
        let mut out = json!({});
        if let Some(c) = a.get("calc") {
            let mut k = tr::Calc::default();
            let f = |n: &str| c.get(n).and_then(|v| v.as_f64());
            if let Some(pi) = a.get("pcb").and_then(|v| v.as_u64()).map(|v| v as usize).filter(|i| *i < self.ws.pcbs.len()) {
                // start from the board's top layer
                let st = tr::stack(&self.ws.pcbs[pi].board);
                k.geometry = st.line(Layer::Top);
                k.copper_oz = st.copper_oz(Layer::Top);
                k.temp_rise = self.ws.pcbs[pi].board.rules.temp_rise;
            }
            k.width = f("width").unwrap_or(k.width);
            k.copper_oz = f("copper_oz").unwrap_or(k.copper_oz);
            k.outer = c.get("layer").and_then(|v| v.as_str()) != Some("inner");
            k.current = f("current").unwrap_or(k.current);
            k.length = f("length").unwrap_or(k.length);
            k.temp_rise = f("temp_rise").unwrap_or(k.temp_rise);
            k.ambient = f("ambient").unwrap_or(k.ambient);
            k.geometry.h = f("h").unwrap_or(k.geometry.h);
            k.geometry.h2 = f("h2").unwrap_or(0.0);
            k.geometry.er = f("er").unwrap_or(k.geometry.er);
            k.gap = f("gap").unwrap_or(0.0);
            match c.get("line").and_then(|v| v.as_str()) {
                None | Some("microstrip") => k.geometry.kind = LineKind::Microstrip,
                Some("stripline") => k.geometry.kind = LineKind::Stripline,
                Some(o) => return Err(format!("calc.line is microstrip or stripline, not {o}")),
            }
            if k.width <= 0.0 || k.copper_oz <= 0.0 || k.geometry.h <= 0.0 || k.geometry.er < 1.0 {
                return Err("calc: width, copper_oz and h must be positive and er at least 1".into());
            }
            let o = k.solve();
            let g = tr::Geometry { t: k.t(), ..k.geometry };
            let mut v = serde_json::to_value(&o).unwrap_or_default();
            if let Some(m) = v.as_object_mut() {
                for x in m.values_mut() {
                    if let Some(n) = x.as_f64() {
                        *x = json!((n * 10000.0).round() / 10000.0);
                    }
                }
            }
            v["inputs"] = serde_json::to_value(&k).unwrap_or_default();
            if let Some(z) = f("z_target") {
                v["width_for_z"] = json!(g.width_for(z).map(|w| (w * 10000.0).round() / 10000.0));
            }
            if let Some(z) = f("zdiff_target") {
                let s = if k.gap > 0.0 { k.gap } else { k.width };
                v["width_for_zdiff"] = json!(g.diff_width_for(z, s).map(|w| (w * 10000.0).round() / 10000.0));
                v["gap_for_zdiff"] = json!(g.diff_gap_for(z, k.width).map(|w| (w * 10000.0).round() / 10000.0));
            }
            out["calc"] = v;
        }
        if a.get("pcb").is_some() {
            let pi = self.pcb_idx(a)?;
            let p = &self.ws.pcbs[pi];
            let nl = p.netlist();
            let pads = p.pads(&nl);
            let rep = tr::nets(p, &nl, &pads, &|n| tr::target(p, &nl, n));
            let r4 = |x: f64| if x.is_finite() { json!((x * 10000.0).round() / 10000.0) } else { Value::Null };
            out["pcb"] = json!(pi);
            out["stackup"] = serde_json::to_value(tr::stack(&p.board)).unwrap_or_default();
            out["temp_rise"] = json!(p.board.rules.temp_rise);
            out["nets"] = rep
                .iter()
                .map(|r| {
                    json!({"net": r.net, "current": r.current, "impedance": r.impedance, "tracks": r.tracks, "vias": r.vias, "length": r4(r.length), "min_width": r4(r.min_width), "max_width": r4(r.max_width),
                        "resistance": r4(r.resistance), "worst_path": [r.worst.0, r.worst.1], "drop": r4(r.drop), "rise": r4(r.rise), "need_width": r4(r.need_width),
                        "z_min": r4(r.z_min), "z_max": r4(r.z_max), "z_width": r4(r.z_width), "poured": r.poured, "open": r.open})
                })
                .collect::<Vec<_>>()
                .into();
            out["issues"] = tr::check(p, &|n| tr::target(p, &nl, n)).iter().map(|i| json!({"at": r2(i.at), "message": i.message, "error": i.error})).collect::<Vec<_>>().into();
        }
        if out.as_object().is_some_and(|o| o.is_empty()) {
            return Err("give pcb (to solve its nets) or calc (a track to work out), or both".into());
        }
        Ok(out)
    }

    pub(super) fn pcb_call(&mut self, name: &str, a: &Value) -> R<(Value, Option<Vec<u8>>)> {
        let v = match name {
            "create_pcb" => {
                // names are unique: each PCB tab has its own "<name> 3D" Part Studio
                let name = match a.get("name").and_then(|v| v.as_str()).map(clean_name).filter(|n| !n.is_empty()) {
                    Some(n) => pcb::unique_name(&self.ws.pcbs, &n, None),
                    None => pcb::next_name(&self.ws.pcbs),
                };
                let mut p = Pcb::new(name);
                apply_board(&mut p, a)?;
                self.ws.pcbs.push(p);
                let pi = self.ws.pcbs.len() - 1;
                let mut v = self.pcb_summary(pi);
                v["hint"] = json!("add parts with pcb_add_parts, join pins with pcb_connect, then pcb_edit auto_place, pcb_route and check_pcb");
                v
            }
            "pcb_add_parts" => {
                let pi = self.pcb_idx(a)?;
                let list = a.get("parts").and_then(|v| v.as_array()).ok_or("give parts: [{symbol, value, footprint, …}]")?.clone();
                let mut p = self.ws.pcbs[pi].clone();
                let mut added = vec![];
                for (k, x) in list.iter().enumerate() {
                    let sym = x.get("symbol").and_then(|v| v.as_str()).ok_or_else(|| format!("parts[{k}]: give symbol ({})", pcb_lib::SYMBOLS.iter().map(|s| s.key).collect::<Vec<_>>().join(", ")))?;
                    let sch = x.get("sch").and_then(pt).unwrap_or_else(|| free_sch_spot(&p));
                    let i = p.add_part(sym, x.get("footprint").and_then(|v| v.as_str()), x.get("value").and_then(|v| v.as_str()), sch).map_err(|e| format!("parts[{k}]: {e}"))?;
                    if let Some(r) = x.get("reference").and_then(|v| v.as_str()) {
                        if p.part_index(r).is_some_and(|j| j != i) {
                            return Err(format!("parts[{k}]: there is already a {r}"));
                        }
                        p.parts[i].reference = r.to_string();
                    }
                    edit_part(&mut p, i, x).map_err(|e| format!("parts[{k}]: {e}"))?;
                    if x.get("sch").is_none() {
                        clear_sch_spot(&mut p, i);
                    }
                    added.push(json!({"reference": p.parts[i].reference, "footprint": p.parts[i].footprint, "pins": pins_json(&p, i)}));
                }
                let nl = p.netlist();
                p.retag_copper(&nl);
                self.ws.pcbs[pi] = p;
                json!({"pcb": pi, "added": added})
            }
            "pcb_connect" => {
                let pi = self.pcb_idx(a)?;
                let nets = a.get("nets").and_then(|v| v.as_object()).ok_or("give nets: {\"GND\": [\"U1.4\", \"C1.2\"], …}")?;
                let mut p = self.ws.pcbs[pi].clone();
                for (net, pins) in nets {
                    let pins: Vec<(usize, String)> = pins.as_array().ok_or_else(|| format!("{net}: a list of pins like \"R1.2\""))?.iter().map(|x| x.as_str().ok_or_else(|| format!("{net}: pins are strings like \"R1.2\"")).and_then(|s| pin_ref(&p, s))).collect::<R<_>>()?;
                    connect(&mut p, net, &pins);
                }
                let nl = p.netlist();
                p.retag_copper(&nl);
                self.ws.pcbs[pi] = p;
                let nl = self.ws.pcbs[pi].netlist();
                json!({"pcb": pi, "nets": nl.nets.iter().map(|n| json!({"name": n.name, "pins": n.pins.iter().map(|(i, num)| format!("{}.{}", self.ws.pcbs[pi].parts[*i].reference, num)).collect::<Vec<_>>()})).collect::<Vec<_>>()})
            }
            "pcb_edit" => {
                let pi = self.pcb_idx(a)?;
                let mut p = self.ws.pcbs[pi].clone();
                if let Some(n) = a.get("name").and_then(|v| v.as_str()).map(clean_name).filter(|n| !n.is_empty()) {
                    p.name = pcb::unique_name(&self.ws.pcbs, &n, Some(pi));
                }
                apply_board(&mut p, a)?;
                for (k, x) in a.get("parts").and_then(|v| v.as_array()).into_iter().flatten().enumerate() {
                    let r = x.get("reference").and_then(|v| v.as_str()).ok_or_else(|| format!("parts[{k}]: give reference"))?;
                    let i = p.part_index(r).ok_or_else(|| format!("parts[{k}]: no part {r}"))?;
                    if x.get("delete").and_then(|v| v.as_bool()) == Some(true) {
                        p.parts.remove(i);
                        continue;
                    }
                    if let Some(nr) = x.get("new_reference").and_then(|v| v.as_str()) {
                        p.parts[i].reference = nr.to_string();
                    }
                    edit_part(&mut p, i, x).map_err(|e| format!("parts[{k}]: {e}"))?;
                }
                for (k, w) in a.get("wires").and_then(|v| v.as_array()).into_iter().flatten().enumerate() {
                    let v = pts(w, &format!("wires[{k}]"))?;
                    for s in v.windows(2) {
                        p.wires.push(Wire { a: s[0], b: s[1] });
                    }
                }
                for (k, l) in a.get("labels").and_then(|v| v.as_array()).into_iter().flatten().enumerate() {
                    let at = l.get("at").and_then(pt).ok_or_else(|| format!("labels[{k}]: give at [x, y]"))?;
                    let text = l.get("text").and_then(|v| v.as_str()).ok_or_else(|| format!("labels[{k}]: give text"))?;
                    let kind = match l.get("kind").and_then(|v| v.as_str()) {
                        None | Some("net") => LabelKind::Net,
                        Some("power") => LabelKind::Power,
                        Some("ground") => LabelKind::Ground,
                        Some(o) => return Err(format!("labels[{k}]: kind is net, power or ground, not {o}")),
                    };
                    p.labels.push(Label { at, text: text.into(), kind, rot: 0 });
                }
                if let Some(h) = a.get("holes").and_then(|v| v.as_array()) {
                    p.board.holes = h
                        .iter()
                        .map(|x| {
                            let at = x.get("at").and_then(pt).ok_or("holes: each {at: [x, y], d}")?;
                            let d = x.get("d").and_then(|v| v.as_f64()).unwrap_or(3.2);
                            if !(d > 0.0 && d <= 50.0) {
                                return Err(format!("holes: d is the hole diameter in mm, more than 0, not {d}"));
                            }
                            Ok(pcb::Hole { at, d })
                        })
                        .collect::<R<_>>()?;
                }
                if let Some(z) = a.get("zones").and_then(|v| v.as_array()) {
                    p.board.zones = z
                        .iter()
                        .map(|x| {
                            let net = x.get("net").and_then(|v| v.as_str()).ok_or("zones: each {net, layer, outline?}")?.to_string();
                            let outline = match x.get("outline") {
                                Some(o) => pcb::ccw(pts(o, "zones outline")?),
                                None => p.board.outline.clone(),
                            };
                            let pads = match x.get("pads").and_then(|v| v.as_str()) {
                                None | Some("tht_relief") => pcb::PadConnect::ThtRelief,
                                Some("relief") => pcb::PadConnect::Relief,
                                Some("solid") => pcb::PadConnect::Solid,
                                Some(o) => return Err(format!("zones pads is tht_relief, relief or solid, not {o}")),
                            };
                            let f = |k: &str| x.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0).max(0.0);
                            Ok(Zone { net, layer: layer(x.get("layer"))?.unwrap_or(Layer::Bottom), outline, pads, clearance: f("clearance"), relief_gap: f("relief_gap"), spoke: f("spoke") })
                        })
                        .collect::<R<_>>()?;
                }
                if let Some(k) = a.get("keepouts").and_then(|v| v.as_array()) {
                    p.board.keepouts = k
                        .iter()
                        .map(|x| {
                            let outline = pcb::ccw(pts(x.get("outline").ok_or("keepouts: each {outline, layer?, tracks?, vias?, pours?, parts?}")?, "keepouts outline")?);
                            if outline.len() < 3 {
                                return Err("keepouts outline: at least 3 points".to_string());
                            }
                            let b = |k: &str, d: bool| x.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
                            Ok(pcb::Keepout { outline, layer: layer(x.get("layer"))?, tracks: b("tracks", true), vias: b("vias", true), pours: b("pours", true), parts: b("parts", false) })
                        })
                        .collect::<R<_>>()?;
                }
                if let Some(n) = a.get("no_connects").and_then(|v| v.as_array()) {
                    let pins = p.sch_pins();
                    p.no_connects = n
                        .iter()
                        .map(|x| match x.as_str() {
                            Some(r) => {
                                let (i, num) = pin_ref(&p, r)?;
                                pins.iter().find(|s| s.part == i && s.num == num).map(|s| s.at).ok_or_else(|| format!("no_connects: {r} has no schematic pin"))
                            }
                            None => pt(x).ok_or_else(|| "no_connects: each a pin (U1.5) or [x, y]".to_string()),
                        })
                        .collect::<R<_>>()?;
                }
                if let Some(t) = a.get("pin_types").and_then(|v| v.as_object()) {
                    for (k, v) in t {
                        let (i, num) = pin_ref(&p, k)?;
                        let key = format!("{}.{num}", p.parts[i].reference);
                        match v.as_str() {
                            Some("auto") | None => {
                                p.pin_types.remove(&key);
                            }
                            Some(_) => {
                                let ty: crate::pcb_erc::PinType = serde_json::from_value(v.clone()).map_err(|_| format!("pin_types {k}: one of passive, input, output, bidir, power_in, power_out, open_collector, no_connect, auto"))?;
                                p.pin_types.insert(key, ty);
                            }
                        }
                    }
                }
                if a.get("clear_routing").and_then(|v| v.as_bool()) == Some(true) {
                    p.board.tracks.clear();
                    p.board.vias.clear();
                }
                let mut placed = None;
                match a.get("auto_place").and_then(|v| v.as_str()) {
                    None => {}
                    Some("unplaced") => placed = Some(crate::pcb_route::auto_place(&mut p, false)),
                    Some("all") => placed = Some(crate::pcb_route::auto_place(&mut p, true)),
                    Some(o) => return Err(format!("auto_place is unplaced or all, not {o}")),
                }
                let nl = p.netlist();
                p.retag_copper(&nl);
                self.ws.pcbs[pi] = p;
                let mut v = self.pcb_summary(pi);
                if let Some(n) = placed {
                    v["placed"] = json!(n);
                    let off: Vec<String> = {
                        let p = &self.ws.pcbs[pi];
                        p.parts
                            .iter()
                            .filter(|x| {
                                let (lo, hi) = p.court_world(x);
                                !(pcb::inside(&p.board.outline, lo) && pcb::inside(&p.board.outline, hi))
                            })
                            .map(|x| x.reference.clone())
                            .collect()
                    };
                    if !off.is_empty() {
                        v["not_on_board"] = json!(off);
                        v["hint"] = json!("the board is too small for every part: make it bigger, or place those parts with pcb_edit parts [{reference, at, rot, side}]");
                    }
                }
                v
            }
            "pcb_route" => {
                let pi = self.pcb_idx(a)?;
                let mut p = self.ws.pcbs[pi].clone();
                let nl = p.netlist();
                let pads = p.pads(&nl);
                for (k, t) in a.get("tracks").and_then(|v| v.as_array()).into_iter().flatten().enumerate() {
                    let v = pts(t.get("points").ok_or_else(|| format!("tracks[{k}]: give points"))?, &format!("tracks[{k}].points"))?;
                    if v.len() < 2 {
                        return Err(format!("tracks[{k}]: needs two points or more"));
                    }
                    let l = layer(t.get("layer"))?.unwrap_or(Layer::Top);
                    let net = match t.get("net").and_then(|v| v.as_str()) {
                        Some(n) => n.to_string(),
                        None => pcb::net_at(&p, &nl, &pads, v[0], l).or_else(|| pcb::net_at(&p, &nl, &pads, v[v.len() - 1], l)).map(|x| x.0).unwrap_or_default(),
                    };
                    let w = t.get("width").and_then(|v| v.as_f64()).unwrap_or_else(|| p.track_width(&nl, &net));
                    for s in v.windows(2) {
                        p.board.tracks.push(Track { a: s[0], b: s[1], width: w, layer: l, net: net.clone() });
                    }
                }
                for (k, x) in a.get("vias").and_then(|v| v.as_array()).into_iter().flatten().enumerate() {
                    let at = x.get("at").and_then(pt).ok_or_else(|| format!("vias[{k}]: give at [x, y]"))?;
                    let net = x.get("net").and_then(|v| v.as_str()).map(String::from).or_else(|| pcb::net_at(&p, &nl, &pads, at, Layer::Top).map(|x| x.0)).unwrap_or_default();
                    let nr = p.net_rules(&nl, &net);
                    p.board.vias.push(Via { at, d: nr.via_d, drill: nr.via_drill, net });
                }
                let mut v = json!({});
                // differential pairs first, so the autorouter goes round them
                let mut pairs_out = vec![];
                if let Some(which) = a.get("pairs").and_then(|v| v.as_array()) {
                    let names: Vec<&str> = which.iter().filter_map(|x| x.as_str()).collect();
                    let list: Vec<pcb::DiffPair> = p.board.rules.pairs.iter().filter(|d| names.contains(&"all") || names.contains(&d.name.as_str())).cloned().collect();
                    if list.is_empty() {
                        return Err(format!("pairs: no such pair; the pairs are {}", p.board.rules.pairs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", ")));
                    }
                    for d in list {
                        match crate::pcb_route::route_pair(&mut p, &d) {
                            Ok(o) => pairs_out.push(json!({"pair": d.name, "tracks": o.tracks, "layer": o.layer.label().to_lowercase(), "lengths": [round(o.p_len), round(o.n_len)]})),
                            Err(e) => pairs_out.push(json!({"pair": d.name, "error": e})),
                        }
                    }
                    v["pairs"] = json!(pairs_out);
                }
                let tuning = a.get("tune").is_some() || a.get("match").is_some();
                let auto = a.get("auto").and_then(|v| v.as_bool()).unwrap_or(a.get("tracks").is_none() && a.get("vias").is_none() && a.get("pairs").is_none() && !tuning);
                if auto {
                    let o = crate::pcb_route::autoroute(&mut p, a.get("net").and_then(|v| v.as_str()));
                    for (k, x) in json!({"routed": o.routed, "tracks_added": o.tracks, "vias_added": o.vias, "failed": o.failed}).as_object().unwrap() {
                        v[k] = x.clone();
                    }
                }
                // length tuning last, on the finished routing
                let mut tuned = vec![];
                for (k, t) in a.get("tune").and_then(|v| v.as_array()).into_iter().flatten().enumerate() {
                    let net = t.get("net").and_then(|v| v.as_str()).ok_or_else(|| format!("tune[{k}]: give net"))?;
                    let target = t.get("length").and_then(|v| v.as_f64()).ok_or_else(|| format!("tune[{k}]: give length (mm)"))?;
                    let before = crate::pcb_tune::net_length(&p, net);
                    let after = crate::pcb_tune::tune(&mut p, net, target)?;
                    tuned.push(json!({"net": net, "before": round(before), "after": round(after), "target": target}));
                }
                if let Some(m) = a.get("match").and_then(|v| v.as_str()) {
                    let rule = p.board.rules.lengths.iter().find(|r| r.name == m).cloned().ok_or_else(|| format!("match: no length rule {m}"))?;
                    for (n, b, af) in crate::pcb_tune::match_lengths(&mut p, &rule) {
                        tuned.push(json!({"net": n, "before": round(b), "after": round(af)}));
                    }
                }
                if !tuned.is_empty() {
                    v["tuned"] = json!(tuned);
                }
                let nl = p.netlist();
                p.retag_copper(&nl);
                self.ws.pcbs[pi] = p;
                let s = self.pcb_summary(pi);
                for (k, x) in s.as_object().unwrap() {
                    v[k] = x.clone();
                }
                if v["unrouted"].as_u64().unwrap_or(0) > 0 {
                    v["hint"] = json!("some connections are unrouted: spread the parts (pcb_edit), add a ground pour (pcb_edit zones), or route them with tracks");
                }
                v
            }
            "pcb_info" => {
                let pi = self.pcb_idx(a)?;
                let p = &self.ws.pcbs[pi];
                let nl = p.netlist();
                let pads = p.pads(&nl);
                let fills = pcb::fill_zones(p, &nl, &pads);
                let its = pcb::items(p, &nl, &pads, &fills);
                let air = pcb::ratsnest(&its, &pads, &fills);
                let parts: Vec<Value> = p
                    .parts
                    .iter()
                    .enumerate()
                    .map(|(i, x)| {
                        let (lo, hi) = p.court_world(x);
                        json!({
                            "reference": x.reference, "value": x.value, "symbol": x.symbol, "footprint": x.footprint,
                            "at": r2(x.at), "rot": round(x.rot), "side": if x.bottom { "bottom" } else { "top" },
                            "courtyard": [r2(lo), r2(hi)], "sch": r2(x.sch),
                            "bom": {"mpn": x.mpn, "manufacturer": x.manufacturer, "description": x.description, "datasheet": x.datasheet, "digikey": x.digikey, "package": x.package, "fields": x.fields.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<String, Value>>()},
                            "sim": sim_json(x, &p.sim),
                            "pads": pads.iter().filter(|q| q.part == i).map(|q| json!({"pin": q.num, "at": r2(q.at), "net": q.net.map(|n| nl.nets[n].name.clone())})).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                let mut v = self.pcb_summary(pi);
                v["outline"] = json!(p.board.outline.iter().map(|q| r2(*q)).collect::<Vec<_>>());
                v["rules"] = serde_json::to_value(&p.board.rules).unwrap_or_default();
                v["parts"] = json!(parts);
                v["nets"] = json!(nl.nets.iter().map(|n| json!({"name": n.name, "pins": n.pins.iter().map(|(i, num)| format!("{}.{}", p.parts[*i].reference, num)).collect::<Vec<_>>()})).collect::<Vec<_>>());
                v["lengths"] = json!(nl.nets.iter().filter(|n| p.board.tracks.iter().any(|t| t.net == n.name)).map(|n| json!({"net": n.name, "length": round(crate::pcb_tune::net_length(p, &n.name))})).collect::<Vec<_>>());
                v["airwires"] = json!(air.iter().take(100).map(|w| json!({"net": w.net, "from": r2(w.a), "to": r2(w.b)})).collect::<Vec<_>>());
                v
            }
            "check_pcb" => {
                let pi = self.pcb_idx(a)?;
                let p = &self.ws.pcbs[pi];
                let nl = p.netlist();
                let pads = p.pads(&nl);
                let fills = pcb::fill_zones(p, &nl, &pads);
                let issues = pcb::drc(p, &nl, &pads, &fills);
                let erc = crate::pcb_erc::erc(p, &nl);
                let errors = issues.iter().chain(&erc).filter(|i| i.error).count();
                let list = |v: &[pcb::Issue]| v.iter().take(100).map(|i| json!({"at": r2(i.at), "message": i.message, "error": i.error, "rule": i.rule})).collect::<Vec<_>>();
                json!({"pcb": pi, "ok": errors == 0, "errors": errors, "warnings": issues.len() + erc.len() - errors, "issues": list(&issues), "erc": list(&erc)})
            }
            "render_pcb" => {
                let pi = self.pcb_idx(a)?;
                let p = &self.ws.pcbs[pi];
                let nl = p.netlist();
                let width = a.get("width").and_then(|v| v.as_u64()).unwrap_or(1000).clamp(100, 4000) as u32;
                let png = match a.get("view").and_then(|v| v.as_str()).unwrap_or("board") {
                    "schematic" => {
                        let list = crate::pcb_export::schematic_scene(p, &nl, None);
                        let (lo, hi) = crate::pcb_export::extent(&list);
                        crate::pcb_export::png(&list, ([lo[0] - 5.0, lo[1] - 5.0], [hi[0] + 5.0, hi[1] + 5.0]), width, [250, 249, 244])?
                    }
                    "board" => {
                        let pads = p.pads(&nl);
                        let fills = pcb::fill_zones(p, &nl, &pads);
                        let its = pcb::items(p, &nl, &pads, &fills);
                        let air = pcb::ratsnest(&its, &pads, &fills);
                        let view = crate::pcb_export::BoardView { active: layer(a.get("layer"))?.unwrap_or(Layer::Top), ..Default::default() };
                        let list = crate::pcb_export::board_scene(p, &nl, &pads, &fills, &air, &view);
                        let (lo, hi) = crate::pcb_export::extent(&list);
                        crate::pcb_export::png(&list, ([lo[0] - 2.0, lo[1] - 2.0], [hi[0] + 2.0, hi[1] + 2.0]), width, [34, 38, 46])?
                    }
                    o => return Err(format!("view is board or schematic, not {o}")),
                };
                if let Some(path) = a.get("path").and_then(|p| p.as_str()) {
                    super::write_out(path, &png)?;
                }
                return Ok((json!({"pcb": pi, "saved": a.get("path")}), Some(png)));
            }
            "export_pcb" => {
                let pi = self.pcb_idx(a)?;
                let path = a.get("path").and_then(|v| v.as_str()).ok_or("give path: a .zip for the Gerbers and drill files, or a folder")?;
                let sets = a.get("outputs").and_then(|v| v.as_object());
                if let Some(o) = sets {
                    let job = &mut self.ws.pcbs[pi].outputs;
                    for (k, v) in o {
                        let slot = job.get_mut(k).ok_or_else(|| format!("unknown output {k}; outputs are {}", crate::pcb_fab::OutJob::NAMES.iter().map(|n| n.0).collect::<Vec<_>>().join(", ")))?;
                        *slot = v.as_bool().ok_or_else(|| format!("outputs.{k} should be true or false"))?;
                    }
                }
                if sets.is_some() || a.get("job").and_then(|v| v.as_bool()).unwrap_or(false) {
                    let p = &self.ws.pcbs[pi];
                    let files = crate::pcb_fab::run(p, &p.outputs, std::path::Path::new(path))?;
                    json!({"pcb": pi, "job": p.outputs, "written": files})
                } else {
                    let files = crate::pcb_export::export(&self.ws.pcbs[pi], std::path::Path::new(path))?;
                    json!({"pcb": pi, "written": files})
                }
            }
            "pcb_library" => {
                let pi = self.pcb_idx(a)?;
                let mut im = crate::pcb_kicad::Imported::default();
                if let Some(path) = a.get("import").and_then(|v| v.as_str()) {
                    im = crate::pcb_kicad::import_path(std::path::Path::new(path))?;
                }
                for g in a.get("generate").and_then(|v| v.as_array()).into_iter().flatten() {
                    let spec: crate::pcb_ipc::IpcSpec = serde_json::from_value(g.clone()).map_err(|e| format!("generate: {e}"))?;
                    im.footprints.push(crate::pcb_ipc::generate(&spec)?);
                }
                let fps: Vec<Value> = im.footprints.iter().map(|f| json!({"name": f.name, "pads": f.pads.len()})).collect();
                let syms: Vec<Value> = im.symbols.iter().map(|s| json!({"symbol": s.key, "pins": s.pins.len()})).collect();
                let p = &mut self.ws.pcbs[pi];
                let notes = crate::pcb_kicad::add_to(p, im);
                let syms: Vec<Value> = syms.into_iter().map(|mut v| {
                    let key = v["symbol"].as_str().unwrap_or("").to_string();
                    if let Some(s) = p.symbols.iter().find(|s| s.key == key) {
                        v["footprint"] = json!(s.footprint);
                        v["reference"] = json!(s.prefix);
                    }
                    v
                }).collect();
                json!({"pcb": pi, "footprints": fps, "symbols": syms, "notes": notes,
                    "library": {"footprints": p.footprints.iter().map(|f| f.name.clone()).collect::<Vec<_>>(), "symbols": p.symbols.iter().map(|s| s.key.clone()).collect::<Vec<_>>()},
                    "hint": "add parts with pcb_add_parts using symbol: \"Library:Name\" for imported symbols, and footprint: \"<name>\" for imported or generated footprints"})
            }
            "pcb_traces" => self.pcb_traces(a)?,
            "pcb_3d" => {
                let pi = self.pcb_idx(a)?;
                self.sync();
                let si = crate::pcb_export::to_studio(&mut self.ws, pi)?;
                // the board's Part Studio becomes the active one
                self.cur = si;
                self.doc = self.ws.studios[si].doc.clone();
                self.rebuild()?;
                let r = Some(&self.result);
                let errors: Vec<Value> = r.into_iter().flat_map(|r| r.features.iter()).filter_map(|f| f.error.clone().map(|e| json!({"feature": f.id, "error": e}))).collect();
                json!({"studio": si, "name": self.ws.studios[si].name, "parts": r.map(|r| r.bodies.len()).unwrap_or(0), "errors": errors, "hint": "it is now the active Part Studio: render it, export it as STEP, or insert its parts into an assembly"})
            }
            "digikey_search" => {
                let q = a.get("query").and_then(|v| v.as_str()).ok_or("give query: a part number or keywords")?;
                let set = crate::digikey::load_settings();
                let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                let hits: Vec<crate::digikey::Product> = if let Some(path) = a.get("csv").and_then(|v| v.as_str()) {
                    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
                    let ql = q.to_ascii_lowercase();
                    crate::digikey::parse_csv(&text)?.into_iter().filter(|p| ql.is_empty() || ql == "*" || format!("{} {} {} {}", p.digikey, p.mpn, p.manufacturer, p.description).to_ascii_lowercase().contains(&ql)).take(limit).collect()
                } else {
                    if !set.has_creds() {
                        return Err(format!("no Digi-Key API keys are set up (PCB tab → Digi-Key → Settings, or DIGIKEY_CLIENT_ID/DIGIKEY_CLIENT_SECRET). Without keys, give csv: a table downloaded from {}", crate::digikey::web_search_url(q)));
                    }
                    crate::digikey::search(&set, q, limit)?
                };
                json!({"products": hits.iter().take(limit).map(crate::digikey::product_json).collect::<Vec<_>>()})
            }
            "pcb_digikey" => {
                let pi = self.pcb_idx(a)?;
                let pn = a.get("part_number").and_then(|v| v.as_str()).ok_or("give part_number: a Digi-Key or manufacturer part number, or a digikey.com product link")?;
                let product = if let Some(path) = a.get("csv").and_then(|v| v.as_str()) {
                    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
                    let want = crate::digikey::part_number_of(pn);
                    crate::digikey::parse_csv(&text)?.into_iter().find(|p| p.digikey.eq_ignore_ascii_case(&want) || p.mpn.eq_ignore_ascii_case(&want)).ok_or_else(|| format!("{want} is not in {path}"))?
                } else {
                    let set = crate::digikey::load_settings();
                    if !set.has_creds() {
                        return Err("no Digi-Key API keys are set up: give csv (a table downloaded from digikey.com), or add keys under PCB tab → Digi-Key → Settings".into());
                    }
                    crate::digikey::details(&set, pn)?
                };
                let mut p = self.ws.pcbs[pi].clone();
                let (i, changed) = match a.get("reference").and_then(|v| v.as_str()) {
                    Some(r) => {
                        let i = p.part_index(r).ok_or_else(|| format!("no part {r}"))?;
                        let retype = a.get("set_value").and_then(|v| v.as_bool()).unwrap_or(true);
                        (i, crate::digikey::apply(&mut p.parts[i], &product, retype))
                    }
                    None => {
                        let sym = a.get("symbol").and_then(|v| v.as_str()).unwrap_or(crate::digikey::symbol_for(&product));
                        let sch = a.get("sch").and_then(pt).unwrap_or_else(|| free_sch_spot(&p));
                        let i = p.add_part(sym, None, None, sch)?;
                        let changed = crate::digikey::apply(&mut p.parts[i], &product, true);
                        let part = &mut p.parts[i];
                        part.pin_names = pcb_lib::default_pin_names(&part.symbol, &part.value);
                        let at = p.free_spot(&p.parts[i]);
                        p.parts[i].at = at;
                        (i, changed)
                    }
                };
                let nl = p.netlist();
                p.retag_copper(&nl);
                self.ws.pcbs[pi] = p;
                let x = &self.ws.pcbs[pi].parts[i];
                json!({"pcb": pi, "reference": x.reference, "symbol": x.symbol, "value": x.value, "footprint": x.footprint, "mpn": x.mpn, "manufacturer": x.manufacturer, "digikey": x.digikey, "datasheet": x.datasheet, "changed": changed, "pins": pins_json(&self.ws.pcbs[pi], i)})
            }
            _ => return self.circuit_call(name, a),
        };
        Ok((v, None))
    }
}

/// A part's simulation model and its parameters (with any overrides).
fn sim_json(x: &pcb::Part, setup: &crate::circuit::SimSetup) -> Value {
    match crate::circuit::model_params(x, setup) {
        Ok((model, params)) => json!({
            "model": model, "excluded": x.no_sim,
            "params": params.iter().map(|(k, label, unit, v)| json!({"key": k, "label": label, "unit": unit, "default": v, "override": x.sim.get(*k)})).collect::<Vec<_>>(),
        }),
        Err(e) => json!({"error": e, "excluded": x.no_sim}),
    }
}

fn pins_json(p: &Pcb, i: usize) -> Value {
    let part = &p.parts[i];
    let nums = part.footprint().pin_numbers();
    let sym = part.symbol();
    json!(nums.iter().map(|n| {
        let name = sym.pins.iter().find(|x| &x.num == n).map(|x| x.name.clone()).unwrap_or_default();
        if name.is_empty() { n.clone() } else { format!("{n} {name}") }
    }).collect::<Vec<_>>())
}

fn edit_part(p: &mut Pcb, i: usize, x: &Value) -> R<()> {
    let part = &mut p.parts[i];
    if let Some(v) = x.get("value").and_then(|v| v.as_str()) {
        part.value = v.to_string();
    }
    if let Some(f) = x.get("footprint").and_then(|v| v.as_str()) {
        if pcb_lib::footprint(f).is_none() {
            return Err(format!("no footprint {f}; e.g. {}", pcb_lib::footprint_names().join(", ")));
        }
        part.footprint = f.to_string();
    }
    if let Some(sym) = x.get("symbol").and_then(|v| v.as_str()) {
        if sym != part.symbol {
            let k = pcb_lib::kind_info(sym).ok_or_else(|| format!("no symbol {sym}; the symbols are {}", pcb_lib::symbol_keys()))?;
            part.symbol = k.key;
        }
    }
    for (key, field) in [
        ("mpn", &mut part.mpn),
        ("manufacturer", &mut part.manufacturer),
        ("description", &mut part.description),
        ("datasheet", &mut part.datasheet),
        ("digikey", &mut part.digikey),
        ("package", &mut part.package),
    ] {
        if let Some(m) = x.get(key).and_then(|v| v.as_str()) {
            *field = m.trim().to_string();
        }
    }
    if let Some(f) = x.get("fields").and_then(|v| v.as_object()) {
        for (k, v) in f {
            let v = match v {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                o => o.to_string(),
            };
            match part.fields.iter().position(|(n, _)| n.eq_ignore_ascii_case(k)) {
                Some(i) if v.is_empty() => {
                    part.fields.remove(i);
                }
                Some(i) => part.fields[i].1 = v,
                None if v.is_empty() => {}
                None => part.fields.push((k.clone(), v)),
            }
        }
    }
    if let Some(f) = x.get("sim").and_then(|v| v.as_object()) {
        for (k, v) in f {
            let v = match v {
                Value::String(s) => s.trim().to_string(),
                Value::Null => String::new(),
                o => o.to_string(),
            };
            if v.is_empty() {
                part.sim.remove(k);
            } else {
                part.sim.insert(k.to_ascii_lowercase(), v);
            }
        }
    }
    if let Some(b) = x.get("no_sim").and_then(|v| v.as_bool()) {
        part.no_sim = b;
    }
    if let Some(b) = x.get("sch_mirror").and_then(|v| v.as_bool()) {
        part.sch_mirror = b;
    }
    if let Some(n) = x.get("pin_names").and_then(|v| v.as_array()) {
        part.pin_names = n.iter().map(|s| s.as_str().unwrap_or("").to_string()).collect();
    }
    if let Some(at) = x.get("at").and_then(pt) {
        part.at = at;
    }
    if let Some(r) = x.get("rot").and_then(|v| v.as_f64()) {
        part.rot = r;
    }
    match x.get("side").and_then(|v| v.as_str()) {
        None => {}
        Some("top") => part.bottom = false,
        Some("bottom") => part.bottom = true,
        Some(o) => return Err(format!("side is top or bottom, not {o}")),
    }
    if let Some(s) = x.get("sch").and_then(pt) {
        part.sch = pcb::snap(s, GRID);
    }
    if let Some(r) = x.get("sch_rot").and_then(|v| v.as_u64()) {
        part.sch_rot = (r % 4) as u8;
    }
    Ok(())
}

/// A tab name on one line, without control characters.
fn clean_name(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_string()
}

fn apply_board(p: &mut Pcb, a: &Value) -> R<()> {
    let b = &mut p.board;
    if let Some(o) = a.get("outline") {
        let v = pts(o, "outline")?;
        if v.len() < 3 {
            return Err("outline: three points or more".into());
        }
        b.outline = pcb::ccw(v);
    } else if a.get("width").is_some() || a.get("height").is_some() || a.get("corner_radius").is_some() {
        let size = b.size();
        let w = a.get("width").and_then(|v| v.as_f64()).unwrap_or(size[0]);
        let h = a.get("height").and_then(|v| v.as_f64()).unwrap_or(size[1]);
        if w <= 0.0 || h <= 0.0 {
            return Err("width and height must be positive".into());
        }
        b.outline = pcb::rect_outline(w, h, a.get("corner_radius").and_then(|v| v.as_f64()).unwrap_or(0.0));
    }
    if let Some(t) = a.get("thickness").and_then(|v| v.as_f64()) {
        if !(0.2..=5.0).contains(&t) {
            return Err(format!("thickness: {t} mm; boards are 0.2 to 5 mm thick (1.6 is usual)"));
        }
        b.thickness = t;
    }
    b.stackup = crate::pcb_trace::stack(b);
    if let Some(oz) = a.get("copper_oz").and_then(|v| v.as_f64()) {
        if !(0.25..=6.0).contains(&oz) {
            return Err("copper_oz: 0.5, 1, 2 or 3 (oz/ft²)".into());
        }
        b.stackup.set_copper(Some(oz), Some(oz));
        b.stackup.set_total(b.thickness);
    }
    if let Some(er) = a.get("er").and_then(|v| v.as_f64()) {
        if !(1.0..=15.0).contains(&er) {
            return Err("er: the dielectric's relative permittivity, 1 to 15 (FR4 about 4.5)".into());
        }
        for l in b.stackup.layers.iter_mut().filter(|l| matches!(l.kind, crate::pcb_trace::StackKind::Core | crate::pcb_trace::StackKind::Prepreg)) {
            l.er = er;
        }
    }
    if let Some(c) = a.get("color").and_then(|v| v.as_str()) {
        b.color = if c == "green" { String::new() } else { c.to_string() };
    }
    if let Some(r) = a.get("rules") {
        let mut cur = serde_json::to_value(&b.rules).unwrap_or_default();
        for (k, v) in r.as_object().ok_or("rules: an object")? {
            if cur.get(k).is_none() {
                return Err(format!("rules has no {k}; the rules are {}", cur.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>().join(", ")).unwrap_or_default()));
            }
            cur[k] = v.clone();
        }
        b.rules = serde_json::from_value(cur).map_err(|e| format!("rules: {e}"))?;
    }
    Ok(())
}

pub fn tools() -> Vec<Value> {
    let obj = |props: Value, req: &[&str]| json!({"type": "object", "properties": props, "required": req});
    let pcb = json!({"type": "integer", "minimum": 0, "description": "PCB tab index (from create_pcb)"});
    let xy = |d: &str| json!({"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2, "description": d});
    let pts = |d: &str| json!({"type": "array", "items": xy("[x, y] mm"), "description": d});
    let num = |d: &str| json!({"type": "number", "description": d});
    let layer = json!({"type": "string", "enum": ["top", "bottom"], "description": "copper layer"});
    let board = json!({
        "name": {"type": "string", "description": "tab name"},
        "width": {"type": "number", "description": "rectangular board width, mm (from the origin)"},
        "height": {"type": "number", "description": "rectangular board height, mm"},
        "corner_radius": {"type": "number", "description": "rectangular board corner radius, mm"},
        "outline": pts("board outline [[x, y], …] mm, instead of width/height"),
        "thickness": {"type": "number", "description": "board thickness mm (default 1.6)"},
        "color": {"type": "string", "enum": ["green", "black", "blue", "red", "white", "purple"], "description": "solder mask colour"},
        "copper_oz": {"type": "number", "description": "copper weight of every layer, oz/ft² (default 1; 2 for high current)"},
        "er": {"type": "number", "description": "dielectric relative permittivity for impedance (default 4.5, FR4)"},
        "rules": {"type": "object", "properties": {
            "clearance": num("copper to copper (0.2)"),
            "track": num("signal track width (0.25)"),
            "power_track": num("track width for nets named by a power/ground port (0.5)"),
            "via_d": num("via diameter (0.6)"),
            "via_drill": num("via drill (0.3)"),
            "edge": num("copper to board edge (0.3)"),
            "min_track": num("narrowest allowed track (0.15)"),
            "min_drill": num("smallest allowed drill (0.3)"),
            "min_annular": num("smallest allowed annular ring (0.13)"),
            "power_nets": {"type": "array", "items": {"type": "string"}, "description": "more nets that get the power track width, by name (M1_* matches a prefix): motor phases and other high-current outputs"},
            "classes": {"type": "array", "items": {"type": "object", "properties": {
                "name": {"type": "string", "description": "class name, e.g. Power, HV, USB"},
                "nets": {"type": "array", "items": {"type": "string"}, "description": "net names (M1_* matches a prefix)"},
                "clearance": num("clearance to other nets, 0 = board rule"),
                "track": num("track width, 0 = board rule"),
                "via_d": num("via diameter, 0 = board rule"),
                "via_drill": num("via drill, 0 = board rule"),
                "current": num("current the nets carry, A (0 = none)"),
                "impedance": num("target impedance, ohm (0 = none)"),
            }, "required": ["name", "nets"], "additionalProperties": false}, "description": "net classes (replaces the list): the first class naming a net sets its clearance, width and via for routing and the check"},
            "hole_gap": num("hole edge to hole edge (0.25)"),
            "silk_gap": num("silkscreen to exposed pad copper (0)"),
            "max_height": num("tallest part allowed above/below the board, 0 = no limit"),
            "body_gap": num("gap kept between parts' 3D bodies (0)"),
            "pairs": {"type": "array", "items": {"type": "object", "properties": {
                "name": {"type": "string", "description": "pair name, e.g. USB"},
                "p": {"type": "string", "description": "positive net"},
                "n": {"type": "string", "description": "negative net"},
                "width": num("track width, 0 = the net's"),
                "gap": num("gap between the tracks, 0 = the clearance"),
            }, "required": ["name", "p", "n"], "additionalProperties": false}, "description": "differential pairs (replaces the list); pcb_route pairs routes them side by side"},
            "lengths": {"type": "array", "items": {"type": "object", "properties": {
                "name": {"type": "string", "description": "rule name"},
                "nets": {"type": "array", "items": {"type": "string"}, "description": "nets (M1_* matches a prefix)"},
                "min": num("shortest routed length, mm (0 = none)"),
                "max": num("longest routed length, mm (0 = none)"),
                "tolerance": num("matched length: every net within this of the longest, mm (0 = not matched)"),
            }, "required": ["name", "nets"], "additionalProperties": false}, "description": "length and matched-length rules (replaces the list), checked by check_pcb; pcb_route match tunes a group"},
            "temp_rise": num("allowed track temperature rise at its net class's current, °C (10)"),
            "off": {"type": "array", "items": {"type": "string", "enum": pcb::CHECKS.iter().map(|c| c.0).collect::<Vec<_>>()}, "description": "checks to turn off"},
        }, "additionalProperties": false, "description": "design rules, mm; only the ones given change"},
    });
    let part_fields = |extra: Value| {
        let mut m = json!({
            "value": {"type": "string", "description": "value, e.g. 10k, 100n, NE555"},
            "footprint": {"type": "string", "description": "footprint: R_0402/0603/0805/1206 (also C_, L_, LED_), R_Axial, CP_D5, CP_D8, LED_D5, D_SOD-123, D_DO-41, SOT-23, SOT-23-5, SOT-223, TO-92, TO-220, SOIC-8/14/16, TSSOP-8/14/16/20, DIP-4…64, PinHeader_1xNN / 2xNN (2.54 mm), Terminal_2P/3P (5.08 mm), SW_6mm, Crystal_HC49; the pins follow the footprint's pads"},
            "mpn": {"type": "string", "description": "manufacturer part number (or an LCSC number like C25804) for the BOM"},
            "manufacturer": {"type": "string", "description": "manufacturer, for the BOM"},
            "description": {"type": "string", "description": "description, for the BOM"},
            "datasheet": {"type": "string", "description": "datasheet link"},
            "digikey": {"type": "string", "description": "Digi-Key part number, for the BOM"},
            "package": {"type": "string", "description": "supplier package name (informational; the footprint sets the pads)"},
            "fields": {"type": "object", "additionalProperties": {"type": "string", "description": "value; empty removes the field"}, "description": "extra BOM fields by name: {\"Tolerance\": \"1%\", \"LCSC\": \"C17414\"}"},
            "sim": {"type": "object", "additionalProperties": {"type": "string", "description": "value with SI prefix (300, 2.5m, 10p); empty resets it"}, "description": "simulation model parameter overrides by key; pcb_info lists each part's model parameters and keys (e.g. a BJT's bf, a diode's is/n/bv/rs/cj, an op amp's gbw/sr, a regulator's vset)"},
            "no_sim": {"type": "boolean", "description": "leave the part out of the circuit simulation"},
            "sch_mirror": {"type": "boolean", "description": "mirror the schematic symbol"},
            "pin_names": {"type": "array", "items": {"type": "string"}, "description": "ICs and connectors: pin names in pin order (pins can then be given as U1.VCC)"},
            "at": xy("board position of the footprint origin (its centre), mm"),
            "rot": {"type": "number", "description": "board rotation, degrees anticlockwise"},
            "side": {"type": "string", "enum": ["top", "bottom"], "description": "board side"},
            "sch": xy("schematic position, mm (2.54 grid; default laid out in columns)"),
            "sch_rot": {"type": "integer", "description": "schematic quarter turns"},
        });
        for (k, v) in extra.as_object().unwrap() {
            m[k] = v.clone();
        }
        m
    };
    let job_props: serde_json::Map<String, Value> = crate::pcb_fab::OutJob::NAMES.iter().map(|(k, d)| (k.to_string(), json!({"type": "boolean", "description": d}))).collect();
    let mut edit = board.clone();
    edit["pcb"] = pcb.clone();
    edit["parts"] = json!({"type": "array", "items": {"type": "object", "properties": part_fields(json!({"reference": {"type": "string", "description": "which part (R1, U2, …)"}, "new_reference": {"type": "string", "description": "rename it"}, "symbol": {"type": "string", "description": "change the library symbol (part type)"}, "delete": {"type": "boolean", "description": "remove the part"}})), "required": ["reference"], "additionalProperties": false}, "description": "change parts: [{reference, at, rot, side, value, footprint, …}]"});
    edit["wires"] = json!({"type": "array", "items": pts("polyline"), "description": "schematic wires to add, each a polyline [[x, y], …] (pins connect where a wire passes through their end point)"});
    edit["labels"] = json!({"type": "array", "items": {"type": "object", "properties": {"at": xy("label position, mm"), "text": {"type": "string", "description": "net name"}, "kind": {"type": "string", "enum": ["net", "power", "ground"], "description": "default net"}}, "required": ["at", "text"], "additionalProperties": false}, "description": "schematic labels to add [{at: [x, y], text, kind: net | power | ground}]; labels with the same text are one net"});
    edit["holes"] = json!({"type": "array", "items": {"type": "object", "properties": {"at": xy("hole centre, mm"), "d": num("diameter mm (default 3.2)")}, "required": ["at"], "additionalProperties": false}, "description": "replace the mounting holes: [{at: [x, y], d (default 3.2)}]"});
    edit["zones"] = json!({"type": "array", "items": {"type": "object", "properties": {"net": {"type": "string", "description": "net name"}, "layer": layer.clone(), "outline": pts("pour outline, default the whole board"), "pads": {"type": "string", "enum": ["tht_relief", "relief", "solid"], "description": "how the pour joins its net's pads: thermal reliefs on through-hole pads (default), on all pads, or solid"}, "clearance": num("gap to other nets, 0 = the net's clearance"), "relief_gap": num("thermal relief gap, 0 = 0.3 mm"), "spoke": num("thermal spoke width, 0 = the net's track")}, "required": ["net"], "additionalProperties": false}, "description": "replace the copper pours: [{net, layer: top | bottom (default bottom), outline (default the whole board), pads}]; a GND pour on the bottom is a good default"});
    edit["keepouts"] = json!({"type": "array", "items": {"type": "object", "properties": {"outline": pts("keepout outline"), "layer": layer.clone(), "tracks": {"type": "boolean", "description": "no tracks (default true)"}, "vias": {"type": "boolean", "description": "no vias (default true)"}, "pours": {"type": "boolean", "description": "no pour copper (default true)"}, "parts": {"type": "boolean", "description": "no parts (default false)"}}, "required": ["outline"], "additionalProperties": false}, "description": "replace the keepout areas (antenna clearance, mounting hardware, enclosure ribs); layer omitted = both"});
    edit["no_connects"] = json!({"type": "array", "items": {"type": "string", "description": "a pin, REF.PIN"}, "description": "replace the schematic no-connect marks: pins left open on purpose, so the electrical rule check doesn't flag them"});
    edit["pin_types"] = json!({"type": "object", "additionalProperties": {"type": "string", "enum": ["passive", "input", "output", "bidir", "power_in", "power_out", "open_collector", "no_connect", "auto"]}, "description": "set pins' electrical types for the rule check: {\"U1.3\": \"output\"}; auto goes back to the library's"});
    edit["clear_routing"] = json!({"type": "boolean", "description": "delete every track and via first"});
    edit["auto_place"] = json!({"type": "string", "enum": ["unplaced", "all"], "description": "lay parts out on the board in rows, biggest first: unplaced = only parts not on the board yet"});
    let create = board;
    vec![
        json!({"name": "create_pcb", "description": "Add a PCB tab: a schematic and a two-layer board (default 50 × 40 mm from the origin). Workflow: pcb_add_parts, pcb_connect (nets by pin), pcb_edit auto_place (or parts with at/rot), pcb_edit zones (GND pour), pcb_route (autoroute), check_pcb, render_pcb, export_pcb (Gerber ZIP), pcb_3d (Part Studio for assemblies and STEP). Units mm, y up.", "inputSchema": obj(create, &[])}),
        json!({"name": "pcb_add_parts", "description": format!("Add parts from the library. symbol: {}. Each part gets the next reference for its prefix (R1, C2, U1) unless reference is given, its library footprint and value unless given, and a spot beside the board until placed. Returns each part's reference and pins.", pcb_lib::SYMBOLS.iter().map(|s| format!("{} ({}, {})", s.key, s.name, s.footprint)).collect::<Vec<_>>().join(", ")), "inputSchema": obj(json!({"pcb": pcb, "parts": {"type": "array", "description": "parts to add", "items": {"type": "object", "properties": part_fields(json!({"symbol": {"type": "string", "description": "library symbol key"}, "reference": {"type": "string", "description": "reference designator, default the next free one"}})), "required": ["symbol"], "additionalProperties": false}}}), &["pcb", "parts"])}),
        json!({"name": "pcb_connect", "description": "Connect pins into nets on the schematic: {\"nets\": {\"VCC\": [\"U1.8\", \"R1.1\"], \"GND\": [\"U1.1\", \"C1.2\"]}}. Pins are REF.PIN by number or by pin name. Each pin gets a short wire and a label (ground and supply ports for names like GND, VCC, +5V, 3V3); nets named by a port get the wider power track width. Returns the netlist.", "inputSchema": obj(json!({"pcb": pcb, "nets": {"type": "object", "additionalProperties": {"type": "array", "items": {"type": "string", "description": "REF.PIN"}, "description": "pins"}, "description": "net name → list of pins"}}), &["pcb", "nets"])}),
        json!({"name": "pcb_edit", "description": "Change a PCB tab: board size/outline/thickness/colour/rules, parts (place them with at/rot/side, change value or footprint, delete), schematic wires and labels, mounting holes, copper pours (zones), clear_routing, auto_place.", "inputSchema": obj(edit, &["pcb"])}),
        json!({"name": "pcb_route", "description": "Route the board. With no tracks: autoroute every unrouted connection (net: only that one), shortest first on both layers with vias, keeping the clearance; reports what failed. tracks: [{points: [[x, y], …], layer: top | bottom, width, net (default the net under the first point)}] and vias: [{at, net}] add copper by hand (auto: true also autoroutes the rest). pairs routes differential pairs together; tune and match lengthen nets with meanders. Returns the unrouted count.", "inputSchema": obj(json!({"pcb": pcb, "auto": {"type": "boolean", "description": "autoroute (default when no tracks or vias are given)"}, "net": {"type": "string", "description": "autoroute only this net"}, "tracks": {"type": "array", "items": {"type": "object", "properties": {"points": pts("track path"), "layer": layer.clone(), "width": num("width mm (default from the rules)"), "net": {"type": "string", "description": "net (default the one under the first point)"}}, "required": ["points"], "additionalProperties": false}, "description": "tracks to add"}, "vias": {"type": "array", "items": {"type": "object", "properties": {"at": xy("via centre, mm"), "net": {"type": "string", "description": "net (default the one under it)"}}, "required": ["at"], "additionalProperties": false}, "description": "vias to add"}, "pairs": {"type": "array", "items": {"type": "string", "description": "pair name or all"}, "description": "route these differential pairs (rules.pairs) side by side at their gap, before any autorouting"}, "tune": {"type": "array", "items": {"type": "object", "properties": {"net": {"type": "string", "description": "net"}, "length": num("target routed length, mm")}, "required": ["net", "length"], "additionalProperties": false}, "description": "lengthen nets to a target with meanders on their straight tracks"}, "match": {"type": "string", "description": "tune every net of this length rule up to its longest"}}), &["pcb"])}),
        json!({"name": "pcb_info", "description": "A PCB tab's parts (board position, rotation, side, courtyard, schematic position, every pad with its board position and net), nets with their pins, the outline, rules and up to 100 airwires (unrouted connections).", "inputSchema": obj(json!({"pcb": pcb}), &["pcb"])}),
        json!({"name": "check_pcb", "description": "Design rule check of the board (issues): clearances per net class and shorts, track widths, via drills and rings, hole spacing, copper near or off the board edge, keepouts, silkscreen on pads, overlapping courtyards, colliding 3D bodies, part heights, dangling tracks, starved thermals, unrouted connections. And the electrical rule check of the schematic (erc): unconnected pins, one-pin nets, outputs driving each other, power pins with no supply, undriven inputs, no-connect marks on wired pins, dangling labels and wires, duplicate references, unknown footprints. Each issue has its rule id (turn checks off with pcb_edit rules.off). ok:true when there are no errors.", "inputSchema": obj(json!({"pcb": pcb}), &["pcb"])}),
        json!({"name": "render_pcb", "description": "PNG of the board (copper of both layers, pours, pads, silkscreen, outline and airwires; layer: the one drawn on top) or of the schematic.", "inputSchema": obj(json!({"pcb": pcb, "view": {"type": "string", "enum": ["board", "schematic"], "description": "default board"}, "layer": {"type": "string", "enum": ["top", "bottom"], "description": "board layer drawn on top"}, "width": {"type": "integer", "description": "image width px (default 1000)"}, "path": {"type": "string", "description": "also save the PNG here"}}), &["pcb"])}),
        json!({"name": "export_pcb", "description": "Write the fabrication files: path ending .zip gets a ZIP of Gerbers (copper, mask, paste, silkscreen, edge cuts) and Excellon drill files as board makers such as JLCPCB take it, plus <name>-BOM.csv and <name>-CPL.csv (pick and place) beside it; any other path is a folder that gets every file. job: true (or outputs) runs the PCB's output job instead: every file in one .zip or folder, in gerbers/ (with a Gerber X2 job file and a drill drawing), assembly/ (BOM, pick and place), test/ (IPC-D-356A netlist), odb/ (ODB++) and a PDF of the schematic, assembly drawings and drill drawing.", "inputSchema": obj(json!({"pcb": pcb, "path": {"type": "string", "description": "output .zip or folder"}, "job": {"type": "boolean", "description": "run the output job (all outputs unless outputs turned some off)"}, "outputs": {"type": "object", "description": "turn job outputs on or off (kept with the PCB) and run the job", "properties": job_props}}), &["pcb", "path"])}),
        json!({"name": "digikey_search", "description": "Search Digi-Key for parts (needs the user's Digi-Key API client ID and secret in the PCB tab's Digi-Key settings), or search a CSV downloaded from digikey.com (search table, cart or list) with csv. Returns products with Digi-Key number, MPN, manufacturer, description, datasheet, package, stock, price and parameters, plus the symbol, value and footprint they would get.", "inputSchema": obj(json!({"query": {"type": "string", "description": "part number or keywords (\"10k 0805 1%\"); with csv, text to filter rows by (* for all)"}, "limit": {"type": "integer", "description": "most results (default 10)"}, "csv": {"type": "string", "description": "path of a CSV downloaded from digikey.com, instead of the API"}}), &["query"])}),
        json!({"name": "pcb_digikey", "description": "Use a Digi-Key part on a PCB: fill part reference's MPN, manufacturer, description, datasheet, Digi-Key number, package and BOM fields (and its value and footprint unless set_value is false), or with no reference add a new part from it (symbol, value and footprint from the product). Uses the API, or a CSV from digikey.com with csv.", "inputSchema": obj(json!({"pcb": pcb, "part_number": {"type": "string", "description": "Digi-Key or manufacturer part number, or a digikey.com product link"}, "reference": {"type": "string", "description": "the part to fill (default: add a new part)"}, "set_value": {"type": "boolean", "description": "also set the value and footprint from the product (default true)"}, "symbol": {"type": "string", "description": "new part: symbol key (default from the product's category)"}, "sch": xy("new part: schematic position"), "csv": {"type": "string", "description": "path of a CSV downloaded from digikey.com, instead of the API"}}), &["pcb", "part_number"])}),
        json!({"name": "pcb_library", "description": "The PCB's own part library. import: a KiCad footprint (.kicad_mod), symbol library (.kicad_sym), a folder (.pretty, .kicad_symdir or a download with both) or a ZIP (SnapEDA, Ultra Librarian, SamacSys and similar give KiCad ZIPs). Symbols become \"Library:Name\" keys with their pins, drawing and electrical pin types (used by the ERC); a symbol's footprint is linked when it was imported too. generate: IPC-7351B land patterns from datasheet dimensions (mm, nominal), named the IPC way (RESC2012X50N, SOIC127P600X175-8N, QFN50P400X400X90-17N): chip {family: chip, size: 0805, kind: R|C|L|D|LED} or {body: [L, W], lead, height}; sop/qfp {pins, pitch, body, span: [toe-to-toe x, y], lead (foot length), lead_w, height, ep}; son/qfn {pins, pitch, body, lead, lead_w, height, ep: [w, h] exposed pad}; bga {pitch, ball, rows, cols, height}. density M, N (default) or L.", "inputSchema": obj(json!({"pcb": pcb, "import": {"type": "string", "description": "KiCad file, folder or ZIP to import"}, "generate": {"type": "array", "description": "IPC-7351 footprints to generate", "items": {"type": "object", "properties": {
            "family": {"type": "string", "enum": ["chip", "sop", "qfp", "son", "qfn", "bga"], "description": "package family"},
            "kind": {"type": "string", "description": "chip: R, C, L, D, LED or F"},
            "size": {"type": "string", "description": "chip: EIA size 0201 0402 0603 0805 1206 1210 2010 2512"},
            "pins": {"type": "integer", "description": "leads, not counting the exposed pad"},
            "pitch": {"type": "number", "description": "lead or ball pitch"},
            "body": {"type": "array", "items": {"type": "number"}, "description": "[length x, width y]"},
            "span": {"type": "array", "items": {"type": "number"}, "description": "gull-wing toe-to-toe span [x, y]"},
            "lead": {"type": "number", "description": "terminal (foot) length"},
            "lead_w": {"type": "number", "description": "lead width"},
            "height": {"type": "number", "description": "seated height"},
            "ep": {"type": "array", "items": {"type": "number"}, "description": "exposed pad [w, h]"},
            "span_tol": {"type": "number", "description": "span tolerance range (default 0.2)"},
            "lead_tol": {"type": "number", "description": "foot length tolerance range (default 0.3)"},
            "width_tol": {"type": "number", "description": "lead width tolerance range (default 0.1)"},
            "ball": {"type": "number", "description": "BGA ball diameter"},
            "rows": {"type": "integer", "description": "BGA rows"},
            "cols": {"type": "integer", "description": "BGA columns"},
            "density": {"type": "string", "enum": ["M", "N", "L"], "description": "IPC density level: M most, N nominal, L least"}
        }, "additionalProperties": false}}}), &["pcb"])}),
        json!({"name": "pcb_traces", "description": "Trace solver. pcb: every routed net (or net with a current/impedance target) solved: track length and widths, worst pad-to-pad resistance through tracks and vias and the voltage drop at the net's current, temperature rise (IPC-2221), the width the current needs at the allowed rise, impedance of its tracks (microstrip over the stackup) and the width an impedance target needs; plus the stackup and the current/impedance findings check_pcb also reports. calc: work out one track: current capacity, rise, resistance, drop, power, Z0, εeff, delay and differential impedance, and the widths or gap for z_target / zdiff_target (defaults from the board's top layer when pcb is given).", "inputSchema": obj(json!({"pcb": pcb, "calc": {"type": "object", "properties": {
            "width": num("track width mm"), "copper_oz": num("copper weight oz (1 = 35 µm)"), "layer": {"type": "string", "enum": ["outer", "inner"], "description": "for heating (default outer)"},
            "current": num("A"), "length": num("mm, for resistance and drop"), "temp_rise": num("allowed rise °C (10)"), "ambient": num("°C (25)"),
            "line": {"type": "string", "enum": ["microstrip", "stripline"], "description": "default microstrip"}, "h": num("dielectric to the plane mm (stripline: to each plane)"), "h2": num("stripline: to the other plane mm (0 centred)"),
            "er": num("relative permittivity (4.5)"), "gap": num("differential pair edge gap mm"), "z_target": num("solve the width for this single-ended Ω"), "zdiff_target": num("solve width (at gap) and gap (at width) for this differential Ω"),
        }, "additionalProperties": false, "description": "one track to work out"}}), &[])}),
        json!({"name": "pcb_3d", "description": "Make or update the Part Studio \"<pcb name> 3D\": the board (with its holes) and a simple body for every part, as separate parts named by reference and value, ready to insert into assemblies or export as STEP. Running it again updates the same parts. The Part Studio becomes the active one.", "inputSchema": obj(json!({"pcb": pcb}), &["pcb"])}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_values_are_checked() {
        let mut p = Pcb::new("t");
        assert!(apply_board(&mut p, &json!({"thickness": 0})).is_err());
        assert!(apply_board(&mut p, &json!({"thickness": -1.6})).is_err());
        assert!(apply_board(&mut p, &json!({"thickness": 1.0})).is_ok());
        assert_eq!(p.board.thickness, 1.0);
        assert_eq!(clean_name(" a\nb "), "a b");
    }

    #[test]
    fn connect_makes_the_nets() {
        let mut p = Pcb::new("t");
        p.add_part("U", Some("SOIC-8"), Some("NE555"), [0.0, 0.0]).unwrap();
        p.add_part("R", None, None, [25.4, 0.0]).unwrap();
        p.parts[0].pin_names = ["GND", "TRIG", "OUT", "RESET", "CTRL", "THR", "DIS", "VCC"].iter().map(|s| s.to_string()).collect();
        let pins: Vec<(usize, String)> = ["U1.VCC", "R1.1", "U1.4"].iter().map(|s| pin_ref(&p, s).unwrap()).collect();
        connect(&mut p, "VCC", &pins);
        let gnd = vec![pin_ref(&p, "U1.1").unwrap()];
        connect(&mut p, "GND", &gnd);
        let nl = p.netlist();
        let vcc = &nl.nets[nl.by_name("VCC").unwrap()];
        assert_eq!(vcc.pins.len(), 3);
        assert!(vcc.power);
        assert!(nl.by_name("GND").is_some());
        assert!(pin_ref(&p, "U1.9").is_err());
    }

    #[test]
    fn the_555_example_checks_clean() {
        let steps: Vec<Value> = serde_json::from_str(include_str!("../examples/pcb-555.json")).unwrap();
        let mut s = Session::new();
        // the board steps (the rest need the modelling kernel)
        for st in steps.iter().take_while(|st| st["tool"] != "check_pcb").filter(|st| st["tool"].as_str().is_some_and(|t| t.contains("pcb"))) {
            s.call(st["tool"].as_str().unwrap(), &st["args"]).unwrap();
        }
        let (v, _) = s.call("check_pcb", &json!({"pcb": 0})).unwrap();
        assert_eq!(v["ok"], true, "{v:#}");
        assert!(v["erc"].as_array().unwrap().iter().all(|i| i["error"] == false), "{v:#}");
        // net classes and pour settings go through pcb_edit
        let (v, _) = s.call("pcb_edit", &json!({"pcb": 0, "rules": {"classes": [{"name": "Power", "nets": ["VCC", "GND"], "track": 0.6}], "off": ["silk"]}, "zones": [{"net": "GND", "layer": "bottom", "pads": "relief"}], "keepouts": [{"outline": [[0, 0], [5, 0], [5, 5], [0, 5]], "layer": "top"}], "no_connects": [], "pin_types": {"U1.DIS": "open_collector"}})).unwrap();
        let _ = v;
        let p = &s.ws.pcbs[0];
        assert_eq!(p.track_width(&p.netlist(), "VCC"), 0.6);
        assert_eq!(p.board.zones[0].pads, pcb::PadConnect::Relief);
        assert_eq!(p.board.keepouts.len(), 1);
        assert_eq!(p.pin_types.get("U1.7"), Some(&crate::pcb_erc::PinType::OpenCollector));
    }
}
