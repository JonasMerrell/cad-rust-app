//! Agent tools for the rest of the document: more Part Studios, Assemblies and Drawings.

use glam::{DMat3, DVec3};
use serde_json::{json, Value};

use super::{id_arg, round, Session, R};
use crate::assembly::{self, Assembly, Connector, ExplodeStep, Instance, Mate, MateKind, Pose, Relation, RelationKind};
use crate::doc::{BodyId, Document};
use crate::drawing::{DrawDim, DrawView, Drawing, Gdt, Note, GDT_KINDS};
use crate::drawing_ui::{self, DocCtx, Prim};
use crate::kernel::{BodyMesh, ProjView, RebuildResult};

impl DocCtx for Session {
    fn ws(&self) -> &crate::workspace::Workspace {
        &self.ws
    }
    fn bodies(&self, studio: usize) -> &[BodyMesh] {
        if studio == self.cur { &self.result.bodies } else { self.others.get(&studio).map(|r| r.bodies.as_slice()).unwrap_or(&[]) }
    }
    fn studio_doc(&self, studio: usize) -> Option<&Document> {
        if studio == self.cur { Some(&self.doc) } else { self.ws.studios.get(studio).map(|s| &s.doc) }
    }
    fn proj(&self, studio: usize, dir: &str) -> Option<&ProjView> {
        self.proj.get(&(studio, dir.to_string()))
    }
}

const DIM_KINDS: [&str; 6] = ["aligned", "horizontal", "vertical", "radius", "diameter", "angle"];

fn arr3(v: &Value) -> Option<DVec3> {
    let a = v.as_array()?;
    Some(DVec3::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?))
}

fn arr2(v: &Value) -> Option<[f64; 2]> {
    let a = v.as_array()?;
    Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?])
}

fn mat_rows(m: DMat3) -> Value {
    let r = |v: DVec3| json!([round(v.x), round(v.y), round(v.z)]);
    json!([r(m.row(0)), r(m.row(1)), r(m.row(2))])
}

fn mate_kind(s: &str) -> R<MateKind> {
    MateKind::ALL.into_iter().find(|k| format!("{k:?}").eq_ignore_ascii_case(s)).ok_or_else(|| format!("unknown mate kind {s}: fastened, revolute, slider, cylindrical, planar or ball"))
}

/// Pose from {position:[x,y,z], rotate:{axis:[x,y,z], angle:deg}} on top of `base`.
fn pose_arg(a: &Value, base: Pose) -> Pose {
    let mut p = base;
    if let Some(r) = a.get("rotate") {
        let axis = r.get("axis").and_then(arr3).unwrap_or(DVec3::Z).normalize_or(DVec3::Z);
        let ang = r.get("angle").and_then(|v| v.as_f64()).unwrap_or(0.0).to_radians();
        p.r = (DMat3::from_axis_angle(axis, ang) * p.m()).to_cols_array();
    }
    if let Some(t) = a.get("position").and_then(arr3) {
        p.t = t.into();
    }
    p
}

impl Session {
    /// Copy the live Part Studio back into the workspace.
    pub(super) fn sync(&mut self) {
        if self.ws.studios.is_empty() {
            self.ws.studios.push(crate::workspace::Studio { name: "Part Studio 1".into(), doc: Document::default() });
        }
        let cur = self.cur.min(self.ws.studios.len() - 1);
        self.cur = cur;
        self.ws.studios[cur].doc = self.doc.clone();
    }

    pub(super) fn tabs_summary(&self) -> Value {
        json!({
            "part_studios": self.ws.studios.iter().enumerate().map(|(i, s)| json!({"index": i, "name": s.name, "active": i == self.cur})).collect::<Vec<_>>(),
            "assemblies": self.ws.assemblies.iter().enumerate().map(|(i, a)| json!({"index": i, "name": a.name, "instances": a.instances.len(), "mates": a.mates.len()})).collect::<Vec<_>>(),
            "drawings": self.ws.drawings.iter().enumerate().map(|(i, d)| json!({"index": i, "name": d.name, "sheets": d.sheet_count(), "views": d.views.len()})).collect::<Vec<_>>(),
        })
    }

    /// Make sure a Part Studio other than the active one has been built.
    fn ensure_built(&mut self, studio: usize) -> R<()> {
        if studio == self.cur || self.others.contains_key(&studio) {
            return Ok(());
        }
        let doc = self.ws.studios.get(studio).ok_or_else(|| format!("no Part Studio {studio}"))?.doc.clone();
        let r: RebuildResult = self.kernel.rebuild_blocking(studio, &doc)?;
        self.others.insert(studio, r);
        Ok(())
    }

    fn build_for_assembly(&mut self, ai: usize) -> R<()> {
        let a = self.ws.assemblies.get(ai).ok_or_else(|| format!("no assembly {ai}"))?;
        let studios: Vec<usize> = a.instances.iter().flat_map(|x| assembly::leaves(&self.ws.assemblies, x)).map(|l| l.studio).collect();
        for s in studios {
            self.ensure_built(s)?;
        }
        Ok(())
    }

    fn mesh(&self, studio: usize, body: BodyId) -> Option<&BodyMesh> {
        self.bodies(studio).iter().find(|b| b.id == body)
    }

