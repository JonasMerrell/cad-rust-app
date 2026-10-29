//! Agent interface: the same modelling tools as the GUI, driven by JSON.
//!
//! * `cad-app mcp` runs a Model Context Protocol server on stdio.
//! * `cad-app run script.json` runs a list of tool calls and prints the results.
//!
//! Both work headless and keep one Part Studio document in memory; every
//! change rebuilds the model in the kernel worker and reports errors.

use std::collections::HashMap;
use std::io::{BufRead, Write};

use glam::{DVec2, DVec3};
use serde_json::{json, Map, Value};

use crate::doc::{Document, FeatId, Op, OriginPlane, Ref};
use crate::kernel::RebuildResult;
use crate::regions::find_regions;
use crate::sketch::{Constraint, Dim, Frame, Geom, Id, Sketch};
use crate::worker::KernelClient;

#[path = "agent_cam.rs"]
mod cam;
#[path = "agent_ws.rs"]
mod ws;
#[path = "agent_fea.rs"]
mod fea;
#[path = "agent_pcb.rs"]
mod pcb;
#[path = "agent_circuit.rs"]
mod circuit;
#[cfg(test)]
pub(crate) use pcb::connect as pcb_connect;
#[path = "agent_query.rs"]
pub(crate) mod query;
#[path = "agent_schema.rs"]
mod schema;

pub struct Session {
    /// The active Part Studio (its copy in `ws` is stale until `sync`).
    pub doc: Document,
    pub path: Option<String>,
    pub result: RebuildResult,
    kernel: KernelClient,
    /// The whole document: Part Studios, Assemblies and Drawings.
    ws: crate::workspace::Workspace,
    cur: usize,
    /// Built results of the other Part Studios, and drawing projections.
    others: HashMap<usize, RebuildResult>,
    proj: HashMap<(usize, String), crate::kernel::ProjView>,
    /// Relative input files that aren't in the working directory are looked for here
    /// (the script's folder for `run`).
    pub base_dir: Option<std::path::PathBuf>,
    /// Rebuilds so far, to tell whether a failed call needs one to undo it.
    rebuilds: u64,
}

type R<T> = Result<T, String>;

impl Session {
    pub fn new() -> Session {
        Session {
            doc: Document::default(),
            path: None,
            result: RebuildResult::default(),
            kernel: KernelClient::new_headless(),
            ws: Default::default(),
            cur: 0,
            others: HashMap::new(),
            proj: HashMap::new(),
            base_dir: None,
            rebuilds: 0,
        }
    }

    /// Load a whole document with Part Studio `studio` active (the in-app Claude chat
    /// starts from what is open in the editor).
    pub fn load_workspace(&mut self, ws: crate::workspace::Workspace, studio: usize) -> R<()> {
        self.cur = studio.min(ws.studios.len().saturating_sub(1));
        self.doc = ws.studios.get(self.cur).map(|s| s.doc.clone()).unwrap_or_default();
        self.ws = ws;
        self.rebuild()
    }

    /// The whole document as saved, and the active Part Studio.
    pub fn workspace_json(&mut self) -> (String, usize) {
        self.sync();
        (self.ws.to_json(), self.cur)
    }

    fn rebuild(&mut self) -> R<()> {
        self.rebuilds += 1;
        // solve every sketch first, as the editor does
        let (vars, _) = self.doc.var_values();
        let units = self.doc.units;
        for f in &mut self.doc.features {
            if let Op::Sketch { sketch, .. } = &mut f.op {
                sketch.solve(&crate::expr::Env { vars: &vars, units }, &[]);
            }
        }
        self.sync();
        self.kernel.set_studios(self.ws.studios.iter().map(|s| s.doc.clone()).collect());
        self.others.clear();
        self.proj.clear();
        self.result = self.kernel.rebuild_blocking(self.cur, &self.doc)?;
        for f in &mut self.doc.features {
            if let (Op::Sketch { frame, .. }, Some(fr)) = (&mut f.op, self.result.frames.get(&f.id)) {
                *frame = *fr;
            }
        }
        // persistent names of the faces and edges features refer to, so later changes keep them
        self.doc.apply_ref_updates(&self.result.refs);
        Ok(())
    }

    fn status(&self) -> Value {
        let errors: Vec<Value> = self
            .result
            .features
            .iter()
            .filter_map(|f| f.error.as_ref().map(|e| json!({"feature": f.id, "name": self.doc.feature(f.id).map(|x| x.name.clone()), "error": e})))
            .collect();
        json!({
            "parts": self.result.bodies.iter().map(|b| json!({
                "body": [b.id.0, b.id.1],
                "name": self.doc.parts.get(&crate::doc::body_key(b.id)).and_then(|p| p.name.clone()).unwrap_or_else(|| b.name.clone()),
                "volume_mm3": round(b.volume),
                "area_mm2": round(b.area),
                "bbox_mm": [b.bbox[0].map(round), b.bbox[1].map(round)],
                "centroid_mm": b.centroid.map(round),
            })).collect::<Vec<_>>(),
            "errors": errors,
            "warnings": self.result.issues.iter().map(|i| json!({"body": [i.body.0, i.body.1], "kind": i.kind, "message": i.describe(), "at": i.at.map(round)})).collect::<Vec<_>>(),
        })
    }

    fn sketch_mut(&mut self, id: FeatId) -> R<&mut Sketch> {
        match self.doc.feature_mut(id).map(|f| &mut f.op) {
            Some(Op::Sketch { sketch, .. }) => Ok(sketch),
            _ => Err(format!("feature {id} is not a sketch")),
        }
    }

    fn sketch(&self, id: FeatId) -> R<(&Sketch, Frame)> {
        match self.doc.feature(id).map(|f| &f.op) {
            Some(Op::Sketch { sketch, frame, .. }) => Ok((sketch, *frame)),
            _ => Err(format!("feature {id} is not a sketch")),
        }
    }

    /// Run one tool. Returns JSON and optionally a PNG image.
    ///
    /// The arguments are checked against the tool's schema first. A modelling call is atomic: when
    /// it fails the Part Studio is left as it was, and when it leaves a feature failing the result
    /// has `ok: false` and `new_errors` (and with `rollback: true` the change is undone).
    pub fn call(&mut self, name: &str, a: &Value) -> R<(Value, Option<Vec<u8>>)> {
        let empty = json!({});
        let a = if a.is_null() { &empty } else { a };
        check_args(name, a)?;
        if !schema::MODELLING_TOOLS.contains(&name) {
            return self.call_tool(name, a);
        }
        let (cur, doc, rebuilds) = (self.cur, self.doc.clone(), self.rebuilds);
        let old: Vec<(FeatId, String)> = self.result.features.iter().filter_map(|f| f.error.clone().map(|e| (f.id, e))).collect();
        let undo = |s: &mut Session| -> R<()> {
            s.doc = doc.clone();
            if s.rebuilds != rebuilds { s.rebuild() } else { Ok(()) }
        };
        let (mut v, img) = match self.call_tool(name, a) {
            Ok(r) => r,
            Err(e) => {
                if self.cur == cur {
                    undo(self)?;
                }
                return Err(e);
            }
        };
        let new: Vec<Value> = self
            .result
            .features
            .iter()
            .filter_map(|f| f.error.as_ref().filter(|e| !old.iter().any(|o| o.0 == f.id && &o.1 == *e)).map(|e| json!({"feature": f.id, "name": self.doc.feature(f.id).map(|x| x.name.clone()), "error": e})))
            .collect();
        if !new.is_empty() && self.cur == cur {
            if a.get("rollback").and_then(|r| r.as_bool()).unwrap_or(false) {
                undo(self)?;
                v = self.status();
                v["rolled_back"] = json!(true);
            }
            v["ok"] = json!(false);
            v["new_errors"] = json!(new);
        }
        Ok((v, img))
    }

