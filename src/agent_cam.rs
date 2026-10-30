//! Agent tools for Manufacture (CAM) tabs: setups, tools, operations, simulation and G-code.

use glam::DVec3;
use serde_json::{Map, Value, json};

use super::{R, Session, round};
use crate::cam::{self, Cam, OpKind, Operation, Tool, ToolKind};
use crate::cam_paths::{self, Move, Part, Toolpath};
use crate::cam_sim::Sim;
use crate::cam_stock::StockMap;
use crate::drawing_ui::DocCtx;

/// Overlay JSON fields onto a serde value of `T`.
fn overlay<T: serde::Serialize + serde::de::DeserializeOwned>(base: &T, fields: &Map<String, Value>, skip: &[&str]) -> R<T> {
    let mut v = serde_json::to_value(base).map_err(|e| e.to_string())?;
    for (k, x) in fields {
        if skip.contains(&k.as_str()) {
            continue;
        }
        v[k] = x.clone();
    }
    serde_json::from_value(v).map_err(|e| format!("bad value: {e}"))
}

/// `axis` [x, y, z]: the part direction to point at the spindle (3+2), as orient top plus a tilt.
fn apply_axis(c: &mut Cam, a: &Value) -> R<()> {
    let Some(v) = a.get("axis") else { return Ok(()) };
    let v: [f64; 3] = serde_json::from_value(v.clone()).map_err(|_| "axis: [x, y, z]")?;
    if v.iter().map(|x| x * x).sum::<f64>() < 1e-12 {
        return Err("axis: [x, y, z], not zero".into());
    }
    c.orient = "top".into();
    c.tilt = cam::tilt_for_axis(v);
    Ok(())
}

fn tool_json(t: &Tool) -> Value {
    let mut v = json!({"id": t.id, "number": t.number, "name": t.name, "kind": t.kind.key(), "diameter": t.diameter, "description": t.describe(), "rpm": t.rpm, "feed": t.feed, "plunge_feed": t.plunge_feed, "stepdown": t.stepdown, "stepover": t.stepover, "flute_length": t.flute_length, "stickout": t.stickout, "coolant": t.coolant, "holder": if t.holder.len() >= 2 { json!(if t.holder_name.is_empty() { "custom" } else { t.holder_name.as_str() }) } else { json!(format!("Ø{} cylinder", crate::cam::fmt_mm(t.holder_diameter))) }});
    if t.kind == ToolKind::Tap {
        v["tapping_head"] = json!(t.tapping_head);
        if t.tapping_head {
            v["reverse_ratio"] = json!(t.reverse_ratio);
        }
    }
    v
}

/// `machine`: a preset name, or an object of fields (name, travel [x,y,z], max_feed, rapid_z, tool_changer).
fn set_machine_arg(c: &mut Cam, a: &Value) -> R<()> {
    match a.get("machine") {
        None => Ok(()),
        Some(Value::String(n)) => {
            if cam::set_machine(c, n) {
                Ok(())
            } else {
                Err(format!("unknown machine '{n}'; presets: {}", cam::MACHINES.iter().map(|m| m.0).collect::<Vec<_>>().join(", ")))
            }
        }
        Some(Value::Object(m)) => {
            c.machine = overlay(&c.machine, m, &[])?;
            Ok(())
        }
        Some(_) => Err("machine: a preset name or an object".into()),
    }
}

/// `save_machine: true`: keep the setup's machine settings as the user's machine, for new setups.
fn save_machine_arg(c: &Cam, a: &Value) -> R<Option<String>> {
    if a.get("save_machine").and_then(|v| v.as_bool()) != Some(true) {
        return Ok(None);
    }
    cam::save_machine(&cam::MachineSettings::of(c)).map(|p| Some(p.display().to_string())).map_err(|e| format!("could not save the machine: {e}"))
}

/// `holder`: a preset name, or an outline [[diameter, height], …] from the holder's bottom face up.
fn set_holder_arg(t: &mut Tool, a: &Value) -> R<()> {
    match a.get("holder") {
        None => Ok(()),
        Some(Value::String(n)) => {
            if cam::set_holder(t, n) {
                Ok(())
            } else {
                Err(format!("unknown holder '{n}'; presets: {}", cam::holder_presets().iter().map(|h| h.0).collect::<Vec<_>>().join(", ")))
            }
        }
        Some(v) => {
            t.holder = serde_json::from_value(v.clone()).map_err(|_| "holder: a preset name or [[diameter, height], …]")?;
            t.holder_name.clear();
            Ok(())
        }
    }
}

impl Session {
    fn cam_idx(&self, a: &Value) -> R<usize> {
        let i = a.get("cam").and_then(|v| v.as_u64()).ok_or("give cam (the Manufacture tab's index from create_cam)")? as usize;
        if i >= self.ws.cams.len() {
            return Err(format!("no Manufacture tab {i}"));
        }
        Ok(i)
    }

    /// Size the setup's stock to the smallest standard plate or bar its part fits in (stock {standard_size: true}).
    fn cam_standard_stock(&mut self, ci: usize) -> R<()> {
        self.sync();
        let c = self.ws.cams[ci].clone();
        self.ensure_built(c.studio)?;
        let input = crate::cam_ui::part_input(self.bodies(c.studio), self.studio_doc(c.studio), &c.bodies);
        let input = crate::cam_jaws::input_for(&self.ws.cams, ci, input)?;
        if input.tris.is_empty() {
            return Err(format!("Part Studio {} has no parts to machine", c.studio));
        }
        let part = Part::from_input(&input, &c);
        let doc = self.studio_doc(c.studio);
        let solids: Vec<crate::doc::BodyId> = self.bodies(c.studio).iter().filter(|b| !b.surface).map(|b| b.id).collect();
        let one = match &c.bodies[..] {
            [b] => Some(*b),
            [] if solids.len() == 1 => Some(solids[0]),
            _ => None,
        };
        let pref = one.and_then(|b| doc?.parts.get(&crate::doc::body_key(b))?.stock.clone()).unwrap_or_default();
        let imperial = crate::stock_size::imperial_for(&pref, doc.map(|d| d.units).unwrap_or_default());
        let e = part.hi - part.lo;
        let (size, below) = crate::stock_size::for_setup([e.x, e.y, e.z], &pref, imperial);
        let st = &mut self.ws.cams[ci].stock;
        st.size = Some(size);
        st.bottom = below;
        Ok(())
    }

    /// The setup's parts and toolpaths.
    fn cam_build(&mut self, ci: usize) -> R<(Part, Vec<Toolpath>)> {
        self.cam_build_from(ci).map(|(p, t, _)| (p, t))
    }

    /// The setup's parts, toolpaths and the stock the setups before it leave.
    fn cam_build_from(&mut self, ci: usize) -> R<(Part, Vec<Toolpath>, Option<StockMap>)> {
        self.sync();
        let c = self.ws.cams[ci].clone();
        self.ensure_built(c.studio)?;
        let input = crate::cam_ui::part_input(self.bodies(c.studio), self.studio_doc(c.studio), &c.bodies);
        let input = crate::cam_jaws::input_for(&self.ws.cams, ci, input)?;
        if input.tris.is_empty() {
            return Err(format!("Part Studio {} has no parts to machine", c.studio));
        }
        let stock = cam_paths::link_setups(&mut self.ws.cams, ci, &input)?;
        let c = &self.ws.cams[ci];
        let part = Part::from_input(&input, c);
        let paths = cam_paths::generate_from(c, &part, stock.as_ref());
        Ok((part, paths, stock))
    }