    fn centroid_local(&self, x: &Instance) -> DVec3 {
        let (mut sum, mut w) = (DVec3::ZERO, 0.0);
        for l in assembly::leaves(&self.ws.assemblies, x) {
            if let Some(m) = self.mesh(l.studio, l.body) {
                let v = m.volume.abs().max(1e-9);
                sum += l.pose.apply(m.centroid.into()) * v;
                w += v;
            }
        }
        if w > 0.0 { sum / w } else { DVec3::ZERO }
    }

    fn solve(&mut self, ai: usize, hold: &[u64]) -> Vec<u64> {
        let centers: Vec<(u64, DVec3)> = self.ws.assemblies[ai].instances.iter().map(|x| (x.id, self.centroid_local(x))).collect();
        let r = assembly::solve(&mut self.ws.assemblies[ai], hold, &|x: &Instance| centers.iter().find(|c| c.0 == x.id).map(|c| c.1).unwrap_or(DVec3::ZERO));
        r.failed
    }

    /// A mate connector from {instance, face:[x,y,z]} / {instance, edge:[x,y,z]} (points in the
    /// instance's own coordinates, as list_geometry reports them) or {instance, origin, z, x}.
    fn connector(&self, ai: usize, v: &Value) -> R<Connector> {
        let id = v.get("instance").and_then(|i| i.as_u64()).ok_or("each side needs an instance id")?;
        let a = &self.ws.assemblies[ai];
        let x = a.instance(id).ok_or_else(|| format!("no instance {id}"))?;
        if let (Some(o), Some(z)) = (v.get("origin").and_then(arr3), v.get("z").and_then(arr3)) {
            let z = z.normalize();
            let xx = v.get("x").and_then(arr3).map(|x| x.normalize()).unwrap_or_else(|| DVec3::from(crate::kernel::frame_from(o, z).x));
            return Ok(Connector { inst: id, origin: o.into(), z: z.into(), x: xx.into() });
        }
        let (pt, is_edge) = match (v.get("face").and_then(arr3), v.get("edge").and_then(arr3)) {
            (Some(p), _) => (p, false),
            (_, Some(p)) => (p, true),
            _ => return Err("give face:[x,y,z], edge:[x,y,z] or origin+z for each side".into()),
        };
        // nearest face or edge over the instance's parts
        let mut best: Option<(f64, usize, Option<usize>, Option<usize>, DVec3)> = None;
        let leaves = assembly::leaves(&self.ws.assemblies, x);
        for (li, l) in leaves.iter().enumerate() {
            let Some(m) = self.mesh(l.studio, l.body) else { continue };
            let lp = l.pose.inverse_apply(pt);
            if is_edge {
                for (ei, e) in m.edges.iter().enumerate() {
                    let d = e.pts.windows(2).map(|s| seg_dist(lp, DVec3::from(s[0].map(|v| v as f64)), DVec3::from(s[1].map(|v| v as f64)))).fold(f64::INFINITY, f64::min);
                    if best.map(|b| d < b.0).unwrap_or(true) {
                        best = Some((d, li, None, Some(ei), lp));
                    }
                }
            } else {
                for (ti, t) in m.indices.chunks(3).enumerate() {
                    let p = |i: u32| DVec3::new(m.positions[3 * i as usize] as f64, m.positions[3 * i as usize + 1] as f64, m.positions[3 * i as usize + 2] as f64);
                    let d = tri_dist(lp, p(t[0]), p(t[1]), p(t[2]));
                    if best.map(|b| d < b.0).unwrap_or(true) {
                        best = Some((d, li, Some(m.tri_face[ti] as usize), None, lp));
                    }
                }
            }
        }
        let (d, li, face, edge, lp) = best.ok_or("that instance has no geometry yet")?;
        if d > 0.5 {
            return Err(format!("no {} of instance {id} within 0.5 mm of that point (nearest is {} mm away)", if is_edge { "edge" } else { "face" }, round(d)));
        }
        let l = leaves[li];
        let m = self.mesh(l.studio, l.body).unwrap();
        let c = assembly::infer_connector(id, m, face, edge, lp).ok_or("could not place a connector there")?;
        Ok(Connector { origin: l.pose.apply(c.origin.into()).into(), z: l.pose.dir(c.z.into()).into(), x: l.pose.dir(c.x.into()).into(), ..c })
    }