    fn call_tool(&mut self, name: &str, a: &Value) -> R<(Value, Option<Vec<u8>>)> {
        let v = match name {
            "new_document" => {
                let units = a.get("units").and_then(|u| u.as_str()).unwrap_or("mm");
                self.doc = Document::default();
                self.ws = Default::default();
                self.cur = 0;
                self.doc.units = serde_json::from_value(json!(units)).map_err(|_| "units must be mm, cm, m, in or ft")?;
                self.path = None;
                self.rebuild()?;
                json!({"ok": true})
            }
            "open_document" => {
                let path = self.input_path(&str_arg(a, "path")?);
                let text = std::fs::read_to_string(&path).map_err(|e| format!("can't open {path}: {e}"))?;
                let ws = crate::workspace::Workspace::from_json(&text).map_err(|e| format!("can't open {path}: {e}"))?;
                self.doc = ws.studios.first().map(|s| s.doc.clone()).unwrap_or_default();
                self.ws = ws;
                self.cur = 0;
                self.path = Some(path);
                self.rebuild()?;
                self.status()
            }
            "save_document" => {
                let path = a.get("path").and_then(|p| p.as_str()).map(String::from).or(self.path.clone()).ok_or("give a path")?;
                self.sync();
                write_out(&path, self.ws.to_json())?;
                self.path = Some(path.clone());
                json!({"saved": path})
            }
            "describe" => {
                let mut v = self.status();
                v["units"] = json!(self.doc.units.label());
                v["document"] = self.tabs_summary();
                v["variables"] = json!(self.doc.variables.iter().map(|x| json!({"name": x.name, "expr": x.expr})).collect::<Vec<_>>());
                v["features"] = json!(self
                    .doc
                    .features
                    .iter()
                    .map(|f| json!({"id": f.id, "name": f.name, "kind": f.op.kind(), "suppressed": f.suppressed, "params": described_params(&f.op)}))
                    .collect::<Vec<_>>());
                v
            }
            "set_variable" => {
                let name = str_arg(a, "name")?;
                let expr = val_str(a.get("expr").ok_or("give expr")?);
                check_var_name(&name)?;
                let before = self.doc.var_values().1;
                match self.doc.variables.iter_mut().find(|v| v.name == name) {
                    Some(v) => v.expr = expr.clone(),
                    None => self.doc.variables.push(crate::doc::Variable { name: name.clone(), expr: expr.clone() }),
                }
                // the value and every variable that uses it must still evaluate
                let after = self.doc.var_values().1;
                for (i, e) in after.iter().enumerate() {
                    let Some(e) = e else { continue };
                    if before.get(i).map(|b| b.as_ref() == Some(e)).unwrap_or(false) {
                        continue; // broken before this change
                    }
                    let v = &self.doc.variables[i];
                    return Err(if v.name == name { format!("{name} = {expr}: {e}") } else { format!("{name} = {expr} breaks {} = {}: {e}", v.name, v.expr) });
                }
                self.rebuild()?;
                self.status()
            }
            "create_sketch" => {
                let plane = parse_plane(a.get("plane").unwrap_or(&json!("top")))?;
                let frame = match &plane {
                    Ref::OriginPlane { plane } => plane.frame(),
                    Ref::Face { point, normal, .. } => crate::kernel::plane_frame((*point).into(), (*normal).into()),
                    Ref::FeaturePlane { feature } => self.result.frames.get(feature).copied().ok_or("that plane has not been built")?,
                    _ => Frame::TOP,
                };
                let name = a.get("name").and_then(|n| n.as_str()).map(String::from).unwrap_or_else(|| self.doc.next_name("sketch"));
                let id = self.doc.insert(name, Op::Sketch { plane, sketch: Sketch::default(), frame });
                self.rebuild()?;
                let frame = self.result.frames.get(&id).copied().unwrap_or(frame);
                json!({"sketch_id": id, "frame": frame})
            }
            "sketch_add" => {
                let sid = id_arg(a, "sketch_id")?;
                let items = a.get("items").and_then(|i| i.as_array()).ok_or("give items")?.clone();
                let mut created = vec![];
                // all or nothing: a bad item leaves the sketch as it was
                let before = self.sketch_mut(sid)?.clone();
                for (i, it) in items.iter().enumerate() {
                    match self.sketch_item(sid, it) {
                        Ok(c) => created.push(c),
                        Err(e) => {
                            *self.sketch_mut(sid)? = before;
                            return Err(format!("item {}: {e} (nothing was added)", i + 1));
                        }
                    }
                }
                // points at the same place (e.g. a polyline end and a spline start) become one point
                let merged = merge_coincident(self.sketch_mut(sid)?);
                self.rebuild()?;
                json!({"created": created, "merged_points": merged.iter().map(|(a, b)| json!({"removed": a, "kept": b})).collect::<Vec<_>>(), "sketch": self.sketch_summary(sid)?})
            }
            "sketch_constrain" => {
                let sid = id_arg(a, "sketch_id")?;
                let list = a.get("constraints").and_then(|i| i.as_array()).ok_or("give constraints")?.clone();
                let mut applied = vec![];
                let (vars, _) = self.doc.var_values();
                let env = crate::expr::Env { vars: &vars, units: self.doc.units };
                for (i, c) in list.iter().enumerate() {
                    let k = parse_constraint(c).map_err(|e| format!("constraints[{i}]: {e}"))?;
                    check_constraint(self.sketch(sid)?.0, c, &k, &env).map_err(|e| format!("constraints[{i}] ({}): {e}", c.get("type").and_then(|t| t.as_str()).unwrap_or("?")))?;
                    let sk = self.sketch_mut(sid)?;
                    // coincident points are merged like in the editor
                    if c.get("type").and_then(|t| t.as_str()) == Some("coincident") {
                        let (p, q) = (c["a"].as_u64().ok_or("a")? as Id, c["b"].as_u64().ok_or("b")? as Id);
                        if sk.point(p).is_some() && sk.point(q).is_some() {
                            sk.merge_point(q, p);
                            applied.push(format!("merged point {q} into {p}"));
                            continue;
                        }
                    }
                    applied.push(format!("added {}", c.get("type").and_then(|t| t.as_str()).unwrap_or("constraint")));
                    sk.constraints.push(k);
                }
                self.rebuild()?;
                json!({"applied": applied, "sketch": self.sketch_summary(sid)?})
            }
            "sketch_info" => {
                let sid = id_arg(a, "sketch_id")?;
                self.sketch_summary(sid)?
            }
            "add_feature" => {
                let kind = str_arg(a, "kind")?;
                let mut op = build_op(&kind, a.get("params").unwrap_or(&json!({})), self)?;
                check_values(&op, &self.doc)?;
                if let Op::Custom { name, select, .. } = &mut op {
                    let def = self.doc.custom_features.iter().find(|d| d.name == *name).ok_or_else(|| format!("no custom feature named {name:?}; define it with define_custom_feature"))?;
                    *select = def.selection.clone();
                }
                let auto = match &op {
                    Op::Custom { name, .. } => format!("{name} {}", self.doc.features.iter().filter(|f| matches!(&f.op, Op::Custom { name: n, .. } if n == name)).count() + 1),
                    _ => self.doc.next_name(&kind),
                };
                let name = a.get("name").and_then(|n| n.as_str()).map(String::from).unwrap_or(auto);
                let id = self.doc.insert(name, op);
                self.rebuild()?;
                let mut v = self.status();
                v["feature_id"] = json!(id);
                v
            }
            "edit_feature" => {
                let id = id_arg(a, "id")?;
                let f = self.doc.feature(id).ok_or("no such feature")?;
                let mut cur = serde_json::to_value(&f.op).map_err(|e| e.to_string())?;
                merge(&mut cur, a.get("params").unwrap_or(&json!({})), self)?;
                let op: Op = serde_json::from_value(cur).map_err(|e| format!("bad params: {e}"))?;
                check_values(&op, &self.doc)?;
                if let Some(f) = self.doc.feature_mut(id) {
                    f.op = op;
                    if let Some(s) = a.get("suppressed").and_then(|s| s.as_bool()) {
                        f.suppressed = s;
                    }
                }
                self.rebuild()?;
                self.status()
            }
            "delete_feature" => {
                let id = id_arg(a, "id")?;
                let i = self.doc.index_of(id).ok_or("no such feature")?;
                self.doc.features.remove(i);
                self.rebuild()?;
                self.status()
            }
            "list_geometry" => self.geometry(a),
            "feature_defaults" => match a.get("kind").and_then(|k| k.as_str()) {
                Some(kind) => {
                    let op = Op::new_default(kind).ok_or_else(|| format!("unknown feature kind {kind}{}", schema::did_you_mean(kind, schema::FEATURE_KINDS)))?;
                    let mut params = serde_json::to_value(&op).map_err(|e| e.to_string())?;
                    if let Some(o) = params.as_object_mut() {
                        o.remove("type");
                    }
                    json!({"kind": kind, "params": params, "note": "Numbers are expressions in document units (angles in degrees). Refs: see add_feature."})
                }
                None => json!({"kinds": schema::FEATURE_KINDS.to_vec()}),
            },
            "mass_properties" => self.mass_properties(a)?,
            "ray_cast" => {
                let mut q = json!({"ray": {"origins": a.get("origins").cloned().unwrap_or_default(), "direction": a.get("direction").cloned().unwrap_or_default()}});
                if let Some(b) = a.get("bodies") {
                    q["bodies"] = b.clone();
                }
                let r = self.kernel.query_blocking(&self.doc, q)?;
                round_json(r)
            }
            "classify_points" => {
                let mut q = json!({"classify": a.get("points").cloned().unwrap_or_default()});
                for k in ["bodies", "tolerance"] {
                    if let Some(x) = a.get(k) {
                        q[k] = x.clone();
                    }
                }
                let r = self.kernel.query_blocking(&self.doc, q)?;
                round_json(r)
            }
            "add_decal" => {
                let path = self.input_path(&str_arg(a, "image")?);
                let bytes = std::fs::read(&path).map_err(|e| format!("can't open {path}: {e}"))?;
                image::load_from_memory(&bytes).map_err(|_| "the image must be a PNG or JPEG")?;
                let face = a.get("face").ok_or("give face: a planar face ref {point, normal}")?;
                let at = face.get("point").and_then(pt3).ok_or("face.point")?;
                let normal = face.get("normal").and_then(pt3).ok_or("face.normal")?;
                let id = self.doc.decals.iter().map(|d| d.id).max().unwrap_or(0) + 1;
                let name = std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("Decal".into());
                self.doc.decals.push(crate::doc::Decal {
                    id,
                    name,
                    image: crate::import::pack(&bytes),
                    at: at.into(),
                    normal: normal.into(),
                    width: a.get("width").and_then(|w| w.as_f64()).unwrap_or(20.0),
                    angle: a.get("angle").and_then(|w| w.as_f64()).unwrap_or(0.0),
                });
                json!({"decal": id, "note": "Decals show in the editor (not in render, STL or STEP)."})
            }
            "set_part" => {
                let b = a.get("body").and_then(|b| b.as_array()).ok_or("give body [f,i]")?;
                let id: crate::doc::BodyId = (b.first().and_then(|v| v.as_u64()).ok_or("body [f,i]")?, b.get(1).and_then(|v| v.as_u64()).ok_or("body [f,i]")? as u32);
                if !self.result.bodies.iter().any(|x| x.id == id) {
                    return Err(format!("no part {id:?}"));
                }
                let key = crate::doc::body_key(id);
                let mut p = self.doc.parts.get(&key).cloned().unwrap_or_default();
                if let Some(n) = a.get("name").and_then(|v| v.as_str()) {
                    p.name = Some(n.to_string());
                }
                if let Some(c) = a.get("color") {
                    p.color = match c {
                        Value::String(h) => {
                            let h = h.trim_start_matches('#');
                            let v = u32::from_str_radix(h, 16).map_err(|_| "color: #rrggbb or [r,g,b]")?;
                            Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
                        }
                        Value::Array(v) if v.len() == 3 => Some([0, 1, 2].map(|k| v[k].as_u64().unwrap_or(0).min(255) as u8)),
                        _ => None,
                    };
                }
                if let Some(m) = a.get("material").and_then(|v| v.as_str()) {
                    if !crate::doc::MATERIALS.iter().any(|x| x.0 == m) {
                        return Err(format!("material must be one of {}", crate::doc::MATERIALS.iter().map(|x| x.0).collect::<Vec<_>>().join(", ")));
                    }
                    p.material = Some(m.to_string());
                }
                if let Some(f) = a.get("finish").and_then(|v| v.as_str()) {
                    p.finish = if f.is_empty() || f == "default" { None } else { Some(f.to_string()) };
                }
                if let Some(h) = a.get("hidden").and_then(|v| v.as_bool()) {
                    p.hidden = h;
                }
                self.doc.parts.insert(key, p);
                self.rebuild()?;
                self.status()
            }
            "set_parts" => {
                let list = a.get("parts").and_then(|p| p.as_array()).ok_or("give parts: [{body, name, color, …}]")?.clone();
                for a in &list {
                    let b = a.get("body").and_then(|b| b.as_array()).ok_or("give body [f,i]")?;
                    let id: crate::doc::BodyId = (b.first().and_then(|v| v.as_u64()).ok_or("body [f,i]")?, b.get(1).and_then(|v| v.as_u64()).ok_or("body [f,i]")? as u32);
                    if !self.result.bodies.iter().any(|x| x.id == id) {
                        return Err(format!("no part {id:?}"));
                    }
                    let key = crate::doc::body_key(id);
                    let mut p = self.doc.parts.get(&key).cloned().unwrap_or_default();
                    if let Some(n) = a.get("name").and_then(|v| v.as_str()) {
                        p.name = Some(n.to_string());
                    }
                    if let Some(c) = a.get("color") {
                        p.color = match c {
                            Value::String(h) => {
                                let h = h.trim_start_matches('#');
                                let v = u32::from_str_radix(h, 16).map_err(|_| "color: #rrggbb or [r,g,b]")?;
                                Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
                            }
                            Value::Array(v) if v.len() == 3 => Some([0, 1, 2].map(|k| v[k].as_u64().unwrap_or(0).min(255) as u8)),
                            _ => None,
                        };
                    }
                    if let Some(m) = a.get("material").and_then(|v| v.as_str()) {
                        if !crate::doc::MATERIALS.iter().any(|x| x.0 == m) {
                            return Err(format!("material must be one of {}", crate::doc::MATERIALS.iter().map(|x| x.0).collect::<Vec<_>>().join(", ")));
                        }
                        p.material = Some(m.to_string());
                    }
                    if let Some(f) = a.get("finish").and_then(|v| v.as_str()) {
                        p.finish = if f.is_empty() || f == "default" { None } else { Some(f.to_string()) };
                    }
                    if let Some(h) = a.get("hidden").and_then(|v| v.as_bool()) {
                        p.hidden = h;
                    }
                    self.doc.parts.insert(key, p);
                }
                self.rebuild()?;
                let mut v = self.status();
                v["set"] = json!(list.len());
                v
            }
            "printability" => {
                let st = crate::printing::PrintSettings::default();
                let overhang = a.get("overhang").and_then(|v| v.as_f64()).unwrap_or(st.overhang);
                let min_wall = a.get("min_wall").and_then(|v| v.as_f64()).unwrap_or(st.min_wall);
                let floor = self.result.bodies.iter().map(|b| b.bbox[0][2]).fold(f64::INFINITY, f64::min);
                let parts: Vec<Value> = self
                    .result
                    .bodies
                    .iter()
                    .filter(|b| !b.surface)
                    .map(|b| {
                        let cls = crate::printing::classify(b, floor, overhang, min_wall);
                        let p = |i: u32| DVec3::new(b.positions[3 * i as usize] as f64, b.positions[3 * i as usize + 1] as f64, b.positions[3 * i as usize + 2] as f64);
                        let (mut over, mut thin) = (0.0, 0.0);
                        let (mut over_at, mut thin_at) = (vec![], vec![]);
                        for (t, c) in b.indices.chunks(3).zip(&cls) {
                            let (x, y, z) = (p(t[0]), p(t[1]), p(t[2]));
                            let area = (y - x).cross(z - x).length() / 2.0;
                            let mid = ((x + y + z) / 3.0).to_array().map(round);
                            match c {
                                1 => {
                                    over += area;
                                    if over_at.len() < 5 {
                                        over_at.push(mid);
                                    }
                                }
                                2 => {
                                    thin += area;
                                    if thin_at.len() < 5 {
                                        thin_at.push(mid);
                                    }
                                }
                                _ => {}
                            }
                        }
                        json!({"body": [b.id.0, b.id.1], "overhang_area_mm2": round(over), "overhang_examples": over_at, "thin_wall_area_mm2": round(thin), "thin_wall_examples": thin_at})
                    })
                    .collect();
                json!({"overhang_limit_deg": overhang, "min_wall_mm": min_wall, "build_plate_z": round(floor), "parts": parts})
            }
            "send_to_slicer" => {
                let url = a.get("url").and_then(|u| u.as_str()).map(String::from).unwrap_or_else(|| crate::printing::load().orcaweb);
                let tol = a.get("tolerance").and_then(|t| t.as_f64()).unwrap_or(0.01);
                let tmp = std::env::temp_dir().join(format!("cad-agent-send-{}.stl", std::process::id()));
                self.kernel.export_blocking(&self.doc, &format!("stl@{tol}"), tmp.to_string_lossy().to_string())?;
                let bytes = std::fs::read(&tmp).map_err(|e| e.to_string())?;
                let _ = std::fs::remove_file(&tmp);
                let name = a.get("name").and_then(|n| n.as_str()).map(String::from).or_else(|| self.path.as_ref().and_then(|p| std::path::Path::new(p).file_stem().map(|s| s.to_string_lossy().to_string()))).unwrap_or("part".into());
                let page = crate::printing::send_to_orcaweb(&url, &name, &bytes)?;
                json!({"slice_page": page, "stl_bytes": bytes.len()})
            }
            "import_file" if crate::sketch_import::is_drawing(std::path::Path::new(&str_arg(a, "path")?)) => {
                let path = self.input_path(&str_arg(a, "path")?);
                let d = crate::sketch_import::read(std::path::Path::new(&path)).map_err(|e| if e.contains(&path) { e } else { format!("can't open {path}: {e}") })?;
                let scale = a.get("scale").and_then(|v| v.as_f64()).unwrap_or(1.0);
                let at = match a.get("at") {
                    Some(v) => pt2(v)?,
                    None => DVec2::ZERO,
                };
                let sid = match a.get("sketch_id") {
                    Some(_) => id_arg(a, "sketch_id")?,
                    None => {
                        let plane = parse_plane(a.get("plane").unwrap_or(&json!("top")))?;
                        let frame = match &plane {
                            Ref::OriginPlane { plane } => plane.frame(),
                            Ref::Face { point, normal, .. } => crate::kernel::plane_frame((*point).into(), (*normal).into()),
                            Ref::FeaturePlane { feature } => self.result.frames.get(feature).copied().ok_or("that plane has not been built")?,
                            _ => Frame::TOP,
                        };
                        let name = a.get("name").and_then(|n| n.as_str()).map(String::from).unwrap_or_else(|| self.doc.unique_name(&std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("Drawing".into())));
                        self.doc.insert(name, Op::Sketch { plane, sketch: Sketch::default(), frame })
                    }
                };
                let n = crate::sketch_import::add_to_sketch(self.sketch_mut(sid)?, &d, scale, at);
                self.rebuild()?;
                let skipped: Vec<Value> = d.skipped.iter().map(|(t, n)| json!({"type": t, "count": n})).collect();
                json!({"sketch_id": sid, "curves": n, "skipped": skipped, "sketch": self.sketch_summary(sid)?})
            }
            "import_file" => {
                let path = self.input_path(&str_arg(a, "path")?);
                let p = std::path::Path::new(&path);
                let format = crate::import::format_of(p).ok_or("import .step, .stp, .iges, .igs, .stl, .obj, .dxf or .svg files")?;
                let bytes = std::fs::read(p).map_err(|e| format!("can't open {path}: {e}"))?;
                let name = a.get("name").and_then(|n| n.as_str()).map(String::from).unwrap_or_else(|| p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("Import".into()));
                let scale = a.get("scale").map(val_str).unwrap_or_else(|| "1".into());
                let merge = a.get("merge_faces").and_then(|m| m.as_bool()).unwrap_or(true);
                let id = self.doc.insert(name.clone(), Op::Import { name, step: String::new(), format: format.into(), data: crate::import::pack(&bytes), scale, merge });
                self.rebuild()?;
                let mut v = self.status();
                v["feature_id"] = json!(id);
                v
            }
            "export" => {
                let format = str_arg(a, "format")?;
                let path = str_arg(a, "path")?;
                let ascii = a.get("ascii").and_then(|v| v.as_bool()).unwrap_or(false);
                match format.as_str() {
                    "stl" if !ascii => write_out(&path, stl_binary(&self.result))?,
                    "step" | "stl" | "iges" => {
                        make_parent(&path)?;
                        self.kernel.export_blocking(&self.doc, &format, path.clone()).map_err(|e| format!("can't write {path}: {e}"))?
                    }
                    "obj" => write_out(&path, obj(&self.result))?,
                    _ => return Err("format must be step, stl, iges or obj".into()),
                }
                json!({"exported": path})
            }
            "render" => {
                let view = a.get("view").and_then(|v| v.as_str()).unwrap_or("iso");
                let w = a.get("width").and_then(|v| v.as_u64()).unwrap_or(800).clamp(64, 4096) as u32;
                let h = a.get("height").and_then(|v| v.as_u64()).unwrap_or(600).clamp(64, 4096) as u32;
                let dir = direction_arg(a)?;
                let sketches = a.get("sketches").and_then(|s| s.as_str()).unwrap_or("unused");
                let png = render_png(&self.doc, &self.result, view, dir, sketches, w, h)?;
                if let Some(p) = a.get("path").and_then(|p| p.as_str()) {
                    write_out(p, &png)?;
                }
                return Ok((json!({"view": view, "width": w, "height": h, "saved": a.get("path")}), Some(png)));
            }
            "export_drawing" => {
                let path = str_arg(a, "path")?;
                let format = a.get("format").and_then(|f| f.as_str()).unwrap_or_else(|| path.rsplit('.').next().unwrap_or("pdf")).to_lowercase();
                let data = match a.get("drawing").and_then(|d| d.as_u64()) {
                    Some(i) => self.export_drawing_tab(i as usize, &format, a)?,
                    None => self.drawing(&format, a)?,
                };
                write_out(&path, data)?;
                json!({"exported": path})
            }
            _ => return self.ws_call(name, a),
        };
        Ok((v, None))
    }

