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

    pub(super) fn pcb_call(&mut self, name: &str, a: &Value) -> R<(Value, Option<Vec<u8>>)> {
        let v = match name {
            "create_pcb" => {
                let n = self.ws.pcbs.len() + 1;
                let mut p = Pcb::new(a.get("name").and_then(|v| v.as_str()).map(String::from).unwrap_or(format!("PCB {n}")));
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
                    added.push(json!({"reference": p.parts[i].reference, "footprint": p.parts[i].footprint, "pins": pins_json(&p, i)}));
                }
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
                self.ws.pcbs[pi] = p;
                let nl = self.ws.pcbs[pi].netlist();
                json!({"pcb": pi, "nets": nl.nets.iter().map(|n| json!({"name": n.name, "pins": n.pins.iter().map(|(i, num)| format!("{}.{}", self.ws.pcbs[pi].parts[*i].reference, num)).collect::<Vec<_>>()})).collect::<Vec<_>>()})
            }
            "pcb_edit" => {
                let pi = self.pcb_idx(a)?;
                let mut p = self.ws.pcbs[pi].clone();
                if let Some(n) = a.get("name").and_then(|v| v.as_str()) {
                    p.name = n.to_string();
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
                            Ok(pcb::Hole { at, d: x.get("d").and_then(|v| v.as_f64()).unwrap_or(3.2) })
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
                            Ok(Zone { net, layer: layer(x.get("layer"))?.unwrap_or(Layer::Bottom), outline })
                        })
                        .collect::<R<_>>()?;
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
                    p.board.vias.push(Via { at, d: p.board.rules.via_d, drill: p.board.rules.via_drill, net });
                }
                let auto = a.get("auto").and_then(|v| v.as_bool()).unwrap_or(a.get("tracks").is_none() && a.get("vias").is_none());
                let mut v = json!({});
                if auto {
                    let o = crate::pcb_route::autoroute(&mut p, a.get("net").and_then(|v| v.as_str()));
                    v = json!({"routed": o.routed, "tracks_added": o.tracks, "vias_added": o.vias, "failed": o.failed});
                }
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
                            "pads": pads.iter().filter(|q| q.part == i).map(|q| json!({"pin": q.num, "at": r2(q.at), "net": q.net.map(|n| nl.nets[n].name.clone())})).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                let mut v = self.pcb_summary(pi);
                v["outline"] = json!(p.board.outline.iter().map(|q| r2(*q)).collect::<Vec<_>>());
                v["rules"] = serde_json::to_value(&p.board.rules).unwrap_or_default();
                v["parts"] = json!(parts);
                v["nets"] = json!(nl.nets.iter().map(|n| json!({"name": n.name, "pins": n.pins.iter().map(|(i, num)| format!("{}.{}", p.parts[*i].reference, num)).collect::<Vec<_>>()})).collect::<Vec<_>>());
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
                let errors = issues.iter().filter(|i| i.error).count();
                json!({"pcb": pi, "ok": errors == 0, "errors": errors, "warnings": issues.len() - errors, "issues": issues.iter().take(100).map(|i| json!({"at": r2(i.at), "message": i.message, "error": i.error})).collect::<Vec<_>>()})
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
                let files = crate::pcb_export::export(&self.ws.pcbs[pi], std::path::Path::new(path))?;
                json!({"pcb": pi, "written": files})
            }
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
            _ => return self.circuit_call(name, a),
        };
        Ok((v, None))
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
    if let Some(m) = x.get("mpn").and_then(|v| v.as_str()) {
        part.mpn = m.to_string();
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
        b.thickness = t;
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
        }, "additionalProperties": false, "description": "design rules, mm; only the ones given change"},
    });
    let part_fields = |extra: Value| {
        let mut m = json!({
            "value": {"type": "string", "description": "value, e.g. 10k, 100n, NE555"},
            "footprint": {"type": "string", "description": "footprint: R_0402/0603/0805/1206 (also C_, L_, LED_), R_Axial, CP_D5, CP_D8, LED_D5, D_SOD-123, D_DO-41, SOT-23, SOT-23-5, SOT-223, TO-92, TO-220, SOIC-8/14/16, TSSOP-8/14/16/20, DIP-4…64, PinHeader_1xNN / 2xNN (2.54 mm), Terminal_2P/3P (5.08 mm), SW_6mm, Crystal_HC49; the pins follow the footprint's pads"},
            "mpn": {"type": "string", "description": "manufacturer or LCSC part number for the BOM"},
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
    let mut edit = board.clone();
    edit["pcb"] = pcb.clone();
    edit["parts"] = json!({"type": "array", "items": {"type": "object", "properties": part_fields(json!({"reference": {"type": "string", "description": "which part (R1, U2, …)"}, "new_reference": {"type": "string", "description": "rename it"}, "delete": {"type": "boolean", "description": "remove the part"}})), "required": ["reference"], "additionalProperties": false}, "description": "change parts: [{reference, at, rot, side, value, footprint, …}]"});
    edit["wires"] = json!({"type": "array", "items": pts("polyline"), "description": "schematic wires to add, each a polyline [[x, y], …] (pins connect where a wire passes through their end point)"});
    edit["labels"] = json!({"type": "array", "items": {"type": "object", "properties": {"at": xy("label position, mm"), "text": {"type": "string", "description": "net name"}, "kind": {"type": "string", "enum": ["net", "power", "ground"], "description": "default net"}}, "required": ["at", "text"], "additionalProperties": false}, "description": "schematic labels to add [{at: [x, y], text, kind: net | power | ground}]; labels with the same text are one net"});
    edit["holes"] = json!({"type": "array", "items": {"type": "object", "properties": {"at": xy("hole centre, mm"), "d": num("diameter mm (default 3.2)")}, "required": ["at"], "additionalProperties": false}, "description": "replace the mounting holes: [{at: [x, y], d (default 3.2)}]"});
    edit["zones"] = json!({"type": "array", "items": {"type": "object", "properties": {"net": {"type": "string", "description": "net name"}, "layer": layer.clone(), "outline": pts("pour outline, default the whole board")}, "required": ["net"], "additionalProperties": false}, "description": "replace the copper pours: [{net, layer: top | bottom (default bottom), outline (default the whole board)}]; a GND pour on the bottom is a good default"});
    edit["clear_routing"] = json!({"type": "boolean", "description": "delete every track and via first"});
    edit["auto_place"] = json!({"type": "string", "enum": ["unplaced", "all"], "description": "lay parts out on the board in rows, biggest first: unplaced = only parts not on the board yet"});
    let create = board;
    vec![
        json!({"name": "create_pcb", "description": "Add a PCB tab: a schematic and a two-layer board (default 50 × 40 mm from the origin). Workflow: pcb_add_parts, pcb_connect (nets by pin), pcb_edit auto_place (or parts with at/rot), pcb_edit zones (GND pour), pcb_route (autoroute), check_pcb, render_pcb, export_pcb (Gerber ZIP), pcb_3d (Part Studio for assemblies and STEP). Units mm, y up.", "inputSchema": obj(create, &[])}),
        json!({"name": "pcb_add_parts", "description": format!("Add parts from the library. symbol: {}. Each part gets the next reference for its prefix (R1, C2, U1) unless reference is given, its library footprint and value unless given, and a spot beside the board until placed. Returns each part's reference and pins.", pcb_lib::SYMBOLS.iter().map(|s| format!("{} ({}, {})", s.key, s.name, s.footprint)).collect::<Vec<_>>().join(", ")), "inputSchema": obj(json!({"pcb": pcb, "parts": {"type": "array", "description": "parts to add", "items": {"type": "object", "properties": part_fields(json!({"symbol": {"type": "string", "description": "library symbol key"}, "reference": {"type": "string", "description": "reference designator, default the next free one"}})), "required": ["symbol"], "additionalProperties": false}}}), &["pcb", "parts"])}),
        json!({"name": "pcb_connect", "description": "Connect pins into nets on the schematic: {\"nets\": {\"VCC\": [\"U1.8\", \"R1.1\"], \"GND\": [\"U1.1\", \"C1.2\"]}}. Pins are REF.PIN by number or by pin name. Each pin gets a short wire and a label (ground and supply ports for names like GND, VCC, +5V, 3V3); nets named by a port get the wider power track width. Returns the netlist.", "inputSchema": obj(json!({"pcb": pcb, "nets": {"type": "object", "additionalProperties": {"type": "array", "items": {"type": "string", "description": "REF.PIN"}, "description": "pins"}, "description": "net name → list of pins"}}), &["pcb", "nets"])}),
        json!({"name": "pcb_edit", "description": "Change a PCB tab: board size/outline/thickness/colour/rules, parts (place them with at/rot/side, change value or footprint, delete), schematic wires and labels, mounting holes, copper pours (zones), clear_routing, auto_place.", "inputSchema": obj(edit, &["pcb"])}),
        json!({"name": "pcb_route", "description": "Route the board. With no tracks: autoroute every unrouted connection (net: only that one), shortest first on both layers with vias, keeping the clearance; reports what failed. tracks: [{points: [[x, y], …], layer: top | bottom, width, net (default the net under the first point)}] and vias: [{at, net}] add copper by hand (auto: true also autoroutes the rest). Returns the unrouted count.", "inputSchema": obj(json!({"pcb": pcb, "auto": {"type": "boolean", "description": "autoroute (default when no tracks or vias are given)"}, "net": {"type": "string", "description": "autoroute only this net"}, "tracks": {"type": "array", "items": {"type": "object", "properties": {"points": pts("track path"), "layer": layer.clone(), "width": num("width mm (default from the rules)"), "net": {"type": "string", "description": "net (default the one under the first point)"}}, "required": ["points"], "additionalProperties": false}, "description": "tracks to add"}, "vias": {"type": "array", "items": {"type": "object", "properties": {"at": xy("via centre, mm"), "net": {"type": "string", "description": "net (default the one under it)"}}, "required": ["at"], "additionalProperties": false}, "description": "vias to add"}}), &["pcb"])}),
        json!({"name": "pcb_info", "description": "A PCB tab's parts (board position, rotation, side, courtyard, schematic position, every pad with its board position and net), nets with their pins, the outline, rules and up to 100 airwires (unrouted connections).", "inputSchema": obj(json!({"pcb": pcb}), &["pcb"])}),
        json!({"name": "check_pcb", "description": "Design rule check: clearance violations and shorts between nets, track widths, via drills and rings, copper near or off the board edge, overlapping parts, parts off the board, unrouted connections. ok:true when there are no errors.", "inputSchema": obj(json!({"pcb": pcb}), &["pcb"])}),
        json!({"name": "render_pcb", "description": "PNG of the board (copper of both layers, pours, pads, silkscreen, outline and airwires; layer: the one drawn on top) or of the schematic.", "inputSchema": obj(json!({"pcb": pcb, "view": {"type": "string", "enum": ["board", "schematic"], "description": "default board"}, "layer": {"type": "string", "enum": ["top", "bottom"], "description": "board layer drawn on top"}, "width": {"type": "integer", "description": "image width px (default 1000)"}, "path": {"type": "string", "description": "also save the PNG here"}}), &["pcb"])}),
        json!({"name": "export_pcb", "description": "Write the fabrication files: path ending .zip gets a ZIP of Gerbers (copper, mask, paste, silkscreen, edge cuts) and Excellon drill files as board makers such as JLCPCB take it, plus <name>-BOM.csv and <name>-CPL.csv (pick and place) beside it; any other path is a folder that gets every file.", "inputSchema": obj(json!({"pcb": pcb, "path": {"type": "string", "description": "output .zip or folder"}}), &["pcb", "path"])}),
        json!({"name": "pcb_3d", "description": "Make or update the Part Studio \"<pcb name> 3D\": the board (with its holes) and a simple body for every part, as separate parts named by reference and value, ready to insert into assemblies or export as STEP. Running it again updates the same parts. The Part Studio becomes the active one.", "inputSchema": obj(json!({"pcb": pcb}), &["pcb"])}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