    fn cam_info(&mut self, ci: usize) -> R<Value> {
        let (part, paths) = self.cam_build(ci)?;
        let c = &self.ws.cams[ci];
        let o = cam_paths::work_origin(c, &part);
        let (lo, hi) = cam_paths::stock_box(c, &part);
        let w = |p: DVec3| [round(p.x - o.x), round(p.y - o.y), round(p.z - o.z)];
        let ctx = cam_paths::Ctx::new(c, &part);
        let holes: Vec<Value> = ctx.geo.holes().iter().map(|h| json!({"at": [round(h.x - o.x), round(h.y - o.y)], "diameter": round(h.r * 2.0), "top": round(h.top - o.z), "bottom": round(h.bottom - o.z), "through": h.through})).collect();
        let ops: Vec<Value> = c
            .ops
            .iter()
            .map(|op| {
                let tp = paths.iter().find(|p| p.op == op.id);
                let mut v = serde_json::to_value(op).unwrap_or_default();
                v["kind"] = json!(op.kind.key());
                v["tool_number"] = json!(c.tool(op.tool).map(|t| t.number));
                if let Some(tp) = tp {
                    v["time_s"] = json!(tp.time.round());
                    v["cut_length"] = json!(tp.cut_length.round());
                    v["moves"] = json!(tp.moves.len());
                    v["warnings"] = json!(tp.warnings);
                }
                v
            })
            .collect();
        let total: f64 = paths.iter().map(|p| p.time).sum();
        // what will likely break something, and what is likely not meant
        let errors: Vec<String> = c.tools.iter().filter(|t| paths.iter().any(|p| p.tool == t.id && !p.moves.is_empty())).filter_map(|t| cam_paths::tap_danger(c, t)).collect();
        let hints = cam_paths::hints(c, &part, &paths);
        let jaws = crate::cam_jaws::jaws(c, &part).map(|j| {
            let (plo, phi) = j.rects.iter().fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)), |(l, h), (a, b)| (l.min(a.extend(j.bottom)), h.max(b.extend(j.top))));
            json!({
                "settings": c.soft_jaws,
                "blanks": j.rects.iter().map(|(a, b)| json!({"min": w(a.extend(j.bottom)), "max": w(b.extend(j.top))})).collect::<Vec<_>>(),
                "pocket_floor_z": round(j.floor - o.z), "jaw_top_z": round(j.top - o.z), "span": {"min": w(plo), "max": w(phi)},
                "machined_by": self.ws.cams.iter().position(|x| x.jaws_of == c.name),
            })
        });
        Ok(json!({
            "name": c.name, "studio": c.studio, "orient": c.orient, "tilt": c.tilt, "origin": c.origin.key(), "work_zero": cam_paths::work_zero_note(c, &part), "post": c.post,
            "follows": c.follows, "setup_notes": c.setup_notes(), "coolant": c.coolant,
            "machine": {"name": c.machine.name, "travel": c.machine.travel, "max_feed": c.machine.max_feed, "max_rpm": c.max_rpm, "min_rpm": c.min_rpm, "rapid_rate": c.rapid_rate, "rapid_z": c.machine.rapid_z, "tool_changer": c.machine.tool_changer, "spin_up": c.machine.spin_up},
            "errors": errors, "hints": hints,
            "stock": {"min": w(lo), "max": w(hi), "material": c.stock.material, "side": c.stock.side, "top": c.stock.top, "bottom": c.stock.bottom, "size": c.stock.size},
            "part": {"min": w(part.lo), "max": w(part.hi)},
            "note": "heights and points are in work coordinates (relative to the work zero)",
            "tools": c.tools.iter().map(tool_json).collect::<Vec<_>>(),
            "operations": ops,
            "holes": holes,
            "fixtures": c.fixtures, "probe": c.probe, "post_text": c.post_text,
            "soft_jaws": jaws, "jaws_of": c.jaws_of,
            "sketches": part.sketches.iter().map(|s| json!({"id": s.id, "name": s.name, "curves": s.paths.len(), "closed": s.paths.iter().filter(|p| p.1).count()})).collect::<Vec<_>>(),
            // an empty sum is -0.0
            "cycle_time_s": round(total.round()),
        }))
    }

    fn cam_sim(&mut self, ci: usize, resolution: usize) -> R<(Part, Vec<Toolpath>, Sim)> {
        let (part, paths, left) = self.cam_build_from(ci)?;
        let c = &self.ws.cams[ci];
        let stock = cam_paths::stock_box(c, &part);
        let mut sim = Sim::new(c, &part, stock, &paths, resolution);
        if let Some(m) = &left {
            sim.start_from(m);
        }
        sim.run_to(usize::MAX, usize::MAX);
        Ok((part, paths, sim))
    }

    pub(super) fn cam_call(&mut self, name: &str, a: &Value) -> R<(Value, Option<Vec<u8>>)> {
        let empty = Map::new();
        let args = a.as_object().unwrap_or(&empty);
        let v = match name {
            "create_cam" => {
                self.sync();
                let n = self.ws.cams.len() + 1;
                // the machine settings of the document's last setup, else the saved machine
                let mut base = cam::new_setup(format!("Manufacture {n}"), self.cur, &self.ws.cams, cam::load_machine());
                // a later setup of the same part: start with the machine and tools of the one it follows
                let follows = a.get("follows").and_then(|f| f.as_str()).unwrap_or("");
                if let Some(prev) = self.ws.cams.iter().find(|p| !follows.is_empty() && p.name == follows) {
                    cam::MachineSettings::of(prev).apply(&mut base);
                    base.tools = prev.tools.clone();
                    base.next_id = prev.next_id;
                    base.studio = prev.studio;
                    base.stock.material = prev.stock.material.clone();
                }
                let mut c: Cam = overlay(&base, args, &["stock", "origin", "tools", "ops", "next_id", "machine", "save_machine"])?;
                set_machine_arg(&mut c, a)?;
                if let Some(s) = a.get("stock").and_then(|s| s.as_object()) {
                    c.stock = overlay(&c.stock, s, &[])?;
                }
                if let Some(o) = a.get("origin").and_then(|o| o.as_str()) {
                    c.origin = cam::Origin::parse(o).ok_or("origin: stock_top_corner, stock_top_center, stock_bottom_corner, part_top_corner or model")?;
                }
                apply_axis(&mut c, a)?;
                if !cam::ORIENTS.contains(&c.orient.as_str()) {
                    return Err("orient: top, bottom, front, back, left or right".into());
                }
                if c.studio >= self.ws.studios.len() {
                    return Err(format!("no Part Studio {}", c.studio));
                }
                self.ws.cams.push(c);
                let ci = self.ws.cams.len() - 1;
                if let Err(e) = cam_paths::setup_chain(&self.ws.cams, ci) {
                    self.ws.cams.pop();
                    return Err(e);
                }
                if wants_standard_stock(a) {
                    self.cam_standard_stock(ci)?;
                }
                let saved = save_machine_arg(&self.ws.cams[ci], a)?;
                let mut v = self.cam_info(ci)?;
                v["cam"] = json!(ci);
                if let Some(p) = saved {
                    v["saved_machine"] = json!(p);
                }
                v
            }
            "edit_cam" => {
                let ci = self.cam_idx(a)?;
                let mut c: Cam = overlay(&self.ws.cams[ci], args, &["cam", "stock", "origin", "tools", "ops", "next_id", "machine", "save_machine"])?;
                set_machine_arg(&mut c, a)?;
                apply_axis(&mut c, a)?;
                let old = self.ws.cams[ci].clone();
                if c.name != old.name {
                    for x in self.ws.cams.iter_mut().filter(|x| x.follows == old.name) {
                        x.follows = c.name.clone();
                    }
                    for x in self.ws.cams.iter_mut().filter(|x| x.jaws_of == old.name) {
                        x.jaws_of = c.name.clone();
                    }
                }
                if let Some(s) = a.get("stock").and_then(|s| s.as_object()) {
                    c.stock = overlay(&c.stock, s, &[])?;
                }
                if let Some(o) = a.get("origin").and_then(|o| o.as_str()) {
                    c.origin = cam::Origin::parse(o).ok_or("unknown origin")?;
                }
                self.ws.cams[ci] = c;
                if let Err(e) = cam_paths::setup_chain(&self.ws.cams, ci) {
                    self.ws.cams[ci] = old;
                    return Err(e);
                }
                if wants_standard_stock(a) {
                    self.cam_standard_stock(ci)?;
                }
                let saved = save_machine_arg(&self.ws.cams[ci], a)?;
                let mut v = self.cam_info(ci)?;
                if let Some(p) = saved {
                    v["saved_machine"] = json!(p);
                }
                v
            }
            "import_tool_library" => {
                let path = self.input_path(a.get("path").and_then(|p| p.as_str()).ok_or("give path")?);
                let path = path.as_str();
                let r = crate::cam_tool_import::import_file(std::path::Path::new(path)).map_err(|e| if e.contains(path) { e } else { format!("can't import {path}: {e}") })?;
                let new = crate::cam_tool_import::add_to_library(&r.tools);
                let mut added = vec![];
                if a.get("cam").is_some() {
                    let ci = self.cam_idx(a)?;
                    let (material, max_rpm, min_rpm) = (self.ws.cams[ci].stock.material.clone(), self.ws.cams[ci].max_rpm, self.ws.cams[ci].min_rpm);
                    for t in &r.tools {
                        let mut t = t.clone();
                        if t.rpm <= 0.0 {
                            cam::suggest(&mut t, &material, max_rpm, min_rpm);
                        }
                        added.push(self.ws.cams[ci].add_tool(t));
                    }
                }
                json!({
                    "imported": r.tools.iter().map(|t| json!({"name": t.name, "kind": t.kind.key(), "diameter": t.diameter, "number": t.number})).collect::<Vec<_>>(),
                    "new_in_library": new, "skipped": r.skipped, "added_to_cam": added,
                })
            }
            "cam_tool_library" => {
                let kind = a.get("kind").and_then(|k| k.as_str()).map(|k| ToolKind::parse(k).ok_or(format!("unknown tool kind {k}"))).transpose()?;
                let q = a.get("search").and_then(|s| s.as_str()).unwrap_or("").to_lowercase();
                let mut all = cam::load_library();
                all.extend(cam::presets());
                let list: Vec<Value> = all
                    .iter()
                    .filter(|t| kind.map(|k| k == t.kind).unwrap_or(true))
                    .filter(|t| {
                        let hay = format!("{} {} {}", t.name.to_lowercase(), t.kind.key(), t.describe().to_lowercase());
                        q.split_whitespace().all(|w| hay.contains(w))
                    })
                    .map(|t| json!({"name": t.name, "kind": t.kind.key(), "diameter": t.diameter, "pitch": t.pitch, "angle": t.angle}))
                    .collect();
                json!({"tools": list, "kinds": ToolKind::ALL.iter().map(|k| k.key()).collect::<Vec<_>>()})
            }
            "cam_tool" => {
                let ci = self.cam_idx(a)?;
                if let Some(id) = a.get("id").and_then(|v| v.as_u64()) {
                    if a.get("delete").and_then(|v| v.as_bool()).unwrap_or(false) {
                        self.ws.cams[ci].tools.retain(|t| t.id != id);
                        json!({"deleted": id})
                    } else {
                        let c = &mut self.ws.cams[ci];
                        let i = c.tools.iter().position(|t| t.id == id).ok_or(format!("no tool {id}"))?;
                        let mut t: Tool = overlay(&c.tools[i], args, &["cam", "id", "kind", "preset", "holder"])?;
                        if let Some(k) = a.get("kind").and_then(|k| k.as_str()) {
                            t.kind = ToolKind::parse(k).ok_or(format!("unknown tool kind {k}"))?;
                        }
                        set_holder_arg(&mut t, a)?;
                        c.tools[i] = t.clone();
                        tool_json(&t)
                    }
                } else {
                    let (material, max_rpm, min_rpm) = (self.ws.cams[ci].stock.material.clone(), self.ws.cams[ci].max_rpm, self.ws.cams[ci].min_rpm);
                    let base = match a.get("preset").and_then(|p| p.as_str()) {
                        Some(q) => {
                            let q = q.to_lowercase();
                            let mut all = cam::load_library();
                            all.extend(cam::presets());
                            all.into_iter()
                                .find(|t| {
                                    let hay = format!("{} {} {}", t.name.to_lowercase(), t.kind.key(), t.describe().to_lowercase());
                                    t.name.to_lowercase() == q || q.split_whitespace().all(|w| hay.contains(w))
                                })
                                .ok_or(format!("no library tool matches '{q}' (see cam_tool_library)"))?
                        }
                        None => {
                            let k = a.get("kind").and_then(|k| k.as_str()).ok_or("give preset (a library search such as 'Ø6 flat') or kind")?;
                            let k = ToolKind::parse(k).ok_or(format!("unknown tool kind {k}"))?;
                            cam::presets().into_iter().find(|t| t.kind == k).unwrap_or(Tool { kind: k, ..Default::default() })
                        }
                    };
                    // the user's own tools keep their feeds and speeds when they have them
                    let skip = ["cam", "id", "kind", "preset", "name", "holder"];
                    let mut t: Tool = overlay(&base, args, &skip)?;
                    if let Some(k) = a.get("kind").and_then(|k| k.as_str()) {
                        t.kind = ToolKind::parse(k).ok_or(format!("unknown tool kind {k}"))?;
                    }
                    // suggest for the tool as given (its diameter, flutes and material), then keep the speeds given
                    if base.rpm <= 0.0 || cam::presets().iter().any(|p| p.name == base.name) {
                        cam::suggest(&mut t, &material, max_rpm, min_rpm);
                        let kind = t.kind;
                        t = overlay(&t, args, &skip)?;
                        t.kind = kind;
                    }
                    set_holder_arg(&mut t, a)?;
                    t.name = match a.get("name").and_then(|n| n.as_str()) {
                        Some(n) => n.to_string(),
                        None if a.get("preset").is_some() && args.keys().all(|k| ["cam", "preset", "number"].contains(&k.as_str())) => t.name,
                        None => t.describe(),
                    };
                    let id = self.ws.cams[ci].add_tool(t);
                    tool_json(self.ws.cams[ci].tool(id).unwrap())
                }
            }
            "cam_op" => {
                let ci = self.cam_idx(a)?;
                let tool_ref = |c: &Cam, v: &Value| -> R<u64> {
                    let n = v.as_u64().ok_or("tool: give the tool id (or number)")?;
                    c.tools.iter().find(|t| t.id == n).or_else(|| c.tools.iter().find(|t| t.number as u64 == n)).map(|t| t.id).ok_or(format!("no tool {n}"))
                };
                let id = match a.get("id").and_then(|v| v.as_u64()) {
                    Some(id) => {
                        if a.get("delete").and_then(|v| v.as_bool()).unwrap_or(false) {
                            self.ws.cams[ci].ops.retain(|o| o.id != id);
                            return Ok((json!({"deleted": id}), None));
                        }
                        id
                    }
                    None => {
                        let k = a.get("kind").and_then(|k| k.as_str()).ok_or("give kind: face, clear, pocket, contour, chamfer, drill, bore, thread, tap, adaptive, parallel, waterline, scallop, pencil or deburr")?;
                        let k = OpKind::parse(k).ok_or(format!("unknown operation {k}"))?;
                        let tool = match a.get("tool") {
                            Some(t) => tool_ref(&self.ws.cams[ci], t)?,
                            None => 0,
                        };
                        self.ws.cams[ci].add_op(k, tool)
                    }
                };
                let c = &mut self.ws.cams[ci];
                let i = c.ops.iter().position(|o| o.id == id).ok_or(format!("no operation {id}"))?;
                let mut op: Operation = overlay(&c.ops[i], args, &["cam", "id", "kind", "tool", "delete", "side", "sketches"])?;
                if let Some(t) = a.get("tool") {
                    op.tool = tool_ref(c, t)?;
                }
                if let Some(s) = a.get("side").and_then(|s| s.as_str()) {
                    op.side = serde_json::from_value(json!(s)).map_err(|_| "side: outside, inside or both")?;
                }
                let studio = c.studio;
                let known = self.studio_doc(studio).map(crate::cam_ui::sketch_input).unwrap_or_default();
                if let Some(v) = a.get("sketches") {
                    let list = if v.is_array() { v.as_array().cloned().unwrap_or_default() } else { vec![v.clone()] };
                    op.sketches = list
                        .iter()
                        .map(|x| {
                            known
                                .iter()
                                .find(|s| x.as_u64() == Some(s.id) || x.as_str().map(|n| n.trim().eq_ignore_ascii_case(&s.name)).unwrap_or(false))
                                .map(|s| s.id)
                                .ok_or(format!("no sketch {x} (sketches: {})", known.iter().map(|s| format!("{} \"{}\"", s.id, s.name)).collect::<Vec<_>>().join(", ")))
                        })
                        .collect::<R<Vec<u64>>>()?;
                } else if matches!(op.kind, OpKind::Trace | OpKind::VCarve) && op.sketches.is_empty() {
                    // start on the latest sketch
                    op.sketches = known.iter().rev().find(|s| !s.paths.is_empty()).map(|s| vec![s.id]).unwrap_or_default();
                }
                let c = &mut self.ws.cams[ci];
                c.ops[i] = op;
                let (_, paths) = self.cam_build(ci)?;
                let tp = paths.iter().find(|p| p.op == id);
                let c = &self.ws.cams[ci];
                let mut v = serde_json::to_value(c.op(id).unwrap()).unwrap_or_default();
                v["kind"] = json!(c.op(id).unwrap().kind.key());
                if let Some(tp) = tp {
                    v["time_s"] = json!(tp.time.round());
                    v["moves"] = json!(tp.moves.len());
                    v["warnings"] = json!(tp.warnings);
                }
                v
            }
            "cam_info" => {
                let ci = self.cam_idx(a)?;
                self.cam_info(ci)?
            }
            "simulate_cam" => {
                let ci = self.cam_idx(a)?;
                let res = a.get("resolution").and_then(|v| v.as_u64()).unwrap_or(500).clamp(50, 2000) as usize;
                let (part, _paths, sim) = self.cam_sim(ci, res)?;
                let c = &self.ws.cams[ci];
                let o = cam_paths::work_origin(c, &part);
                let names = |id: u64| c.op(id).map(|o| o.name.clone()).unwrap_or_default();
                let events: Vec<Value> = sim
                    .events
                    .iter()
                    .map(|e| json!({"kind": format!("{:?}", e.kind).to_lowercase(), "what": e.kind.label(), "operation": names(e.op), "op_id": e.op, "at": [round(e.at[0] - o.x), round(e.at[1] - o.y), round(e.at[2] - o.z)], "depth": round(e.depth)}))
                    .collect();
                let mut v = json!({"moves": sim.total(), "removed_mm3": sim.removed.round(), "problems": events, "ok": sim.events.is_empty(), "cell_mm": round(sim.cell)});
                if a.get("render").and_then(|r| r.as_bool()).unwrap_or(false) || a.get("path").is_some() {
                    let png = self.render_sim(&sim, a)?;
                    if let Some(p) = a.get("path").and_then(|p| p.as_str()) {
                        super::write_out(p, &png)?;
                        v["saved"] = json!(p);
                    }
                    return Ok((v, Some(png)));
                }
                v
            }
            "render_cam" => {
                let ci = self.cam_idx(a)?;
                let (part, paths) = self.cam_build(ci)?;
                let c = self.ws.cams[ci].clone();
                let only = a.get("operation").and_then(|v| v.as_u64());
                let mut lines: Vec<Vec<DVec3>> = vec![];
                for tp in paths.iter().filter(|p| only.map(|o| o == p.op).unwrap_or(true)) {
                    let mut cur: Vec<DVec3> = vec![];
                    let mut prev = DVec3::ZERO;
                    for m in &tp.moves {
                        let q = m.end(prev);
                        match m {
                            Move::Feed(..) => {
                                if cur.is_empty() {
                                    cur.push(prev);
                                }
                                cur.push(q);
                            }
                            _ => {
                                if cur.len() > 1 {
                                    lines.push(std::mem::take(&mut cur));
                                }
                                cur.clear();
                            }
                        }
                        prev = q;
                    }
                    if cur.len() > 1 {
                        lines.push(cur);
                    }
                }
                let (lo, hi) = cam_paths::stock_box(&c, &part);
                let corner = |k: usize| DVec3::new([lo.x, hi.x][k & 1], [lo.y, hi.y][(k >> 1) & 1], [lo.z, hi.z][(k >> 2) & 1]);
                for (p, q) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
                    lines.push(vec![corner(p), corner(q)]);
                }
                let doc = self.studio_doc(c.studio).cloned();
                // drawn as on the machine, spindle up: the parts turned by the setup's rotation
                let turned = crate::assembly::Pose { r: part.rot.to_cols_array(), t: [0.0; 3] };
                let items: Vec<(&crate::kernel::BodyMesh, crate::assembly::Pose, [u8; 3])> = self
                    .bodies(c.studio)
                    .iter()
                    .filter(|b| c.bodies.is_empty() || c.bodies.contains(&b.id))
                    .map(|b| {
                        (
                            b,
                            turned,
                            doc.as_ref().and_then(|d| d.parts.get(&crate::doc::body_key(b.id))).and_then(|p| p.color).unwrap_or([0x9a, 0xa9, 0xba]),
                        )
                    })
                    .collect();
                // soft jaws: the jaw tab shows the jaws instead of the parts, a holding setup shows them around the part
                let jaws = if !c.jaws_of.is_empty() {
                    Some(part.tris.clone())
                } else {
                    crate::cam_jaws::jaws(&c, &part).map(|j| crate::cam_jaws::jaw_tris(&j))
                }
                .map(|t| crate::cam_jaws::body_mesh("Soft jaws", &t));
                let mut items = if c.jaws_of.is_empty() { items } else { vec![] };
                if let Some(m) = &jaws {
                    items.push((m, crate::assembly::Pose::default(), [0x6f, 0x9f, 0xd8]));
                }
                let view = a.get("view").and_then(|v| v.as_str()).unwrap_or("iso");
                let w = a.get("width").and_then(|v| v.as_u64()).unwrap_or(900).clamp(64, 4096) as u32;
                let h = a.get("height").and_then(|v| v.as_u64()).unwrap_or(700).clamp(64, 4096) as u32;
                let png = super::raster_scene(&items, &lines, view, None, w, h)?;
                if let Some(p) = a.get("path").and_then(|p| p.as_str()) {
                    super::write_out(p, &png)?;
                }
                return Ok((json!({"cam": ci, "saved": a.get("path")}), Some(png)));
            }
            "cam_template" => {
                use crate::cam_template as tpl;
                if let Some(n) = a.get("delete").and_then(|v| v.as_str()) {
                    tpl::delete(n)?;
                    json!({"deleted": n})
                } else if let Some(n) = a.get("save").and_then(|v| v.as_str()) {
                    let ci = self.cam_idx(a)?;
                    let ids: Vec<u64> = a.get("operations").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_u64()).collect()).unwrap_or_default();
                    let t = tpl::from_cam(&self.ws.cams[ci], n, &ids);
                    if t.ops.is_empty() {
                        return Err("no operations to save".into());
                    }
                    let p = tpl::save(&t)?;
                    json!({"saved": t.name, "file": p.display().to_string(), "operations": t.ops.iter().map(|o| &o.name).collect::<Vec<_>>(), "tools": t.tools.iter().map(|t| &t.name).collect::<Vec<_>>()})
                } else if let Some(n) = a.get("apply").and_then(|v| v.as_str()) {
                    let ci = self.cam_idx(a)?;
                    let t = tpl::find(n)?;
                    let studio = self.ws.cams[ci].studio;
                    let sketch = self.studio_doc(studio).map(crate::cam_ui::sketch_input).unwrap_or_default().into_iter().rev().find(|s| !s.paths.is_empty()).map(|s| s.id);
                    let ids = tpl::apply(&mut self.ws.cams[ci], &t, sketch);
                    let (_, paths) = self.cam_build(ci)?;
                    let c = &self.ws.cams[ci];
                    let ops: Vec<Value> = ids
                        .iter()
                        .filter_map(|id| c.op(*id))
                        .map(|o| {
                            let tp = paths.iter().find(|p| p.op == o.id);
                            json!({"id": o.id, "name": o.name, "kind": o.kind.key(), "tool": o.tool, "time_s": tp.map(|p| p.time.round()), "warnings": tp.map(|p| p.warnings.clone()).unwrap_or_default()})
                        })
                        .collect();
                    json!({"applied": t.name, "operations": ops, "tools": c.tools.len()})
                } else {
                    json!({"templates": tpl::list().iter().map(|t| json!({"name": t.name, "operations": t.ops.iter().map(|o| format!("{} ({})", o.name, o.kind.key())).collect::<Vec<_>>(), "tools": t.tools.iter().map(|t| &t.name).collect::<Vec<_>>()})).collect::<Vec<_>>()})
                }
            }
            "cam_soft_jaws" => {
                let ci = self.cam_idx(a)?;
                if !self.ws.cams[ci].jaws_of.is_empty() {
                    return Err("this Manufacture tab machines soft jaws; give the setup that holds the part".into());
                }
                if a.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
                    self.ws.cams[ci].soft_jaws = None;
                    return Ok((self.cam_info(ci)?, None));
                }
                let mut base = self.ws.cams[ci].soft_jaws.clone().unwrap_or_default();
                // a mounting preset by name (with the blank it comes with), "none", or fields over the current one
                match a.get("mount") {
                    Some(Value::String(n)) if n.trim().eq_ignore_ascii_case("none") || n.trim().is_empty() => base.mount = None,
                    Some(Value::String(n)) => {
                        let (m, w, t) = cam::jaw_mount(n).ok_or_else(|| {
                            format!("no jaw mounting \"{n}\"; presets: {}", cam::JAW_MOUNTS.iter().map(|m| m.1).collect::<Vec<_>>().join(", "))
                        })?;
                        base.mount = Some(m);
                        base.width = w;
                        base.thickness = t;
                    }
                    Some(Value::Null) => base.mount = None,
                    Some(Value::Object(m)) => base.mount = Some(overlay(&base.mount.clone().unwrap_or_default(), m, &[])?),
                    Some(_) => return Err("mount: a preset name, \"none\" or an object of fields".into()),
                    None => {}
                }
                let sj: cam::SoftJaws = overlay(&base, args, &["cam", "enabled", "machine_jaws", "print_jaws", "export_step", "mount"])?;
                self.ws.cams[ci].soft_jaws = Some(sj);
                let mut v = json!({"cam": ci});
                if a.get("print_jaws").and_then(|v| v.as_bool()).unwrap_or(false) || a.get("export_step").is_some() {
                    let (part, _) = self.cam_build(ci)?;
                    let c = self.ws.cams[ci].clone();
                    let j = crate::cam_jaws::jaws(&c, &part).ok_or("no soft jaws")?;
                    let doc = crate::cam_jaws::print_doc(&j);
                    let name = format!("{} jaws (print)", c.name);
                    self.sync();
                    let si = match self.ws.studios.iter().position(|s| s.name == name) {
                        Some(i) => {
                            self.ws.studios[i].doc = doc.clone();
                            i
                        }
                        None => {
                            self.ws.studios.push(crate::workspace::Studio { name: name.clone(), doc: doc.clone() });
                            self.ws.studios.len() - 1
                        }
                    };
                    if si == self.cur {
                        self.doc = doc.clone();
                        self.rebuild()?;
                    }
                    v["print_studio"] = json!(si);
                    if let Some(path) = a.get("export_step").and_then(|p| p.as_str()) {
                        super::make_parent(path)?;
                        self.kernel.export_blocking(&doc, "step", path.to_string()).map_err(|e| format!("can't write {path}: {e}"))?;
                        v["exported"] = json!(path);
                    }
                }
                if a.get("machine_jaws").and_then(|v| v.as_bool()).unwrap_or(false) {
                    let held = self.ws.cams[ci].clone();
                    let ji = match self.ws.cams.iter().position(|x| x.jaws_of == held.name) {
                        Some(j) => j,
                        None => {
                            let mut j = crate::cam_jaws::jaw_setup(&held);
                            let base = j.name.clone();
                            let mut n = 2;
                            while self.ws.cams.iter().any(|x| x.name == j.name) {
                                j.name = format!("{base} {n}");
                                n += 1;
                            }
                            self.ws.cams.push(j);
                            self.ws.cams.len() - 1
                        }
                    };
                    let (jp, _) = self.cam_build(ji)?;
                    let (hp, _) = self.cam_build(ci)?;
                    let off = crate::cam_jaws::zero_offset(&self.ws.cams[ji], &jp, &self.ws.cams[ci], &hp);
                    v["jaw_cam"] = json!(ji);
                    v["jaw_setup"] = self.cam_info(ji)?;
                    v["held_zero_from_jaw_zero"] = json!([round(off.x), round(off.y), round(off.z)]);
                }
                v["setup"] = self.cam_info(ci)?;
                v
            }
            "export_setup_sheet" => {
                let ci = self.cam_idx(a)?;
                let path = super::str_arg(a, "path")?;
                let (part, paths) = self.cam_build(ci)?;
                let c = &self.ws.cams[ci];
                let png = crate::cam_sheet::picture(c, &part, 640, 480);
                let text = crate::cam_sheet::html(c, &part, &paths, png.as_deref());
                super::write_out(&path, &text)?;
                json!({"exported": path, "bytes": text.len(), "tools": c.tools.len(), "operations": paths.iter().filter(|p| !p.moves.is_empty()).count(), "cycle_time_s": round(paths.iter().map(|p| p.time).sum::<f64>().round())})
            }
            "export_gcode" => {
                let ci = self.cam_idx(a)?;
                if let Some(p) = a.get("post").and_then(|p| p.as_str()) {
                    if !cam::POSTS.iter().any(|x| x.0 == p) {
                        return Err("post: grbl, linuxcnc, fanuc or mach3".into());
                    }
                    self.ws.cams[ci].post = p.to_string();
                }
                let path = super::str_arg(a, "path")?;
                let (part, paths) = self.cam_build(ci)?;
                let c = &self.ws.cams[ci];
                let text = crate::cam_ui::gcode_for(c, &part, &paths);
                super::write_out(&path, &text)?;
                let warnings: Vec<String> =
                    paths.iter().flat_map(|p| p.warnings.iter().map(move |w| format!("{}: {w}", c.op(p.op).map(|o| o.name.as_str()).unwrap_or("?")))).collect();
                json!({"exported": path, "lines": text.lines().count(), "post": c.post, "cycle_time_s": round(paths.iter().map(|p| p.time).sum::<f64>().round()), "warnings": warnings, "hint": "run simulate_cam to check for crashes and gouges"})
            }
            _ => return self.fea_call(name, a),
        };
        Ok((v, None))
    }

    fn render_sim(&self, sim: &Sim, a: &Value) -> R<Vec<u8>> {
        let (tris, cls) = sim.mesh(300);
        let n = cls.iter().copied().max().unwrap_or(0) as usize + 1;
        let mut meshes: Vec<crate::kernel::BodyMesh> = (0..n).map(|_| crate::kernel::BodyMesh::default()).collect();
        for (t, cl) in tris.iter().zip(cls) {
            let m = &mut meshes[cl as usize];
            for p in t {
                // as the machine sees it: the side being cut faces up
                let q = *p;
                let i = m.positions.len() as u32 / 3;
                m.positions.extend([q.x as f32, q.y as f32, q.z as f32]);
                m.normals.extend([0.0, 0.0, 1.0]);
                m.indices.push(i);
            }
            m.tri_face.push(0);
        }
        for m in meshes.iter_mut() {
            let mut lo = [f64::INFINITY; 3];
            let mut hi = [f64::NEG_INFINITY; 3];
            for p in m.positions.chunks(3) {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k] as f64);
                    hi[k] = hi[k].max(p[k] as f64);
                }
            }
            m.bbox = [lo, hi];
        }
        let items: Vec<(&crate::kernel::BodyMesh, crate::assembly::Pose, [u8; 3])> = meshes
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.indices.is_empty())
            .map(|(k, m)| (m, crate::assembly::Pose::default(), crate::cam_sim::class_rgb(k as u8)))
            .collect();
        let view = a.get("view").and_then(|v| v.as_str()).unwrap_or("iso");
        let w = a.get("width").and_then(|v| v.as_u64()).unwrap_or(900).clamp(64, 4096) as u32;
        let h = a.get("height").and_then(|v| v.as_u64()).unwrap_or(700).clamp(64, 4096) as u32;
        let marks: Vec<Vec<DVec3>> = sim
            .events
            .iter()
            .map(|e| {
                let p = DVec3::from(e.at);
                vec![p - DVec3::X * 2.0, p + DVec3::X * 2.0, p, p - DVec3::Y * 2.0, p + DVec3::Y * 2.0]
            })
            .collect();
        super::raster_scene(&items, &marks, view, None, w, h)
    }
}