    /// An input file: as given, or else next to the script being run.
    pub(crate) fn input_path(&self, p: &str) -> String {
        let path = std::path::Path::new(p);
        if path.is_relative() && !path.exists() {
            if let Some(q) = self.base_dir.as_ref().map(|d| d.join(path)).filter(|q| q.exists()) {
                return q.to_string_lossy().into_owned();
            }
        }
        p.to_string()
    }

    /// Mass, centre of gravity and inertia of the parts, from their materials (or `material` /
    /// `density` for all of them).
    fn mass_properties(&mut self, a: &Value) -> R<Value> {
        let density_of = |name: &str| crate::doc::MATERIALS.iter().find(|m| m.0 == name).map(|m| m.1);
        // (density g/cm³, material name: None when a density is given)
        let all: Option<(f64, Option<String>)> = match (a.get("density").and_then(|d| d.as_f64()), a.get("material").and_then(|m| m.as_str())) {
            (Some(d), _) => Some((d, None)),
            (None, Some(m)) => Some((density_of(m).ok_or_else(|| format!("no material {m}"))?, Some(m.to_string()))),
            _ => None,
        };
        let want: Option<Vec<crate::doc::BodyId>> =
            a.get("bodies").and_then(|b| b.as_array()).map(|b| b.iter().filter_map(|x| Some((x.get(0)?.as_u64()?, x.get(1)?.as_u64()? as u32))).collect());
        let mut list = vec![];
        let mut info = vec![];
        for b in self.result.bodies.iter().filter(|b| !b.surface && want.as_ref().map(|w| w.contains(&b.id)).unwrap_or(true)) {
            let part = self.doc.parts.get(&crate::doc::body_key(b.id));
            let (d, mat) = match &all {
                Some(x) => x.clone(),
                None => match part.and_then(|p| p.material.clone()) {
                    Some(m) => (density_of(&m).unwrap_or(1.0), Some(m)),
                    None => (1.0, Some(String::new())),
                },
            };
            let name = part.and_then(|p| p.name.clone()).unwrap_or_else(|| b.name.clone());
            list.push(json!({"body": [b.id.0, b.id.1], "density": d * 1e-3}));
            info.push((name, mat, d));
        }
        if let Some(w) = &want {
            if let Some(id) = w.iter().find(|id| !self.result.bodies.iter().any(|b| b.id == **id)) {
                return Err(format!("no part {id:?}"));
            }
        }
        let r = self.kernel.query_blocking(&self.doc, json!({"mass": list}))?;
        let mut parts = vec![];
        let (mut total, mut moment) = (0.0, DVec3::ZERO);
        let rows = |v: &Value| -> glam::DMat3 {
            let r = |i: usize| DVec3::from(pt3(&v[i]).unwrap_or_default());
            glam::DMat3::from_cols(r(0), r(1), r(2)).transpose()
        };
        let mut items = vec![];
        for (m, (name, mat, d)) in r["mass"].as_array().into_iter().flatten().zip(&info) {
            let (mass, cg) = (m["mass"].as_f64().unwrap_or(0.0), pt3(&m["cg"]).unwrap_or_default());
            total += mass;
            moment += cg * mass;
            items.push((mass, cg, rows(&m["inertia"])));
            let mut p = json!({"body": m["body"], "name": name, "volume_mm3": round(m["volume"].as_f64().unwrap_or(0.0)), "density_g_cm3": d, "mass_g": round(mass), "cg_mm": cg.to_array().map(round), "inertia_g_mm2": mat_json(rows(&m["inertia"]))});
            match mat.as_deref() {
                Some("") => p["note"] = json!("no material set: density 1 g/cm³ (set_part material, or pass material or density)"),
                Some(m) => p["material"] = json!(m),
                None => {}
            }
            parts.push(p);
        }
        let mut v = json!({"parts": parts, "units": "mm, g; inertia about each centre of gravity, axes x y z"});
        if items.len() > 1 && total > 0.0 {
            // the parts together, inertia about their common centre of gravity (parallel axis theorem)
            let cg = moment / total;
            let mut it = glam::DMat3::ZERO;
            for (m, c, i) in &items {
                let d = *c - cg;
                it += *i + (glam::DMat3::IDENTITY * d.length_squared() - glam::DMat3::from_cols(d * d.x, d * d.y, d * d.z)) * *m;
            }
            v["total"] = json!({"mass_g": round(total), "cg_mm": cg.to_array().map(round), "inertia_g_mm2": mat_json(it)});
        }
        Ok(v)
    }

    fn sketch_item(&mut self, sid: FeatId, it: &Value) -> R<Value> {
        let ty = it.get("type").and_then(|t| t.as_str()).ok_or("each item needs a type")?.to_string();
        let keys = SKETCH_ITEMS.iter().find(|x| x.0 == ty).map(|x| x.1).ok_or_else(|| format!("unknown sketch item {ty}{}", schema::did_you_mean(&ty, SKETCH_ITEMS.iter().map(|x| x.0))))?;
        for k in it.as_object().into_iter().flat_map(|o| o.keys()) {
            if k != "type" && k != "construction" && !keys.contains(&k.as_str()) {
                return Err(format!("{ty} takes {}; unknown key '{k}'{}", keys.join(", "), schema::did_you_mean(k, keys.iter().copied())));
            }
        }
        let construction = it.get("construction").and_then(|c| c.as_bool()).unwrap_or(false);
        let p = |k: &str| -> R<DVec2> { pt2(it.get(k).ok_or(format!("{ty} needs {k}"))?) };
        let num = |k: &str| -> R<f64> { it.get(k).and_then(|v| v.as_f64()).filter(|v| v.is_finite()).ok_or(format!("{ty} needs a number {k}")) };
        // sizes are positive: a negative radius is a mistake, not a mirror image
        let size = |k: &str| -> R<f64> { num(k).and_then(|v| if v > 0.0 { Ok(v) } else { Err(format!("{ty} {k} must be more than 0 (got {v})")) }) };
        let pts = |k: &str| -> R<Vec<DVec2>> {
            let v: Vec<DVec2> = it.get(k).and_then(|v| v.as_array()).ok_or(format!("{ty} needs {k}"))?.iter().map(pt2).collect::<R<_>>()?;
            if v.len() < 2 { Err(format!("{ty} needs at least 2 {k}")) } else { Ok(v) }
        };
        let sk = self.sketch_mut(sid)?;
        let out = match ty.as_str() {
            "point" => json!({"point": sk.add_point(p("at")?)}),
            "line" => {
                let (a, b) = (sk.add_point(p("from")?), sk.add_point(p("to")?));
                json!({"line": sk.add_curve(Geom::Line { a, b }, construction), "points": [a, b]})
            }
            "polyline" | "polygon_points" => {
                let pts = pts("points")?;
                let closed = it.get("closed").and_then(|c| c.as_bool()).unwrap_or(false);
                let ids: Vec<Id> = pts.iter().map(|q| sk.add_point(*q)).collect();
                let n = if closed { ids.len() } else { ids.len() - 1 };
                let lines: Vec<Id> = (0..n).map(|i| sk.add_curve(Geom::Line { a: ids[i], b: ids[(i + 1) % ids.len()] }, construction)).collect();
                json!({"lines": lines, "points": ids})
            }
            "rect" | "center_rect" => {
                let (c0, c2) = if ty == "rect" {
                    (p("corner1")?, p("corner2")?)
                } else {
                    let c = p("center")?;
                    let k = p("corner")?;
                    (c * 2.0 - k, k)
                };
                let corners = [c0, DVec2::new(c2.x, c0.y), c2, DVec2::new(c0.x, c2.y)];
                let ids: Vec<Id> = corners.iter().map(|q| sk.add_point(*q)).collect();
                let lines: Vec<Id> = (0..4).map(|i| sk.add_curve(Geom::Line { a: ids[i], b: ids[(i + 1) % 4] }, construction)).collect();
                sk.constraints.push(Constraint::Horizontal { l: lines[0] });
                sk.constraints.push(Constraint::Horizontal { l: lines[2] });
                sk.constraints.push(Constraint::Vertical { l: lines[1] });
                sk.constraints.push(Constraint::Vertical { l: lines[3] });
                json!({"lines": lines, "points": ids})
            }
            "circle" => {
                let c = p("center")?;
                let r = size("radius")?;
                let c = sk.add_point(c);
                json!({"circle": sk.add_curve(Geom::Circle { c, r }, construction), "center": c})
            }
            "arc" => {
                let (c, a, b) = (sk.add_point(p("center")?), sk.add_point(p("start")?), sk.add_point(p("end")?));
                json!({"arc": sk.add_curve(Geom::Arc { c, a, b }, construction), "points": {"center": c, "start": a, "end": b}})
            }
            "ellipse" => {
                size("minor_radius")?;
                let (c, a) = (sk.add_point(p("center")?), sk.add_point(p("major_end")?));
                json!({"ellipse": sk.add_curve(Geom::Ellipse { c, a, rmin: size("minor_radius")? }, construction)})
            }
            "spline" => {
                let pts = pts("points")?;
                let ids: Vec<Id> = pts.iter().map(|q| sk.add_point(*q)).collect();
                let closed = it.get("closed").and_then(|c| c.as_bool()).unwrap_or(false);
                json!({"spline": sk.add_curve(Geom::Spline { pts: ids.clone(), closed }, construction), "points": ids})
            }
            "polygon" => {
                let (c, r, n) = (p("center")?, size("radius")?, it.get("sides").and_then(|v| v.as_u64()).unwrap_or(6).max(3) as usize);
                let ids: Vec<Id> = (0..n).map(|i| sk.add_point(c + DVec2::from_angle(i as f64 / n as f64 * std::f64::consts::TAU) * r)).collect();
                let lines: Vec<Id> = (0..n).map(|i| sk.add_curve(Geom::Line { a: ids[i], b: ids[(i + 1) % n] }, construction)).collect();
                json!({"lines": lines, "points": ids})
            }
            "text" => {
                let (at, height) = (p("at")?, size("height")?);
                let anchor = sk.add_point(at);
                let text = it.get("text").and_then(|t| t.as_str()).ok_or("text needs text")?.to_string();
                sk.texts.push(crate::sketch::TextItem { anchor, text, height, curves: vec![] });
                sk.refresh_texts();
                json!({"anchor_point": anchor, "outlines": sk.texts.last().map(|t| t.curves.clone()).unwrap_or_default()})
            }
            "elliptical_arc" => {
                size("minor_radius")?;
                let (c, a) = (sk.add_point(p("center")?), sk.add_point(p("major_end")?));
                let (t0, t1) = (num("start_angle")?.to_radians(), num("end_angle")?.to_radians());
                json!({"elliptical_arc": sk.add_curve(Geom::EllipticalArc { c, a, rmin: num("minor_radius")?, t0, t1 }, construction)})
            }
            _ => return Err(format!("unknown sketch item {ty}")),
        };
        Ok(out)
    }

