//! Solid modelling on top of OpenCascade. This runs inside the kernel worker
//! process (see `worker.rs`), because a failing OCCT operation can abort the
//! process and must not take the editor down with it.

use std::collections::HashMap;

use glam::{DVec2, DVec3};
use opencascade::primitives::{Edge, Face, Shape, Wire};
use opencascade_sys as ffi;
use serde::{Deserialize, Serialize};

use crate::doc::{BodyId, BoolOp, BooleanKind, Document, EndType, FeatId, HoleType, Op, OriginPlane, Ref};
use crate::expr::{Kind, Units};
use crate::regions::{find_regions, Region, Seg};
use crate::sketch::Frame;

// ------------------------------------------------------------------ results

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FaceInfo {
    pub center: [f64; 3],
    pub normal: [f64; 3],
    pub planar: bool,
    pub area: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EdgeInfo {
    pub pts: Vec<[f32; 3]>,
    pub mid: [f64; 3],
    pub linear: bool,
    pub circular: bool,
    pub length: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BodyMesh {
    pub id: BodyId,
    pub name: String,
    /// xyz triples
    pub positions: Vec<f32>,
    pub normals: Vec<f32>,
    pub indices: Vec<u32>,
    /// Face index of each triangle.
    pub tri_face: Vec<u32>,
    pub faces: Vec<FaceInfo>,
    pub edges: Vec<EdgeInfo>,
    pub volume: f64,
    pub area: f64,
    pub bbox: [[f64; 3]; 2],
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FeatureStatus {
    pub id: FeatId,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RebuildResult {
    pub features: Vec<FeatureStatus>,
    pub bodies: Vec<BodyMesh>,
    /// Frames of plane and sketch features.
    pub frames: HashMap<FeatId, Frame>,
}

// ------------------------------------------------------------------ state

pub struct Body {
    pub id: BodyId,
    pub shape: Shape,
}

pub struct Model<'a> {
    pub doc: &'a Document,
    pub vars: HashMap<String, f64>,
    pub bodies: Vec<Body>,
    pub frames: HashMap<FeatId, Frame>,
}

type R<T> = Result<T, String>;

fn v3(a: [f64; 3]) -> DVec3 {
    DVec3::from(a)
}

impl<'a> Model<'a> {
    pub fn new(doc: &'a Document) -> Self {
        let (vars, _) = doc.var_values();
        Model { doc, vars, bodies: vec![], frames: HashMap::new() }
    }

    fn units(&self) -> Units {
        self.doc.units
    }

    fn len(&self, s: &str) -> R<f64> {
        crate::doc::eval(s, Kind::Length, &self.vars, self.units())
    }
    fn ang(&self, s: &str) -> R<f64> {
        crate::doc::eval(s, Kind::Angle, &self.vars, self.units())
    }
    fn count(&self, s: &str) -> R<usize> {
        let v = crate::doc::eval(s, Kind::Count, &self.vars, self.units())?;
        if v < 1.0 || v > 1000.0 {
            return Err("count must be between 1 and 1000".into());
        }
        Ok(v.round() as usize)
    }

    // ------------------------------------------------------------ references

    pub fn frame_of(&self, r: &Ref) -> R<Frame> {
        match r {
            Ref::OriginPlane { plane } => Ok(plane.frame()),
            Ref::FeaturePlane { feature } => self.frames.get(feature).copied().ok_or("referenced plane is missing".into()),
            Ref::Face { point, normal } => {
                let (face, _) = self.find_face(v3(*point), v3(*normal))?;
                let g = face_geom(&face);
                if !g.planar {
                    return Err("sketch planes must be flat faces".into());
                }
                Ok(plane_frame(g.center, g.normal))
            }
            _ => Err("not a plane".into()),
        }
    }

    /// The live face nearest to a stored reference point.
    pub fn find_face(&self, p: DVec3, n: DVec3) -> R<(Face, usize)> {
        let mut best: Option<(f64, Face, usize)> = None;
        for (bi, b) in self.bodies.iter().enumerate() {
            for f in unique_faces(&b.shape) {
                let d = face_distance(&f, p);
                let fn_ = face_geom(&f).normal;
                let pen = if n.length_squared() > 0.5 && fn_.dot(n) < 0.5 { 1.0 } else { 0.0 };
                let score = d + pen;
                if best.as_ref().map(|b| score < b.0).unwrap_or(true) {
                    best = Some((score, f, bi));
                }
            }
        }
        match best {
            Some((d, f, bi)) if d < 0.05 + 1e-3 * p.length() => Ok((f, bi)),
            _ => Err("a referenced face no longer exists".into()),
        }
    }

    pub fn find_edge(&self, p: DVec3) -> R<(Edge, usize)> {
        let mut best: Option<(f64, Edge, usize)> = None;
        for (bi, b) in self.bodies.iter().enumerate() {
            for e in unique_edges(&b.shape) {
                let pts: Vec<DVec3> = e.approximation_segments().collect();
                let d = polyline_distance(&pts, p);
                if best.as_ref().map(|b| d < b.0).unwrap_or(true) {
                    best = Some((d, e, bi));
                }
            }
        }
        match best {
            Some((d, e, bi)) if d < 0.05 + 1e-3 * p.length() => Ok((e, bi)),
            _ => Err("a referenced edge no longer exists".into()),
        }
    }

    fn body_index(&self, r: &Ref) -> R<usize> {
        match r {
            Ref::Body { body } => self.bodies.iter().position(|b| b.id == *body).ok_or("a referenced body no longer exists".into()),
            Ref::Face { point, normal } => Ok(self.find_face(v3(*point), v3(*normal))?.1),
            Ref::Edge { point } => Ok(self.find_edge(v3(*point))?.1),
            _ => Err("select bodies".into()),
        }
    }

    /// An axis (origin, direction) from a line-like reference.
    fn axis_of(&self, r: &Ref) -> R<(DVec3, DVec3)> {
        match r {
            Ref::Axis { axis } => Ok((DVec3::ZERO, [DVec3::X, DVec3::Y, DVec3::Z][*axis as usize % 3])),
            Ref::SketchCurve { sketch, curve } => {
                let (sk, fr) = self.sketch(*sketch)?;
                match sk.curve(*curve).map(|c| c.geom.clone()) {
                    Some(crate::sketch::Geom::Line { a, b }) => {
                        let (pa, pb) = (fr.to_world(sk.pv(a)), fr.to_world(sk.pv(b)));
                        Ok((pa, (pb - pa).normalize()))
                    }
                    Some(crate::sketch::Geom::Circle { c, .. }) | Some(crate::sketch::Geom::Arc { c, .. }) => Ok((fr.to_world(sk.pv(c)), fr.normal())),
                    None => Err("the axis curve no longer exists".into()),
                }
            }
            Ref::Edge { point } => {
                let (e, _) = self.find_edge(v3(*point))?;
                let pts: Vec<DVec3> = e.approximation_segments().collect();
                if e_is_line(&e) {
                    let (a, b) = (e.start_point(), e.end_point());
                    Ok((a, (b - a).normalize()))
                } else if pts.len() >= 3 {
                    // circular edge: axis through its centre, normal to its plane
                    let (c, n) = circle_from_points(&pts).ok_or("edge is not straight or circular")?;
                    Ok((c, n))
                } else {
                    Err("edge is not straight".into())
                }
            }
            Ref::OriginPlane { .. } | Ref::FeaturePlane { .. } | Ref::Face { .. } => {
                let f = self.frame_of(r)?;
                Ok((v3(f.origin), f.normal()))
            }
            _ => Err("select a line or edge for the axis".into()),
        }
    }

    fn sketch(&self, id: FeatId) -> R<(&crate::sketch::Sketch, Frame)> {
        match self.doc.feature(id).map(|f| &f.op) {
            Some(Op::Sketch { sketch, .. }) => Ok((sketch, self.frames.get(&id).copied().ok_or("sketch plane is missing")?)),
            _ => Err("the referenced sketch no longer exists".into()),
        }
    }

    /// Faces for region references, grouped with the frame they lie in.
    fn profile_faces(&self, refs: &[Ref]) -> R<Vec<(Face, Frame)>> {
        let mut out = vec![];
        let mut cache: HashMap<FeatId, Vec<Region>> = HashMap::new();
        for r in refs {
            match r {
                Ref::Region { sketch, point } => {
                    let (sk, fr) = self.sketch(*sketch)?;
                    let regions = cache.entry(*sketch).or_insert_with(|| find_regions(sk));
                    let p = DVec2::from(*point);
                    let reg = regions
                        .iter()
                        .filter(|g| g.contains(p))
                        .min_by(|a, b| a.area.total_cmp(&b.area))
                        .ok_or("a profile region no longer exists")?;
                    out.push((region_face(reg, &fr)?, fr));
                }
                Ref::Face { point, normal } => {
                    let (f, _) = self.find_face(v3(*point), v3(*normal))?;
                    let g = face_geom(&f);
                    let fr = frame_from(g.center, g.normal);
                    out.push((f, fr));
                }
                _ => return Err("select sketch regions or faces".into()),
            }
        }
        if out.is_empty() {
            return Err("select at least one profile".into());
        }
        Ok(out)
    }

    fn model_size(&self) -> f64 {
        let mut lo = DVec3::splat(-100.0);
        let mut hi = DVec3::splat(100.0);
        for b in &self.bodies {
            let bb = opencascade::bounding_box::aabb(&b.shape);
            lo = lo.min(bb.min());
            hi = hi.max(bb.max());
        }
        (hi - lo).length() + lo.abs().max(hi.abs()).length()
    }

    // ------------------------------------------------------------ booleans

    /// Combine new tool solids with the model according to `op`.
    fn apply(&mut self, fid: FeatId, tools: Vec<Shape>, op: BoolOp) -> R<()> {
        if tools.is_empty() {
            return Err("nothing was produced".into());
        }
        let tool = fuse_all(tools)?;
        match op {
            BoolOp::New => {
                for (i, s) in solids_of(&tool).into_iter().enumerate() {
                    self.bodies.push(Body { id: (fid, i as u32), shape: s });
                }
            }
            BoolOp::Add => {
                let mut merged = tool;
                let mut keep = vec![];
                let mut first_id = None;
                for b in self.bodies.drain(..) {
                    if overlaps(&b.shape, &merged) {
                        first_id.get_or_insert(b.id);
                        merged = checked(merged.union(&b.shape).shape)?;
                    } else {
                        keep.push(b);
                    }
                }
                self.bodies = keep;
                let base = first_id.unwrap_or((fid, 0));
                for (i, s) in solids_of(&merged).into_iter().enumerate() {
                    let id = if i == 0 { base } else { (fid, 100 + i as u32) };
                    self.bodies.push(Body { id, shape: s });
                }
            }
            BoolOp::Remove | BoolOp::Intersect => {
                if self.bodies.is_empty() {
                    return Err("there is no body to cut".into());
                }
                let mut out = vec![];
                for b in self.bodies.drain(..) {
                    let r = if op == BoolOp::Remove { b.shape.subtract(&tool).shape } else { b.shape.intersect(&tool).shape };
                    let r = checked(r)?;
                    for (i, s) in solids_of(&r).into_iter().enumerate() {
                        let id = if i == 0 { b.id } else { (b.id.0, b.id.1 + 1000 * i as u32) };
                        out.push(Body { id, shape: s });
                    }
                }
                self.bodies = out;
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ features

    pub fn run(&mut self, fid: FeatId) -> R<()> {
        let f = self.doc.feature(fid).ok_or("missing feature")?;
        match &f.op {
            Op::Sketch { plane, frame, .. } => {
                let fr = self.frame_of(plane).unwrap_or(*frame);
                self.frames.insert(fid, fr);
                Ok(())
            }
            Op::Plane { base, offset } => {
                let base = base.as_ref().ok_or("select a base plane or face")?;
                let mut fr = self.frame_of(base)?;
                let d = self.len(offset)?;
                fr.origin = (v3(fr.origin) + fr.normal() * d).into();
                self.frames.insert(fid, fr);
                Ok(())
            }
            Op::Extrude { profiles, end, depth, opposite, second, depth2, up_to, op, draft } => {
                let faces = self.profile_faces(profiles)?;
                let d = match end {
                    EndType::ThroughAll | EndType::UpToFace => self.model_size() * 2.0,
                    _ => self.len(depth)?,
                };
                let d2 = if *second { self.len(depth2)? } else { 0.0 };
                let _ = self.ang(draft)?;
                let sign = if *opposite { -1.0 } else { 1.0 };
                let build = |sign: f64| -> R<Vec<Shape>> {
                    let mut tools = vec![];
                    for (face, fr) in &faces {
                        let n = fr.normal() * sign;
                        let solid: Shape = match end {
                            EndType::Symmetric => {
                                let moved = Shape::from(face).translated(-n * (d / 2.0));
                                let f2 = moved.faces().next().ok_or("bad profile")?;
                                checked(f2.extrude(n * d).into())?
                            }
                            _ => {
                                let mut s: Shape = checked(face.extrude(n * d).into())?;
                                if *second && d2 > 0.0 {
                                    let back: Shape = checked(face.extrude(-n * d2).into())?;
                                    s = checked(s.union(&back).shape)?;
                                }
                                s
                            }
                        };
                        let solid = if *end == EndType::UpToFace {
                            let r = up_to.as_ref().ok_or("select the face to extrude up to")?;
                            let (target, _) = self.find_face_ref(r)?;
                            // keep the part of the long prism that lies before the target plane
                            let g = face_geom(&target);
                            let (c, tn) = (g.center, g.normal);
                            let half = half_space(c, if tn.dot(n) > 0.0 { tn } else { -tn }, self.model_size() * 4.0);
                            checked(solid.subtract(&half).shape)?
                        } else {
                            solid
                        };
                        tools.push(solid);
                    }
                    Ok(tools)
                };
                let mut tools = build(sign)?;
                // Cutting away from the material does nothing: go into it instead, as the original does.
                let cutting = matches!(op, BoolOp::Remove | BoolOp::Intersect);
                if cutting && *end != EndType::Symmetric && !*second && !tools.iter().any(|t| self.bodies.iter().any(|b| volume(&b.shape.intersect(t).shape) > 1e-9)) {
                    tools = build(-sign)?;
                }
                self.apply(fid, tools, *op)
            }
            Op::Revolve { profiles, axis, mode, angle, op } => {
                let faces = self.profile_faces(profiles)?;
                let axis = axis.as_ref().ok_or("select an axis")?;
                let (o, dir) = self.axis_of(axis)?;
                let a = if mode == "full" { 360.0 } else { self.ang(angle)? };
                let mut tools = vec![];
                for (face, _) in &faces {
                    let s: Shape = if mode == "symmetric" {
                        let rotated = Shape::from(face).rotated_about(o, dir, -(a / 2.0).to_radians());
                        let f = rotated.faces().next().ok_or("bad profile")?;
                        f.revolve(o, dir, Some(opencascade::angle::Angle::Degrees(a))).into()
                    } else {
                        face.revolve(o, dir, Some(opencascade::angle::Angle::Degrees(a))).into()
                    };
                    tools.push(checked(s)?);
                }
                self.apply(fid, tools, *op)
            }
            Op::Sweep { profiles, path, op } => {
                let faces = self.profile_faces(profiles)?;
                let wire = self.path_wire(path)?;
                let mut tools = vec![];
                for (face, _) in &faces {
                    tools.push(checked(face.sweep_along(&wire).into())?);
                }
                self.apply(fid, tools, *op)
            }
            Op::Loft { profiles, ruled, op } => {
                let faces = self.profile_faces(profiles)?;
                if faces.len() < 2 {
                    return Err("select two or more profiles".into());
                }
                let wires: Vec<Wire> = faces.iter().map(|(f, _)| f.outer_wire()).collect();
                let s = loft(&wires, *ruled)?;
                self.apply(fid, vec![s], *op)
            }
            Op::Fillet { edges, radius, variable, end_radius } => {
                let r = self.len(radius)?;
                if r <= 0.0 {
                    return Err("radius must be positive".into());
                }
                let r2 = if *variable { Some(self.len(end_radius)?) } else { None };
                self.edge_op(edges, |shape, es| match r2 {
                    Some(r2) => shape.variable_fillet_edges([(0.0, r), (1.0, r2)], es),
                    None => shape.fillet_edges(r, es),
                })
            }
            Op::Chamfer { edges, distance } => {
                let d = self.len(distance)?;
                if d <= 0.0 {
                    return Err("distance must be positive".into());
                }
                self.edge_op(edges, |shape, es| shape.chamfer_edges(d, es))
            }
            Op::Shell { faces, thickness } => {
                let t = self.len(thickness)?;
                if faces.is_empty() {
                    return Err("select faces to remove".into());
                }
                let mut by_body: HashMap<usize, Vec<Face>> = HashMap::new();
                for r in faces {
                    let (f, bi) = self.find_face_ref(r)?;
                    by_body.entry(bi).or_default().push(f);
                }
                for (bi, fs) in by_body {
                    let s = self.bodies[bi].shape.hollow(-t, fs.iter());
                    self.bodies[bi].shape = checked(s)?;
                }
                Ok(())
            }
            Op::Hole { points, hole_type, diameter, depth, through, cb_diameter, cb_depth, cs_diameter, cs_angle, tapped: _ } => {
                let dia = self.len(diameter)?;
                let size = self.model_size();
                let depth = if *through { size * 2.0 } else { self.len(depth)? };
                let mut tools = vec![];
                for r in points {
                    let (p, n) = self.hole_point(r)?;
                    let down = -n;
                    // start slightly above the surface so the top face is cut cleanly
                    let start = p - down * 0.01;
                    let mut tool = Shape::cylinder(start, dia / 2.0, down, depth + 0.01);
                    match hole_type {
                        HoleType::Simple => {}
                        HoleType::Counterbore => {
                            let cd = self.len(cb_diameter)?;
                            let ch = self.len(cb_depth)?;
                            let cb = Shape::cylinder(p - down * size, cd / 2.0, down, size + ch);
                            tool = checked(tool.union(&cb).shape)?;
                        }
                        HoleType::Countersink => {
                            let cd = self.len(cs_diameter)?;
                            let ca = self.ang(cs_angle)?.to_radians();
                            let h = (cd - dia) / 2.0 / (ca / 2.0).tan();
                            let cone = cone_shape(p, down, cd / 2.0, dia / 2.0, h.max(0.01))?;
                            let top = Shape::cylinder(p - down * size, cd / 2.0, down, size);
                            tool = checked(tool.union(&cone).shape)?;
                            tool = checked(tool.union(&top).shape)?;
                        }
                    }
                    tools.push(tool);
                }
                if tools.is_empty() {
                    return Err("select sketch points for the holes".into());
                }
                self.apply(fid, tools, BoolOp::Remove)
            }
            Op::Draft { .. } => Err("Draft is not available in this kernel build yet".into()),
            Op::LinearPattern { bodies, direction, spacing, count, second, spacing2, count2 } => {
                let dir = direction.as_ref().ok_or("select a direction edge")?;
                let (_, d1) = self.axis_of(dir)?;
                let (s1, n1) = (self.len(spacing)?, self.count(count)?);
                let (d2, s2, n2) = match second {
                    Some(r) => (self.axis_of(r)?.1, self.len(spacing2)?, self.count(count2)?),
                    None => (DVec3::ZERO, 0.0, 1),
                };
                let idx = self.pattern_bodies(bodies)?;
                for bi in idx {
                    let base = self.bodies[bi].shape.translated(DVec3::ZERO);
                    let mut acc = base.translated(DVec3::ZERO);
                    for i in 0..n1 {
                        for j in 0..n2 {
                            if i == 0 && j == 0 {
                                continue;
                            }
                            let off = d1 * s1 * i as f64 + d2 * s2 * j as f64;
                            acc = checked(acc.union(&base.translated(off)).shape)?;
                        }
                    }
                    self.bodies[bi].shape = acc;
                }
                Ok(())
            }
            Op::CircularPattern { bodies, axis, count, angle } => {
                let ax = axis.as_ref().ok_or("select an axis")?;
                let (o, d) = self.axis_of(ax)?;
                let n = self.count(count)?;
                let total = self.ang(angle)?;
                let step = if (total - 360.0).abs() < 1e-9 { total / n as f64 } else { total / (n.max(2) - 1) as f64 };
                let idx = self.pattern_bodies(bodies)?;
                for bi in idx {
                    let base = self.bodies[bi].shape.translated(DVec3::ZERO);
                    let mut acc = base.translated(DVec3::ZERO);
                    for i in 1..n {
                        acc = checked(acc.union(&base.rotated_about(o, d, (step * i as f64).to_radians())).shape)?;
                    }
                    self.bodies[bi].shape = acc;
                }
                Ok(())
            }
            Op::Mirror { bodies, plane } => {
                let pl = plane.as_ref().ok_or("select a mirror plane")?;
                let fr = self.frame_of(pl)?;
                let idx = self.pattern_bodies(bodies)?;
                for bi in idx {
                    let m = mirror_shape(&self.bodies[bi].shape, v3(fr.origin), fr.normal());
                    let u = checked(self.bodies[bi].shape.union(&m).shape)?;
                    self.bodies[bi].shape = u;
                }
                Ok(())
            }
            Op::Boolean { kind, target, tools, keep_tools } => {
                let t = self.body_index(target.as_ref().ok_or("select the target body")?)?;
                let mut tool_idx = vec![];
                for r in tools {
                    let i = self.body_index(r)?;
                    if i != t && !tool_idx.contains(&i) {
                        tool_idx.push(i);
                    }
                }
                if tool_idx.is_empty() {
                    return Err("select tool bodies".into());
                }
                let mut shape = self.bodies[t].shape.translated(DVec3::ZERO);
                for &i in &tool_idx {
                    let tool = &self.bodies[i].shape;
                    shape = checked(match kind {
                        BooleanKind::Union => shape.union(tool).shape,
                        BooleanKind::Subtract => shape.subtract(tool).shape,
                        BooleanKind::Intersect => shape.intersect(tool).shape,
                    })?;
                }
                self.bodies[t].shape = shape;
                if !keep_tools || *kind == BooleanKind::Union {
                    let ids: Vec<BodyId> = tool_idx.iter().map(|&i| self.bodies[i].id).collect();
                    self.bodies.retain(|b| !ids.contains(&b.id));
                }
                Ok(())
            }
            Op::Split { target, plane } => {
                let t = self.body_index(target.as_ref().ok_or("select the body to split")?)?;
                let fr = self.frame_of(plane.as_ref().ok_or("select a split plane")?)?;
                let size = self.model_size() * 4.0;
                let half = half_space(v3(fr.origin), fr.normal(), size);
                let b = self.bodies.remove(t);
                let a = checked(b.shape.subtract(&half).shape)?;
                let c = checked(b.shape.intersect(&half).shape)?;
                let mut k = 0;
                for s in solids_of(&a).into_iter().chain(solids_of(&c)) {
                    let id = if k == 0 { b.id } else { (fid, k) };
                    self.bodies.push(Body { id, shape: s });
                    k += 1;
                }
                Ok(())
            }
            Op::DeleteBody { bodies } => {
                let mut ids = vec![];
                for r in bodies {
                    ids.push(self.bodies[self.body_index(r)?].id);
                }
                self.bodies.retain(|b| !ids.contains(&b.id));
                Ok(())
            }
            Op::Import { step, .. } => {
                let path = std::env::temp_dir().join(format!("cad-rust-import-{}-{fid}.step", std::process::id()));
                std::fs::write(&path, step).map_err(|e| e.to_string())?;
                let s = Shape::read_step(&path).map_err(|e| format!("could not read STEP: {e:?}"));
                let _ = std::fs::remove_file(&path);
                let s = s?;
                for (i, so) in solids_of(&s).into_iter().enumerate() {
                    self.bodies.push(Body { id: (fid, i as u32), shape: so });
                }
                Ok(())
            }
        }
    }

    fn find_face_ref(&self, r: &Ref) -> R<(Face, usize)> {
        match r {
            Ref::Face { point, normal } => self.find_face(v3(*point), v3(*normal)),
            _ => Err("select a face".into()),
        }
    }

    fn hole_point(&self, r: &Ref) -> R<(DVec3, DVec3)> {
        match r {
            Ref::SketchPoint { sketch, point } => {
                let (sk, fr) = self.sketch(*sketch)?;
                let p = sk.point(*point).ok_or("a hole point no longer exists")?;
                Ok((fr.to_world(p.v()), fr.normal()))
            }
            Ref::Vertex { point } => Ok((v3(*point), DVec3::Z)),
            _ => Err("select sketch points for the holes".into()),
        }
    }

    fn pattern_bodies(&self, refs: &[Ref]) -> R<Vec<usize>> {
        if refs.is_empty() {
            return Err("select bodies".into());
        }
        let mut v = vec![];
        for r in refs {
            let i = self.body_index(r)?;
            if !v.contains(&i) {
                v.push(i);
            }
        }
        Ok(v)
    }

    fn path_wire(&self, refs: &[Ref]) -> R<Wire> {
        let mut edges = vec![];
        for r in refs {
            match r {
                Ref::SketchCurve { sketch, curve } => {
                    let (sk, fr) = self.sketch(*sketch)?;
                    let c = sk.curve(*curve).ok_or("a path curve no longer exists")?;
                    let pts = sk.tessellate(c, 1.0);
                    match c.geom {
                        crate::sketch::Geom::Line { .. } => edges.push(Edge::segment(fr.to_world(pts[0]), fr.to_world(pts[1]))),
                        _ => {
                            let (a, m, b) = (pts[0], pts[pts.len() / 2], pts[pts.len() - 1]);
                            if (a - b).length() < 1e-9 {
                                let q = pts[pts.len() / 4];
                                let q3 = pts[3 * pts.len() / 4];
                                edges.push(Edge::arc(fr.to_world(a), fr.to_world(q), fr.to_world(m)));
                                edges.push(Edge::arc(fr.to_world(m), fr.to_world(q3), fr.to_world(b)));
                            } else {
                                edges.push(Edge::arc(fr.to_world(a), fr.to_world(m), fr.to_world(b)));
                            }
                        }
                    }
                }
                Ref::Edge { point } => edges.push(self.find_edge(v3(*point))?.0),
                _ => return Err("select sketch curves or edges for the path".into()),
            }
        }
        if edges.is_empty() {
            return Err("select a path".into());
        }
        Ok(Wire::from_unordered_edges(edges.iter(), opencascade::primitives::EdgeConnection::default()))
    }

    fn edge_op(&mut self, refs: &[Ref], f: impl Fn(&Shape, Vec<&Edge>) -> Shape) -> R<()> {
        if refs.is_empty() {
            return Err("select edges".into());
        }
        let mut by_body: HashMap<usize, Vec<Edge>> = HashMap::new();
        for r in refs {
            let p = match r {
                Ref::Edge { point } => v3(*point),
                _ => return Err("select edges".into()),
            };
            let (e, bi) = self.find_edge(p)?;
            by_body.entry(bi).or_default().push(e);
        }
        for (bi, es) in by_body {
            let s = f(&self.bodies[bi].shape, es.iter().collect());
            self.bodies[bi].shape = checked(s)?;
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ rebuild

/// Rebuild the document up to its rollback bar. `skip` lists features to treat
/// as failed without running (they crashed the kernel on an earlier attempt);
/// `progress` is told which feature is about to run.
pub fn rebuild(doc: &Document, skip: &[FeatId], mut progress: impl FnMut(FeatId)) -> RebuildResult {
    let mut m = Model::new(doc);
    let mut out = RebuildResult::default();
    let end = doc.rollback_index();
    for (i, f) in doc.features.iter().enumerate() {
        // Sketch frames are needed for display even past the rollback bar.
        if i >= end || f.suppressed {
            if let Op::Sketch { plane, frame, .. } = &f.op {
                out.frames.insert(f.id, m.frame_of(plane).unwrap_or(*frame));
            }
            continue;
        }
        let error = if skip.contains(&f.id) {
            Some("the kernel crashed on this feature".to_string())
        } else {
            progress(f.id);
            m.run(f.id).err()
        };
        out.features.push(FeatureStatus { id: f.id, error });
    }
    out.frames.extend(m.frames.iter().map(|(k, v)| (*k, *v)));
    let names: Vec<String> = (1..=m.bodies.len()).map(|i| format!("Body {i}")).collect();
    for (b, name) in m.bodies.iter().zip(names) {
        if let Ok(mut mesh) = mesh_body(&b.shape) {
            mesh.id = b.id;
            mesh.name = name;
            out.bodies.push(mesh);
        }
    }
    out
}

/// Rebuild and write every body to a STEP or STL file.
pub fn export(doc: &Document, format: &str, path: &str) -> Result<(), String> {
    let mut m = Model::new(doc);
    let end = doc.rollback_index();
    for (i, f) in doc.features.iter().enumerate() {
        if i < end && !f.suppressed {
            let _ = m.run(f.id);
        }
    }
    if m.bodies.is_empty() {
        return Err("there are no bodies to export".into());
    }
    let shapes: Vec<&Shape> = m.bodies.iter().map(|b| &b.shape).collect();
    let all = opencascade::primitives::Compound::from_shapes(shapes);
    let shape: Shape = all.into();
    match format {
        "step" => shape.write_step(path).map_err(|e| format!("{e:?}")),
        "stl" => shape.write_stl_with_tolerance(path, 0.01).map_err(|e| format!("{e:?}")),
        _ => Err("unknown format".into()),
    }
}

// ------------------------------------------------------------------ helpers

/// The frame of the plane through `p` with normal `n`, anchored at the projection
/// of the world origin so it does not depend on where on the face you clicked.
pub fn plane_frame(p: DVec3, n: DVec3) -> Frame {
    let n = n.normalize();
    frame_from(n * p.dot(n), n)
}

pub fn frame_from(p: DVec3, n: DVec3) -> Frame {
    let n = n.normalize();
    let x = if n.cross(DVec3::Z).length() < 1e-6 {
        DVec3::X * n.z.signum()
    } else {
        // horizontal x axis for vertical-ish faces
        let x = DVec3::Z.cross(n).normalize();
        if n.z.abs() > 0.99 { DVec3::X } else { x }
    };
    let x = (x - n * x.dot(n)).normalize();
    let y = n.cross(x);
    Frame { origin: p.into(), x: x.into(), y: y.into(), normal: n.into() }
}

/// Join neighbouring arcs of the same circle (regions split circles at artificial points).
fn merge_arcs(segs: &[Seg]) -> Vec<Seg> {
    use std::f64::consts::TAU;
    let joinable = |a: &Seg, b: &Seg| -> Option<Seg> {
        if let (Seg::Arc { c, r, t0, t1 }, Seg::Arc { c: c2, r: r2, t0: u0, t1: u1 }) = (a, b) {
            let same = (*c - *c2).length() < 1e-7 && (r - r2).abs() < 1e-7;
            let dir = (t1 - t0).signum() == (u1 - u0).signum();
            let gap = (u0 - t1).rem_euclid(TAU);
            if same && dir && (gap < 1e-9 || TAU - gap < 1e-9) {
                return Some(Seg::Arc { c: *c, r: *r, t0: *t0, t1: t1 + (u1 - u0) });
            }
        }
        None
    };
    let mut out: Vec<Seg> = vec![];
    for s in segs {
        if let Some(m) = out.last().and_then(|l| joinable(l, s)) {
            *out.last_mut().unwrap() = m;
        } else {
            out.push(s.clone());
        }
    }
    if out.len() > 1 {
        if let Some(m) = joinable(&out[out.len() - 1], &out[0]) {
            out.pop();
            out[0] = m;
        }
    }
    out
}

fn region_face(r: &Region, fr: &Frame) -> R<Face> {
    let wire = |segs: &[Seg]| -> Wire {
        let segs = merge_arcs(segs);
        let edges: Vec<Edge> = segs
            .iter()
            .map(|s| match s {
                Seg::Line { a, b } => Edge::segment(fr.to_world(*a), fr.to_world(*b)),
                Seg::Arc { c, r, t0, t1 } if (t1 - t0).abs() > std::f64::consts::TAU - 1e-6 => {
                    // full circle; its direction follows the axis
                    let n = if t1 > t0 { fr.normal() } else { -fr.normal() };
                    Edge::circle(fr.to_world(*c), n, *r)
                }
                Seg::Arc { .. } => Edge::arc(fr.to_world(s.start()), fr.to_world(s.mid()), fr.to_world(s.end())),
            })
            .collect();
        Wire::from_edges(edges.iter())
    };
    let outer = wire(&r.outer.segs);
    let holes: Vec<Wire> = r.holes.iter().map(|h| wire(&h.segs)).collect();
    Ok(Face::from_wire_with_holes(&outer, &holes))
}

fn checked(s: Shape) -> R<Shape> {
    if s.inner.IsNull() {
        return Err("the operation failed".into());
    }
    Ok(s)
}

fn fuse_all(mut tools: Vec<Shape>) -> R<Shape> {
    let mut acc = tools.remove(0);
    for t in tools {
        acc = checked(acc.union(&t).shape)?;
    }
    Ok(acc)
}

fn overlaps(a: &Shape, b: &Shape) -> bool {
    let ba = opencascade::bounding_box::aabb(a);
    let bb = opencascade::bounding_box::aabb(b);
    let tol = 1e-6;
    if ba.min().cmpgt(bb.max() + tol).any() || bb.min().cmpgt(ba.max() + tol).any() {
        return false;
    }
    volume(&a.intersect(b).shape) > 1e-9 || shares_face(a, b)
}

fn shares_face(a: &Shape, b: &Shape) -> bool {
    let fa: Vec<(DVec3, DVec3)> = unique_faces(a).iter().map(face_geom).filter(|g| g.planar).map(|g| (g.center, g.normal)).collect();
    unique_faces(b).iter().map(face_geom).filter(|g| g.planar).any(|g| {
        let (c, n) = (g.center, g.normal);
        fa.iter().any(|(c2, n2)| n.dot(*n2) < -0.999 && (c - *c2).dot(n).abs() < 1e-6)
    })
}

pub fn volume(s: &Shape) -> f64 {
    let mut props = ffi::g_prop::GProps_new();
    ffi::b_rep_g_prop::BRepGProp::VolumeProperties(&s.inner, props.pin_mut(), true, false, false);
    props.Mass()
}

pub fn surface_area(s: &Shape) -> f64 {
    let mut props = ffi::g_prop::GProps_new();
    ffi::b_rep_g_prop::BRepGProp::SurfaceProperties(&s.inner, props.pin_mut(), false, false);
    props.Mass()
}

fn explore(s: &Shape, kind: ffi::top_abs::TopAbs_ShapeEnum) -> Vec<cxx::UniquePtr<ffi::topo_ds::TopoDS_Shape>> {
    let mut map = ffi::top_tools::new_indexed_map_of_shape();
    ffi::top_exp::TopExp::MapShapes(&s.inner, kind, map.pin_mut());
    (1..=map.Extent()).map(|i| ffi::topo_ds::TopoDS_Shape_to_owned(map.FindKey(i))).collect()
}

pub fn solids_of(s: &Shape) -> Vec<Shape> {
    explore(s, ffi::top_abs::TopAbs_ShapeEnum::TopAbs_SOLID).iter().map(|x| Shape::from_shape(x)).collect()
}

pub fn unique_faces(s: &Shape) -> Vec<Face> {
    explore(s, ffi::top_abs::TopAbs_ShapeEnum::TopAbs_FACE).iter().map(|x| Face::from_face(ffi::topo_ds::TopoDS::Face(x))).collect()
}

pub fn unique_edges(s: &Shape) -> Vec<Edge> {
    explore(s, ffi::top_abs::TopAbs_ShapeEnum::TopAbs_EDGE).iter().map(|x| Edge::from_edge(ffi::topo_ds::TopoDS::Edge(x))).collect()
}

/// Edges shared by two faces: leaves out seams of closed surfaces such as cylinders.
pub fn visible_edges(s: &Shape) -> Vec<Edge> {
    let mut map = ffi::top_tools::new_indexed_data_map_of_shape_list_of_shape();
    ffi::top_exp::TopExp::MapShapesAndUniqueAncestors(&s.inner, ffi::top_abs::TopAbs_ShapeEnum::TopAbs_EDGE, ffi::top_abs::TopAbs_ShapeEnum::TopAbs_FACE, map.pin_mut(), false);
    (1..=map.Extent())
        .filter(|&i| map.FindFromIndex(i).Size() != 1)
        .map(|i| Edge::from_edge(ffi::topo_ds::TopoDS::Edge(map.FindKey(i))))
        .collect()
}

fn e_is_line(e: &Edge) -> bool {
    e.edge_type() == opencascade::primitives::EdgeType::Line
}

fn half_space(p: DVec3, n: DVec3, size: f64) -> Shape {
    // a big box on the +n side of the plane through p
    let fr = frame_from(p, n);
    let (x, y) = (v3(fr.x), v3(fr.y));
    let base = [p - x * size - y * size, p + x * size - y * size, p + x * size + y * size, p - x * size + y * size];
    let w = Wire::from_ordered_points(base).expect("square");
    let f = Face::from_wire(&w);
    f.extrude(n * size).into()
}

fn cone_shape(apex_side: DVec3, dir: DVec3, r_top: f64, r_bottom: f64, h: f64) -> R<Shape> {
    let s = Shape::cone().at(DVec3::ZERO).bottom_radius(r_top).top_radius(r_bottom).height(h).build();
    // the builder makes a Z-up cone at the origin; orient it along `dir`
    let z = DVec3::Z;
    let axis = z.cross(dir);
    let s = if axis.length() < 1e-9 {
        if dir.z < 0.0 { s.rotated(DVec3::X, std::f64::consts::PI) } else { s }
    } else {
        s.rotated(axis.normalize(), z.angle_between(dir))
    };
    checked(s.translated(apex_side))
}

fn mirror_shape(s: &Shape, p: DVec3, n: DVec3) -> Shape {
    // point inversion through p followed by a half turn about n is a plane mirror
    s.scaled(p, -1.0).rotated_about(p, n, std::f64::consts::PI)
}

fn loft(wires: &[Wire], ruled: bool) -> R<Shape> {
    let mut mk = ffi::b_rep_offset_api::BRepOffsetAPI_ThruSections_new(true);
    let _ = ruled;
    for w in wires {
        mk.pin_mut().AddWire(&w.inner);
    }
    mk.pin_mut().CheckCompatibility(true);
    mk.pin_mut().Build(&ffi::message::Message_ProgressRange_new());
    if !mk.IsDone() {
        return Err("loft failed".into());
    }
    checked(Shape::from_shape(mk.pin_mut().Shape()))
}

trait RotateAbout {
    fn rotated_about(&self, o: DVec3, axis: DVec3, angle: f64) -> Shape;
}

impl RotateAbout for Shape {
    fn rotated_about(&self, o: DVec3, axis: DVec3, angle: f64) -> Shape {
        self.translated(-o).rotated(axis, angle).translated(o)
    }
}

fn face_distance(f: &Face, p: DVec3) -> f64 {
    face_tris(f).iter().map(|t| point_triangle_distance(p, t[0], t[1], t[2])).fold(f64::INFINITY, f64::min)
}

/// Triangles of a face (meshing it first if needed), wound so the normal points out of the material.
pub fn face_tris(face: &Face) -> Vec<[DVec3; 3]> {
    let mut loc = ffi::top_loc::Location_new();
    let mut th = ffi::b_rep::BRep_Tool_Triangulation(&face.inner, loc.pin_mut());
    if th.IsNull() {
        let shape = ffi::topo_ds::cast_face_to_shape(&face.inner);
        let _m = ffi::b_rep_mesh::IncrementalMesh_new(shape, 0.05);
        loc = ffi::top_loc::Location_new();
        th = ffi::b_rep::BRep_Tool_Triangulation(&face.inner, loc.pin_mut());
    }
    let Ok(tri) = ffi::poly::Handle_Poly_Triangulation_Get(&th) else { return vec![] };
    let trsf = ffi::top_loc::TopLoc_Location_Transformation(&loc);
    let verts: Vec<DVec3> = (1..=tri.NbNodes())
        .map(|i| {
            let mut p = ffi::poly::Poly_Triangulation_Node(tri, i);
            p.pin_mut().Transform(&trsf);
            DVec3::new(p.X(), p.Y(), p.Z())
        })
        .collect();
    let reversed = face.orientation() != opencascade::primitives::FaceOrientation::Forward;
    (1..=tri.NbTriangles())
        .map(|i| {
            let t = tri.Triangle(i);
            let (a, b, c) = (t.Value(1) as usize - 1, t.Value(2) as usize - 1, t.Value(3) as usize - 1);
            if reversed { [verts[c], verts[b], verts[a]] } else { [verts[a], verts[b], verts[c]] }
        })
        .collect()
}

pub struct FaceGeom {
    /// A point on the face near its middle.
    pub center: DVec3,
    /// Area-weighted average normal.
    pub normal: DVec3,
    pub planar: bool,
    pub area: f64,
}

/// Face position and normal from its triangulation (OCCT's projection-based
/// normal query throws for points such as a cylinder's centre of mass).
pub fn face_geom(face: &Face) -> FaceGeom {
    let tris = face_tris(face);
    let mut area = 0.0;
    let mut nsum = DVec3::ZERO;
    let mut csum = DVec3::ZERO;
    for t in &tris {
        let n = (t[1] - t[0]).cross(t[2] - t[0]) * 0.5;
        let a = n.length();
        area += a;
        nsum += n;
        csum += (t[0] + t[1] + t[2]) / 3.0 * a;
    }
    let normal = nsum.normalize_or_zero();
    let centroid = if area > 0.0 { csum / area } else { DVec3::ZERO };
    let planar = !tris.is_empty() && tris.iter().all(|t| {
        let n = (t[1] - t[0]).cross(t[2] - t[0]);
        n.length() < 1e-12 || n.normalize().dot(normal) > 0.9999
    });
    // nearest triangle centre to the centroid, so the point lies on the face
    let center = tris
        .iter()
        .map(|t| (t[0] + t[1] + t[2]) / 3.0)
        .min_by(|a, b| (*a - centroid).length().total_cmp(&(*b - centroid).length()))
        .unwrap_or(centroid);
    let center = if planar { centroid_on_plane(centroid, center, normal) } else { center };
    FaceGeom { center, normal, planar, area }
}

fn centroid_on_plane(centroid: DVec3, on: DVec3, n: DVec3) -> DVec3 {
    centroid - n * (centroid - on).dot(n)
}

pub fn point_triangle_distance(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> f64 {
    // Ericson, Real-Time Collision Detection 5.1.5
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return (p - a).length();
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return (p - b).length();
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return (p - (a + ab * v)).length();
    }
    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return (p - c).length();
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return (p - (a + ac * w)).length();
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return (p - (b + (c - b) * w)).length();
    }
    let denom = 1.0 / (va + vb + vc);
    let v = vb * denom;
    let w = vc * denom;
    (p - (a + ab * v + ac * w)).length()
}

pub fn polyline_distance(pts: &[DVec3], p: DVec3) -> f64 {
    let mut best = f64::INFINITY;
    for w in pts.windows(2) {
        let d = w[1] - w[0];
        let t = if d.length_squared() < 1e-18 { 0.0 } else { ((p - w[0]).dot(d) / d.length_squared()).clamp(0.0, 1.0) };
        best = best.min((w[0] + d * t - p).length());
    }
    if pts.len() == 1 {
        best = (pts[0] - p).length();
    }
    best
}

fn circle_from_points(pts: &[DVec3]) -> Option<(DVec3, DVec3)> {
    let (a, b, c) = (pts[0], pts[pts.len() / 3], pts[2 * pts.len() / 3]);
    let ab = b - a;
    let ac = c - a;
    let n = ab.cross(ac);
    if n.length() < 1e-12 {
        return None;
    }
    let center = a + (n.cross(ab) * ac.length_squared() + ac.cross(n) * ab.length_squared()) / (2.0 * n.length_squared());
    Some((center, n.normalize()))
}

/// Triangulate a body face by face, keeping the face index of each triangle,
/// and collect its edges as polylines.
pub fn mesh_body(shape: &Shape) -> R<BodyMesh> {
    let bb = opencascade::bounding_box::aabb(shape);
    let diag = (bb.max() - bb.min()).length().max(1.0);
    let tol = (diag * 0.0008).clamp(0.005, 0.5);
    let mesher = ffi::b_rep_mesh::IncrementalMesh_new(&shape.inner, tol);
    if !mesher.IsDone() {
        return Err("meshing failed".into());
    }
    let mut out = BodyMesh {
        bbox: [bb.min().into(), bb.max().into()],
        volume: volume(shape),
        area: surface_area(shape),
        ..Default::default()
    };
    for (fi, face) in unique_faces(shape).into_iter().enumerate() {
        let mut loc = ffi::top_loc::Location_new();
        let th = ffi::b_rep::BRep_Tool_Triangulation(&face.inner, loc.pin_mut());
        let Ok(tri) = ffi::poly::Handle_Poly_Triangulation_Get(&th) else { continue };
        let trsf = ffi::top_loc::TopLoc_Location_Transformation(&loc);
        let base = (out.positions.len() / 3) as u32;
        let nn = tri.NbNodes();
        let mut verts = Vec::with_capacity(nn as usize);
        for i in 1..=nn {
            let mut p = ffi::poly::Poly_Triangulation_Node(tri, i);
            p.pin_mut().Transform(&trsf);
            verts.push(DVec3::new(p.X(), p.Y(), p.Z()));
        }
        let reversed = face.orientation() != opencascade::primitives::FaceOrientation::Forward;
        let mut normals = vec![DVec3::ZERO; verts.len()];
        let mut idx = vec![];
        for i in 1..=tri.NbTriangles() {
            let t = tri.Triangle(i);
            let (mut a, b, mut c) = (t.Value(1) as usize - 1, t.Value(2) as usize - 1, t.Value(3) as usize - 1);
            if reversed {
                std::mem::swap(&mut a, &mut c);
            }
            let n = (verts[b] - verts[a]).cross(verts[c] - verts[a]);
            normals[a] += n;
            normals[b] += n;
            normals[c] += n;
            idx.push([a, b, c]);
        }
        for (v, n) in verts.iter().zip(&normals) {
            let n = n.normalize_or_zero();
            out.positions.extend([v.x as f32, v.y as f32, v.z as f32]);
            out.normals.extend([n.x as f32, n.y as f32, n.z as f32]);
        }
        for t in idx {
            out.indices.extend(t.map(|i| base + i as u32));
            out.tri_face.push(fi as u32);
        }
        let g = face_geom(&face);
        out.faces.push(FaceInfo { center: g.center.into(), normal: g.normal.into(), planar: g.planar, area: g.area });
    }
    for e in visible_edges(shape) {
        let pts: Vec<DVec3> = e.approximation_segments().collect();
        if pts.len() < 2 {
            continue;
        }
        let length: f64 = pts.windows(2).map(|w| (w[1] - w[0]).length()).sum();
        // midpoint by arc length
        let mut acc = 0.0;
        let mut mid = pts[0];
        for w in pts.windows(2) {
            let l = (w[1] - w[0]).length();
            if acc + l >= length / 2.0 {
                mid = w[0].lerp(w[1], ((length / 2.0 - acc) / l.max(1e-12)).clamp(0.0, 1.0));
                break;
            }
            acc += l;
        }
        let ty = e.edge_type();
        out.edges.push(EdgeInfo {
            pts: pts.iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect(),
            mid: mid.into(),
            linear: ty == opencascade::primitives::EdgeType::Line,
            circular: ty == opencascade::primitives::EdgeType::Circle,
            length,
        });
    }
    Ok(out)
}

#[allow(dead_code)]
pub fn origin_frames() -> Vec<(OriginPlane, Frame)> {
    OriginPlane::ALL.iter().map(|p| (*p, p.frame())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_faces_have_outward_normals() {
        let b = Shape::box_with_dimensions(10.0, 20.0, 30.0);
        let m = mesh_body(&b).unwrap();
        assert_eq!(m.faces.len(), 6);
        for f in &m.faces {
            let c = DVec3::from(f.center);
            let n = DVec3::from(f.normal);
            let mid = DVec3::new(5.0, 10.0, 15.0);
            eprintln!("center {c:?} normal {n:?}");
            assert!((c - mid).dot(n) > 0.0, "normal should point outwards");
        }
        assert!((m.volume - 6000.0).abs() < 1e-6);
        assert_eq!(m.edges.len(), 12);
    }
}

#[cfg(test)]
mod feature_tests {
    use super::*;
    use crate::doc::{Document, Op, OriginPlane, Ref};
    use crate::sketch::{Geom, Sketch};

    fn rect_sketch(doc: &mut Document, w: f64, h: f64) -> FeatId {
        let mut s = Sketch::default();
        let p: Vec<_> = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        for i in 0..4 {
            s.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false);
        }
        doc.insert("Sketch 1".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Top }, sketch: s, frame: Frame::TOP })
    }

    fn extrude(doc: &mut Document, sk: FeatId, at: [f64; 2], depth: &str, end: EndType, op: BoolOp) -> FeatId {
        let mut o = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, depth: d, end: e, op: bop, .. } = &mut o {
            profiles.push(Ref::Region { sketch: sk, point: at });
            *d = depth.into();
            *e = end;
            *bop = op;
        }
        doc.insert("Extrude".into(), o)
    }

    fn ok(r: &RebuildResult) {
        for f in &r.features {
            assert!(f.error.is_none(), "feature {} failed: {:?}", f.id, f.error);
        }
    }

    #[test]
    fn extrude_then_cut_through_all_from_top_face() {
        let mut doc = Document::default();
        let s1 = rect_sketch(&mut doc, 40.0, 20.0);
        extrude(&mut doc, s1, [5.0, 5.0], "10", EndType::Blind, BoolOp::New);
        // circle on the top face, cut through all (direction must flip into the material)
        let mut s = Sketch::default();
        let c = s.add_point(DVec2::new(20.0, 10.0));
        s.add_curve(Geom::Circle { c, r: 5.0 }, false);
        let top = Ref::Face { point: [20.0, 10.0, 10.0], normal: [0.0, 0.0, 1.0] };
        let s2 = doc.insert("Sketch 2".into(), Op::Sketch { plane: top, sketch: s, frame: Frame::TOP });
        extrude(&mut doc, s2, [20.0, 10.0], "10", EndType::ThroughAll, BoolOp::Remove);
        let r = rebuild(&doc, &[], |_| {});
        ok(&r);
        assert_eq!(r.bodies.len(), 1);
        let want = 40.0 * 20.0 * 10.0 - std::f64::consts::PI * 25.0 * 10.0;
        assert!((r.bodies[0].volume - want).abs() < 1.0, "volume {} want {want}", r.bodies[0].volume);
        // the hole wall is one cylindrical face (no seam split)
        assert_eq!(r.bodies[0].faces.len(), 7);
    }

    #[test]
    fn fillet_and_shell() {
        let mut doc = Document::default();
        let s1 = rect_sketch(&mut doc, 40.0, 20.0);
        extrude(&mut doc, s1, [5.0, 5.0], "10", EndType::Blind, BoolOp::New);
        let mut f = Op::new_default("fillet").unwrap();
        if let Op::Fillet { edges, radius, .. } = &mut f {
            edges.push(Ref::Edge { point: [20.0, 0.0, 10.0] });
            *radius = "3".into();
        }
        doc.insert("Fillet".into(), f);
        let mut sh = Op::new_default("shell").unwrap();
        if let Op::Shell { faces, thickness } = &mut sh {
            faces.push(Ref::Face { point: [30.0, 15.0, 0.0], normal: [0.0, 0.0, -1.0] });
            *thickness = "1".into();
        }
        doc.insert("Shell".into(), sh);
        let r = rebuild(&doc, &[], |_| {});
        ok(&r);
        assert!(r.bodies[0].volume < 8000.0 * 0.5);
    }

    #[test]
    fn patterns_mirror_and_revolve() {
        let mut doc = Document::default();
        let s1 = rect_sketch(&mut doc, 10.0, 10.0);
        let e = extrude(&mut doc, s1, [5.0, 5.0], "10", EndType::Blind, BoolOp::New);
        let mut lp = Op::new_default("linearPattern").unwrap();
        if let Op::LinearPattern { bodies, direction, spacing, count, .. } = &mut lp {
            bodies.push(Ref::Body { body: (e, 0) });
            *direction = Some(Ref::Axis { axis: 0 });
            *spacing = "20".into();
            *count = "3".into();
        }
        doc.insert("LP".into(), lp);
        let mut m = Op::new_default("mirror").unwrap();
        if let Op::Mirror { bodies, plane } = &mut m {
            bodies.push(Ref::Body { body: (e, 0) });
            *plane = Some(Ref::OriginPlane { plane: OriginPlane::Front });
        }
        doc.insert("Mirror".into(), m);
        let r = rebuild(&doc, &[], |_| {});
        ok(&r);
        assert!((r.bodies[0].volume - 6000.0).abs() < 1e-3, "{}", r.bodies[0].volume);
        let bb = r.bodies[0].bbox;
        assert!((bb[0][1] + 10.0).abs() < 1e-6 && (bb[1][1] - 10.0).abs() < 1e-6, "{bb:?}");

        // revolve a rectangle about the Y axis -> a tube
        let mut doc = Document::default();
        let mut s = Sketch::default();
        let p: Vec<_> = [(5.0, 0.0), (10.0, 0.0), (10.0, 20.0), (5.0, 20.0)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        for i in 0..4 {
            s.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false);
        }
        let sk = doc.insert("S".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Top }, sketch: s, frame: Frame::TOP });
        let mut rv = Op::new_default("revolve").unwrap();
        if let Op::Revolve { profiles, axis, .. } = &mut rv {
            profiles.push(Ref::Region { sketch: sk, point: [7.0, 10.0] });
            *axis = Some(Ref::Axis { axis: 1 });
        }
        doc.insert("R".into(), rv);
        let r = rebuild(&doc, &[], |_| {});
        ok(&r);
        let want = std::f64::consts::PI * (100.0 - 25.0) * 20.0;
        assert!((r.bodies[0].volume - want).abs() < 1.0, "{} vs {want}", r.bodies[0].volume);
    }
}
