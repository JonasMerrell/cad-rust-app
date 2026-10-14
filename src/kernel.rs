//! Solid modelling on top of OpenCascade. This runs inside the kernel worker
//! process (see `worker.rs`), because a failing OCCT operation can abort the
//! process and must not take the editor down with it.

use std::collections::HashMap;

use glam::{DMat3, DVec2, DVec3};
use opencascade::primitives::{Edge, Face, Shape, Wire};
use opencascade_sys as ffi;
use serde::{Deserialize, Serialize};

use crate::doc::{BodyId, BoolOp, BooleanKind, Document, EndType, FeatId, HoleType, Op, OriginPlane, Ref};
use crate::expr::{Kind, Units};
use crate::regions::{find_regions, Region, Seg};
use crate::occ;
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
    #[serde(default)]
    pub centroid: [f64; 3],
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CurveMesh {
    pub feature: FeatId,
    pub pts: Vec<[f32; 3]>,
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
    #[serde(default)]
    pub curves: Vec<CurveMesh>,
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
    /// Wire results (helices).
    pub curves: Vec<(FeatId, Shape)>,
    /// Tool bodies and operation of each feature, for feature patterns.
    pub tools: HashMap<FeatId, (Vec<Shape>, BoolOp)>,
}

type R<T> = Result<T, String>;

fn v3(a: [f64; 3]) -> DVec3 {
    DVec3::from(a)
}