    fn sketch_summary(&self, sid: FeatId) -> R<Value> {
        let (sk, fr) = self.sketch(sid)?;
        let mut s = sk.clone();
        let (vars, _) = self.doc.var_values();
        let st = s.solve(&crate::expr::Env { vars: &vars, units: self.doc.units }, &[]);
        let regions: Vec<Value> = find_regions(&s)
            .iter()
            .map(|r| json!({"ref": {"kind": "region", "sketch": sid, "point": [round(r.sample.x), round(r.sample.y)]}, "area_mm2": round(r.area), "holes": r.holes.len()}))
            .collect();
        Ok(json!({
            "sketch_id": sid,
            "points": s.points.iter().map(|p| json!({"id": p.id, "at": [round(p.x), round(p.y)]})).collect::<Vec<_>>(),
            "curves": s.curves.iter().map(|c| json!({"id": c.id, "geom": serde_json::to_value(&c.geom).unwrap_or_default(), "construction": c.construction})).collect::<Vec<_>>(),
            "constraints": s.constraints.len(),
            "degrees_of_freedom": st.dof,
            "solved": st.ok,
            "regions": regions,
            "plane_origin": fr.origin.map(round),
            "plane_normal": fr.normal.map(round),
        }))
    }

    fn geometry(&self, a: &Value) -> Value {
        let only = a.get("body").and_then(|b| b.as_array()).map(|b| (b[0].as_u64().unwrap_or(0), b.get(1).and_then(|x| x.as_u64()).unwrap_or(0) as u32));
        let want = a.get("type").and_then(|t| t.as_str()).unwrap_or("all");
        let fnormal = a.get("normal").and_then(|n| n.as_array()).map(|n| DVec3::new(n[0].as_f64().unwrap_or(0.0), n.get(1).and_then(|x| x.as_f64()).unwrap_or(0.0), n.get(2).and_then(|x| x.as_f64()).unwrap_or(0.0)).normalize_or_zero());
        let bx = a.get("box").and_then(|b| b.as_array()).and_then(|b| Some((pt3(b.first()?)?, pt3(b.get(1)?)?)));
        let in_box = |p: DVec3| bx.map(|(lo, hi)| p.cmpge(lo - DVec3::splat(1e-6)).all() && p.cmple(hi + DVec3::splat(1e-6)).all()).unwrap_or(true);
        let planar_only = a.get("planar").and_then(|p| p.as_bool());
        let mut out = vec![];
        for b in &self.result.bodies {
            if only.map(|o| o != b.id).unwrap_or(false) {
                continue;
            }
            let faces: Vec<Value> = b
                .faces
                .iter()
                .enumerate()
                .filter(|(_, f)| want != "edges" && in_box(f.center.into()) && planar_only.map(|p| p == f.planar).unwrap_or(true))
                .filter(|(_, f)| fnormal.map(|n| DVec3::from(f.normal).dot(n) > 0.9998).unwrap_or(true))
                .map(|(i, f)| {
                    json!({"index": i, "ref": {"kind": "face", "point": f.center.map(round), "normal": f.normal.map(round)}, "planar": f.planar, "area_mm2": round(f.area)})
                })
                .collect();
            let edges: Vec<Value> = b
                .edges
                .iter()
                .enumerate()
                .filter(|(_, e)| want != "faces" && fnormal.is_none() && in_box(e.mid.into()))
                .map(|(i, e)| {
                    let kind = if e.linear { "line" } else if e.circular { "circle" } else { "curve" };
                    let mut v = json!({"index": i, "ref": {"kind": "edge", "point": e.mid.map(round)}, "type": kind, "length_mm": round(e.length),
                        "start": e.pts[0].map(|x| round(x as f64)), "end": e.pts[e.pts.len() - 1].map(|x| round(x as f64))});
                    if let Some((c, n, r)) = e.circle {
                        v["center"] = json!(c.map(round));
                        v["axis"] = json!(n.map(round));
                        v["radius"] = json!(round(r));
                    }
                    v
                })
                .collect();
            out.push(json!({"body": [b.id.0, b.id.1], "ref": {"kind": "body", "body": [b.id.0, b.id.1]}, "faces": faces, "edges": edges}));
        }
        json!({"bodies": out})
    }

    fn drawing(&mut self, format: &str, a: &Value) -> R<Vec<u8>> {
        use crate::drawing_ui::Prim;
        let sheet = crate::drawing_ui::sheet_size(a.get("sheet").and_then(|s| s.as_str()).unwrap_or("A3"));
        let dirs = ["front", "top", "right", "iso"];
        let views: Vec<([f64; 3], [f64; 3], bool)> = dirs.iter().map(|d| {
            let (x, y) = crate::drawing_ui::dir_vectors(d);
            (x, y, false)
        }).collect();
        let proj = self.kernel.project_blocking(0, "agent-drawing", &self.doc, views)?;
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for b in &self.result.bodies {
            lo = lo.min(b.bbox[0].into());
            hi = hi.max(b.bbox[1].into());
        }
        let size = if lo.x.is_finite() { hi - lo } else { DVec3::splat(50.0) };
        let scale = a.get("scale").and_then(|s| s.as_f64()).unwrap_or_else(|| {
            let raw = ((sheet[0] - 60.0) / (size.x + size.y * 1.8).max(1.0)).min((sheet[1] - 80.0) / (size.z + size.y * 1.6).max(1.0));
            [100.0, 50.0, 20.0, 10.0, 5.0, 4.0, 2.0, 1.0, 0.5, 0.25, 0.2, 0.1, 0.05, 0.02, 0.01].into_iter().find(|s| *s <= raw).unwrap_or(0.005)
        });
        let (fw, fh) = (size.x * scale, size.z * scale);
        let front = [40.0 + fw / 2.0, 60.0 + fh / 2.0];
        let pos = [
            front,
            [front[0], front[1] + fh / 2.0 + 25.0 + size.y * scale / 2.0],
            [front[0] + fw / 2.0 + 25.0 + size.y * scale / 2.0, front[1]],
            [sheet[0] - 30.0 - (size.x + size.y) * scale * 0.45, sheet[1] - 30.0 - (size.z + size.y) * scale * 0.45],
        ];
        let ink = [20, 20, 20];
        let mut prims = vec![];
        let m = 10.0;
        let (w, h) = (sheet[0], sheet[1]);
        for (p, q) in [([m, m], [w - m, m]), ([w - m, m], [w - m, h - m]), ([w - m, h - m], [m, h - m]), ([m, h - m], [m, m])] {
            prims.push(Prim::Line { a: p, b: q, w: 0.5, dashed: false, color: ink });
        }
        let title = a.get("title").and_then(|t| t.as_str()).unwrap_or("Part").to_string();
        prims.push(Prim::Text { at: [w - m - 166.0, m + 24.0], size: 6.0, text: title, anchor: 0, color: ink });
        prims.push(Prim::Text { at: [w - m - 166.0, m + 6.0], size: 3.2, text: format!("Scale {}  ·  units mm", if scale >= 1.0 { format!("{scale}:1") } else { format!("1:{}", 1.0 / scale) }), anchor: 0, color: ink });
        for (i, pv) in proj.iter().enumerate() {
            let c = [(pv.bbox[0][0] + pv.bbox[1][0]) / 2.0, (pv.bbox[0][1] + pv.bbox[1][1]) / 2.0];
            let map = |p: [f64; 2]| [pos[i][0] + (p[0] - c[0]) * scale, pos[i][1] + (p[1] - c[1]) * scale];
            for pl in &pv.visible {
                for s in pl.windows(2) {
                    prims.push(Prim::Line { a: map(s[0]), b: map(s[1]), w: 0.5, dashed: false, color: ink });
                }
            }
            let bottom = map([0.0, pv.bbox[0][1]])[1];
            prims.push(Prim::Text { at: [pos[i][0], bottom - 7.0], size: 3.2, text: dirs[i].to_uppercase(), anchor: 1, color: [80, 80, 80] });
        }
        Ok(match format {
            "pdf" => crate::drawing_export::pdf(&prims, sheet),
            "dxf" => crate::drawing_export::dxf(&prims).into_bytes(),
            "svg" => crate::drawing_export::svg(&prims, sheet).into_bytes(),
            _ => return Err("format must be pdf, svg or dxf".into()),
        })
    }
}

// ---------------------------------------------------------------- argument helpers

fn round(v: f64) -> f64 {
    // + 0.0 turns -0.0 into 0
    (v * 1e4).round() / 1e4 + 0.0
}

/// Numbers in a query result rounded like the other results.
fn round_json(v: Value) -> Value {
    match v {
        Value::Number(n) if n.is_f64() => json!(round(n.as_f64().unwrap_or(0.0))),
        Value::Array(a) => Value::Array(a.into_iter().map(round_json).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, x)| (k, round_json(x))).collect()),
        other => other,
    }
}

fn mat_json(m: glam::DMat3) -> Value {
    json!([m.row(0).to_array().map(round), m.row(1).to_array().map(round), m.row(2).to_array().map(round)])
}

/// Check a call's arguments against the tool's schema.
fn check_args(name: &str, a: &Value) -> R<()> {
    static SCHEMAS: std::sync::OnceLock<HashMap<String, Value>> = std::sync::OnceLock::new();
    let schemas = SCHEMAS.get_or_init(|| {
        tools().as_array().into_iter().flatten().map(|t| (t["name"].as_str().unwrap_or("").to_string(), t["inputSchema"].clone())).collect()
    });
    let schema = schemas.get(name).ok_or_else(|| format!("unknown tool {name}{}", schema::did_you_mean(name, schemas.keys().map(|k| k.as_str()))))?;
    if !a.is_object() {
        return Err("arguments must be an object".into());
    }
    schema::validate(schema, a, "").map_err(|e| format!("{name}: {e}"))
}

/// Create the folder of an output file.
pub(crate) fn make_parent(path: &str) -> R<()> {
    match std::path::Path::new(path).parent() {
        Some(d) if !d.as_os_str().is_empty() => std::fs::create_dir_all(d).map_err(|e| format!("can't write {path}: can't create {}: {e}", d.display())),
        _ => Ok(()),
    }
}

/// Write an output file, creating its folder; errors name the path.
pub(crate) fn write_out(path: &str, data: impl AsRef<[u8]>) -> R<()> {
    make_parent(path)?;
    std::fs::write(path, data).map_err(|e| format!("can't write {path}: {e}"))
}