    fn asm_info(&mut self, ai: usize, interference: bool) -> R<Value> {
        self.build_for_assembly(ai)?;
        let a = &self.ws.assemblies[ai];
        let name_of = |s: usize, b: BodyId| {
            self.studio_doc(s).and_then(|d| d.parts.get(&crate::doc::body_key(b))).and_then(|p| p.name.clone()).or_else(|| self.mesh(s, b).map(|m| m.name.clone())).unwrap_or_default()
        };
        let insts: Vec<Value> = a
            .instances
            .iter()
            .map(|x| {
                let mut lo = DVec3::splat(f64::INFINITY);
                let mut hi = DVec3::splat(f64::NEG_INFINITY);
                for l in assembly::leaves(&self.ws.assemblies, x) {
                    if let Some(m) = self.mesh(l.studio, l.body) {
                        let p = x.pose.compose(&l.pose);
                        for c in 0..8 {
                            let w = p.apply(DVec3::new(m.bbox[c & 1][0], m.bbox[(c >> 1) & 1][1], m.bbox[(c >> 2) & 1][2]));
                            lo = lo.min(w);
                            hi = hi.max(w);
                        }
                    }
                }
                json!({
                    "id": x.id,
                    "name": x.name,
                    "part": if x.sub.is_none() { json!({"studio": x.studio, "body": [x.body.0, x.body.1], "name": name_of(x.studio, x.body)}) } else { Value::Null },
                    "sub_assembly": x.sub,
                    "position": x.pose.t.map(round),
                    "rotation_rows": mat_rows(x.pose.m()),
                    "fixed": x.fixed,
                    "hidden": x.hidden,
                    "world_bbox": if lo.x.is_finite() { json!([[round(lo.x), round(lo.y), round(lo.z)], [round(hi.x), round(hi.y), round(hi.z)]]) } else { Value::Null },
                })
            })
            .collect();
        let mates: Vec<Value> = a.mates.iter().map(|m| json!({"id": m.id, "name": m.name, "kind": format!("{:?}", m.kind).to_lowercase(), "a": m.a.map(|c| c.inst), "b": m.b.map(|c| c.inst), "offset": m.offset, "rotation": m.rotation, "flip": m.flip, "suppressed": m.suppressed})).collect();
        let bom: Vec<Value> = crate::asm_ui::bom(self, a).into_iter().map(|(i, n, q, m, mass)| json!({"item": i, "part": n, "qty": q, "material": m, "mass_g": mass.map(round)})).collect();
        let mut v = json!({"name": a.name, "instances": insts, "mates": mates, "bom": bom, "explode_steps": a.explode_steps});
        if interference {
            let mut items = vec![];
            let mut ids = vec![];
            for x in a.instances.iter().filter(|x| !x.hidden) {
                for l in assembly::leaves(&self.ws.assemblies, x) {
                    let p = x.pose.compose(&l.pose);
                    let (m, t) = (p.m(), p.t);
                    items.push((l.studio, l.body, [m.x_axis.x, m.y_axis.x, m.z_axis.x, t[0], m.x_axis.y, m.y_axis.y, m.z_axis.y, t[1], m.x_axis.z, m.y_axis.z, m.z_axis.z, t[2]]));
                    ids.push(x.id);
                }
            }
            let pairs = self.kernel.interference_blocking(items)?;
            v["interference"] = json!(pairs.into_iter().filter(|(i, j, _)| ids[*i] != ids[*j]).map(|(i, j, vol)| json!({"a": ids[i], "b": ids[j], "volume_mm3": round(vol)})).collect::<Vec<_>>());
        }
        Ok(v)
    }

    fn render_assembly(&mut self, ai: usize, a: &Value) -> R<Vec<u8>> {
        self.build_for_assembly(ai)?;
        let asm = &self.ws.assemblies[ai];
        let explode = a.get("explode").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let centers: Vec<DVec3> = asm.instances.iter().map(|x| x.pose.apply(self.centroid_local(x))).collect();
        let mid = centers.iter().fold(DVec3::ZERO, |s, p| s + *p) / centers.len().max(1) as f64;
        let mut items = vec![];
        for (k, x) in asm.instances.iter().enumerate().filter(|(_, x)| !x.hidden) {
            let mut pose = x.pose;
            if explode > 0.0 {
                if asm.explode_steps.is_empty() {
                    pose.t = (DVec3::from(pose.t) + (centers[k] - mid) * explode).into();
                } else {
                    let total = explode.clamp(0.0, 1.0) * asm.explode_steps.len() as f64;
                    for (i, st) in asm.explode_steps.iter().enumerate() {
                        if st.insts.contains(&x.id) {
                            pose.t = (DVec3::from(pose.t) + DVec3::from(st.dir).normalize_or_zero() * st.distance * (total - i as f64).clamp(0.0, 1.0)).into();
                        }
                    }
                }
            }
            for l in assembly::leaves(&self.ws.assemblies, x) {
                if let Some(m) = self.mesh(l.studio, l.body) {
                    let col = self.studio_doc(l.studio).and_then(|d| d.parts.get(&crate::doc::body_key(l.body))).and_then(|p| p.color).unwrap_or([0x9a, 0xa9, 0xba]);
                    items.push((m, pose.compose(&l.pose), col));
                }
            }
        }
        let view = a.get("view").and_then(|v| v.as_str()).unwrap_or("iso");
        let w = a.get("width").and_then(|v| v.as_u64()).unwrap_or(800).clamp(64, 4096) as u32;
        let h = a.get("height").and_then(|v| v.as_u64()).unwrap_or(600).clamp(64, 4096) as u32;
        let dir = a.get("direction").and_then(arr3).map(|d| d.to_array());
        super::raster_scene(&items, &[], view, dir, w, h)
    }

    /// Fetch the projections a drawing needs.
    fn project_drawing(&mut self, di: usize) -> R<()> {
        let views: Vec<(usize, String)> = self.ws.drawings.get(di).ok_or_else(|| format!("no drawing {di}"))?.views.iter().map(|v| (v.studio, v.dir.clone())).collect();
        for (studio, dir) in views {
            if self.proj.contains_key(&(studio, dir.clone())) {
                continue;
            }
            self.ensure_built(studio)?;
            let doc = self.studio_doc(studio).cloned().ok_or("no such Part Studio")?;
            let (d, u) = drawing_ui::dir_vectors(&dir);
            let pv = self.kernel.project_blocking(studio, &dir, &doc, vec![(d, u, dir.starts_with("section:"))])?;
            if let Some(p) = pv.into_iter().next() {
                self.proj.insert((studio, dir), p);
            }
        }
        // a BOM table needs its assembly's parts
        if let Some((ai, _)) = self.ws.drawings[di].bom {
            if ai < self.ws.assemblies.len() {
                self.build_for_assembly(ai)?;
            }
        }
        Ok(())
    }

