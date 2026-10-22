//! Agent tools for Manufacture (CAM) tabs: setups, tools, operations, simulation and G-code.

use glam::DVec3;
use serde_json::{Map, Value, json};

use super::{R, Session, round};
use crate::cam::{self, Cam, OpKind, Operation, Tool, ToolKind};
use crate::cam_paths::{self, Move, Part, Toolpath};
use crate::cam_sim::Sim;
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

fn tool_json(t: &Tool) -> Value {
    json!({"id": t.id, "number": t.number, "name": t.name, "kind": t.kind.key(), "diameter": t.diameter, "description": t.describe(), "rpm": t.rpm, "feed": t.feed, "plunge_feed": t.plunge_feed, "stepdown": t.stepdown, "stepover": t.stepover, "flute_length": t.flute_length})
}

impl Session {
    fn cam_idx(&self, a: &Value) -> R<usize> {
        let i = a.get("cam").and_then(|v| v.as_u64()).ok_or("give cam (the Manufacture tab's index from create_cam)")? as usize;
        if i >= self.ws.cams.len() {
            return Err(format!("no Manufacture tab {i}"));
        }
        Ok(i)
    }

    /// The setup's parts and toolpaths.
    fn cam_build(&mut self, ci: usize) -> R<(Part, Vec<Toolpath>)> {
        self.sync();
        let c = self.ws.cams[ci].clone();
        self.ensure_built(c.studio)?;
        let (tris, circles) = crate::cam_ui::part_input(self.bodies(c.studio), self.studio_doc(c.studio), &c.bodies);
        if tris.is_empty() {
            return Err(format!("Part Studio {} has no parts to machine", c.studio));
        }
        let part = Part::new(&tris, &circles, &c.orient);
        let paths = cam_paths::generate(&c, &part);
        Ok((part, paths))
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
        Ok(json!({
            "name": c.name, "studio": c.studio, "orient": c.orient, "origin": c.origin.key(), "post": c.post,
            "stock": {"min": w(lo), "max": w(hi), "material": c.stock.material, "side": c.stock.side, "top": c.stock.top, "bottom": c.stock.bottom, "size": c.stock.size},
            "part": {"min": w(part.lo), "max": w(part.hi)},
            "note": "heights and points are in work coordinates (relative to the work zero)",
            "tools": c.tools.iter().map(tool_json).collect::<Vec<_>>(),
            "operations": ops,
            "holes": holes,
            "cycle_time_s": total.round(),
        }))
    }