/// `direction` [x, y, z] for renders: not zero.
pub(crate) fn direction_arg(a: &Value) -> R<Option<[f64; 3]>> {
    let Some(d) = a.get("direction") else { return Ok(None) };
    let v = pt3(d).ok_or("direction: [x, y, z]")?;
    if v.length() < 1e-12 {
        return Err("direction: [x, y, z], not zero".into());
    }
    Ok(Some(v.to_array()))
}

/// A feature's params for describe: imported files are summarized (size and hash), not inlined.
fn described_params(op: &Op) -> Value {
    let mut v = serde_json::to_value(op).unwrap_or_default();
    if let Op::Import { .. } = op {
        for k in ["data", "step"] {
            if let Some(s) = v.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()) {
                // FNV-1a, to tell files apart
                let h = s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
                v[k] = json!(format!("<{} bytes, not shown; fnv1a64 {h:016x}>", s.len()));
            }
        }
    }
    v
}

fn check_var_name(name: &str) -> R<()> {
    let ok = name.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        return Err(format!("variable name {name:?}: a letter or _, then letters, digits or _"));
    }
    // units and constants would be read as such in expressions
    if ["mm", "cm", "m", "in", "inch", "ft", "deg", "rad", "pi", "PI", "e"].contains(&name) {
        return Err(format!("variable name {name:?} is a unit or constant in expressions; pick another"));
    }
    Ok(())
}

/// A constraint's keys, the points and curves it names, and its dimension's value.
fn check_constraint(sk: &Sketch, c: &Value, k: &Constraint, env: &crate::expr::Env) -> R<()> {
    let parsed = serde_json::to_value(k).unwrap_or_default();
    let mut keys: Vec<&str> = parsed.as_object().into_iter().flat_map(|o| o.keys().map(|k| k.as_str())).collect();
    keys.extend(["type", "value"]);
    if k.dim().is_some() {
        keys.push("driven");
    }
    if c.get("type").and_then(|t| t.as_str()) == Some("coincident") {
        keys.extend(["a", "b"]);
    }
    for key in c.as_object().into_iter().flat_map(|o| o.keys()) {
        if !keys.contains(&key.as_str()) {
            return Err(format!("unknown key '{key}'{}; it takes {}", schema::did_you_mean(key, keys.iter().copied()), keys.join(", ")));
        }
    }
    for f in ["p", "l", "c", "a", "b", "q", "m"] {
        let Some(id) = parsed.get(f).and_then(|x| x.as_u64()).map(|x| x as Id) else { continue };
        let (point, curve) = (sk.point(id).is_some(), sk.curves.iter().any(|c| c.id == id));
        let coincident = c.get("type").and_then(|t| t.as_str()) == Some("coincident");
        let (ok, what) = match f {
            "p" | "q" => (point, "point"),
            "l" if coincident => (point || curve, "point or curve"),
            "l" | "c" | "m" => (curve, "curve"),
            _ => (point || curve, "point or curve"),
        };
        if !ok {
            return Err(format!("{f}: no {what} {id} in this sketch (sketch_info lists the ids)"));
        }
    }
    if let Some(d) = k.dim() {
        let kind = if k.is_angle() { crate::expr::Kind::Angle } else { crate::expr::Kind::Length };
        let v = crate::expr::eval(&d.expr, kind, env).map_err(|e| format!("value {:?}: {e}", d.expr))?;
        if matches!(k, Constraint::Length { .. } | Constraint::Radius { .. } | Constraint::Diameter { .. }) && v <= 0.0 {
            return Err(format!("value {:?} must be more than 0", d.expr));
        }
    }
    Ok(())
}

/// Sizes and counts that only make sense positive, checked when their expressions evaluate (an
/// expression that doesn't is reported by the rebuild as the feature's error).
fn check_values(op: &Op, doc: &Document) -> R<()> {
    use crate::expr::Kind;
    let fields: &[(&str, Kind)] = match op {
        Op::Fillet { variable, .. } => {
            if *variable {
                &[("radius", Kind::Length), ("end_radius", Kind::Length)]
            } else {
                &[("radius", Kind::Length)]
            }
        }
        Op::Chamfer { .. } => &[("distance", Kind::Length)],
        Op::Hole { .. } => &[("diameter", Kind::Length)],
        Op::Helix { .. } => &[("radius", Kind::Length), ("pitch", Kind::Length), ("turns", Kind::Count)],
        Op::LinearPattern { .. } => &[("count", Kind::Count), ("count2", Kind::Count)],
        Op::CircularPattern { .. } | Op::CurvePattern { .. } => &[("count", Kind::Count)],
        _ => &[],
    };
    let (vars, _) = doc.var_values();
    let env = crate::expr::Env { vars: &vars, units: doc.units };
    let v = serde_json::to_value(op).unwrap_or_default();
    for (f, kind) in fields {
        let Some(src) = v.get(*f).and_then(|x| x.as_str()) else { continue };
        if let Ok(x) = crate::expr::eval(src, *kind, &env) {
            if x <= 0.0 || (*kind == Kind::Count && x < 1.0) {
                return Err(format!("{} {f} must be more than 0 (got {src})", op.kind()));
            }
        }
    }
    Ok(())
}

/// Sketch item types and the keys each takes (besides type and construction).
const SKETCH_ITEMS: [(&str, &[&str]); 13] = [
    ("point", &["at"]),
    ("line", &["from", "to"]),
    ("polyline", &["points", "closed"]),
    ("rect", &["corner1", "corner2"]),
    ("center_rect", &["center", "corner"]),
    ("circle", &["center", "radius"]),
    ("arc", &["center", "start", "end"]),
    ("ellipse", &["center", "major_end", "minor_radius"]),
    ("elliptical_arc", &["center", "major_end", "minor_radius", "start_angle", "end_angle"]),
    ("spline", &["points", "closed"]),
    ("polygon", &["center", "radius", "sides"]),
    ("text", &["at", "text", "height"]),
    ("polygon_points", &["points", "closed"]),
];

pub(crate) fn str_arg(a: &Value, k: &str) -> R<String> {
    a.get(k).and_then(|v| v.as_str()).map(String::from).ok_or(format!("give {k}"))
}

fn id_arg(a: &Value, k: &str) -> R<u64> {
    a.get(k).and_then(|v| v.as_u64()).ok_or(format!("give {k} (a number)"))
}

fn val_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn pt3(v: &Value) -> Option<DVec3> {
    let a = v.as_array()?;
    Some(DVec3::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?))
}

fn pt2(v: &Value) -> R<DVec2> {
    let a = v.as_array().ok_or("points are [x, y]")?;
    Ok(DVec2::new(a.first().and_then(|x| x.as_f64()).ok_or("x")?, a.get(1).and_then(|x| x.as_f64()).ok_or("y")?))
}

fn parse_plane(v: &Value) -> R<Ref> {
    if let Some(s) = v.as_str() {
        let p = match s.to_lowercase().as_str() {
            "top" | "xy" => OriginPlane::Top,
            "front" | "xz" => OriginPlane::Front,
            "right" | "yz" => OriginPlane::Right,
            _ => return Err("plane must be top, front or right, or a ref".into()),
        };
        return Ok(Ref::OriginPlane { plane: p });
    }
    serde_json::from_value(v.clone()).map_err(|e| format!("bad plane ref: {e}"))
}

/// Constraints use the document format, with `value` accepted for dimensions.
fn parse_constraint(c: &Value) -> R<Constraint> {
    let mut c = c.clone();
    if let Some(o) = c.as_object_mut() {
        if o.get("type").and_then(|t| t.as_str()) == Some("coincident") {
            // point on curve when b is a curve; points are merged by the caller
            let a = o.get("a").cloned().unwrap_or_default();
            let b = o.get("b").cloned().unwrap_or_default();
            o.insert("type".into(), json!("point_on_line"));
            o.insert("p".into(), a);
            o.insert("l".into(), b);
        }
        let driven = o.remove("driven");
        if let Some(v) = o.remove("value") {
            o.insert("dim".into(), json!({"expr": val_str(&v), "offset": [0.0, 0.0]}));
        }
        if let (Some(d), Some(dim)) = (driven, o.get_mut("dim").and_then(|d| d.as_object_mut())) {
            dim.insert("driven".into(), d);
        }
    }
    let k: Constraint = serde_json::from_value(c.clone()).map_err(|e| format!("bad constraint {c}: {e}"))?;
    let _ = Dim::new("", [0.0; 2]);
    Ok(k)
}

/// Merge user params into an operation's JSON. Numbers given for text
/// fields (expressions) become text; `{"sketch": id}` in a profile list means
/// every region of that sketch.
fn merge(base: &mut Value, params: &Value, s: &Session) -> R<()> {
    let (Some(b), Some(p)) = (base.as_object_mut(), params.as_object()) else { return Err("params must be an object".into()) };
    let kind = b.get("type").and_then(|t| t.as_str()).unwrap_or("feature").to_string();
    for k in p.keys() {
        if k == "type" || !b.contains_key(k) {
            let names: Vec<&str> = b.keys().map(|k| k.as_str()).filter(|k| *k != "type").collect();
            return Err(format!("{kind} has no param '{k}'{}; it takes {}", schema::did_you_mean(k, names.iter().copied()), names.join(", ")));
        }
    }
    for (k, v) in p {
        let v = expand_refs(v, s)?;
        match (b.get(k), &v) {
            (Some(Value::String(_)), Value::Number(n)) => {
                b.insert(k.clone(), json!(n.to_string()));
            }
            _ => {
                b.insert(k.clone(), v);
            }
        }
    }
    Ok(())
}

fn expand_refs(v: &Value, s: &Session) -> R<Value> {
    let Some(arr) = v.as_array() else { return Ok(v.clone()) };
    let mut out = vec![];
    for item in arr {
        if let (Some(sid), None) = (item.get("sketch").and_then(|x| x.as_u64()), item.get("kind")) {
            let (sk, _) = s.sketch(sid)?;
            let regions = find_regions(sk);
            let all = item.get("all").and_then(|a| a.as_bool()).unwrap_or(false);
            for r in &regions {
                // like a part outline: regions that are holes in another region stay empty
                let is_hole = regions.iter().any(|o| o.holes.iter().any(|h| crate::regions::point_in_poly(r.sample, &h.poly)));
                if all || !is_hole {
                    out.push(json!({"kind": "region", "sketch": sid, "point": [r.sample.x, r.sample.y]}));
                }
            }
        } else {
            out.push(item.clone());
        }
    }
    Ok(Value::Array(out))
}

fn build_op(kind: &str, params: &Value, s: &Session) -> R<Op> {
    let base = Op::new_default(kind).ok_or(format!("unknown feature kind {kind}"))?;
    let mut v = serde_json::to_value(&base).map_err(|e| e.to_string())?;
    merge(&mut v, params, s)?;
    serde_json::from_value(v).map_err(|e| format!("bad params for {kind}: {e}"))
}

fn obj(r: &RebuildResult) -> String {
    let mut out = String::new();
    let mut base = 1usize;
    for b in &r.bodies {
        out.push_str(&format!("o {}\n", b.name.replace(' ', "_")));
        for v in b.positions.chunks(3) {
            out.push_str(&format!("v {} {} {}\n", v[0], v[1], v[2]));
        }
        for t in b.indices.chunks(3) {
            out.push_str(&format!("f {} {} {}\n", t[0] as usize + base, t[1] as usize + base, t[2] as usize + base));
        }
        base += b.positions.len() / 3;
    }
    out
}

// ---------------------------------------------------------------- software renderer

/// Shaded image of the parts (and sketches) from a standard view.
/// `view`: front, back, top, bottom, left, right, or iso / iso_front_right, iso_front_left,
/// iso_back_right, iso_back_left (and *_below variants); `direction` (from the part towards the
/// camera) overrides it. `sketches`: all, unused (not consumed by a feature) or none.
pub fn render_png(doc: &Document, r: &RebuildResult, view: &str, direction: Option<[f64; 3]>, sketches: &str, w: u32, h: u32) -> R<Vec<u8>> {
    // sketches that some feature uses are hidden by default, as in Onshape
    let used: std::collections::HashSet<FeatId> = if sketches == "unused" {
        doc.features
            .iter()
            .filter(|f| !matches!(f.op, Op::Sketch { .. }))
            .flat_map(|f| {
                let v = serde_json::to_value(&f.op).unwrap_or_default();
                let mut ids = vec![];
                collect_sketch_ids(&v, &mut ids);
                ids
            })
            .collect()
    } else {
        Default::default()
    };
    let sketch_lines: Vec<Vec<DVec3>> = doc
        .features
        .iter()
        .filter(|f| sketches != "none" && !used.contains(&f.id) && !f.hidden)
        .filter_map(|f| if let Op::Sketch { sketch, frame, .. } = &f.op { Some((sketch, frame)) } else { None })
        .flat_map(|(sk, fr)| sk.curves.iter().map(move |c| sk.tessellate(c, 1.0).into_iter().map(|p| fr.to_world(p)).collect::<Vec<_>>()))
        .collect();
    let parts: HashMap<String, [u8; 3]> = doc.parts.iter().filter_map(|(k, p)| p.color.map(|c| (k.clone(), c))).collect();
    let items: Vec<(&crate::kernel::BodyMesh, crate::assembly::Pose, [u8; 3])> =
        r.bodies.iter().map(|b| (b, crate::assembly::Pose::default(), parts.get(&crate::doc::body_key(b.id)).copied().unwrap_or([0x9a, 0xa9, 0xba]))).collect();
    raster_scene(&items, &sketch_lines, view, direction, w, h)
}