    pub(super) fn export_drawing_tab(&mut self, di: usize, format: &str, a: &Value) -> R<Vec<u8>> {
        self.sync();
        self.project_drawing(di)?;
        let d = self.ws.drawings[di].clone();
        let size = drawing_ui::sheet_size(&d.sheet);
        let sheet = a.get("sheet").and_then(|s| s.as_u64()).map(|s| s as usize - 1).unwrap_or(0);
        Ok(match format {
            "pdf" => {
                let pages: Vec<Vec<Prim>> = (0..d.sheet_count()).map(|i| drawing_ui::sheet_prims(self, &d, i)).collect();
                let refs: Vec<&[Prim]> = pages.iter().map(|p| p.as_slice()).collect();
                crate::drawing_export::pdf_pages(&refs, size)
            }
            "dxf" => crate::drawing_export::dxf(&drawing_ui::sheet_prims(self, &d, sheet)).into_bytes(),
            "svg" => crate::drawing_export::svg(&drawing_ui::sheet_prims(self, &d, sheet), size).into_bytes(),
            "png" => crate::drawing_export::png(&drawing_ui::sheet_prims(self, &d, sheet), size, a.get("width").and_then(|w| w.as_u64()).unwrap_or(1600).clamp(200, 6000) as u32)?,
            _ => return Err("format must be pdf, svg, dxf or png".into()),
        })
    }

    fn drawing_info(&mut self, di: usize) -> R<Value> {
        self.sync();
        self.project_drawing(di)?;
        let d = &self.ws.drawings[di];
        let views: Vec<Value> = d
            .views
            .iter()
            .map(|v| {
                let p = self.proj.get(&(v.studio, v.dir.clone()));
                // corners and circles to dimension, in the view's own coordinates (model mm)
                let mut pts: Vec<[f64; 2]> = vec![];
                let mut circles: Vec<Value> = vec![];
                let mut lines: Vec<Value> = vec![];
                if let Some(p) = p {
                    for pl in &p.visible {
                        if let Some((c, r, full)) = drawing_ui::fit_circle(pl) {
                            circles.push(json!({"center": c.map(round), "radius": round(r), "full": full}));
                            continue;
                        }
                        for q in [pl[0], pl[pl.len() - 1]] {
                            if !pts.iter().any(|x| (x[0] - q[0]).abs() + (x[1] - q[1]).abs() < 1e-6) {
                                pts.push(q);
                            }
                        }
                        if pl.len() == 2 && lines.len() < 60 {
                            lines.push(json!([pl[0].map(round), pl[1].map(round)]));
                        }
                    }
                }
                pts.truncate(80);
                json!({
                    "id": v.id, "studio": v.studio, "dir": v.dir, "sheet": v.sheet + 1, "position": v.pos.map(round), "scale": v.scale,
                    "model_bbox_2d": p.map(|p| json!([p.bbox[0].map(round), p.bbox[1].map(round)])),
                    "points": pts.iter().map(|q| q.map(round)).collect::<Vec<_>>(), "circles": circles, "lines": lines,
                    "detail_of": v.detail.map(|x| x.0),
                })
            })
            .collect();
        Ok(json!({
            "name": d.name, "sheet_size": d.sheet, "sheets": d.sheet_count(), "views": views,
            "dimensions": d.dims.iter().map(|x| json!({"id": x.id, "view": x.view, "kind": DIM_KINDS.get(x.kind as usize), "a": x.a, "b": x.b, "prefix": x.prefix})).collect::<Vec<_>>(),
            "notes": d.notes.iter().map(|n| json!({"id": n.id, "text": n.text, "position": n.pos, "sheet": n.sheet + 1})).collect::<Vec<_>>(),
            "gdt": d.gdts.iter().map(|g| json!({"id": g.id, "kind": g.kind, "tolerance": g.tol, "datums": g.datums, "position": g.pos, "sheet": g.sheet + 1})).collect::<Vec<_>>(),
            "note": "Coordinates: view points/circles are in the view's model mm (pass them to dimension a/b); positions are sheet mm from the bottom-left.",
        }))
    }