    fn cam_sim(&mut self, ci: usize, resolution: usize) -> R<(Part, Vec<Toolpath>, Sim)> {
        let (part, paths) = self.cam_build(ci)?;
        let c = &self.ws.cams[ci];
        let stock = cam_paths::stock_box(c, &part);
        let mut sim = Sim::new(c, &part, stock, &paths, resolution);
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
                let base = Cam { name: format!("Manufacture {n}"), studio: self.cur, ..Default::default() };
                let mut c: Cam = overlay(&base, args, &["stock", "origin", "tools", "ops", "next_id"])?;
                if let Some(s) = a.get("stock").and_then(|s| s.as_object()) {
                    c.stock = overlay(&c.stock, s, &[])?;
                }
                if let Some(o) = a.get("origin").and_then(|o| o.as_str()) {
                    c.origin = cam::Origin::parse(o).ok_or("origin: stock_top_corner, stock_top_center, stock_bottom_corner, part_top_corner or model")?;
                }
                if !cam::ORIENTS.contains(&c.orient.as_str()) {
                    return Err("orient: top, bottom, front, back, left or right".into());
                }
                if c.studio >= self.ws.studios.len() {
                    return Err(format!("no Part Studio {}", c.studio));
                }
                self.ws.cams.push(c);
                let ci = self.ws.cams.len() - 1;
                let mut v = self.cam_info(ci)?;
                v["cam"] = json!(ci);
                v
            }
            "edit_cam" => {
                let ci = self.cam_idx(a)?;
                let mut c: Cam = overlay(&self.ws.cams[ci], args, &["cam", "stock", "origin", "tools", "ops", "next_id"])?;
                if let Some(s) = a.get("stock").and_then(|s| s.as_object()) {
                    c.stock = overlay(&c.stock, s, &[])?;
                }
                if let Some(o) = a.get("origin").and_then(|o| o.as_str()) {
                    c.origin = cam::Origin::parse(o).ok_or("unknown origin")?;
                }
                self.ws.cams[ci] = c;
                self.cam_info(ci)?
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
                        let mut t: Tool = overlay(&c.tools[i], args, &["cam", "id", "kind", "preset"])?;
                        if let Some(k) = a.get("kind").and_then(|k| k.as_str()) {
                            t.kind = ToolKind::parse(k).ok_or(format!("unknown tool kind {k}"))?;
                        }
                        c.tools[i] = t.clone();
                        tool_json(&t)
                    }
                } else {
                    let (material, max_rpm) = (self.ws.cams[ci].stock.material.clone(), self.ws.cams[ci].max_rpm);
                    let mut base = match a.get("preset").and_then(|p| p.as_str()) {
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
                    cam::suggest(&mut base, &material, max_rpm);
                    let mut t: Tool = overlay(&base, args, &["cam", "id", "kind", "preset", "name"])?;
                    if let Some(k) = a.get("kind").and_then(|k| k.as_str()) {
                        t.kind = ToolKind::parse(k).ok_or(format!("unknown tool kind {k}"))?;
                    }
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
                        let k = a.get("kind").and_then(|k| k.as_str()).ok_or("give kind: face, clear, pocket, contour, chamfer, drill, bore, thread or tap")?;
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
                let mut op: Operation = overlay(&c.ops[i], args, &["cam", "id", "kind", "tool", "delete", "side"])?;
                if let Some(t) = a.get("tool") {
                    op.tool = tool_ref(c, t)?;
                }
                if let Some(s) = a.get("side").and_then(|s| s.as_str()) {
                    op.side = serde_json::from_value(json!(s)).map_err(|_| "side: outside, inside or both")?;
                }
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
                    let png = self.render_sim(&part, &sim, a)?;
                    if let Some(p) = a.get("path").and_then(|p| p.as_str()) {
                        std::fs::write(p, &png).map_err(|e| e.to_string())?;
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
                                    cur.push(part.to_part(prev));
                                }
                                cur.push(part.to_part(q));
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
                let corner = |k: usize| part.to_part(DVec3::new([lo.x, hi.x][k & 1], [lo.y, hi.y][(k >> 1) & 1], [lo.z, hi.z][(k >> 2) & 1]));
                for (p, q) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
                    lines.push(vec![corner(p), corner(q)]);
                }
                let doc = self.studio_doc(c.studio).cloned();
                let items: Vec<(&crate::kernel::BodyMesh, crate::assembly::Pose, [u8; 3])> = self
                    .bodies(c.studio)
                    .iter()
                    .filter(|b| c.bodies.is_empty() || c.bodies.contains(&b.id))
                    .map(|b| {
                        (
                            b,
                            crate::assembly::Pose::default(),
                            doc.as_ref().and_then(|d| d.parts.get(&crate::doc::body_key(b.id))).and_then(|p| p.color).unwrap_or([0x9a, 0xa9, 0xba]),
                        )
                    })
                    .collect();
                let view = a.get("view").and_then(|v| v.as_str()).unwrap_or("iso");
                let w = a.get("width").and_then(|v| v.as_u64()).unwrap_or(900).clamp(64, 4096) as u32;
                let h = a.get("height").and_then(|v| v.as_u64()).unwrap_or(700).clamp(64, 4096) as u32;
                let png = super::raster_scene(&items, &lines, view, None, w, h)?;
                if let Some(p) = a.get("path").and_then(|p| p.as_str()) {
                    std::fs::write(p, &png).map_err(|e| e.to_string())?;
                }
                return Ok((json!({"cam": ci, "saved": a.get("path")}), Some(png)));
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
                std::fs::write(&path, &text).map_err(|e| e.to_string())?;
                let warnings: Vec<String> =
                    paths.iter().flat_map(|p| p.warnings.iter().map(move |w| format!("{}: {w}", c.op(p.op).map(|o| o.name.as_str()).unwrap_or("?")))).collect();
                json!({"exported": path, "lines": text.lines().count(), "post": c.post, "cycle_time_s": paths.iter().map(|p| p.time).sum::<f64>().round(), "warnings": warnings, "hint": "run simulate_cam to check for crashes and gouges"})
            }
            _ => return Err(format!("unknown tool {name}")),
        };
        Ok((v, None))
    }

    fn render_sim(&self, part: &Part, sim: &Sim, a: &Value) -> R<Vec<u8>> {
        let (tris, cls) = sim.mesh(300);
        let mut meshes: Vec<crate::kernel::BodyMesh> = (0..3).map(|_| crate::kernel::BodyMesh::default()).collect();
        for (t, cl) in tris.iter().zip(cls) {
            let m = &mut meshes[cl as usize];
            for p in t {
                let q = part.to_part(*p);
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
        let colors = [[0xd2, 0xb4, 0x82], [0x9f, 0xb4, 0xc8], [0xe0, 0x40, 0x40]];
        let items: Vec<(&crate::kernel::BodyMesh, crate::assembly::Pose, [u8; 3])> =
            meshes.iter().zip(colors).filter(|(m, _)| !m.indices.is_empty()).map(|(m, c)| (m, crate::assembly::Pose::default(), c)).collect();
        let view = a.get("view").and_then(|v| v.as_str()).unwrap_or("iso");
        let w = a.get("width").and_then(|v| v.as_u64()).unwrap_or(900).clamp(64, 4096) as u32;
        let h = a.get("height").and_then(|v| v.as_u64()).unwrap_or(700).clamp(64, 4096) as u32;
        let marks: Vec<Vec<DVec3>> = sim
            .events
            .iter()
            .map(|e| {
                let p = part.to_part(DVec3::from(e.at));
                vec![p - DVec3::X * 2.0, p + DVec3::X * 2.0, p, p - DVec3::Y * 2.0, p + DVec3::Y * 2.0]
            })
            .collect();
        super::raster_scene(&items, &marks, view, None, w, h)
    }
}

pub fn tools() -> Vec<Value> {
    let obj = |props: Value, req: &[&str]| json!({"type": "object", "properties": props, "required": req});
    vec![
        json!({"name": "create_cam", "description": "Add a Manufacture (CAM) tab for 2.5D milling of a Part Studio's parts. Optional: name, studio (default the active one), bodies [[f,i],…] (default every visible part), orient (which part side faces the spindle: top, bottom, front, back, left, right), origin (work zero: stock_top_corner (default), stock_top_center, stock_bottom_corner, part_top_corner, model), stock {side, top, bottom (margins mm), size [x,y,z] (fixed size instead), material (Aluminium, Brass, Mild steel, Stainless steel, Plastic, Hardwood, Softwood / MDF, Foam)}, post (grbl, linuxcnc, fanuc, mach3), clearance, retract, max_rpm, rapid_rate. Returns the cam index and cam_info. Heights and points in CAM tools are work coordinates (relative to the work zero).", "inputSchema": obj(json!({"name": {"type": "string"}, "studio": {"type": "integer"}, "bodies": {"type": "array"}, "orient": {"type": "string"}, "origin": {"type": "string"}, "stock": {"type": "object"}, "post": {"type": "string"}, "clearance": {"type": "number"}, "retract": {"type": "number"}, "max_rpm": {"type": "number"}, "rapid_rate": {"type": "number"}}), &[])}),
        json!({"name": "edit_cam", "description": "Change a Manufacture tab's setup (same fields as create_cam).", "inputSchema": obj(json!({"cam": {"type": "integer"}, "name": {"type": "string"}, "studio": {"type": "integer"}, "bodies": {"type": "array"}, "orient": {"type": "string"}, "origin": {"type": "string"}, "stock": {"type": "object"}, "post": {"type": "string"}, "clearance": {"type": "number"}, "retract": {"type": "number"}}), &["cam"])}),
        json!({"name": "cam_tool_library", "description": "List the tool library (the user's tools and built-in presets): flat, ball, bull, tapered_ball, chamfer, engrave, slot, thread_mill, face_mill, drill, spot_drill, center_drill, tap, reamer. Filter by kind and/or search words (e.g. '6 flat', 'M5').", "inputSchema": obj(json!({"kind": {"type": "string"}, "search": {"type": "string"}}), &[])}),
        json!({"name": "cam_tool", "description": "Add a tool to a Manufacture tab: preset (a library search such as 'Ø6 flat end mill' or 'M5 × 0.8 tap') and/or kind plus any fields to override: number, name, diameter, corner_radius, tip_diameter, angle (included; per side for tapered_ball), flute_length, stickout, shank_diameter, holder_diameter, flutes, pitch, cutting_width, neck_diameter, material (carbide/hss/cobalt), coolant, rpm, feed, plunge_feed (mm/min), stepdown, stepover (fraction of Ø). Feeds and speeds are suggested for the stock material. With id: edit that tool, or delete:true. Returns the tool with its id.", "inputSchema": obj(json!({"cam": {"type": "integer"}, "preset": {"type": "string"}, "kind": {"type": "string"}, "id": {"type": "integer"}, "delete": {"type": "boolean"}, "diameter": {"type": "number"}}), &["cam"])}),
        json!({"name": "cam_op", "description": "Add (kind) or edit (id) an operation; delete:true removes it. kind: face (top of stock down to the part top), clear (rough all stock around the part, level by level), pocket (closed pockets with floors, spiral out), contour (outline; side outside/inside/both; tabs, tab_width, tab_height; breakthrough for cut-outs; slot mills cut at bottom with slot_depth), chamfer (top edges; chamfer width; chamfer mill/V-bit/spot drill), drill (drill/spot/center drill/reamer; peck, dwell), bore (helical with an end mill), thread (thread mill; thread_diameter, pitch, left_hand), tap. tool: id or tool number (default: the first suitable tool). Optional: top, bottom (work Z; default from the part), stepdown, stepover (fraction of Ø), leave, leave_floor, climb, ramp_angle, at [x,y] (only that pocket/hole), points [[x,y],…] (explicit hole positions; need bottom), hole_min, hole_max (diameter range), breakthrough, coolant, suppressed, name. Returns the op with time and warnings.", "inputSchema": obj(json!({"cam": {"type": "integer"}, "kind": {"type": "string"}, "id": {"type": "integer"}, "tool": {"type": "integer"}, "delete": {"type": "boolean"}}), &["cam"])}),
        json!({"name": "cam_info", "description": "A Manufacture tab's setup, stock and part extents (work coordinates), tools, operations with times and warnings, the holes found in the part, and the cycle time.", "inputSchema": obj(json!({"cam": {"type": "integer"}}), &["cam"])}),
        json!({"name": "simulate_cam", "description": "Run all toolpaths on a height map of the stock and report problems: rapid moves into stock, shank or holder hitting material, cuts into the part (gouges), with where and how deep. render:true (or path) returns a PNG of the machined stock (view as in render; red = gouge, crosses mark problems). resolution: cells along the longer stock side (default 500).", "inputSchema": obj(json!({"cam": {"type": "integer"}, "render": {"type": "boolean"}, "path": {"type": "string"}, "view": {"type": "string"}, "resolution": {"type": "integer"}, "width": {"type": "integer"}, "height": {"type": "integer"}}), &["cam"])}),
        json!({"name": "render_cam", "description": "PNG of the part with the stock box and the cutting moves (blue) of all operations, or of one (operation: id).", "inputSchema": obj(json!({"cam": {"type": "integer"}, "operation": {"type": "integer"}, "view": {"type": "string"}, "width": {"type": "integer"}, "height": {"type": "integer"}, "path": {"type": "string"}}), &["cam"])}),
        json!({"name": "export_gcode", "description": "Write the G-code of a Manufacture tab (post: grbl, linuxcnc, fanuc, mach3; default the tab's). Reports line count, cycle time and toolpath warnings.", "inputSchema": obj(json!({"cam": {"type": "integer"}, "path": {"type": "string"}, "post": {"type": "string"}}), &["cam", "path"])}),
    ]
}