/// Shade posed meshes (and draw 3D polylines on top) into a PNG.
pub fn raster_scene(items: &[(&crate::kernel::BodyMesh, crate::assembly::Pose, [u8; 3])], sketch_lines: &[Vec<DVec3>], view: &str, direction: Option<[f64; 3]>, w: u32, h: u32) -> R<Vec<u8>> {
    use crate::camera::{Camera, StdView};
    let mut cam = Camera { size: glam::Vec2::new(w as f32, h as f32), ..Default::default() };
    let iso = |x: f32, y: f32, z: f32| glam::Vec3::new(x, y, z).normalize();
    let back = match direction {
        Some(d) => Some(glam::Vec3::new(d[0] as f32, d[1] as f32, d[2] as f32).normalize_or(glam::Vec3::ONE)),
        None => match view {
            "iso" | "iso_front_right" => Some(iso(1.0, -1.0, 1.0)),
            "iso_front_left" => Some(iso(-1.0, -1.0, 1.0)),
            "iso_back_right" => Some(iso(1.0, 1.0, 1.0)),
            "iso_back_left" => Some(iso(-1.0, 1.0, 1.0)),
            "iso_front_right_below" => Some(iso(1.0, -1.0, -1.0)),
            "iso_front_left_below" => Some(iso(-1.0, -1.0, -1.0)),
            "iso_back_right_below" => Some(iso(1.0, 1.0, -1.0)),
            "iso_back_left_below" => Some(iso(-1.0, 1.0, -1.0)),
            _ => None,
        },
    };
    match back {
        Some(b) => cam.set_orientation(b, if b.z.abs() > 0.99 { glam::Vec3::Y } else { glam::Vec3::Z }),
        None => cam.set_view(match view {
            "front" => StdView::Front,
            "top" => StdView::Top,
            "right" => StdView::Right,
            "back" => StdView::Back,
            "bottom" => StdView::Bottom,
            "left" => StdView::Left,
            _ => StdView::Iso,
        }),
    }
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for (b, pose, _) in items {
        for c in 0..8 {
            let p = pose.apply(DVec3::new(b.bbox[c & 1][0], b.bbox[(c >> 1) & 1][1], b.bbox[(c >> 2) & 1][2]));
            lo = lo.min(p);
            hi = hi.max(p);
        }
    }
    for l in sketch_lines {
        for p in l {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
    }
    if !lo.x.is_finite() {
        lo = DVec3::ZERO;
        hi = DVec3::splat(50.0);
    }
    cam.fit(lo.as_vec3(), hi.as_vec3());
    let (wu, hu) = (w as usize, h as usize);
    let bg = [0x20u8, 0x25, 0x2c];
    let mut px = vec![0u8; wu * hu * 3];
    for c in px.chunks_mut(3) {
        c.copy_from_slice(&bg);
    }
    let mut depth = vec![f32::INFINITY; wu * hu];
    let vp = cam.view_proj();
    let proj = |p: glam::Vec3| -> glam::Vec3 {
        let n = vp.project_point3(p);
        glam::Vec3::new((n.x + 1.0) * 0.5 * w as f32, (1.0 - n.y) * 0.5 * h as f32, n.z)
    };
    let light = (cam.back + glam::Vec3::new(0.25, 0.15, 0.45)).normalize();
    for (b, pose, base) in items {
        let at = |i: u32| pose.apply(DVec3::new(b.positions[3 * i as usize] as f64, b.positions[3 * i as usize + 1] as f64, b.positions[3 * i as usize + 2] as f64)).as_vec3();
        for t in b.indices.chunks(3) {
            let (a, bb, c) = (at(t[0]), at(t[1]), at(t[2]));
            let mut n = (bb - a).cross(c - a).normalize_or_zero();
            if n.dot(cam.back) < 0.0 {
                n = -n;
            }
            let l = 0.42 + 0.38 * n.dot(light).max(0.0) + 0.25 * n.dot(cam.back).max(0.0).powi(6);
            let col = [(base[0] as f32 * l).min(255.0) as u8, (base[1] as f32 * l).min(255.0) as u8, (base[2] as f32 * l).min(255.0) as u8];
            raster_tri(&mut px, &mut depth, wu, hu, [proj(a), proj(bb), proj(c)], col);
        }
    }
    // edges, then sketches on top
    for (b, pose, _) in items {
        for e in &b.edges {
            for s in e.pts.windows(2) {
                let (p, q) = (pose.apply(DVec3::from(s[0].map(|v| v as f64))), pose.apply(DVec3::from(s[1].map(|v| v as f64))));
                raster_line(&mut px, &depth, wu, hu, proj(p.as_vec3()), proj(q.as_vec3()), [0x1c, 0x20, 0x26], true);
            }
        }
    }
    for l in sketch_lines {
        for s in l.windows(2) {
            raster_line(&mut px, &depth, wu, hu, proj(s[0].as_vec3()), proj(s[1].as_vec3()), [0x56, 0xb6, 0xff], true);
        }
    }
    let mut out = vec![];
    image::write_buffer_with_format(&mut std::io::Cursor::new(&mut out), &px, w, h, image::ColorType::Rgb8, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(out)
}

fn collect_sketch_ids(v: &Value, out: &mut Vec<FeatId>) {
    match v {
        Value::Object(m) => {
            if let (Some(k), Some(id)) = (m.get("kind").and_then(|k| k.as_str()), m.get("sketch").and_then(|s| s.as_u64())) {
                if matches!(k, "region" | "sketch_curve" | "sketch_point") {
                    out.push(id);
                }
            }
            m.values().for_each(|x| collect_sketch_ids(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| collect_sketch_ids(x, out)),
        _ => {}
    }
}

/// Merge points closer than 1 µm; returns (removed, kept) pairs.
fn merge_coincident(sk: &mut Sketch) -> Vec<(Id, Id)> {
    let mut pairs = vec![];
    let pts: Vec<(Id, DVec2)> = sk.points.iter().map(|p| (p.id, p.v())).collect();
    for i in 0..pts.len() {
        for j in 0..i {
            if (pts[i].1 - pts[j].1).length() < 1e-3 && sk.point(pts[i].0).is_some() && sk.point(pts[j].0).is_some() {
                sk.merge_point(pts[i].0, pts[j].0);
                pairs.push((pts[i].0, pts[j].0));
            }
        }
    }
    pairs
}

/// Binary STL of the parts' meshes.
fn stl_binary(r: &RebuildResult) -> Vec<u8> {
    let n: usize = r.bodies.iter().map(|b| b.indices.len() / 3).sum();
    let mut out = vec![0u8; 80];
    out[..24].copy_from_slice(b"cad-app binary STL     \0");
    out.extend((n as u32).to_le_bytes());
    for b in &r.bodies {
        for t in b.indices.chunks(3) {
            let v = |i: u32| glam::Vec3::new(b.positions[3 * i as usize], b.positions[3 * i as usize + 1], b.positions[3 * i as usize + 2]);
            let (p0, p1, p2) = (v(t[0]), v(t[1]), v(t[2]));
            let nrm = (p1 - p0).cross(p2 - p0).normalize_or_zero();
            for q in [nrm, p0, p1, p2] {
                for c in [q.x, q.y, q.z] {
                    out.extend(c.to_le_bytes());
                }
            }
            out.extend([0u8, 0]);
        }
    }
    out
}

fn raster_tri(px: &mut [u8], depth: &mut [f32], w: usize, h: usize, p: [glam::Vec3; 3], col: [u8; 3]) {
    let minx = p.iter().map(|q| q.x).fold(f32::INFINITY, f32::min).floor().max(0.0) as usize;
    let maxx = p.iter().map(|q| q.x).fold(f32::NEG_INFINITY, f32::max).ceil().min(w as f32 - 1.0);
    let miny = p.iter().map(|q| q.y).fold(f32::INFINITY, f32::min).floor().max(0.0) as usize;
    let maxy = p.iter().map(|q| q.y).fold(f32::NEG_INFINITY, f32::max).ceil().min(h as f32 - 1.0);
    if maxx < 0.0 || maxy < 0.0 {
        return;
    }
    let area = (p[1].x - p[0].x) * (p[2].y - p[0].y) - (p[2].x - p[0].x) * (p[1].y - p[0].y);
    if area.abs() < 1e-9 {
        return;
    }
    for y in miny..=maxy as usize {
        for x in minx..=maxx as usize {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let w0 = ((p[1].x - fx) * (p[2].y - fy) - (p[2].x - fx) * (p[1].y - fy)) / area;
            let w1 = ((p[2].x - fx) * (p[0].y - fy) - (p[0].x - fx) * (p[2].y - fy)) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let z = w0 * p[0].z + w1 * p[1].z + w2 * p[2].z;
            let i = y * w + x;
            if z < depth[i] {
                depth[i] = z;
                px[3 * i..3 * i + 3].copy_from_slice(&col);
            }
        }
    }
}

fn raster_line(px: &mut [u8], depth: &[f32], w: usize, h: usize, a: glam::Vec3, b: glam::Vec3, col: [u8; 3], test: bool) {
    let n = ((b.x - a.x).abs().max((b.y - a.y).abs()).ceil() as usize).clamp(1, 20000);
    for k in 0..=n {
        let t = k as f32 / n as f32;
        let p = a.lerp(b, t);
        let (x, y) = (p.x.round() as i64, p.y.round() as i64);
        if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
            continue;
        }
        let i = y as usize * w + x as usize;
        if !test || p.z <= depth[i] + 2e-5 {
            px[3 * i..3 * i + 3].copy_from_slice(&col);
        }
    }
}

// ---------------------------------------------------------------- tool catalogue

pub(crate) fn tools() -> Value {
    let obj = |props: Value, req: &[&str]| json!({"type": "object", "properties": props, "required": req});
    json!([
        {"name": "new_document", "description": "Start a new empty Part Studio. Units: mm (default), cm, m, in, ft.", "inputSchema": obj(json!({"units": {"type": "string"}}), &[])},
        {"name": "open_document", "description": "Open a .json document; its first Part Studio becomes active (see describe for the other tabs).", "inputSchema": obj(json!({"path": {"type": "string"}}), &["path"])},
        {"name": "save_document", "description": "Save the whole document (Part Studios, Assemblies, Drawings) as a .json file the cad-app GUI can open.", "inputSchema": obj(json!({"path": {"type": "string"}}), &[])},
        {"name": "describe", "description": "Features (with their full params; imported files show only their size and a hash), variables, parts (name, volume, area, bbox, centroid) and feature errors.", "inputSchema": obj(json!({}), &[])},
        {"name": "set_variable", "description": "Create or change a variable usable in any value, e.g. name 'w', expr '40' or 'h * 2'.", "inputSchema": obj(json!({"name": {"type": "string"}, "expr": {"type": ["string", "number"]}}), &["name", "expr"])},
        {"name": "create_sketch", "description": "New sketch on 'top' (XY), 'front' (XZ, normal -Y), 'right' (YZ), or a ref such as a planar face ref from list_geometry or {\"kind\":\"feature_plane\",\"feature\":id}. Returns sketch_id and the plane frame (sketch x/y axes in 3D).", "inputSchema": obj(json!({"plane": {}, "name": {"type": "string"}}), &[])},
        {"name": "sketch_add", "description": "Add geometry to a sketch; coordinates are [x, y] in mm in the sketch plane. Item types: point{at}, line{from,to}, polyline{points,closed}, rect{corner1,corner2}, center_rect{center,corner}, circle{center,radius}, arc{center,start,end} (counter-clockwise), ellipse{center,major_end,minor_radius}, elliptical_arc{center,major_end,minor_radius,start_angle,end_angle} (degrees, from the major axis), spline{points,closed}, polygon{center,radius,sides}, text{at,text,height}. Any item may set construction:true. Points at the same position (within 1 µm), also across items and with existing points, are merged into one. Returns created ids and the sketch summary with regions.", "inputSchema": obj(json!({"sketch_id": {"type": "integer"}, "items": {"type": "array"}}), &["sketch_id", "items"])},
        {"name": "sketch_constrain", "description": "Add constraints by point/curve ids. Types: coincident{a,b} (points, or point a on curve b), horizontal{l}, vertical{l}, horizontal_pts{a,b}, vertical_pts{a,b}, parallel{a,b}, perpendicular{a,b}, tangent{a,b}, equal{a,b}, midpoint{p,l}, symmetric{a,b,l}, fix{p,x,y}; dimensions take value (number or expression): distance{a,b}, axis_distance{a,b,axis:0|1}, point_line_distance{p,l}, length{l}, radius{c}, diameter{c}, angle{a,b} (degrees); driven:true makes a dimension a reference that only measures. equal_distance{p,l,q,m}: point p as far from line l as point q from line m.", "inputSchema": obj(json!({"sketch_id": {"type": "integer"}, "constraints": {"type": "array"}}), &["sketch_id", "constraints"])},
        {"name": "sketch_info", "description": "Points, curves, degrees of freedom and closed regions (with ready-made region refs) of a sketch.", "inputSchema": obj(json!({"sketch_id": {"type": "integer"}}), &["sketch_id"])},
        {"name": "add_feature", "description": "Add a feature. kind: extrude, revolve, sweep, loft, fillet, chamfer, shell, hole, draft, thicken, rib, moveFace, deleteFace, helix, plane, linearPattern, circularPattern, curvePattern {kind:'part'|'feature', bodies, path:[curve refs], count, align}, pointPattern {kind, bodies, points:[sketch_point refs], first point = where the original is}, mirror, boolean, split, transform, deleteBody, sheetMetal, heal {bodies, tolerance}, custom. params override the defaults (see describe for the shape). Refs: {kind:'region',sketch,point}, or {sketch:id} for every region of a sketch except holes ({sketch:id, all:true} includes holes); {kind:'face',point,normal}; {kind:'edge',point}; {kind:'body',body:[f,i]}; {kind:'axis',axis:0|1|2}; {kind:'origin_plane',plane:'Top'|'Front'|'Right'}; {kind:'sketch_curve',sketch,curve}; {kind:'sketch_point',sketch,point}; {kind:'feature',feature}; {kind:'curve',feature}. Values are expressions in document units. Examples: extrude {profiles:[{sketch:3}], depth:10, op:'new'|'add'|'remove'|'intersect', end:'blind'|'symmetric'|'through_all'|'up_to_face'|'up_to_next'|'up_to_vertex', up_to:ref, draft:deg}; fillet {edges:[edge refs], radius:2}; chamfer {edges, distance, mode:'equal'|'two'|'angle', distance2, angle}; hole {points:[sketch_point refs], diameter:5, through:true}. feature_defaults gives any kind's params and defaults; unknown param names are rejected. A feature that fails to build stays in the history (as in the editor) and the result has ok:false and new_errors; rollback:true undoes the call instead.", "inputSchema": obj(json!({"kind": {"type": "string"}, "params": {"type": "object"}, "name": {"type": "string"}}), &["kind"])},
        {"name": "feature_defaults", "description": "A feature kind's params with their default values, the shape add_feature params and edit_feature take; without kind, the list of kinds.", "inputSchema": obj(json!({"kind": {"type": "string"}}), &[])},
        {"name": "edit_feature", "description": "Change a feature's params (same format as add_feature) or suppressed flag.", "inputSchema": obj(json!({"id": {"type": "integer"}, "params": {"type": "object"}, "suppressed": {"type": "boolean"}}), &["id"])},
        {"name": "delete_feature", "description": "Delete a feature.", "inputSchema": obj(json!({"id": {"type": "integer"}}), &["id"])},
        {"name": "import_file", "description": "Import a STEP, IGES, STL or OBJ file into the active Part Studio as a feature; the file is stored in the document. A .dxf or .svg drawing goes into a sketch instead: sketch_id (default a new sketch on plane, default top, named after the file), scale, at [x,y] offset; DXF lines, arcs, circles, polylines (with bulges), splines, ellipses and block inserts, in the file's units ($INSUNITS, unitless = mm); SVG paths in mm at 96 px/inch, Y up from the page bottom, curves as fixed outlines. STEP/IGES give exact solids. STL/OBJ meshes become solids (or surfaces if not closed); merge_faces (default true) joins coplanar triangles into flat faces you can sketch on. scale converts units, e.g. 25.4 for a file in inches. Then use list_geometry and add features as usual.", "inputSchema": obj(json!({"path": {"type": "string"}, "scale": {"type": ["number", "string"]}, "merge_faces": {"type": "boolean"}, "name": {"type": "string"}, "sketch_id": {"type": "integer"}, "plane": {}, "at": {"type": "array"}}), &["path"])},
        {"name": "printability", "description": "3D-printing check of the parts as they sit (Z up, build plate at the lowest point): area of downward faces steeper than the overhang limit (default 45° from vertical, these need supports) and of walls thinner than min_wall (default 0.8 mm), with example points.", "inputSchema": obj(json!({"overhang": {"type": "number"}, "min_wall": {"type": "number"}}), &[])},
        {"name": "send_to_slicer", "description": "Export the parts as STL (tolerance mm, default 0.01) and stage them in the orcaweb slicer (url defaults to the app's setting). Returns the page where the model can be sliced and sent to the printer.", "inputSchema": obj(json!({"url": {"type": "string"}, "tolerance": {"type": "number"}, "name": {"type": "string"}}), &[])},
        {"name": "set_part", "description": "Set a part's name, color (#rrggbb or [r,g,b]), material (PLA, ABS, PETG, Nylon, Aluminium 6061, Aluminium 7075, Steel, Stainless steel 303, Tungsten alloy, Urethane foam, Stainless steel, Brass, Copper, Titanium, Oak, Pine, …: gives mass in the BOM), finish (matte, satin, glossy, metal, glass) or hidden.", "inputSchema": obj(json!({"body": {"type": "array"}, "name": {"type": "string"}, "color": {}, "material": {"type": "string"}, "finish": {"type": "string"}, "hidden": {"type": "boolean"}}), &["body"])},
        {"name": "set_parts", "description": "set_part for many parts at once, with one rebuild: parts is a list of {body, name, color, material, finish, hidden}.", "inputSchema": obj(json!({"parts": {"type": "array"}}), &["parts"])},
        {"name": "add_decal", "description": "Lay a PNG/JPEG image on a flat face (a label or logo), centred on face.point. width in mm (height keeps the image's proportions), angle turns it about the face normal. Shown in the editor; not exported.", "inputSchema": obj(json!({"image": {"type": "string"}, "face": {"type": "object"}, "width": {"type": "number"}, "angle": {"type": "number"}}), &["image", "face"])},
        {"name": "list_geometry", "description": "Faces and edges of the parts with refs ready for add_feature (fillet edges, shell faces, sketch on a face, …), plus normals, lengths and circle centres. Filters: type 'faces'|'edges', body [f,i], box [[xmin,ymin,zmin],[xmax,ymax,zmax]] (ref point inside), normal [x,y,z] (faces facing that way), planar true/false.", "inputSchema": obj(json!({"body": {"type": "array"}, "type": {"type": "string"}, "box": {"type": "array"}, "normal": {"type": "array"}, "planar": {"type": "boolean"}}), &[])},
        {"name": "render", "description": "Render a shaded PNG of the model. view: front, top, right, back, bottom, left, iso (= iso_front_right), iso_front_left, iso_back_right, iso_back_left, or any of those + '_below'. direction [x,y,z] (from the part towards the camera, front is -Y) overrides view. sketches: 'unused' (default: hide sketches a feature consumed), 'all' or 'none'. Returns the image; optionally saves it to path.", "inputSchema": obj(json!({"view": {"type": "string"}, "direction": {"type": "array"}, "sketches": {"type": "string"}, "width": {"type": "integer"}, "height": {"type": "integer"}, "path": {"type": "string"}}), &[])},
        {"name": "export", "description": "Export the parts: format step, iges, stl (binary; ascii:true for text) or obj.", "inputSchema": obj(json!({"format": {"type": "string"}, "path": {"type": "string"}, "ascii": {"type": "boolean"}}), &["format", "path"])},
        {"name": "export_drawing", "description": "Export a drawing as pdf (all sheets), svg, dxf or png (one sheet: sheet 1, 2, …). With drawing (index from create_drawing) it exports that Drawing tab with its dimensions, notes and GD&T; without, a quick sheet with front, top, right and iso views of the active Part Studio (optional sheet size A4/A3/A2/A1/Letter/Tabloid, scale, title).", "inputSchema": obj(json!({"path": {"type": "string"}, "format": {"type": "string"}, "drawing": {"type": "integer"}, "sheet": {}, "scale": {"type": "number"}, "title": {"type": "string"}, "width": {"type": "integer"}}), &["path"])},
        {"name": "mass_properties", "description": "Exact mass properties of the active Part Studio's parts: volume, mass, centre of gravity and inertia tensor about the centre of gravity (g·mm²), from each part's material (set_part) or a material or density (g/cm³) for all; with several parts also their total. bodies limits it to some parts.", "inputSchema": obj(json!({"bodies": {"type": "array"}, "material": {"type": "string"}, "density": {"type": "number"}}), &[])},
        {"name": "ray_cast", "description": "Cast rays from origins along direction through the active Part Studio's parts (or bodies): every surface crossing with its distance, point and part, nearest first.", "inputSchema": obj(json!({"origins": {"type": "array"}, "direction": {"type": "array"}, "bodies": {"type": "array"}}), &["origins", "direction"])},
        {"name": "classify_points", "description": "Whether each point is in, on (within tolerance) or out of the active Part Studio's parts (or bodies), and which part.", "inputSchema": obj(json!({"points": {"type": "array"}, "tolerance": {"type": "number"}, "bodies": {"type": "array"}}), &["points"])},
        {"name": "define_custom_feature", "description": "Define (or replace) a scripted custom feature in the active Part Studio, then use it with add_feature kind 'custom', params {name, values: {param: expression}, refs: [point or face refs], op: new|add|remove|intersect}. The source is Rhai: parameters are variables (lengths mm, angles degrees, counts integers, bools); `points` and `normals` are arrays of [x,y,z] from refs. Shapes: cube(w,d,h), cube_centered(w,d,h), cylinder(r,h), cone(r1,r2,h), sphere(r), torus(R,r), prism([[x,y],...], h), revolve([[r,z],...], deg). Methods: .move(x,y,z), .rotate_x/y/z(deg), .rotate([ax,ay,az], deg), .mirror([nx,ny,nz]), .scale(f), .fillet(r), .chamfer(d), .place(point, normal), .volume(). a + b joins, a - b cuts, a & b intersects. Return a shape or an array of shapes. params: [{name, kind: length|angle|number|count|bool, default}]; selection: '' | 'points' | 'faces'.", "inputSchema": obj(json!({"name": {"type": "string"}, "params": {"type": "array"}, "selection": {"type": "string"}, "source": {"type": "string"}}), &["name", "source"])},
        {"name": "new_part_studio", "description": "Add another Part Studio to the document and make it active (sketches/features go there). Parts from different studios can be combined in assemblies; describe lists all studios, assemblies and drawings.", "inputSchema": obj(json!({"name": {"type": "string"}}), &[])},
        {"name": "switch_part_studio", "description": "Make another Part Studio (by index) the active one.", "inputSchema": obj(json!({"index": {"type": "integer"}}), &["index"])},
        {"name": "create_assembly", "description": "Add an Assembly tab. Returns its index.", "inputSchema": obj(json!({"name": {"type": "string"}}), &[])},
        {"name": "list_standard_parts", "description": "List the standard parts library: ISO/DIN screws, nuts, washers, dowel pins, shafts, ball/thrust/linear bearings, NEMA stepper motors and T-slot extrusions, with their sizes and placement conventions.", "inputSchema": obj(json!({}), &[])},
        {"name": "insert_standard_part", "description": "Insert a standard part into an assembly (default assembly 0, made if there is none). standard: key from list_standard_parts (e.g. \"ISO 4762\", \"ISO 4032\", \"Bearing\", \"NEMA\"); size: label (\"M5\", \"608\", \"NEMA 17\", \"2020\") or number; length in mm for screws, pins, shafts, motors and extrusions. Parts are built parametrically in the \"Standard parts\" Part Studio (one per size and length, reused). Takes assembly_insert's position, rotate, fixed and name. Returns the instance id.", "inputSchema": obj(json!({"standard": {"type": "string"}, "size": {}, "length": {"type": "number"}, "assembly": {"type": "integer"}, "position": {"type": "array"}, "rotate": {"type": "object"}, "fixed": {"type": "boolean"}, "name": {"type": "string"}}), &["standard"])},
        {"name": "assembly_insert", "description": "Insert an instance into an assembly: a part (studio index, body [f,i] from describe; default the active studio's first part) or a whole other assembly as a rigid sub-assembly (sub_assembly: index). Optional position [x,y,z], rotate {axis:[x,y,z], angle: degrees}, fixed (the first instance is fixed by default), name. Returns the instance id.", "inputSchema": obj(json!({"assembly": {"type": "integer"}, "studio": {"type": "integer"}, "body": {"type": "array"}, "sub_assembly": {"type": "integer"}, "position": {"type": "array"}, "rotate": {"type": "object"}, "fixed": {"type": "boolean"}, "name": {"type": "string"}}), &["assembly"])},
        {"name": "assembly_mate", "description": "Mate two instances and solve. kind: fastened, revolute, slider, cylindrical, planar, ball. a and b each: {instance, face:[x,y,z]} or {instance, edge:[x,y,z]} with a point on that face/edge in the part's own coordinates (as list_geometry gives them; a circular edge or cylindrical face gives its axis), or {instance, origin:[..], z:[..], x:[..]} explicitly. Mate connectors' Z axes end up opposed (faces touch); flip:true aligns them. A face connector sits at the picked point and stays attached to that face when the part is rebuilt (mates that can no longer be met are reported as failed). Optional offset (mm along Z), offset_x, offset_y, rotation (degrees), limits [min,max] for the free motion. Reports instance poses and failed mates.", "inputSchema": obj(json!({"assembly": {"type": "integer"}, "kind": {"type": "string"}, "a": {"type": "object"}, "b": {"type": "object"}, "flip": {"type": "boolean"}, "offset": {"type": "number"}, "offset_x": {"type": "number"}, "offset_y": {"type": "number"}, "rotation": {"type": "number"}, "limits": {"type": "array"}, "name": {"type": "string"}}), &["assembly", "kind", "a", "b"])},
        {"name": "assembly_move", "description": "Move/rotate an instance (position, rotate {axis, angle}), set fixed or hidden, or delete:true; mates are re-solved.", "inputSchema": obj(json!({"assembly": {"type": "integer"}, "instance": {"type": "integer"}, "position": {"type": "array"}, "rotate": {"type": "object"}, "fixed": {"type": "boolean"}, "hidden": {"type": "boolean"}, "delete": {"type": "boolean"}}), &["assembly", "instance"])},
        {"name": "assembly_relation", "description": "Couple two mates: gear (two revolute mates, ratio), rack (revolute mate_a drives slider mate_b, ratio = pitch radius mm) or screw (cylindrical mate_a, ratio = pitch mm/turn).", "inputSchema": obj(json!({"assembly": {"type": "integer"}, "kind": {"type": "string"}, "mate_a": {"type": "integer"}, "mate_b": {"type": "integer"}, "ratio": {"type": "number"}, "reverse": {"type": "boolean"}}), &["assembly", "kind", "mate_a"])},
        {"name": "assembly_explode", "description": "Set the exploded view steps: [{instances:[ids], direction:[x,y,z], distance: mm}], applied in order.", "inputSchema": obj(json!({"assembly": {"type": "integer"}, "steps": {"type": "array"}}), &["assembly", "steps"])},
        {"name": "assembly_info", "description": "Instances (poses, world bounding boxes), mates, bill of materials and explode steps. interference:true also lists overlapping instances with their overlap volume.", "inputSchema": obj(json!({"assembly": {"type": "integer"}, "interference": {"type": "boolean"}}), &["assembly"])},
        {"name": "export_web", "description": "Save an interactive 3D page (.html, opens in a browser): with assembly, its slider, cylindrical and revolute mates move within their limits (sliders, dragging parts, arrow keys), gear and rack relations follow, explode steps drive an explode slider; without, the parts of a Part Studio (studio, default the active one). Also section planes, ghosting by material and part details. inline:false writes a .json and .bin beside the page (serve them over http).", "inputSchema": obj(json!({"path": {"type": "string"}, "assembly": {"type": "integer"}, "studio": {"type": "integer"}, "inline": {"type": "boolean"}}), &["path"])},
        {"name": "render_assembly", "description": "Shaded PNG of an assembly (view/direction/width/height/path as in render). explode: 0..1 runs through the explode steps (or spreads parts out by that factor when there are none).", "inputSchema": obj(json!({"assembly": {"type": "integer"}, "view": {"type": "string"}, "direction": {"type": "array"}, "explode": {"type": "number"}, "width": {"type": "integer"}, "height": {"type": "integer"}, "path": {"type": "string"}}), &["assembly"])},
        {"name": "create_drawing", "description": "Add a Drawing tab (sheet A4/A3/A2/A1/Letter/Tabloid, default A3) with front, top, right and iso views of a Part Studio (standard_views:false for an empty sheet). Returns its index and drawing_info.", "inputSchema": obj(json!({"name": {"type": "string"}, "sheet": {"type": "string"}, "studio": {"type": "integer"}, "standard_views": {"type": "boolean"}}), &[])},
        {"name": "drawing_add", "description": "Add items to a drawing. Types: view{studio, body (one part of the studio, default all), or assembly (instances at their poses), balloons (assembly views), dir: front|top|right|back|bottom|left|iso|section:front|section:top|section:right, through (section cut offset mm from the model centre), letter, position:[x,y] sheet mm, scale, hidden_lines, sheet}; section{view, at (view coord of the cut), line: horizontal|vertical, letter, scale, position} adds a lettered section A-A with a cutting line on the parent view; detail{view, center:[x,y] view coords, radius, position, scale, letter}; dimension{view, kind: horizontal|vertical|aligned|radius|diameter|angle, a, b (view coords from drawing_info; radius/diameter: a = centre, b = point on circle or radius:r; angle: a-b first line, line2:[[x,y],[x,y]]), offset, prefix like '4× ', suffix, tolerance: number (±) or {plus, minus} or {limits:true, plus, minus}} - dimensions attach to model edges and follow model changes; auto_dimensions{view} (overall size, hole positions from the lower-left corner, hole callouts like 2× Ø10 THRU ⌴Ø16 ↧5; regenerated when the model changes); title{part, part_no, material, revision, date, drawn_by, tolerance, projection: first|third, decimals}; note{text, position, sheet}; gdt{kind: position|flatness|straightness|circularity|cylindricity|perpendicularity|parallelism|angularity|profileLine|profileSurface|concentricity|symmetry|runout|totalRunout, tolerance like 'Ø0.1', datums 'A B', position, leader:[x,y] sheet mm, sheet}; datum{letter, position, leader}; bom{assembly, position, sheet}; sheet{} adds a sheet. sheet numbers start at 1.", "inputSchema": obj(json!({"drawing": {"type": "integer"}, "items": {"type": "array"}}), &["drawing", "items"])},
        {"name": "drawing_info", "description": "A drawing's views (with dimensionable points, straight lines and circles in view coordinates), dimensions, notes and GD&T.", "inputSchema": obj(json!({"drawing": {"type": "integer"}}), &["drawing"])},
        {"name": "render_drawing", "description": "PNG of one drawing sheet (sheet: 1-based, width px) to check the layout.", "inputSchema": obj(json!({"drawing": {"type": "integer"}, "sheet": {"type": "integer"}, "width": {"type": "integer"}, "path": {"type": "string"}}), &["drawing"])}
    ])
    .as_array()
    .cloned()
    .map(|mut v| {
        v.extend(cam::tools());
        v.extend(fea::tools());
        v.extend(pcb::tools());
        v.extend(circuit::tools());
        schema::complete(&mut v);
        Value::Array(v)
    })
    .unwrap_or_default()
}

// ---------------------------------------------------------------- MCP server

/// MCP protocol versions this server speaks, newest first.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Whether a tool result reports a failure that the call itself survived (a feature that
/// doesn't build).
pub fn failed(v: &Value) -> bool {
    v.get("ok") == Some(&json!(false)) && v.get("new_errors").is_some()
}

pub fn mcp_main() {
    // `--bridge ADDR --token T`: the cad-app chat panel started this server through Claude Code,
    // and tool calls go to the document open in that window
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let bridge = arg("--bridge").map(|addr| (addr, arg("--token").unwrap_or_default()));
    let mut link: Option<(std::net::TcpStream, std::io::BufReader<std::net::TcpStream>)> = None;
    let mut forward = |name: &str, args: &Value| -> R<Value> {
        let (addr, token) = bridge.as_ref().unwrap();
        if link.is_none() {
            let c = std::net::TcpStream::connect(addr).map_err(|e| format!("the cad-app window is not reachable: {e}"))?;
            let r = std::io::BufReader::new(c.try_clone().map_err(|e| e.to_string())?);
            link = Some((c, r));
        }
        let (c, r) = link.as_mut().unwrap();
        let sent = writeln!(c, "{}", json!({"token": token, "name": name, "arguments": args})).and_then(|_| c.flush());
        let mut line = String::new();
        let got = sent.and_then(|_| r.read_line(&mut line));
        if got.is_err() || line.is_empty() {
            link = None;
            return Err("lost the connection to the cad-app window".into());
        }
        serde_json::from_str(&line).map_err(|e| e.to_string())
    };
    let bridged = bridge.is_some();
    let mut s = if bridged { None } else { Some(Session::new()) };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg = match serde_json::from_str::<Value>(&line) {
            Ok(m) if m.is_object() => m,
            Ok(_) => {
                let _ = writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "invalid request: expected a JSON-RPC object"}}));
                let _ = out.flush();
                continue;
            }
            Err(e) => {
                let _ = writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}));
                let _ = out.flush();
                continue;
            }
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let result: Option<Value> = match method {
            "initialize" => {
                // answer with the client's version when we speak it, else our newest
                let asked = msg["params"].get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
                let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).unwrap_or(&PROTOCOL_VERSIONS[0]);
                Some(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "cad-app", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "Parametric CAD (OpenCascade). Typical flow: create_sketch → sketch_add (+ sketch_constrain) → add_feature extrude with profiles [{sketch: id}] → list_geometry to get face/edge refs → more features → render to check → export / save_document. Lengths are mm. feature_defaults shows a feature kind's params. A call that leaves a feature failing returns isError with ok:false and new_errors (rollback:true undoes it)."
                }))
            }
            "ping" => Some(json!({})),
            "tools/list" if bridged => {
                // the window's document is opened and saved by the user
                let hidden = ["new_document", "open_document", "save_document"];
                Some(json!({"tools": tools().as_array().into_iter().flatten().filter(|t| !hidden.contains(&t["name"].as_str().unwrap_or(""))).collect::<Vec<_>>()}))
            }
            "tools/list" => Some(json!({"tools": tools()})),
            "tools/call" if bridged => {
                let name = msg["params"]["name"].as_str().unwrap_or("");
                let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
                Some(forward(name, &args).unwrap_or_else(|e| json!({"content": [{"type": "text", "text": e}], "isError": true})))
            }
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("");
                let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
                Some(match s.as_mut().unwrap().call(name, &args) {
                    Ok((v, img)) => {
                        // compact JSON: pretty-printing triples the tokens
                        let mut content = vec![json!({"type": "text", "text": v.to_string()})];
                        if let Some(png) = img {
                            content.push(json!({"type": "image", "mimeType": "image/png", "data": b64(&png)}));
                        }
                        json!({"content": content, "isError": failed(&v)})
                    }
                    Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
                })
            }
            _ => None,
        };
        if let Some(id) = id {
            let reply = match result {
                Some(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
                None => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unknown method {method}")}}),
            };
            let _ = writeln!(out, "{reply}");
            let _ = out.flush();
        }
    }
}