    fn drawing_item(&mut self, di: usize, it: &Value) -> R<Value> {
        let ty = it.get("type").and_then(|t| t.as_str()).ok_or("each item needs a type")?.to_string();
        let sheet = it.get("sheet").and_then(|s| s.as_u64()).map(|s| (s as usize).max(1) - 1).unwrap_or(0);
        let d = &mut self.ws.drawings[di];
        if sheet >= d.sheet_count() {
            d.sheets = sheet + 1;
        }
        d.next_id += 1;
        let id = d.next_id;
        let view_of = |d: &Drawing| -> R<DrawView> {
            let v = it.get("view").and_then(|v| v.as_u64()).ok_or("give view (a view id)")?;
            d.views.iter().find(|x| x.id == v).cloned().ok_or_else(|| format!("no view {v}"))
        };
        match ty.as_str() {
            "view" => {
                let studio = it.get("studio").and_then(|s| s.as_u64()).unwrap_or(self.cur as u64) as usize;
                let dir = it.get("dir").and_then(|s| s.as_str()).unwrap_or("front").to_string();
                if !drawing_ui::DIRS.iter().any(|x| x.0 == dir) {
                    return Err(format!("dir must be one of {}", drawing_ui::DIRS.iter().map(|x| x.0).collect::<Vec<_>>().join(", ")));
                }
                let scale = it.get("scale").and_then(|s| s.as_f64()).or_else(|| d.views.first().map(|v| v.scale)).unwrap_or(1.0);
                let size = drawing_ui::sheet_size(&d.sheet);
                let pos = it.get("position").and_then(arr2).unwrap_or([size[0] / 2.0, size[1] / 2.0]);
                let hidden_lines = it.get("hidden_lines").and_then(|h| h.as_bool()).unwrap_or(false);
                d.views.push(DrawView { id, studio, dir, pos, scale, hidden_lines, detail: None, sheet });
                Ok(json!({"view": id}))
            }
            "detail" => {
                let src = view_of(d)?;
                let c = it.get("center").and_then(arr2).ok_or("give center (view model coordinates)")?;
                let r = it.get("radius").and_then(|r| r.as_f64()).ok_or("give radius")?;
                let scale = it.get("scale").and_then(|s| s.as_f64()).unwrap_or(src.scale * 2.0);
                let pos = it.get("position").and_then(arr2).ok_or("give position")?;
                d.views.push(DrawView { id, studio: src.studio, dir: src.dir.clone(), pos, scale, hidden_lines: false, detail: Some((src.id, c, r)), sheet: src.sheet });
                Ok(json!({"view": id}))
            }
            "dimension" => {
                let v = view_of(d)?;
                let kind_s = it.get("kind").and_then(|k| k.as_str()).unwrap_or("aligned");
                let kind = DIM_KINDS.iter().position(|k| *k == kind_s).ok_or("kind: aligned, horizontal, vertical, radius, diameter or angle")? as u8;
                let a = it.get("a").and_then(arr2).ok_or("give a (or center for radius/diameter)").or_else(|_| it.get("center").and_then(arr2).ok_or("give a"))?;
                let b = match kind {
                    3 | 4 => match (it.get("b").and_then(arr2), it.get("radius").and_then(|r| r.as_f64())) {
                        (Some(b), _) => b,
                        (None, Some(r)) => [a[0] + r * 0.7071, a[1] + r * 0.7071],
                        _ => return Err("give b (a point on the circle) or radius".into()),
                    },
                    _ => it.get("b").and_then(arr2).ok_or("give b")?,
                };
                let extra = if kind == 5 {
                    let l = it.get("line2").and_then(|l| l.as_array()).ok_or("angle needs line2: [[x,y],[x,y]]; a-b is the first line")?;
                    vec![l.first().and_then(arr2).ok_or("line2")?, l.get(1).and_then(arr2).ok_or("line2")?]
                } else {
                    vec![]
                };
                let offset = it.get("offset").and_then(|o| o.as_f64()).unwrap_or(match kind {
                    3 | 4 => 8.0,
                    5 => 12.0,
                    _ => 8.0,
                });
                let prefix = it.get("prefix").and_then(|p| p.as_str()).unwrap_or("").to_string();
                d.dims.push(DrawDim { id, view: v.id, a, b, kind, offset, extra, prefix });
                Ok(json!({"dimension": id}))
            }
            "auto_dimensions" => {
                let v = view_of(d)?;
                d.next_id -= 1;
                self.project_drawing(di)?;
                let p = self.proj.get(&(v.studio, v.dir.clone())).cloned().ok_or("the view has no projection")?;
                let d = &mut self.ws.drawings[di];
                let dims = drawing_ui::auto_dims(&p, v.id, &mut d.next_id);
                let ids: Vec<u64> = dims.iter().map(|x| x.id).collect();
                d.dims.extend(dims);
                Ok(json!({"dimensions": ids}))
            }
            "note" => {
                let text = it.get("text").and_then(|t| t.as_str()).ok_or("give text")?.to_string();
                let pos = it.get("position").and_then(arr2).ok_or("give position (sheet mm)")?;
                d.notes.push(Note { id, sheet, pos, text });
                Ok(json!({"note": id}))
            }
            "gdt" | "datum" => {
                let datum = ty == "datum";
                let kind = if datum { "datum".to_string() } else { it.get("kind").and_then(|k| k.as_str()).unwrap_or("position").to_string() };
                if !datum && !GDT_KINDS.iter().any(|k| k.0 == kind) {
                    return Err(format!("kind must be one of {}", GDT_KINDS.iter().map(|k| k.0).collect::<Vec<_>>().join(", ")));
                }
                let tol = if datum { it.get("letter").and_then(|t| t.as_str()).unwrap_or("A") } else { it.get("tolerance").and_then(|t| t.as_str()).unwrap_or("0.1") }.to_string();
                let datums = it.get("datums").and_then(|t| t.as_str()).unwrap_or("").to_string();
                let pos = it.get("position").and_then(arr2).ok_or("give position (sheet mm)")?;
                let leader = it.get("leader").and_then(arr2);
                d.gdts.push(Gdt { id, sheet, pos, kind, tol, datums, leader });
                Ok(json!({"gdt": id}))
            }
            "bom" => {
                let ai = it.get("assembly").and_then(|a| a.as_u64()).ok_or("give assembly")? as usize;
                let size = drawing_ui::sheet_size(&d.sheet);
                let pos = it.get("position").and_then(arr2).unwrap_or([size[0] - 130.0, size[1] - 10.0]);
                d.bom = Some((ai, pos));
                d.bom_sheet = sheet;
                Ok(json!({"bom": ai}))
            }
            "sheet" => {
                d.sheets = d.sheet_count() + 1;
                Ok(json!({"sheets": d.sheets}))
            }
            _ => Err(format!("unknown drawing item {ty}")),
        }
    }