pub fn tools() -> Vec<Value> {
    let obj = |props: Value, req: &[&str]| json!({"type": "object", "properties": props, "required": req});
    vec![
        json!({"name": "create_cam", "description": "Add a Manufacture (CAM) tab (a setup) for 2.5D and 3D milling of a Part Studio's parts. Optional: name, studio (default the active one), bodies [[f,i],…] (default every visible part), orient (which part side faces the spindle: top, bottom, front, back, left, right), tilt [about X, about Y] (degrees, 3+2 positioning after orient: an angle block, a tilting vise or a compound-angle bore), axis [x, y, z] (instead of orient and tilt: the part direction that points up at the spindle, e.g. a bore's axis), origin (work zero: stock_top_corner (default), stock_top_center, stock_bottom_corner, part_top_corner, model), stock {side, top, bottom (margins mm), size [x,y,z] (fixed size instead), standard_size (true: size it from the part's stock settings: the smallest standard plate or bar, or its bounding box plus allowances), material (Aluminium, Brass, Mild steel, Stainless steel, Plastic, Hardwood, Softwood / MDF, Foam)}, post (grbl, linuxcnc, fanuc, mach3), clearance, retract, max_rpm, min_rpm (slowest useful spindle speed, e.g. 12000 for a high-speed spindle that loses torque below it: suggested speeds stay above it and slower tools get a warning), rapid_rate, machine (a preset name: \"Desktop router (3018 class)\", \"Hobby router, 600 × 400\", \"Large router, 1200 × 1200\", \"Shapeoko 4 XXL\", \"Carbide 3D Nomad 3\", \"Tormach PCNC 440\", \"Tormach 770M\", \"Haas Mini Mill\", \"Haas VF-2\", \"Generic VMC\"; or {travel [x,y,z], max_feed, rapid_z, tool_changer (default false), spin_up (seconds to wait after M3, default 5 for a VFD spindle)}: toolpaths are checked against travel and feed, and without a tool changer the G-code stops with M0 and an operator message at tool changes), coolant (auto (default: flood for metal stock, off for wood, plastic and foam), off, flood, mist: for the tools that don't set their own), arcs (default true: write G2/G3 arcs where the toolpath runs along circles; false = only straight G1 lines), fixtures ([{name, min [x,y,z], max [x,y,z]}, …]: clamps, vise jaws and screws as boxes in work coordinates; every toolpath lifts over them with clearance and the simulation reports any hit; replaces the whole list), probe {mode: off | plate (after each tool is loaded, probe a touch plate on the work and set the work Z zero: plate_at [x,y,z] work coordinates with z the surface under it, plate_thickness) | setter (measure each tool on a fixed tool setter and set its length offset against the first tool; LinuxCNC, Mach3, Fanuc only: setter [x,y,z] machine coordinates, z where probing starts), travel, fast, slow (mm/min)}, post_text {header, tool_change (replaces the standard tool change), footer}: your own G-code lines with placeholders {tool} {name} {desc} {diameter} {rpm} {program} {setup}, follows (name of an earlier Manufacture tab: this setup starts from the stock that one leaves, e.g. orient bottom to machine the back after flipping; it takes that setup's stock, tools and machine), save_machine (true: also keep this setup's machine settings (post, spindle, machine, coolant, probing, custom G-code) in ~/.config/cad-rust-app/machine.json as the user's machine). The machine settings not given come from the document's last setup, else from the saved machine. Returns the cam index and cam_info. Heights and points in CAM tools are work coordinates (relative to the work zero).", "inputSchema": obj(json!({"name": {"type": "string"}, "studio": {"type": "integer"}, "bodies": {"type": "array"}, "orient": {"type": "string"}, "tilt": {"type": "array"}, "axis": {"type": "array"}, "origin": {"type": "string"}, "stock": {"type": "object"}, "post": {"type": "string"}, "clearance": {"type": "number"}, "retract": {"type": "number"}, "max_rpm": {"type": "number"}, "min_rpm": {"type": "number"}, "rapid_rate": {"type": "number"}, "machine": {}, "arcs": {"type": "boolean"}, "fixtures": {"type": "array"}, "probe": {"type": "object"}, "post_text": {"type": "object"}, "follows": {"type": "string"}, "coolant": {"type": "string"}, "save_machine": {"type": "boolean"}}), &[])}),
        json!({"name": "edit_cam", "description": "Change a Manufacture tab's setup (same fields as create_cam).", "inputSchema": obj(json!({"cam": {"type": "integer"}, "name": {"type": "string"}, "studio": {"type": "integer"}, "bodies": {"type": "array"}, "orient": {"type": "string"}, "tilt": {"type": "array"}, "axis": {"type": "array"}, "origin": {"type": "string"}, "stock": {"type": "object"}, "post": {"type": "string"}, "clearance": {"type": "number"}, "retract": {"type": "number"}, "max_rpm": {"type": "number"}, "min_rpm": {"type": "number"}, "machine": {}, "follows": {"type": "string"}, "coolant": {"type": "string"}, "save_machine": {"type": "boolean"}}), &["cam"])}),
        json!({"name": "import_tool_library", "description": "Import a tool library file into the user's library (My tools): a Fusion 360 tool library (.json export or .tools) or a tools.json from this app. Tools keep their geometry, holder, tool number and the first feeds-and-speeds preset; entries of the same name are replaced; holders, probes and unsupported types are skipped. With cam, also add every imported tool to that Manufacture tab. Returns the imported tools.", "inputSchema": obj(json!({"path": {"type": "string"}, "cam": {"type": "integer"}}), &["path"])}),
        json!({"name": "cam_tool_library", "description": "List the tool library (the user's tools and built-in presets): flat, ball, bull, tapered_ball, chamfer, engrave, slot, thread_mill, face_mill, drill, spot_drill, center_drill, tap, reamer. Filter by kind and/or search words (e.g. '6 flat', 'M5').", "inputSchema": obj(json!({"kind": {"type": "string"}, "search": {"type": "string"}}), &[])}),
        json!({"name": "cam_tool", "description": "Add a tool to a Manufacture tab: preset (a library search such as 'Ø6 flat end mill' or 'M5 × 0.8 tap') and/or kind plus any fields to override: number, name, diameter, corner_radius, tip_diameter, angle (included; per side for tapered_ball), flute_length, stickout, shank_diameter, holder_diameter, holder (a preset: \"ER11 collet, router spindle Ø52\", \"ER11 collet chuck\", \"ER16 collet chuck\", \"ER20 collet, router spindle Ø80\", \"ER20 collet chuck\", \"ER25 collet chuck\", \"ER32 collet chuck\", \"Shrink-fit holder Ø6\", \"Shrink-fit holder Ø12\", \"Side-lock (Weldon) holder\"; or an outline [[diameter, height], …] from the holder's bottom face up, for collision checks), flutes, pitch, cutting_width, neck_diameter, material (carbide/hss/cobalt), coolant (auto = the setup's (default), off, flood, mist; true = flood), tapping_head (taps: held in a self-reversing tapping head, so the G-code feeds in and out with the spindle running forward, no G84 or spindle reversal: the way to tap on a high-speed spindle), reverse_ratio (the head's reverse ratio, default 1.75: it pulls out that much faster), rpm (capped at the setup's max_rpm when posted, with the feeds scaled to match), feed, plunge_feed (mm/min), stepdown, stepover (fraction of Ø). Feeds and speeds are suggested for the stock material. With id: edit that tool, or delete:true. Returns the tool with its id.", "inputSchema": obj(json!({"cam": {"type": "integer"}, "preset": {"type": "string"}, "kind": {"type": "string"}, "id": {"type": "integer"}, "delete": {"type": "boolean"}, "diameter": {"type": "number"}}), &["cam"])}),
        json!({"name": "cam_op", "description": "Add (kind) or edit (id) an operation; delete:true removes it. kind: face (top of stock down to the part top), clear (rough all stock around the part, level by level), pocket (closed pockets with floors, spiral out), contour (outline; side outside/inside/both; tabs, tab_width, tab_height; breakthrough for cut-outs; lead = radius of arcs onto and off the wall instead of ramping, 0 = ramp; slot mills cut at bottom with slot_depth), chamfer (top edges; chamfer width; chamfer mill/V-bit/spot drill), drill (drill/spot/center drill/reamer; peck, dwell), bore (helical with an end mill), thread (thread mill; thread_diameter, pitch, left_hand), tap; 3D: adaptive (roughing with cycloidal loops at constant engagement; stepover = optimal load, fraction of Ø, default 0.1; trochoid = loop radius mm, 0 = 40% of Ø; big stepdown; chip_thinning true raises the cutting feed for radial chip thinning at that thin cut, at most ×1.5), parallel (finishing along lines at angle degrees), waterline (finishing at constant Z; stepdown), scallop (finishing with constant 3D stepover), pencil (clean up inside corners and fillets a bigger tool left material in, with a small ball mill; ref_diameter = that bigger tool as a ball, default the smallest bigger tool used before), trace (follow the curves of sketches depth mm below the part's surface, in passes of stepdown: engraved lines or cutting along a drawing; closed shapes on the line (side both, default) or beside them (outside/inside, offset by the tool radius + leave)), vcarve (carve the closed shapes of sketches, e.g. text, with a V-bit or chamfer mill, deeper where they are wider so walls meet in sharp corners; depth = deepest cut, 0 = as deep as the cone goes; stepover fraction of Ø, default 0.02). trace and vcarve take sketches: [sketch feature id or name, …] (default the latest sketch; cam_info lists them). deburr (chamfer every sharp convex edge in 3D with a chamfer mill/V-bit; chamfer width; edge_angle; edges [[x,y,z],…] work-coordinate points on the edges to do, default all). rest:true makes clear, pocket, adaptive and finishing ops cut only the material the operations before them (and earlier setups) left, skipping air; it is on by default for roughing after roughing, and in a setup that follows another every such op starts from that setup's leftover stock. Finishing ops take slope_min/slope_max (degrees from horizontal) to split steep and shallow areas, leave, and bottom (lowest Z). tool: id or tool number (default: the first suitable tool). Optional: top, bottom (work Z; default from the part), stepdown, stepover (fraction of Ø), leave, leave_floor, climb, ramp_angle, at [x,y] (only that pocket/hole), points [[x,y],…] (explicit hole positions; need bottom), hole_min, hole_max (diameter range), breakthrough, coolant, suppressed, name. Returns the op with time and warnings.", "inputSchema": obj(json!({"cam": {"type": "integer"}, "kind": {"type": "string"}, "id": {"type": "integer"}, "tool": {"type": "integer"}, "delete": {"type": "boolean"}}), &["cam"])}),
        json!({"name": "cam_info", "description": "A Manufacture tab's setup, stock and part extents (work coordinates), the work zero and what the operator does before running (setup_notes), tools, operations with times and warnings, the holes found in the part, the cycle time, errors (things likely to break a tool, e.g. tapping below the spindle's min_rpm) and hints (operation order and work holding).", "inputSchema": obj(json!({"cam": {"type": "integer"}}), &["cam"])}),
        json!({"name": "simulate_cam", "description": "Run all toolpaths on a height map of the stock and report problems: rapid moves into stock, shank or holder hitting material, cuts into the part (gouges), with where and how deep. render:true (or path) returns a PNG of the machined stock (view as in render; red = gouge, crosses mark problems). resolution: cells along the longer stock side (default 500).", "inputSchema": obj(json!({"cam": {"type": "integer"}, "render": {"type": "boolean"}, "path": {"type": "string"}, "view": {"type": "string"}, "resolution": {"type": "integer"}, "width": {"type": "integer"}, "height": {"type": "integer"}}), &["cam"])}),
        json!({"name": "render_cam", "description": "PNG of the part with the stock box and the cutting moves (blue) of all operations, or of one (operation: id).", "inputSchema": obj(json!({"cam": {"type": "integer"}, "operation": {"type": "integer"}, "view": {"type": "string"}, "width": {"type": "integer"}, "height": {"type": "integer"}, "path": {"type": "string"}}), &["cam"])}),
        json!({"name": "cam_template", "description": "Operation templates, kept for the user across documents. No arguments: list them. save: name (with cam; operations [ids], default every operation that isn't suppressed) saves the operations and their tools, leaving out what belongs to the part (top/bottom heights, at, points, edges, sketches). apply: name (with cam) adds the template's operations to that setup, reusing its tools where one is the same cutter and adding the others; trace/vcarve follow the latest sketch. delete: name.", "inputSchema": obj(json!({"cam": {"type": "integer"}, "save": {"type": "string"}, "apply": {"type": "string"}, "delete": {"type": "string"}, "operations": {"type": "array"}}), &[])}),
        json!({"name": "cam_soft_jaws", "description": "Hold a setup's part in soft jaws: vise jaw blanks with a pocket machined to the part's outline below the jaw tops (plus clearance, with reliefs in corners the cutter can't reach), usually for the flipped second setup (follows). The vise closes along Y. The jaws act as fixtures (toolpaths lift over them, the simulation reports hits) and the setup warns when the part is narrower than the gap, the jaws are too thin, or the stock the earlier setup leaves doesn't fit the pocket. Fields (mm, any subset): width, thickness, height (each blank: along the vise, front to back, height; default 150 × 25 × 40), grip (how deep the part sits, default 6), gap (between the jaw faces while the pocket is cut, default 4), clearance (default 0.05), cutter (end mill diameter for the pocket and its reliefs, default 6), material (jaw material), printed (true: 3D printed jaws, no cutter reliefs; use about 0.15 clearance), one_piece (printed only: one block in between the vise jaws, sitting on the vise bed and squeezed round the part, instead of two jaws, no screw holes; gap is then the slit down its middle (about 1), web how far above the bottom the slit stops (default 1.5, 0 for two halves) and flare the radius of the rounds where it opens at the ends (default 5); height is from the vise bed to the block top). mount: how the jaws bolt into the vise, as a preset name (\"gen3_insert_bar\": in place of the Gen3 Smooth Insert Bars of an M12 dual-station vise, 95.5 × 12.7 jaws with four counterbored screws on a 25.4 pitch, the vise's lip and jaw around them as fixtures; \"top_jaw\": in place of its Top Jaws, 95.5 × 48 with two M8 screws), which also sets width and thickness, \"none\", or an object of fields {holes (X from the jaw middle), inset (from the grip face), drill, cbore, seat (under the head), head, fit (thickness the vise takes, 0 any), lip, lip_height, back, back_height}; the holes go in the jaw meshes and the printed solids (machined blanks get them before the pocket is cut). enabled false removes them. print_jaws true puts both jaws in a Part Studio named after the setup plus \" jaws (print)\" (standing on their bases, pocket up, solids built from extrudes) and returns print_studio; export_step (a path) also writes them as STEP for the slicer. machine_jaws true also makes (or finds) the Manufacture tab that machines the jaws: both blanks on a spacer in the vise, work zero top centre, a skim of the tops and a rough and finish of the pocket with a flat end mill no bigger than cutter; returns jaw_cam, its info and held_zero_from_jaw_zero (where the holding setup's work zero is from the jaws').", "inputSchema": obj(json!({"cam": {"type": "integer"}, "enabled": {"type": "boolean"}, "width": {"type": "number"}, "thickness": {"type": "number"}, "height": {"type": "number"}, "grip": {"type": "number"}, "gap": {"type": "number"}, "clearance": {"type": "number"}, "cutter": {"type": "number"}, "material": {"type": "string"}, "printed": {"type": "boolean"}, "one_piece": {"type": "boolean"}, "web": {"type": "number"}, "flare": {"type": "number"}, "mount": {}, "machine_jaws": {"type": "boolean"}, "print_jaws": {"type": "boolean"}, "export_step": {"type": "string"}}), &["cam"])}),
        json!({"name": "export_setup_sheet", "description": "Save a setup sheet for the machine operator as a self-contained HTML page (path ending .html): stock size and position, work zero, part orientation, fixtures, a picture of the part on the machine, the tools (number, size, holder, speeds and feeds) and the operations in order (tool, Z range, stepdown/stepover, time, warnings).", "inputSchema": obj(json!({"cam": {"type": "integer"}, "path": {"type": "string"}}), &["cam", "path"])}),
        json!({"name": "export_gcode", "description": "Write the G-code of a Manufacture tab (post: grbl, linuxcnc, fanuc, mach3; default the tab's). Reports line count, cycle time and toolpath warnings.", "inputSchema": obj(json!({"cam": {"type": "integer"}, "path": {"type": "string"}, "post": {"type": "string"}}), &["cam", "path"])}),
    ]
}

fn wants_standard_stock(a: &Value) -> bool {
    a.get("stock").and_then(|s| s.get("standard_size")).and_then(|v| v.as_bool()).unwrap_or(false)
}