/// `cad-app run script.json`: a JSON array of {"tool": name, "args": {...}} steps. Relative input
/// files are also looked for next to the script; outputs go to the working directory. A step that
/// fails (including a feature that doesn't build) makes the exit status 1; `stop_on_error` stops there.
pub fn run_script(path: &str, stop_on_error: bool) -> i32 {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("can't open {path}: {e}");
            return 2;
        }
    };
    let steps: Vec<Value> = match serde_json::from_str(&text) {
        Ok(Value::Array(a)) => a,
        Ok(_) => {
            eprintln!("{path}: the script must be a JSON array of {{\"tool\": ..., \"args\": {{...}}}}");
            return 2;
        }
        Err(e) => {
            eprintln!("{path}: not valid JSON: {e}");
            return 2;
        }
    };
    let mut s = Session::new();
    s.base_dir = std::path::Path::new(path).parent().map(|p| p.to_path_buf());
    let mut failed_steps = 0;
    for (i, st) in steps.iter().enumerate() {
        let name = st.get("tool").and_then(|t| t.as_str()).unwrap_or("");
        let args = st.get("args").cloned().unwrap_or(json!({}));
        let ok = match s.call(name, &args) {
            Ok((v, _)) if failed(&v) => {
                let first = v["new_errors"][0].clone();
                println!("{}", json!({"step": i, "tool": name, "error": format!("{}: {}", first["name"].as_str().unwrap_or("feature"), first["error"].as_str().unwrap_or("failed")), "result": v}));
                false
            }
            Ok((v, _)) => {
                println!("{}", json!({"step": i, "tool": name, "result": v}));
                true
            }
            Err(e) => {
                println!("{}", json!({"step": i, "tool": name, "error": e}));
                false
            }
        };
        if !ok {
            failed_steps += 1;
            if stop_on_error {
                eprintln!("stopped at step {i} ({name})");
                break;
            }
        }
    }
    if failed_steps > 0 {
        eprintln!("{failed_steps} of {} steps failed", steps.len());
        1
    } else {
        0
    }
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

#[allow(dead_code)]
fn unused(_: Map<String, Value>) {}