    pub(super) fn ws_call(&mut self, name: &str, a: &Value) -> R<(Value, Option<Vec<u8>>)> {
        let asm_idx = |a: &Value, s: &Session| -> R<usize> {
            let i = a.get("assembly").and_then(|v| v.as_u64()).ok_or("give assembly (its index)")? as usize;
            if i >= s.ws.assemblies.len() {
                return Err(format!("no assembly {i}"));
            }
            Ok(i)
        };
        let dwg_idx = |a: &Value, s: &Session| -> R<usize> {
            let i = a.get("drawing").and_then(|v| v.as_u64()).ok_or("give drawing (its index)")? as usize;
            if i >= s.ws.drawings.len() {
                return Err(format!("no drawing {i}"));
            }
            Ok(i)
        };
        let v = match name {
            "new_part_studio" => {
                self.sync();
                let name = a.get("name").and_then(|n| n.as_str()).map(String::from).unwrap_or_else(|| format!("Part Studio {}", self.ws.studios.len() + 1));
                self.ws.studios.push(crate::workspace::Studio { name, doc: Document { units: self.doc.units, ..Default::default() } });
                self.cur = self.ws.studios.len() - 1;
                self.doc = self.ws.studios[self.cur].doc.clone();
                self.rebuild()?;
                json!({"part_studio": self.cur, "note": "Sketches and features now go into this Part Studio."})
            }
            "switch_part_studio" => {
                let i = id_arg(a, "index")? as usize;
                if i >= self.ws.studios.len() {
                    return Err(format!("no Part Studio {i}"));
                }
                self.sync();
                self.cur = i;
                self.doc = self.ws.studios[i].doc.clone();
                self.rebuild()?;
                let mut v = self.status();
                v["part_studio"] = json!(i);
                v
            }
            "define_custom_feature" => {
                let def = crate::script::CustomDef {
                    name: a.get("name").and_then(|n| n.as_str()).ok_or("give name")?.to_string(),
                    params: a
                        .get("params")
                        .and_then(|p| p.as_array())
                        .map(|ps| {
                            ps.iter()
                                .map(|p| crate::script::ParamDef {
                                    name: p.get("name").and_then(|v| v.as_str()).unwrap_or("p").to_string(),
                                    kind: p.get("kind").and_then(|v| v.as_str()).unwrap_or("length").to_string(),
                                    default: p.get("default").map(super::val_str).unwrap_or_else(|| "0".into()),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    selection: a.get("selection").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    source: a.get("source").and_then(|v| v.as_str()).ok_or("give source")?.to_string(),
                };
                match self.doc.custom_features.iter_mut().find(|d| d.name == def.name) {
                    Some(d) => *d = def,
                    None => self.doc.custom_features.push(def),
                }
                self.rebuild()?;
                let mut v = self.status();
                v["defined"] = json!(a.get("name"));
                v
            }
            "create_assembly" => {
                let name = a.get("name").and_then(|n| n.as_str()).map(String::from).unwrap_or_else(|| format!("Assembly {}", self.ws.assemblies.len() + 1));
                self.ws.assemblies.push(Assembly { name, ..Default::default() });
                json!({"assembly": self.ws.assemblies.len() - 1})
            }
            "assembly_insert" => {
                let ai = asm_idx(a, self)?;
                self.sync();
                let (studio, body, sub) = match a.get("sub_assembly").and_then(|s| s.as_u64()) {
                    Some(k) => {
                        let k = k as usize;
                        if k >= self.ws.assemblies.len() || assembly::contains(&self.ws.assemblies, k, ai) {
                            return Err("that sub-assembly does not exist or contains this assembly".into());
                        }
                        (0, (0, 0), Some(k))
                    }
                    None => {
                        let studio = a.get("studio").and_then(|s| s.as_u64()).unwrap_or(self.cur as u64) as usize;
                        self.ensure_built(studio)?;
                        let body = match a.get("body").and_then(|b| b.as_array()) {
                            Some(b) => (b.first().and_then(|v| v.as_u64()).ok_or("body [f,i]")?, b.get(1).and_then(|v| v.as_u64()).ok_or("body [f,i]")? as u32),
                            None => self.bodies(studio).first().map(|b| b.id).ok_or("that Part Studio has no parts")?,
                        };
                        if self.mesh(studio, body).is_none() {
                            return Err(format!("no part {body:?} in Part Studio {studio}"));
                        }
                        (studio, body, None)
                    }
                };
                let base_name = match sub {
                    Some(k) => self.ws.assemblies[k].name.clone(),
                    // a named part, else the Part Studio's name when it holds one part
                    None => self
                        .studio_doc(studio)
                        .and_then(|d| d.parts.get(&crate::doc::body_key(body)))
                        .and_then(|p| p.name.clone())
                        .or_else(|| (self.bodies(studio).len() == 1 && self.ws.studios.len() > 1).then(|| self.ws.studios[studio].name.clone()))
                        .or_else(|| self.mesh(studio, body).map(|m| m.name.clone()))
                        .unwrap_or_default(),
                };
                let pose = pose_arg(a, Pose::default());
                let asm = &mut self.ws.assemblies[ai];
                let id = asm.new_id();
                let n = asm.instances.iter().filter(|x| x.sub == sub && (sub.is_some() || (x.studio == studio && x.body == body))).count() + 1;
                let fixed = a.get("fixed").and_then(|f| f.as_bool()).unwrap_or(asm.instances.is_empty());
                let name = a.get("name").and_then(|n| n.as_str()).map(String::from).unwrap_or(format!("{base_name} <{n}>"));
                asm.instances.push(Instance { id, name, studio, body, pose, fixed, hidden: false, sub });
                json!({"instance": id, "fixed": fixed})
            }
            "assembly_move" => {
                let ai = asm_idx(a, self)?;
                self.build_for_assembly(ai)?;
                let id = id_arg(a, "instance")?;
                let x = self.ws.assemblies[ai].instances.iter_mut().find(|x| x.id == id).ok_or("no such instance")?;
                x.pose = pose_arg(a, x.pose);
                if let Some(f) = a.get("fixed").and_then(|f| f.as_bool()) {
                    x.fixed = f;
                }
                if let Some(h) = a.get("hidden").and_then(|f| f.as_bool()) {
                    x.hidden = h;
                }
                if a.get("delete").and_then(|d| d.as_bool()).unwrap_or(false) {
                    let asm = &mut self.ws.assemblies[ai];
                    asm.instances.retain(|x| x.id != id);
                    asm.mates.retain(|m| m.a.map(|c| c.inst != id).unwrap_or(true) && m.b.map(|c| c.inst != id).unwrap_or(true));
                }
                let failed = self.solve(ai, &[id]);
                let mut v = self.asm_info(ai, false)?;
                v["failed_mates"] = json!(failed);
                v
            }
            "assembly_mate" => {
                let ai = asm_idx(a, self)?;
                self.build_for_assembly(ai)?;
                let kind = mate_kind(a.get("kind").and_then(|k| k.as_str()).unwrap_or("fastened"))?;
                let ca = self.connector(ai, a.get("a").ok_or("give a")?)?;
                let cb = self.connector(ai, a.get("b").ok_or("give b")?)?;
                if ca.inst == cb.inst {
                    return Err("a mate joins two different instances".into());
                }
                let f = |k: &str| a.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
                let limits = a.get("limits").and_then(arr2);
                let asm = &mut self.ws.assemblies[ai];
                let id = asm.new_id();
                let n = asm.mates.iter().filter(|m| m.kind == kind).count() + 1;
                let name = a.get("name").and_then(|n| n.as_str()).map(String::from).unwrap_or(format!("{} {n}", kind.label()));
                asm.mates.push(Mate {
                    id,
                    name,
                    kind,
                    a: Some(ca),
                    b: Some(cb),
                    flip: a.get("flip").and_then(|v| v.as_bool()).unwrap_or(false),
                    offset: f("offset"),
                    offset_xy: [f("offset_x"), f("offset_y")],
                    rotation: f("rotation"),
                    limits,
                    suppressed: false,
                });
                let failed = self.solve(ai, &[]);
                let mut v = self.asm_info(ai, false)?;
                v["mate"] = json!(id);
                v["failed_mates"] = json!(failed);
                v
            }
            "assembly_relation" => {
                let ai = asm_idx(a, self)?;
                let kind = match a.get("kind").and_then(|k| k.as_str()).unwrap_or("gear") {
                    "gear" => RelationKind::Gear,
                    "rack" => RelationKind::Rack,
                    "screw" => RelationKind::Screw,
                    _ => return Err("kind: gear, rack or screw".into()),
                };
                let ma = id_arg(a, "mate_a")?;
                let mb = a.get("mate_b").and_then(|v| v.as_u64());
                let ratio = a.get("ratio").and_then(|v| v.as_f64()).unwrap_or(1.0);
                let reverse = a.get("reverse").and_then(|v| v.as_bool()).unwrap_or(false);
                let asm = &mut self.ws.assemblies[ai];
                let id = asm.new_id();
                let n = asm.relations.iter().filter(|r| r.kind == kind).count() + 1;
                let mut r = Relation { id, name: format!("{} {n}", kind.label()), kind, a: ma, b: mb, ratio, reverse, phase: 0.0 };
                r.phase = assembly::relation_phase(asm, &r);
                asm.relations.push(r);
                json!({"relation": id})
            }
            "assembly_explode" => {
                let ai = asm_idx(a, self)?;
                let steps = a.get("steps").and_then(|s| s.as_array()).ok_or("give steps: [{instances, direction, distance}]")?;
                let mut out = vec![];
                for st in steps {
                    let insts: Vec<u64> = st.get("instances").and_then(|i| i.as_array()).ok_or("each step needs instances")?.iter().filter_map(|v| v.as_u64()).collect();
                    let dir = st.get("direction").and_then(arr3).unwrap_or(DVec3::Z);
                    let distance = st.get("distance").and_then(|d| d.as_f64()).unwrap_or(50.0);
                    out.push(ExplodeStep { insts, dir: dir.into(), distance });
                }
                self.ws.assemblies[ai].explode_steps = out;
                json!({"steps": self.ws.assemblies[ai].explode_steps.len()})
            }
            "assembly_info" => {
                let ai = asm_idx(a, self)?;
                self.sync();
                self.asm_info(ai, a.get("interference").and_then(|v| v.as_bool()).unwrap_or(false))?
            }
            "render_assembly" => {
                let ai = asm_idx(a, self)?;
                self.sync();
                let png = self.render_assembly(ai, a)?;
                if let Some(p) = a.get("path").and_then(|p| p.as_str()) {
                    std::fs::write(p, &png).map_err(|e| e.to_string())?;
                }
                return Ok((json!({"assembly": ai, "saved": a.get("path")}), Some(png)));
            }
            "create_drawing" => {
                self.sync();
                let name = a.get("name").and_then(|n| n.as_str()).map(String::from).unwrap_or_else(|| format!("Drawing {}", self.ws.drawings.len() + 1));
                let sheet = a.get("sheet").and_then(|n| n.as_str()).unwrap_or("A3").to_string();
                let mut d = Drawing { name, sheet, ..Default::default() };
                if a.get("standard_views").and_then(|v| v.as_bool()).unwrap_or(true) {
                    let studio = a.get("studio").and_then(|s| s.as_u64()).unwrap_or(self.cur as u64) as usize;
                    self.ensure_built(studio)?;
                    standard_views(&mut d, studio, self.bodies(studio));
                }
                self.ws.drawings.push(d);
                let di = self.ws.drawings.len() - 1;
                let mut v = self.drawing_info(di)?;
                v["drawing"] = json!(di);
                v
            }
            "drawing_add" => {
                let di = dwg_idx(a, self)?;
                self.sync();
                let items = a.get("items").and_then(|i| i.as_array()).ok_or("give items")?.clone();
                let mut created = vec![];
                for it in &items {
                    created.push(self.drawing_item(di, it)?);
                }
                json!({"created": created})
            }
            "drawing_info" => {
                let di = dwg_idx(a, self)?;
                self.drawing_info(di)?
            }
            "render_drawing" => {
                let di = dwg_idx(a, self)?;
                let png = self.export_drawing_tab(di, "png", a)?;
                if let Some(p) = a.get("path").and_then(|p| p.as_str()) {
                    std::fs::write(p, &png).map_err(|e| e.to_string())?;
                }
                return Ok((json!({"drawing": di, "saved": a.get("path")}), Some(png)));
            }
            _ => return Err(format!("unknown tool {name}")),
        };
        Ok((v, None))
    }
}

/// Front, top, right and iso views laid out third-angle and scaled to the sheet.
fn standard_views(d: &mut Drawing, studio: usize, bodies: &[BodyMesh]) {
    let sheet = drawing_ui::sheet_size(&d.sheet);
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for b in bodies {
        lo = lo.min(b.bbox[0].into());
        hi = hi.max(b.bbox[1].into());
    }
    let size = if lo.x.is_finite() { hi - lo } else { DVec3::splat(50.0) };
    let raw = ((sheet[0] - 60.0) / (size.x + size.y * 1.8).max(1.0)).min((sheet[1] - 80.0) / (size.z + size.y * 1.6).max(1.0));
    let scale = [100.0, 50.0, 20.0, 10.0, 5.0, 4.0, 2.0, 1.0, 0.5, 0.25, 0.2, 0.1, 0.05, 0.02, 0.01].into_iter().find(|s| *s <= raw).unwrap_or(0.005);
    let (fw, fh) = (size.x * scale, size.z * scale);
    let front = [40.0 + fw / 2.0, 60.0 + fh / 2.0];
    for (dir, pos) in [
        ("front", front),
        ("top", [front[0], front[1] + fh / 2.0 + 25.0 + size.y * scale / 2.0]),
        ("right", [front[0] + fw / 2.0 + 25.0 + size.y * scale / 2.0, front[1]]),
        ("iso", [sheet[0] - 30.0 - (size.x + size.y) * scale * 0.45, sheet[1] - 30.0 - (size.z + size.y) * scale * 0.45]),
    ] {
        d.next_id += 1;
        d.views.push(DrawView { id: d.next_id, studio, dir: dir.into(), pos, scale, hidden_lines: false, detail: None, sheet: 0 });
    }
}

fn seg_dist(p: DVec3, a: DVec3, b: DVec3) -> f64 {
    let d = b - a;
    let t = if d.length_squared() < 1e-18 { 0.0 } else { ((p - a).dot(d) / d.length_squared()).clamp(0.0, 1.0) };
    (a + d * t - p).length()
}

fn tri_dist(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> f64 {
    let n = (b - a).cross(c - a);
    if n.length_squared() < 1e-24 {
        return seg_dist(p, a, b).min(seg_dist(p, b, c)).min(seg_dist(p, c, a));
    }
    let n = n.normalize();
    let q = p - n * (p - a).dot(n);
    let inside = [(a, b), (b, c), (c, a)].iter().all(|(u, v)| (*v - *u).cross(q - *u).dot(n) >= -1e-12);
    if inside { (p - q).length() } else { seg_dist(p, a, b).min(seg_dist(p, b, c)).min(seg_dist(p, c, a)) }
}