impl<'a> Model<'a> {
    pub fn new(doc: &'a Document) -> Self {
        let (vars, _) = doc.var_values();
        Model { doc, vars, bodies: vec![], frames: HashMap::new(), curves: vec![], tools: HashMap::new() }
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
                    Some(crate::sketch::Geom::Circle { c, .. }) | Some(crate::sketch::Geom::Arc { c, .. }) | Some(crate::sketch::Geom::Ellipse { c, .. }) => Ok((fr.to_world(sk.pv(c)), fr.normal())),
                    Some(_) => Err("use a line, circle or arc as the axis".into()),
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

    /// Combine new tool solids with the model according to `op`, remembering
    /// them so feature patterns and mirrors can replay this feature.
    fn apply(&mut self, fid: FeatId, tools: Vec<Shape>, op: BoolOp) -> R<()> {
        self.tools.insert(fid, (tools.iter().map(dup).collect(), op));
        self.apply_tools(fid, tools, op)
    }

    fn apply_tools(&mut self, fid: FeatId, tools: Vec<Shape>, op: BoolOp) -> R<()> {
        if tools.is_empty() {
            return Err("nothing was produced".into());
        }
        let tool = fuse_all(tools)?;
        match op {
            BoolOp::New => {
                let base = self.bodies.iter().filter(|b| b.id.0 == fid).count() as u32;
                for (i, s) in solids_of(&tool).into_iter().enumerate() {
                    self.bodies.push(Body { id: (fid, base + i as u32), shape: s });
                }
            }
            BoolOp::Add => {
                let mut merged = tool;
                let mut keep = vec![];
                let mut first_id = None;
                for b in self.bodies.drain(..) {
                    if overlaps(&b.shape, &merged) {
                        first_id.get_or_insert(b.id);
                        merged = occ::fuse(&merged, &b.shape)?;
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
                for b in std::mem::take(&mut self.bodies) {
                    let touched = overlaps(&b.shape, &tool);
                    if op == BoolOp::Remove && !touched {
                        out.push(b);
                        continue;
                    }
                    let r = if op == BoolOp::Remove { occ::cut(&b.shape, &tool)? } else { occ::common(&b.shape, &tool)? };
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
            Op::Plane { base, offset, mode, refs, angle, flip } => {
                let mut fr = self.plane_feature(base.as_ref(), offset, mode, refs, angle)?;
                if *flip {
                    fr = Frame { origin: fr.origin, x: (-v3(fr.x)).into(), y: fr.y, normal: (-fr.normal()).into() };
                }
                self.frames.insert(fid, fr);
                Ok(())
            }
            Op::Extrude { profiles, end, depth, opposite, second, depth2, up_to, op, draft } => {
                let faces = self.profile_faces(profiles)?;
                let d = match end {
                    EndType::ThroughAll | EndType::UpToFace => self.model_size() * 2.0,
                    _ => self.len(depth)?,
                };
                if d <= 0.0 {
                    return Err("depth must be positive".into());
                }
                let d2 = if *second { self.len(depth2)? } else { 0.0 };
                let _ = self.ang(draft)?;
                let sign = if *opposite { -1.0 } else { 1.0 };
                let build = |sign: f64| -> R<Vec<Shape>> {
                    let mut tools = vec![];
                    for (face, fr) in &faces {
                        let n = fr.normal() * sign;
                        let fs = Shape::from(face);
                        let solid = match end {
                            EndType::Symmetric => occ::prism(&occ::translate(&fs, -n * (d / 2.0))?, n * d)?,
                            _ => {
                                let mut s = occ::prism(&fs, n * d)?;
                                if *second && d2 > 0.0 {
                                    s = occ::fuse(&s, &occ::prism(&fs, -n * d2)?)?;
                                }
                                s
                            }
                        };
                        let solid = if *end == EndType::UpToFace {
                            let r = up_to.as_ref().ok_or("select the face to extrude up to")?;
                            let (target, _) = self.find_face_ref(r)?;
                            // keep the part of the long prism that lies before the target plane
                            let g = face_geom(&target);
                            let half = half_space(g.center, if g.normal.dot(n) > 0.0 { g.normal } else { -g.normal }, self.model_size() * 4.0)?;
                            occ::cut(&solid, &half)?
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
                if cutting && *end != EndType::Symmetric && !*second && !tools.iter().any(|t| self.bodies.iter().any(|b| overlaps_volume(&b.shape, t))) {
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
                    let fs = Shape::from(face);
                    let fs = if mode == "symmetric" { occ::rotate(&fs, o, dir, -(a / 2.0).to_radians())? } else { fs };
                    tools.push(occ::revolve(&fs, o, dir, a.to_radians())?);
                }
                self.apply(fid, tools, *op)
            }
            Op::Sweep { profiles, path, op } => {
                let faces = self.profile_faces(profiles)?;
                let spine = self.path_wire(path)?;
                let mut tools = vec![];
                for (face, _) in &faces {
                    tools.push(occ::sweep(&face.outer_wire(), &spine, true)?);
                }
                self.apply(fid, tools, *op)
            }
            Op::Loft { profiles, ruled, op } => {
                let faces = self.profile_faces(profiles)?;
                if faces.len() < 2 {
                    return Err("select two or more profiles".into());
                }
                let wires: Vec<Wire> = faces.iter().map(|(f, _)| f.outer_wire()).collect();
                let s = occ::loft(&wires, *ruled)?;
                self.apply(fid, vec![s], *op)
            }
            Op::Fillet { edges, radius, variable, end_radius } => {
                let r = self.len(radius)?;
                if r <= 0.0 {
                    return Err("radius must be positive".into());
                }
                let r2 = if *variable { self.len(end_radius)? } else { r };
                self.edge_op(edges, |shape, es| occ::fillet(shape, es, r, r2))
            }
            Op::Chamfer { edges, distance } => {
                let d = self.len(distance)?;
                if d <= 0.0 {
                    return Err("distance must be positive".into());
                }
                self.edge_op(edges, |shape, es| occ::chamfer(shape, es, d))
            }
            Op::Shell { faces, thickness } => {
                let t = self.len(thickness)?;
                if faces.is_empty() {
                    return Err("select faces to remove".into());
                }
                self.face_op(faces, |shape, fs| occ::shell(shape, fs, -t))
            }
            Op::Draft { faces, neutral, angle } => {
                let a = self.ang(angle)?.to_radians();
                let fr = self.frame_of(neutral.as_ref().ok_or("select the neutral plane")?)?;
                let (p, n) = (v3(fr.origin), fr.normal());
                self.face_op(faces, |shape, fs| occ::draft(shape, fs, n, a, p, n))
            }
            Op::DeleteFace { faces } => self.face_op(faces, occ::delete_faces),
            Op::Thicken { faces, thickness, opposite, op } => {
                let t = self.len(thickness)?;
                let profiles = self.profile_faces(faces)?;
                let mut tools = vec![];
                for (f, _) in &profiles {
                    tools.push(occ::thicken(&Shape::from(f), if *opposite { -t } else { t })?);
                }
                self.apply(fid, tools, *op)
            }
            Op::Helix { axis, radius, pitch, turns, left } => {
                let (o, d) = self.axis_of(axis.as_ref().ok_or("select an axis, circle or circular edge")?)?;
                let r = self.len(radius)?;
                let p = self.len(pitch)?;
                let n = crate::doc::eval(turns, Kind::Count, &self.vars, self.units())?;
                if r <= 0.0 || p == 0.0 || n <= 0.0 {
                    return Err("radius, pitch and turns must be positive".into());
                }
                let x = v3(frame_from(o, d).x);
                let h = occ::helix(o, d, x, r, p, n, *left)?;
                self.curves.push((fid, h));
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
                            tool = occ::fuse(&tool, &cb)?;
                        }
                        HoleType::Countersink => {
                            let cd = self.len(cs_diameter)?;
                            let ca = self.ang(cs_angle)?.to_radians();
                            let h = (cd - dia) / 2.0 / (ca / 2.0).tan();
                            let cone = cone_shape(p, down, cd / 2.0, dia / 2.0, h.max(0.01))?;
                            let top = Shape::cylinder(p - down * size, cd / 2.0, down, size);
                            tool = occ::fuse(&occ::fuse(&tool, &cone)?, &top)?;
                        }
                    }
                    tools.push(tool);
                }
                if tools.is_empty() {
                    return Err("select sketch points for the holes".into());
                }
                self.apply(fid, tools, BoolOp::Remove)
            }
            Op::LinearPattern { kind, bodies, direction, spacing, count, second, spacing2, count2 } => {
                let dir = direction.as_ref().ok_or("select a direction edge")?;
                let (_, d1) = self.axis_of(dir)?;
                let (s1, n1) = (self.len(spacing)?, self.count(count)?);
                let (d2, s2, n2) = match second {
                    Some(r) => (self.axis_of(r)?.1, self.len(spacing2)?, self.count(count2)?),
                    None => (DVec3::ZERO, 0.0, 1),
                };
                let mut xf = vec![];
                for i in 0..n1 {
                    for j in 0..n2 {
                        if i + j > 0 {
                            xf.push((DMat3::IDENTITY, d1 * s1 * i as f64 + d2 * s2 * j as f64));
                        }
                    }
                }
                self.pattern(fid, kind, bodies, &xf)
            }
            Op::CircularPattern { kind, bodies, axis, count, angle } => {
                let (o, d) = self.axis_of(axis.as_ref().ok_or("select an axis")?)?;
                let n = self.count(count)?;
                let total = self.ang(angle)?;
                let step = if (total - 360.0).abs() < 1e-9 { total / n as f64 } else { total / (n.max(2) - 1) as f64 };
                let xf: Vec<(DMat3, DVec3)> = (1..n)
                    .map(|i| {
                        let m = DMat3::from_axis_angle(d.normalize(), (step * i as f64).to_radians());
                        (m, o - m * o)
                    })
                    .collect();
                self.pattern(fid, kind, bodies, &xf)
            }
            Op::Mirror { kind, bodies, plane } => {
                let fr = self.frame_of(plane.as_ref().ok_or("select a mirror plane")?)?;
                let (p, n) = (v3(fr.origin), fr.normal());
                if kind == "feature" {
                    for fref in self.feature_refs(bodies)? {
                        let (tools, op) = self.tools.get(&fref).ok_or("that feature has nothing to mirror")?;
                        let op = *op;
                        let t: Vec<Shape> = tools.iter().map(|t| occ::mirror(t, p, n)).collect::<R<_>>()?;
                        self.apply_tools(fid, t, op)?;
                    }
                    return Ok(());
                }
                for bi in self.pattern_bodies(bodies)? {
                    let m = occ::mirror(&self.bodies[bi].shape, p, n)?;
                    self.bodies[bi].shape = occ::fuse(&self.bodies[bi].shape, &m)?;
                }
                Ok(())
            }
            Op::Transform { bodies, mode, direction, distance, axis, angle, copy } => {
                let (m, t) = if mode == "rotate" {
                    let (o, d) = self.axis_of(axis.as_ref().ok_or("select an axis")?)?;
                    let m = DMat3::from_axis_angle(d.normalize(), self.ang(angle)?.to_radians());
                    (m, o - m * o)
                } else {
                    let (_, d) = self.axis_of(direction.as_ref().ok_or("select a direction")?)?;
                    (DMat3::IDENTITY, d * self.len(distance)?)
                };
                let mut k = 0;
                for bi in self.pattern_bodies(bodies)? {
                    let moved = occ::transform(&self.bodies[bi].shape, m, t)?;
                    if *copy {
                        self.bodies.push(Body { id: (fid, k), shape: moved });
                        k += 1;
                    } else {
                        self.bodies[bi].shape = moved;
                    }
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
                let mut shape = dup(&self.bodies[t].shape);
                for &i in &tool_idx {
                    let tool = &self.bodies[i].shape;
                    shape = match kind {
                        BooleanKind::Union => occ::fuse(&shape, tool)?,
                        BooleanKind::Subtract => occ::cut(&shape, tool)?,
                        BooleanKind::Intersect => occ::common(&shape, tool)?,
                    };
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
                let half = half_space(v3(fr.origin), fr.normal(), size)?;
                let b = self.bodies.remove(t);
                let a = occ::cut(&b.shape, &half)?;
                let c = occ::common(&b.shape, &half)?;
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

    fn plane_feature(&self, base: Option<&Ref>, offset: &str, mode: &str, refs: &[Ref], angle: &str) -> R<Frame> {
        match mode {
            "angle" => {
                let base = self.frame_of(base.ok_or("select the plane to rotate")?)?;
                let (o, d) = self.axis_of(refs.first().ok_or("select the line to rotate about")?)?;
                let m = DMat3::from_axis_angle(d.normalize(), self.ang(angle)?.to_radians());
                let n = m * base.normal();
                Ok(Frame { origin: o.into(), x: d.normalize().into(), y: n.cross(d.normalize()).into(), normal: n.into() })
            }
            "threePoint" => {
                let pts: Vec<DVec3> = refs.iter().map(|r| self.point_of(r)).collect::<R<_>>()?;
                if pts.len() < 3 {
                    return Err("select three points".into());
                }
                let n = (pts[1] - pts[0]).cross(pts[2] - pts[0]);
                if n.length() < 1e-9 {
                    return Err("the points are in a line".into());
                }
                let x = (pts[1] - pts[0]).normalize();
                let n = n.normalize();
                Ok(Frame { origin: pts[0].into(), x: x.into(), y: n.cross(x).into(), normal: n.into() })
            }
            "normalToCurve" => {
                let (o, d) = self.tangent_of(refs.first().ok_or("select a curve or edge")?)?;
                Ok(frame_from(o, d))
            }
            "midPlane" => {
                let a = self.frame_of(base.ok_or("select the first plane")?)?;
                let b = self.frame_of(refs.first().ok_or("select the second plane")?)?;
                let n = a.normal();
                if n.cross(b.normal()).length() > 1e-6 {
                    return Err("the planes must be parallel".into());
                }
                let d = (v3(b.origin) - v3(a.origin)).dot(n);
                let mut f = a;
                f.origin = (v3(a.origin) + n * d / 2.0).into();
                Ok(f)
            }
            _ => {
                let mut fr = self.frame_of(base.ok_or("select a base plane or face")?)?;
                let d = self.len(offset)?;
                fr.origin = (v3(fr.origin) + fr.normal() * d).into();
                Ok(fr)
            }
        }
    }

    fn point_of(&self, r: &Ref) -> R<DVec3> {
        match r {
            Ref::SketchPoint { sketch, point } => {
                let (sk, fr) = self.sketch(*sketch)?;
                Ok(fr.to_world(sk.point(*point).ok_or("a point no longer exists")?.v()))
            }
            Ref::Vertex { point } | Ref::Edge { point } => Ok(v3(*point)),
            Ref::Face { point, .. } => Ok(v3(*point)),
            _ => Err("select points".into()),
        }
    }

    /// Start point and tangent of a curve-like reference.
    fn tangent_of(&self, r: &Ref) -> R<(DVec3, DVec3)> {
        let pts: Vec<DVec3> = match r {
            Ref::Edge { point } => self.find_edge(v3(*point))?.0.approximation_segments().collect(),
            Ref::Curve { feature } => self.curve_points(*feature)?,
            Ref::SketchCurve { sketch, curve } => {
                let (sk, fr) = self.sketch(*sketch)?;
                let c = sk.curve(*curve).ok_or("the curve no longer exists")?;
                sk.tessellate(c, 0.5).into_iter().map(|p| fr.to_world(p)).collect()
            }
            _ => return Err("select a curve or edge".into()),
        };
        if pts.len() < 2 {
            return Err("degenerate curve".into());
        }
        Ok((pts[0], (pts[1] - pts[0]).normalize()))
    }

    fn curve_points(&self, feature: FeatId) -> R<Vec<DVec3>> {
        let (_, s) = self.curves.iter().find(|c| c.0 == feature).ok_or("the curve no longer exists")?;
        Ok(unique_edges(s).iter().flat_map(|e| e.approximation_segments().collect::<Vec<_>>()).collect())
    }

    fn feature_refs(&self, refs: &[Ref]) -> R<Vec<FeatId>> {
        let v: Vec<FeatId> = refs.iter().filter_map(|r| if let Ref::Feature { feature } = r { Some(*feature) } else { None }).collect();
        if v.is_empty() {
            return Err("select features in the feature list".into());
        }
        Ok(v)
    }

    /// Repeat bodies (kind "part") or features' tool bodies (kind "feature") with each transform.
    fn pattern(&mut self, fid: FeatId, kind: &str, refs: &[Ref], xf: &[(DMat3, DVec3)]) -> R<()> {
        if kind == "feature" {
            for fref in self.feature_refs(refs)? {
                let (tools, op) = self.tools.get(&fref).ok_or("that feature has nothing to pattern")?;
                let op = *op;
                let base: Vec<Shape> = tools.iter().map(dup).collect();
                let mut all = vec![];
                for (m, t) in xf {
                    for b in &base {
                        all.push(occ::transform(b, *m, *t)?);
                    }
                }
                if !all.is_empty() {
                    self.apply_tools(fid, all, op)?;
                }
            }
            return Ok(());
        }
        for bi in self.pattern_bodies(refs)? {
            let base = dup(&self.bodies[bi].shape);
            let mut acc = dup(&base);
            for (m, t) in xf {
                acc = occ::fuse(&acc, &occ::transform(&base, *m, *t)?)?;
            }
            self.bodies[bi].shape = acc;
        }
        Ok(())
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
                Ref::Curve { feature } => {
                    let (_, s) = self.curves.iter().find(|c| c.0 == *feature).ok_or("the path curve no longer exists")?;
                    edges.extend(unique_edges(s));
                }
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
                _ => return Err("select sketch curves, edges or a helix for the path".into()),
            }
        }
        if edges.is_empty() {
            return Err("select a path".into());
        }
        Ok(Wire::from_unordered_edges(edges.iter(), opencascade::primitives::EdgeConnection::default()))
    }

    fn edge_op(&mut self, refs: &[Ref], f: impl Fn(&Shape, &[Edge]) -> R<Shape>) -> R<()> {
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
            self.bodies[bi].shape = f(&self.bodies[bi].shape, &es)?;
        }
        Ok(())
    }

    fn face_op(&mut self, refs: &[Ref], f: impl Fn(&Shape, &[Face]) -> R<Shape>) -> R<()> {
        if refs.is_empty() {
            return Err("select faces".into());
        }
        let mut by_body: HashMap<usize, Vec<Face>> = HashMap::new();
        for r in refs {
            let (face, bi) = self.find_face_ref(r)?;
            by_body.entry(bi).or_default().push(face);
        }
        for (bi, fs) in by_body {
            self.bodies[bi].shape = f(&self.bodies[bi].shape, &fs)?;
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
    for (i, b) in m.bodies.iter().enumerate() {
        if let Ok(mut mesh) = mesh_body(&b.shape) {
            mesh.id = b.id;
            mesh.name = format!("Part {}", i + 1);
            out.bodies.push(mesh);
        }
    }
    for (fid, s) in &m.curves {
        let pts: Vec<[f32; 3]> = unique_edges(s).iter().flat_map(|e| e.approximation_segments().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect::<Vec<_>>()).collect();
        out.curves.push(CurveMesh { feature: *fid, pts });
    }
    out
}

/// Build the model and return its bodies.
pub fn build_bodies(doc: &Document) -> Vec<Body> {
    let mut m = Model::new(doc);
    let end = doc.rollback_index();
    for (i, f) in doc.features.iter().enumerate() {
        if i < end && !f.suppressed {
            let _ = m.run(f.id);
        }
    }
    m.bodies
}

/// Rebuild and write every body to a STEP or STL file.
pub fn export(doc: &Document, format: &str, path: &str) -> Result<(), String> {
    let bodies = build_bodies(doc);
    if bodies.is_empty() {
        return Err("there are no bodies to export".into());
    }
    let shapes: Vec<&Shape> = bodies.iter().map(|b| &b.shape).collect();
    let shape = occ::compound(&shapes)?;
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

pub fn seg_edges(segs: &[Seg], fr: &Frame) -> R<Vec<Edge>> {
    let segs = merge_arcs(segs);
    let mut edges = vec![];
    for s in &segs {
        match s {
            Seg::Line { a, b } => edges.push(Edge::segment(fr.to_world(*a), fr.to_world(*b))),
            Seg::Arc { c, r, t0, t1 } if (t1 - t0).abs() > std::f64::consts::TAU - 1e-6 => {
                // full circle; its direction follows the axis
                let n = if t1 > t0 { fr.normal() } else { -fr.normal() };
                edges.push(Edge::circle(fr.to_world(*c), n, *r));
            }
            Seg::Arc { .. } => edges.push(Edge::arc(fr.to_world(s.start()), fr.to_world(s.mid()), fr.to_world(s.end()))),
            Seg::Poly { pts, closed } => {
                let w: Vec<DVec3> = pts.iter().map(|p| fr.to_world(*p)).collect();
                edges.push(occ::spline_edge(&w, *closed)?);
            }
        }
    }
    Ok(edges)
}

fn region_face(r: &Region, fr: &Frame) -> R<Face> {
    let outer = occ::wire(&seg_edges(&r.outer.segs, fr)?)?;
    let holes: Vec<Wire> = r.holes.iter().map(|h| occ::wire(&seg_edges(&h.segs, fr)?)).collect::<R<_>>()?;
    occ::face(&outer, &holes)
}

pub fn dup(s: &Shape) -> Shape {
    Shape::from_shape(&s.inner)
}

fn fuse_all(mut tools: Vec<Shape>) -> R<Shape> {
    let mut acc = tools.remove(0);
    for t in tools {
        acc = occ::fuse(&acc, &t)?;
    }
    Ok(acc)
}

fn bbox_apart(a: &Shape, b: &Shape) -> bool {
    let ba = opencascade::bounding_box::aabb(a);
    let bb = opencascade::bounding_box::aabb(b);
    let tol = 1e-6;
    ba.min().cmpgt(bb.max() + tol).any() || bb.min().cmpgt(ba.max() + tol).any()
}

fn overlaps_volume(a: &Shape, b: &Shape) -> bool {
    !bbox_apart(a, b) && occ::common(a, b).map(|c| volume(&c) > 1e-9).unwrap_or(false)
}

/// Overlapping or touching along a face.
fn overlaps(a: &Shape, b: &Shape) -> bool {
    !bbox_apart(a, b) && (overlaps_volume(a, b) || shares_face(a, b))
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

pub fn centroid(s: &Shape) -> DVec3 {
    let mut props = ffi::g_prop::GProps_new();
    ffi::b_rep_g_prop::BRepGProp::VolumeProperties(&s.inner, props.pin_mut(), true, false, false);
    let p = ffi::g_prop::GProp_GProps_CentreOfMass(&props);
    DVec3::new(p.X(), p.Y(), p.Z())
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

fn half_space(p: DVec3, n: DVec3, size: f64) -> R<Shape> {
    // a big box on the +n side of the plane through p
    let fr = frame_from(p, n);
    let (x, y) = (v3(fr.x), v3(fr.y));
    let base = [p - x * size - y * size, p + x * size - y * size, p + x * size + y * size, p - x * size + y * size];
    let w = Wire::from_ordered_points(base).map_err(|e| format!("{e:?}"))?;
    let f = Face::from_wire(&w);
    occ::prism(&Shape::from(&f), n * size)
}

fn cone_shape(apex_side: DVec3, dir: DVec3, r_top: f64, r_bottom: f64, h: f64) -> R<Shape> {
    let s = Shape::cone().at(DVec3::ZERO).bottom_radius(r_top).top_radius(r_bottom).height(h).build();
    // the builder makes a Z-up cone at the origin; orient it along `dir`
    let z = DVec3::Z;
    let axis = z.cross(dir);
    let m = if axis.length() < 1e-9 {
        if dir.z < 0.0 { DMat3::from_axis_angle(DVec3::X, std::f64::consts::PI) } else { DMat3::IDENTITY }
    } else {
        DMat3::from_axis_angle(axis.normalize(), z.angle_between(dir))
    };
    occ::transform(&s, m, apex_side)
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
        centroid: centroid(shape).into(),
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
        if let Op::Mirror { bodies, plane, .. } = &mut m {
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

#[cfg(test)]
mod more_feature_tests {
    use super::*;
    use crate::doc::{Document, Op, OriginPlane, Ref};
    use crate::sketch::{Geom, Sketch};

    fn sketch_rect(doc: &mut Document, x0: f64, y0: f64, w: f64, h: f64) -> FeatId {
        let mut s = Sketch::default();
        let p: Vec<_> = [(x0, y0), (x0 + w, y0), (x0 + w, y0 + h), (x0, y0 + h)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        for i in 0..4 {
            s.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false);
        }
        doc.insert("S".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Top }, sketch: s, frame: Frame::TOP })
    }

    fn extrude(doc: &mut Document, sk: FeatId, at: [f64; 2], depth: &str, op: BoolOp) -> FeatId {
        let mut o = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, depth: d, op: bop, .. } = &mut o {
            profiles.push(Ref::Region { sketch: sk, point: at });
            *d = depth.into();
            *bop = op;
        }
        doc.insert("E".into(), o)
    }

    fn run(doc: &Document) -> RebuildResult {
        let r = rebuild(doc, &[], |_| {});
        for f in &r.features {
            assert!(f.error.is_none(), "feature {} failed: {:?}", f.id, f.error);
        }
        r
    }

    #[test]
    fn draft_thicken_delete_face() {
        let mut doc = Document::default();
        let s = sketch_rect(&mut doc, 0.0, 0.0, 20.0, 20.0);
        extrude(&mut doc, s, [5.0, 5.0], "20", BoolOp::New);
        let mut d = Op::new_default("draft").unwrap();
        if let Op::Draft { faces, neutral, angle } = &mut d {
            faces.push(Ref::Face { point: [10.0, 0.0, 10.0], normal: [0.0, -1.0, 0.0] });
            *neutral = Some(Ref::OriginPlane { plane: OriginPlane::Top });
            *angle = "5".into();
        }
        doc.insert("Draft".into(), d);
        let r = run(&doc);
        assert!((r.bodies[0].volume - 8000.0).abs() > 1.0, "draft changed nothing");

        // thicken a sketch region into a plate
        let mut doc = Document::default();
        let s = sketch_rect(&mut doc, 0.0, 0.0, 10.0, 10.0);
        let mut t = Op::new_default("thicken").unwrap();
        if let Op::Thicken { faces, thickness, .. } = &mut t {
            faces.push(Ref::Region { sketch: s, point: [5.0, 5.0] });
            *thickness = "3".into();
        }
        doc.insert("T".into(), t);
        let r = run(&doc);
        assert!((r.bodies[0].volume - 300.0).abs() < 1e-3, "{}", r.bodies[0].volume);

        // delete the hole wall of a plate with a hole -> the hole heals
        let mut doc = Document::default();
        let s = sketch_rect(&mut doc, 0.0, 0.0, 40.0, 40.0);
        if let Op::Sketch { sketch, .. } = &mut doc.features.last_mut().unwrap().op {
            let c = sketch.add_point(DVec2::new(20.0, 20.0));
            sketch.add_curve(Geom::Circle { c, r: 5.0 }, false);
        }
        extrude(&mut doc, s, [2.0, 2.0], "5", BoolOp::New);
        let mut df = Op::new_default("deleteFace").unwrap();
        if let Op::DeleteFace { faces } = &mut df {
            faces.push(Ref::Face { point: [25.0, 20.0, 2.5], normal: [0.0; 3] });
        }
        doc.insert("DF".into(), df);
        let r = run(&doc);
        assert!((r.bodies[0].volume - 8000.0).abs() < 1e-3, "{}", r.bodies[0].volume);
    }

    #[test]
    fn helix_sweep_and_transform() {
        let mut doc = Document::default();
        let mut h = Op::new_default("helix").unwrap();
        if let Op::Helix { axis, radius, pitch, turns, .. } = &mut h {
            *axis = Some(Ref::Axis { axis: 2 });
            *radius = "10".into();
            *pitch = "5".into();
            *turns = "3".into();
        }
        let hid = doc.insert("H".into(), h);
        // circle profile on the Front plane at the helix start (10, 0, 0)
        let mut s = Sketch::default();
        let c = s.add_point(DVec2::new(10.0, 0.0));
        s.add_curve(Geom::Circle { c, r: 1.0 }, false);
        let sk = doc.insert("P".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Front }, sketch: s, frame: Frame::FRONT });
        let mut sw = Op::new_default("sweep").unwrap();
        if let Op::Sweep { profiles, path, .. } = &mut sw {
            profiles.push(Ref::Region { sketch: sk, point: [10.0, 0.0] });
            path.push(Ref::Curve { feature: hid });
        }
        doc.insert("Sw".into(), sw);
        let r = run(&doc);
        assert_eq!(r.curves.len(), 1);
        // spring volume ≈ area * length
        let len = 3.0 * ((2.0 * std::f64::consts::PI * 10.0f64).powi(2) + 25.0).sqrt();
        let want = std::f64::consts::PI * len;
        assert!((r.bodies[0].volume - want).abs() / want < 0.02, "{} vs {want}", r.bodies[0].volume);

        let mut doc = Document::default();
        let s = sketch_rect(&mut doc, 0.0, 0.0, 10.0, 10.0);
        let e = extrude(&mut doc, s, [5.0, 5.0], "10", BoolOp::New);
        let mut t = Op::new_default("transform").unwrap();
        if let Op::Transform { bodies, direction, distance, copy, .. } = &mut t {
            bodies.push(Ref::Body { body: (e, 0) });
            *direction = Some(Ref::Axis { axis: 0 });
            *distance = "50".into();
            *copy = true;
        }
        doc.insert("T".into(), t);
        let r = run(&doc);
        assert_eq!(r.bodies.len(), 2);
        assert!((r.bodies[1].bbox[0][0] - 50.0).abs() < 1e-6);
    }

    #[test]
    fn feature_pattern_and_planes() {
        let mut doc = Document::default();
        let s = sketch_rect(&mut doc, 0.0, 0.0, 100.0, 20.0);
        extrude(&mut doc, s, [5.0, 5.0], "10", BoolOp::New);
        // a hole cut by an extrude, then patterned as a feature
        let s2 = sketch_rect(&mut doc, 5.0, 5.0, 5.0, 5.0);
        let cut = extrude(&mut doc, s2, [7.0, 7.0], "10", BoolOp::Remove);
        let mut lp = Op::new_default("linearPattern").unwrap();
        if let Op::LinearPattern { kind, bodies, direction, spacing, count, .. } = &mut lp {
            *kind = "feature".into();
            bodies.push(Ref::Feature { feature: cut });
            *direction = Some(Ref::Axis { axis: 0 });
            *spacing = "20".into();
            *count = "4".into();
        }
        doc.insert("LP".into(), lp);
        let mut pl = Op::new_default("plane").unwrap();
        if let Op::Plane { mode, base, refs, .. } = &mut pl {
            *mode = "midPlane".into();
            *base = Some(Ref::Face { point: [50.0, 10.0, 10.0], normal: [0.0, 0.0, 1.0] });
            refs.push(Ref::Face { point: [50.0, 10.0, 0.0], normal: [0.0, 0.0, -1.0] });
        }
        let pid = doc.insert("Mid".into(), pl);
        let r = run(&doc);
        let want = 100.0 * 20.0 * 10.0 - 4.0 * 25.0 * 10.0;
        assert!((r.bodies[0].volume - want).abs() < 1e-3, "{} vs {want}", r.bodies[0].volume);
        assert!((r.frames[&pid].origin[2] - 5.0).abs() < 1e-6, "{:?}", r.frames[&pid]);
    }
}

#[cfg(test)]
mod freeform_tests {
    use super::*;
    use crate::doc::{Document, Op, OriginPlane, Ref};
    use crate::sketch::{Geom, Sketch};

    fn extrude_all(doc: &mut Document, sk: FeatId, s: &Sketch) {
        let regions = find_regions(s);
        assert!(!regions.is_empty(), "no regions");
        let mut o = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, depth, .. } = &mut o {
            for r in &regions {
                profiles.push(Ref::Region { sketch: sk, point: r.sample.into() });
            }
            *depth = "2".into();
        }
        doc.insert("E".into(), o);
    }

    #[test]
    fn ellipse_spline_and_text_extrude() {
        let mut s = Sketch::default();
        let c = s.add_point(DVec2::new(0.0, 0.0));
        let a = s.add_point(DVec2::new(20.0, 0.0));
        s.add_curve(Geom::Ellipse { c, a, rmin: 10.0 }, false);
        let pts: Vec<_> = [(50.0, 0.0), (70.0, 10.0), (60.0, 30.0), (45.0, 15.0)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        s.add_curve(Geom::Spline { pts, closed: true }, false);
        for c in crate::text::outlines("Hi", DVec2::new(0.0, 40.0), 10.0) {
            s.add_curve(Geom::Poly { pts: c.iter().map(|p| (*p).into()).collect(), closed: true }, false);
        }
        let mut doc = Document::default();
        let sk = doc.insert("S".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Top }, sketch: s.clone(), frame: Frame::TOP });
        extrude_all(&mut doc, sk, &s);
        let r = rebuild(&doc, &[], |_| {});
        for f in &r.features {
            assert!(f.error.is_none(), "{:?}", f.error);
        }
        let total: f64 = r.bodies.iter().map(|b| b.volume).sum();
        let ellipse = std::f64::consts::PI * 20.0 * 10.0 * 2.0;
        assert!(total > ellipse, "{total}");
        assert!(r.bodies.len() >= 4, "ellipse, spline, H, i(+dot): {}", r.bodies.len());
    }
}
