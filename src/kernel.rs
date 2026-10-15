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
use crate::occ;
use crate::script;
use crate::regions::{Region, Seg, find_regions};
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
    /// Centre, axis and radius of circular edges.
    #[serde(default)]
    pub circle: Option<([f64; 3], [f64; 3], f64)>,
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
    /// An open surface rather than a solid.
    #[serde(default)]
    pub surface: bool,
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
    #[serde(default)]
    pub flats: Vec<FlatPattern>,
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
    pub flats: Vec<FlatPattern>,
    /// Sheet metal bodies: thickness, bend radius, K-factor, the feature with its flat pattern.
    pub sheet: HashMap<BodyId, SheetInfo>,
}

#[derive(Clone, Copy, Debug)]
pub struct SheetInfo {
    pub t: f64,
    pub r: f64,
    pub k: f64,
    pub feature: FeatId,
    /// Sketch frame of flat-plate parts (their flat pattern is in this frame).
    pub frame: Option<Frame>,
}

type R<T> = Result<T, String>;

/// Every Part Studio of the workspace, for Derived features (set by the editor).
pub static STUDIOS: std::sync::Mutex<Vec<Document>> = std::sync::Mutex::new(Vec::new());

thread_local! {
    static DERIVE_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn v3(a: [f64; 3]) -> DVec3 {
    DVec3::from(a)
}

impl<'a> Model<'a> {
    pub fn new(doc: &'a Document) -> Self {
        let (vars, _) = doc.var_values();
        Model { doc, vars, bodies: vec![], frames: HashMap::new(), curves: vec![], tools: HashMap::new(), flats: vec![], sheet: HashMap::new() }
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
    fn num(&self, s: &str) -> R<f64> {
        crate::doc::eval(s, Kind::Count, &self.vars, self.units())
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
                    Some(crate::sketch::Geom::Circle { c, .. }) | Some(crate::sketch::Geom::Arc { c, .. }) | Some(crate::sketch::Geom::Ellipse { c, .. }) | Some(crate::sketch::Geom::EllipticalArc { c, .. }) => {
                        Ok((fr.to_world(sk.pv(c)), fr.normal()))
                    }
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
                    let reg = regions.iter().filter(|g| g.contains(p)).min_by(|a, b| a.area.total_cmp(&b.area)).ok_or("a profile region no longer exists")?;
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
                let sign = if *opposite { -1.0 } else { 1.0 };
                let d = match end {
                    EndType::ThroughAll | EndType::UpToFace => self.model_size() * 2.0,
                    EndType::UpToNext => self.up_to_next(&faces, sign, *op)?,
                    EndType::UpToVertex => {
                        let r = up_to.as_ref().ok_or("select the vertex or point to extrude up to")?;
                        let p = self.point_of(r)?;
                        let (_, fr) = &faces[0];
                        ((p - DVec3::from(fr.origin)).dot(fr.normal() * sign)).abs()
                    }
                    _ => self.len(depth)?,
                };
                if d <= 0.0 {
                    return Err("depth must be positive".into());
                }
                let d2 = if *second { self.len(depth2)? } else { 0.0 };
                let draft_a = self.ang(draft)?.to_radians();
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
                        // tapered sides
                        let solid = if draft_a.abs() > 1e-9 && matches!(end, EndType::Blind | EndType::UpToNext | EndType::UpToVertex) && !*second {
                            let sides: Vec<Face> = unique_faces(&solid).into_iter().filter(|f| face_geom(f).normal.dot(n).abs() < 1e-6).collect();
                            occ::draft(&solid, &sides, n, draft_a, DVec3::from(fr.origin), n)?
                        } else {
                            solid
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
                if cutting && !matches!(end, EndType::Symmetric | EndType::UpToNext | EndType::UpToVertex) && !*second && !tools.iter().any(|t| self.bodies.iter().any(|b| overlaps_volume(&b.shape, t))) {
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
            Op::Chamfer { edges, distance, mode, distance2, angle } => {
                let d = self.len(distance)?;
                if d <= 0.0 {
                    return Err("distance must be positive".into());
                }
                match mode.as_str() {
                    "two" => {
                        let d2 = self.len(distance2)?;
                        self.edge_op(edges, |shape, es| occ::chamfer2(shape, es, d, d2, 0))
                    }
                    "angle" => {
                        let a = self.ang(angle)?.to_radians();
                        self.edge_op(edges, |shape, es| occ::chamfer2(shape, es, d, a, 1))
                    }
                    _ => self.edge_op(edges, |shape, es| occ::chamfer(shape, es, d)),
                }
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
            Op::SurfaceExtrude { curves, depth, symmetric, opposite } => {
                let d = self.len(depth)?;
                let n = self.curves_normal(curves)? * if *opposite { -1.0 } else { 1.0 };
                let wire = self.path_wire(curves)?;
                let ws = Shape::from(&wire);
                let s = if *symmetric { occ::prism(&occ::translate(&ws, -n * (d / 2.0))?, n * d)? } else { occ::prism(&ws, n * d)? };
                self.add_surface(fid, s);
                Ok(())
            }
            Op::SurfaceRevolve { curves, axis, angle } => {
                let (o, dir) = self.axis_of(axis.as_ref().ok_or("select an axis")?)?;
                let wire = self.path_wire(curves)?;
                let s = occ::revolve(&Shape::from(&wire), o, dir, self.ang(angle)?.to_radians())?;
                self.add_surface(fid, s);
                Ok(())
            }
            Op::SurfaceLoft { curves, ruled } => {
                // one profile per sketch (or per edge)
                let mut groups: Vec<(Option<FeatId>, Vec<Ref>)> = vec![];
                for r in curves {
                    let key = match r {
                        Ref::SketchCurve { sketch, .. } | Ref::Region { sketch, .. } => Some(*sketch),
                        _ => None,
                    };
                    match groups.iter_mut().find(|g| g.0.is_some() && g.0 == key) {
                        Some(g) => g.1.push(r.clone()),
                        None => groups.push((key, vec![r.clone()])),
                    }
                }
                if groups.len() < 2 {
                    return Err("select curves in two or more sketches".into());
                }
                let wires: Vec<Wire> = groups.iter().map(|g| self.path_wire(&g.1)).collect::<R<_>>()?;
                let l = ffi_loft_surface(&wires, *ruled)?;
                self.add_surface(fid, l);
                Ok(())
            }
            Op::Fill { edges } => {
                let mut es = vec![];
                for r in edges {
                    match r {
                        Ref::Edge { point } => es.push(self.find_edge(v3(*point))?.0),
                        Ref::SketchCurve { sketch, curve } => {
                            let (sk, fr) = self.sketch(*sketch)?;
                            es.extend(curve_edges(sk, &fr, *curve)?);
                        }
                        Ref::Curve { feature } => {
                            let (_, s) = self.curves.iter().find(|c| c.0 == *feature).ok_or("the curve no longer exists")?;
                            es.extend(unique_edges(s));
                        }
                        _ => return Err("select the boundary edges or curves".into()),
                    }
                }
                let s = occ::fill(&es)?;
                self.add_surface(fid, s);
                Ok(())
            }
            Op::OffsetSurface { faces, distance } => {
                let d = self.len(distance)?;
                let mut made = vec![];
                for r in faces {
                    match r {
                        Ref::Body { body } => {
                            let i = self.bodies.iter().position(|b| b.id == *body).ok_or("a body no longer exists")?;
                            made.push(occ::offset_shape(&self.bodies[i].shape, d)?);
                        }
                        _ => {
                            let (f, _) = self.find_face_ref(r)?;
                            made.push(occ::offset_shape(&Shape::from(&f), d)?);
                        }
                    }
                }
                if made.is_empty() {
                    return Err("select faces or surfaces".into());
                }
                for m in made {
                    self.add_surface(fid, m);
                }
                Ok(())
            }
            Op::Knit { bodies, solid } => {
                let mut idx = vec![];
                for r in bodies {
                    let i = self.body_index(r)?;
                    if !idx.contains(&i) {
                        idx.push(i);
                    }
                }
                if idx.is_empty() {
                    return Err("select the surfaces to knit".into());
                }
                let shapes: Vec<&Shape> = idx.iter().map(|&i| &self.bodies[i].shape).collect();
                let s = occ::sew(&shapes, *solid)?;
                let ids: Vec<BodyId> = idx.iter().map(|&i| self.bodies[i].id).collect();
                self.bodies.retain(|b| !ids.contains(&b.id));
                if *solid {
                    self.bodies.push(Body { id: (fid, 0), shape: s });
                } else {
                    self.add_surface(fid, s);
                }
                Ok(())
            }
            Op::Derived { studio, bodies } => {
                let other = STUDIOS.lock().map_err(|_| "studio list unavailable")?.get(*studio).cloned().ok_or("that Part Studio does not exist")?;
                if DERIVE_DEPTH.with(|d| d.get()) > 4 {
                    return Err("Part Studios derive from each other in a loop".into());
                }
                DERIVE_DEPTH.with(|d| d.set(d.get() + 1));
                let parts = build_bodies(&other);
                DERIVE_DEPTH.with(|d| d.set(d.get() - 1));
                let mut n = 0;
                for b in parts {
                    if bodies.is_empty() || bodies.contains(&b.id) {
                        self.bodies.push(Body { id: (fid, n), shape: b.shape });
                        n += 1;
                    }
                }
                if n == 0 {
                    return Err("no parts to derive (the other Part Studio has none, or the chosen ones are gone)".into());
                }
                Ok(())
            }
            Op::MoveFace { faces, distance } => {
                let d = self.len(distance)?;
                let mut by_body: HashMap<usize, Vec<Face>> = HashMap::new();
                for r in faces {
                    let (f, bi) = self.find_face_ref(r)?;
                    by_body.entry(bi).or_default().push(f);
                }
                if by_body.is_empty() {
                    return Err("select faces".into());
                }
                for (bi, fs) in by_body {
                    let mut shape = dup(&self.bodies[bi].shape);
                    for f in &fs {
                        let g = face_geom(f);
                        if !g.planar {
                            // curved faces: offset the face within the solid
                            shape = occ::offset_faces(&shape, std::slice::from_ref(f), d)?;
                            continue;
                        }
                        let prism = occ::prism(&Shape::from(f), g.normal * d)?;
                        shape = if d >= 0.0 { occ::fuse(&shape, &prism)? } else { occ::cut(&shape, &prism)? };
                    }
                    self.bodies[bi].shape = shape;
                }
                Ok(())
            }
            Op::Custom { name, values, refs, op, .. } => {
                let def = self.doc.custom_features.iter().find(|d| d.name == *name).cloned().ok_or_else(|| format!("no custom feature named \"{name}\" in this document"))?;
                let mut vals = vec![];
                for p in &def.params {
                    let expr = values.get(&p.name).cloned().unwrap_or_else(|| p.default.clone());
                    let v = match p.kind.as_str() {
                        "length" => script::Value::Num(self.len(&expr)?),
                        "angle" => script::Value::Num(self.ang(&expr)?),
                        "count" => script::Value::Int(self.num(&expr)?.round() as i64),
                        "bool" => script::Value::Bool(matches!(expr.trim(), "true" | "1" | "yes")),
                        _ => script::Value::Num(self.num(&expr)?),
                    };
                    vals.push((p.name.clone(), v));
                }
                let mut sel = vec![];
                for r in refs {
                    let n = match r {
                        Ref::Face { normal, .. } => v3(*normal),
                        Ref::SketchPoint { sketch, .. } => self.sketch(*sketch).map(|(_, fr)| fr.normal()).unwrap_or(DVec3::Z),
                        _ => DVec3::Z,
                    };
                    sel.push((self.point_of(r)?, n));
                }
                let (tools, log) = script::run(&def, vals, &sel)?;
                if !log.is_empty() {
                    eprintln!("[{}] {}", name, log.join("\n"));
                }
                if tools.is_empty() {
                    return Err("the script made no shapes".into());
                }
                self.apply(fid, tools, *op)
            }
            Op::Rib { profile, thickness, flip } => {
                let t = self.len(thickness)?;
                if self.bodies.is_empty() {
                    return Err("a rib needs a part to attach to".into());
                }
                let size = self.model_size() * 2.0;
                let mut tools = vec![];
                for r in profile {
                    let Ref::SketchCurve { sketch, curve } = r else { return Err("select sketch lines".into()) };
                    let (sk, fr) = self.sketch(*sketch)?;
                    let Some(crate::sketch::Geom::Line { a, b }) = sk.curve(*curve).map(|c| c.geom.clone()) else {
                        return Err("ribs are made from sketch lines".into());
                    };
                    let (pa, pb) = (sk.pv(a), sk.pv(b));
                    let dir = (pb - pa).normalize();
                    let n = dir.perp() * if *flip { -1.0 } else { 1.0 };
                    // the half-plane beside the line, long enough to reach the part, thickened about the sketch plane
                    let quad = [pa - dir * size, pb + dir * size, pb + dir * size + n * size, pa - dir * size + n * size];
                    let pts: Vec<DVec3> = quad.iter().map(|p| fr.to_world(*p) - fr.normal() * (t / 2.0)).collect();
                    let w = Wire::from_ordered_points(pts).map_err(|e| format!("{e:?}"))?;
                    let slab = occ::prism(&Shape::from(&Face::from_wire(&w)), fr.normal() * t)?;
                    // keep only what lies within the parts' bounding box in the sketch plane
                    let mut lo = DVec3::splat(f64::INFINITY);
                    let mut hi = DVec3::splat(f64::NEG_INFINITY);
                    for b in &self.bodies {
                        let bb = opencascade::bounding_box::aabb(&b.shape);
                        lo = lo.min(bb.min());
                        hi = hi.max(bb.max());
                    }
                    let bbox = Shape::box_from_corners(lo, hi);
                    let mut region = occ::common(&slab, &bbox)?;
                    // drop pieces that don't touch the rib line (they would fill recesses elsewhere)
                    for b in &self.bodies {
                        region = occ::cut(&region, &b.shape)?;
                    }
                    let (wa, wb) = (fr.to_world(pa), fr.to_world(pb));
                    let near_line = |sh: &Shape| unique_faces(sh).iter().flat_map(face_tris).flatten().any(|p| polyline_distance(&[wa, wb], p) < t + 1e-3);
                    for piece in solids_of(&region) {
                        if near_line(&piece) {
                            tools.push(piece);
                        }
                    }
                }
                if tools.is_empty() {
                    return Err("select the rib lines".into());
                }
                self.apply(fid, tools, BoolOp::Add)
            }
            Op::SheetMetal { profile, thickness, radius, k_factor, depth, flip, op } => {
                let t = self.len(thickness)?;
                let r = self.len(radius)?;
                let k = crate::doc::eval(k_factor, Kind::Count, &self.vars, self.units())?;
                let d = self.len(depth)?;
                let (tools, flat) = self.sheet_metal(fid, profile, t, r, k, d, *flip)?;
                self.flats.push(flat);
                self.apply(fid, tools, *op)?;
                let frame = match profile.first() {
                    Some(Ref::Region { sketch, .. }) => self.frames.get(sketch).copied(),
                    _ => None,
                };
                for b in self.bodies.iter().filter(|b| b.id.0 == fid) {
                    self.sheet.insert(b.id, SheetInfo { t, r, k, feature: fid, frame });
                }
                Ok(())
            }
            Op::Flange { edges, length, angle, flip, inset } => {
                let l = self.len(length)?;
                let ang = self.ang(angle)?.to_radians();
                let inset = self.len(inset)?;
                if edges.is_empty() {
                    return Err("select straight edges of a sheet metal part".into());
                }
                for r in edges {
                    let Ref::Edge { point } = r else { return Err("select edges".into()) };
                    let (e, bi) = self.find_edge(v3(*point))?;
                    let id = self.bodies[bi].id;
                    let info = *self.sheet.get(&id).ok_or("pick an edge of a sheet metal part")?;
                    let (flange, relief, flat_add) = self.flange_shape(&e, bi, &info, l, ang, *flip, inset)?;
                    let mut shape = occ::fuse(&self.bodies[bi].shape, &flange)?;
                    if let Some(rel) = relief {
                        shape = occ::cut(&shape, &rel)?;
                    }
                    self.bodies[bi].shape = shape;
                    if let (Some(add), Some(f)) = (flat_add, self.flats.iter_mut().find(|f| f.feature == info.feature)) {
                        f.outline.push(add.0);
                        f.bends.push(add.1);
                    }
                }
                Ok(())
            }
            Op::Tab { profiles } => {
                let faces = self.profile_faces(profiles)?;
                let mut tools = vec![];
                for (face, fr) in &faces {
                    // thickness from the sheet metal part the tab touches
                    let info = self.sheet.values().next().copied().ok_or("there is no sheet metal part")?;
                    let s = occ::prism(&Shape::from(face), -fr.normal() * info.t)?;
                    let s2 = occ::prism(&Shape::from(face), fr.normal() * info.t)?;
                    // go into the material: whichever side overlaps the part more
                    let pick = if self.bodies.iter().any(|b| overlaps_volume(&b.shape, &s)) { s } else { s2 };
                    tools.push(pick);
                }
                self.apply(fid, tools, BoolOp::Add)
            }
            Op::ConvertSheet { faces, thickness } => {
                let t = self.len(thickness)?;
                if faces.is_empty() {
                    return Err("select the faces to leave open".into());
                }
                self.face_op(faces, |shape, fs| occ::shell(shape, fs, -t))?;
                let ids: Vec<BodyId> = faces.iter().filter_map(|r| self.find_face_ref(r).ok().map(|(_, bi)| self.bodies[bi].id)).collect();
                for id in ids {
                    self.sheet.insert(id, SheetInfo { t, r: 0.0, k: 0.45, feature: fid, frame: None });
                }
                Ok(())
            }
            Op::Thicken { faces, thickness, opposite, op } => {
                let t = self.len(thickness)?;
                let t = if *opposite { -t } else { t };
                let mut tools = vec![];
                // whole surface bodies are thickened and replaced
                let surf: Vec<BodyId> = faces.iter().filter_map(|r| if let Ref::Body { body } = r { Some(*body) } else { None }).collect();
                for b in &surf {
                    let i = self.bodies.iter().position(|x| x.id == *b).ok_or("a surface no longer exists")?;
                    tools.push(occ::thicken(&self.bodies[i].shape, t)?);
                }
                self.bodies.retain(|x| !surf.contains(&x.id));
                let rest: Vec<Ref> = faces.iter().filter(|r| !matches!(r, Ref::Body { .. })).cloned().collect();
                if !rest.is_empty() {
                    for (f, _) in &self.profile_faces(&rest)? {
                        tools.push(occ::thicken(&Shape::from(f), t)?);
                    }
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
                let iges = step.trim_start().len() > 72 && step.lines().next().map(|l| l.len() >= 73 && l.as_bytes()[72] == b'S').unwrap_or(false);
                let path = std::env::temp_dir().join(format!("cad-rust-import-{}-{fid}.{}", std::process::id(), if iges { "igs" } else { "step" }));
                std::fs::write(&path, step).map_err(|e| e.to_string())?;
                let s = if iges { Shape::read_iges(&path) } else { Shape::read_step(&path) }.map_err(|e| format!("could not read the file: {e:?}"));
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

    /// A flange solid along edge `e` of body `bi`, an optional relief cut, and the
    /// flat-pattern addition (outline polygon, bend line) for flat-plate parts.
    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    fn flange_shape(&self, e: &Edge, bi: usize, info: &SheetInfo, l: f64, ang: f64, flip: bool, inset: f64) -> R<(Shape, Option<Shape>, Option<(Vec<[f64; 2]>, [[f64; 2]; 2])>)> {
        if !e_is_line(e) {
            return Err("flanges need straight edges".into());
        }
        let (mut p0, mut p1) = (e.start_point(), e.end_point());
        let u = (p1 - p0).normalize();
        if inset * 2.0 >= (p1 - p0).length() {
            return Err("the inset is longer than the edge".into());
        }
        p0 += u * inset;
        p1 -= u * inset;
        // the large planar face along the edge is the sheet surface
        let mid = (p0 + p1) / 2.0;
        let mut best: Option<(f64, FaceGeom)> = None;
        for f in unique_faces(&self.bodies[bi].shape) {
            let g = face_geom(&f);
            if !g.planar || g.normal.dot(u).abs() > 1e-6 || face_distance(&f, mid) > 1e-4 {
                continue;
            }
            if best.as_ref().map(|b| g.area > b.0).unwrap_or(true) {
                best = Some((g.area, g));
            }
        }
        let (_, g) = best.ok_or("that edge is not on a flat sheet face")?;
        let t = info.t;
        let mut n = g.normal;
        let mut w = n.cross(u).normalize();
        if (mid - g.center).dot(w) < 0.0 {
            w = -w;
        }
        let mut origin = p0;
        if flip {
            // bend away from this face: start from the opposite surface
            origin -= n * t;
            n = -n;
        }
        // profile in (w, n) coordinates, swept along u
        let r = info.r;
        let c = DVec2::new(0.0, r);
        let inner_end = c + DVec2::from_angle(-std::f64::consts::FRAC_PI_2 + ang) * r;
        let outer_end = c + DVec2::from_angle(-std::f64::consts::FRAC_PI_2 + ang) * (r + t);
        let dir = DVec2::from_angle(ang);
        let a0 = -std::f64::consts::FRAC_PI_2;
        let mut segs = vec![];
        if r > 1e-9 {
            segs.push(Seg::Arc { c, r, t0: a0, t1: a0 + ang });
        }
        segs.push(Seg::Line { a: inner_end, b: inner_end + dir * l });
        segs.push(Seg::Line { a: inner_end + dir * l, b: outer_end + dir * l });
        segs.push(Seg::Line { a: outer_end + dir * l, b: outer_end });
        segs.push(Seg::Arc { c, r: r + t, t0: a0 + ang, t1: a0 });
        segs.push(Seg::Line { a: DVec2::new(0.0, -t), b: DVec2::new(0.0, 0.0) });
        let fr = Frame { origin: origin.into(), x: w.into(), y: n.into(), normal: u.into() };
        let wire = occ::wire(&seg_edges(&segs, &fr)?)?;
        let face = occ::face(&wire, &[])?;
        let solid = occ::prism(&Shape::from(&face), u * (p1 - p0).length())?;
        // reliefs: slots the width of the sheet at each end of an inset flange
        let relief = if inset > 1e-9 {
            let depth = r + t;
            let mut cut: Option<Shape> = None;
            for (end, s) in [(p0, -1.0), (p1, 1.0)] {
                let a = end - n * (t * 2.0) - w * depth;
                let b = end + u * (s * t) + n * (t * 2.0 + r + l) + w * 0.01;
                let bx = Shape::box_from_corners(a.min(b), a.max(b));
                cut = Some(match cut {
                    None => bx,
                    Some(c) => occ::fuse(&c, &bx)?,
                });
            }
            cut
        } else {
            None
        };
        // flat pattern: only for flat plates, whose flat is in their sketch frame
        let flat_add = info.frame.map(|sf| {
            let q0 = sf.to_local(p0);
            let q1 = sf.to_local(p1);
            let ww = sf.to_local(sf.to_world(DVec2::ZERO) + w) - sf.to_local(sf.to_world(DVec2::ZERO));
            let ww = ww.normalize_or_zero();
            let ba = ang * (r + info.k * t);
            let ext = ba + l;
            let poly = vec![q0.into(), q1.into(), (q1 + ww * ext).into(), (q0 + ww * ext).into(), q0.into()];
            (poly, [(q0 + ww * ba / 2.0).into(), (q1 + ww * ba / 2.0).into()])
        });
        Ok((solid, relief, flat_add))
    }

    /// Depth to the next part along the profile normal: up to where material starts
    /// (adding) or where the first material run ends (cutting).
    fn up_to_next(&self, faces: &[(Face, Frame)], sign: f64, op: BoolOp) -> R<f64> {
        let size = self.model_size() * 2.0;
        let mut best: Option<f64> = None;
        for (face, fr) in faces {
            let n = fr.normal() * sign;
            let o = DVec3::from(fr.origin);
            let long = occ::prism(&Shape::from(face), n * size)?;
            for b in &self.bodies {
                if bbox_apart(&b.shape, &long) {
                    continue;
                }
                let Ok(c) = occ::common(&b.shape, &long) else { continue };
                // distances along n of the overlap's vertices
                let tris: Vec<f64> = unique_faces(&c).iter().flat_map(face_tris).flat_map(|t| t.map(|p| (p - o).dot(n))).collect();
                if tris.is_empty() {
                    continue;
                }
                let lo = tris.iter().cloned().fold(f64::INFINITY, f64::min);
                let hi = tris.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                let d = if matches!(op, BoolOp::Remove | BoolOp::Intersect) { hi } else { lo };
                if d > 1e-6 && best.map(|x| d < x).unwrap_or(true) {
                    best = Some(d);
                }
            }
        }
        best.ok_or_else(|| "there is no part ahead of the profile".to_string())
    }

    /// Add an open surface (shell or face) as a body of its own.
    fn add_surface(&mut self, fid: FeatId, s: Shape) {
        let n = self.bodies.iter().filter(|b| b.id.0 == fid).count() as u32;
        self.bodies.push(Body { id: (fid, n), shape: s });
    }

    /// Normal of the sketch the curves are in (Z when they are model edges).
    fn curves_normal(&self, refs: &[Ref]) -> R<DVec3> {
        for r in refs {
            if let Ref::SketchCurve { sketch, .. } | Ref::Region { sketch, .. } = r {
                return Ok(self.sketch(*sketch)?.1.normal());
            }
        }
        Ok(DVec3::Z)
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
                    edges.extend(curve_edges(sk, &fr, *curve)?);
                }
                Ref::Region { sketch, point } => {
                    let (sk, fr) = self.sketch(*sketch)?;
                    let reg = find_regions(sk).into_iter().filter(|g| g.contains(DVec2::from(*point))).min_by(|a, b| a.area.total_cmp(&b.area)).ok_or("a region no longer exists")?;
                    edges.extend(seg_edges(&reg.outer.segs, &fr)?);
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
            match f(&self.bodies[bi].shape, &es) {
                Ok(s) => self.bodies[bi].shape = s,
                Err(e) => {
                    // find the edges that fail on their own, to say which ones
                    let bad: Vec<String> = es
                        .iter()
                        .filter(|edge| f(&self.bodies[bi].shape, std::slice::from_ref(*edge)).is_err())
                        .map(|edge| {
                            let pts: Vec<DVec3> = edge.approximation_segments().collect();
                            let m = if pts.len() % 2 == 0 { (pts[pts.len() / 2 - 1] + pts[pts.len() / 2]) / 2.0 } else { pts[pts.len() / 2] };
                            format!("[{:.3}, {:.3}, {:.3}]", m.x, m.y, m.z)
                        })
                        .collect();
                    return Err(if bad.is_empty() { format!("{e} (each edge works alone; try fewer edges at once)") } else { format!("{e}; failing edge(s) near {}", bad.join(", ")) });
                }
            }
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
            mesh.surface = solids_of(&b.shape).is_empty();
            mesh.id = b.id;
            mesh.name = if mesh.surface { format!("Surface {}", i + 1) } else { format!("Part {}", i + 1) };
            out.bodies.push(mesh);
        }
    }
    out.flats = std::mem::take(&mut m.flats);
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
        "iges" => shape.write_iges(path).map_err(|e| format!("{e:?}")),
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

/// Join consecutive collinear line segments (they would become separate faces).
fn merge_lines(segs: &[Seg]) -> Vec<Seg> {
    let join = |a: &Seg, b: &Seg| -> Option<Seg> {
        if let (Seg::Line { a: p, b: q }, Seg::Line { a: q2, b: r }) = (a, b) {
            let (u, v) = (*q - *p, *r - *q2);
            if (*q - *q2).length() < 1e-9 && u.perp_dot(v).abs() < 1e-9 * u.length() * v.length() && u.dot(v) > 0.0 {
                return Some(Seg::Line { a: *p, b: *r });
            }
        }
        None
    };
    let mut out: Vec<Seg> = vec![];
    for s in segs {
        if let Some(m) = out.last().and_then(|l| join(l, s)) {
            *out.last_mut().unwrap() = m;
        } else {
            out.push(s.clone());
        }
    }
    if out.len() > 2 {
        if let Some(m) = join(&out[out.len() - 1], &out[0]) {
            out.pop();
            out[0] = m;
        }
    }
    out
}

pub fn seg_edges(segs: &[Seg], fr: &Frame) -> R<Vec<Edge>> {
    let segs = merge_lines(&merge_arcs(segs));
    let mut edges = vec![];
    for s in &segs {
        match s {
            Seg::Line { a, b } => edges.push(Edge::segment(fr.to_world(*a), fr.to_world(*b))),
            Seg::Arc { c, r, t0, t1 } if (t1 - t0).abs() > std::f64::consts::TAU - 1e-6 => {
                // full circle; its direction follows the axis
                let n = if t1 > t0 { fr.normal() } else { -fr.normal() };
                edges.push(Edge::circle(fr.to_world(*c), n, *r));
            }
            // a zero-radius arc is a sharp corner
            Seg::Arc { r, .. } if *r < 1e-7 => {}
            Seg::Arc { .. } => edges.push(Edge::arc(fr.to_world(s.start()), fr.to_world(s.mid()), fr.to_world(s.end()))),
            Seg::Poly { pts, closed } => {
                let w: Vec<DVec3> = pts.iter().map(|p| fr.to_world(*p)).collect();
                edges.push(occ::spline_edge(&w, *closed)?);
            }
        }
    }
    Ok(edges)
}

fn ffi_loft_surface(wires: &[Wire], ruled: bool) -> R<Shape> {
    let l = {
        let mut l = ffi::top_tools::new_list_of_shape();
        for w in wires {
            l.pin_mut().Append(occ::wire_shape(w));
        }
        l
    };
    let r = ffi::extra::ex_loft(&l, false, ruled);
    if r.is_null() || r.IsNull() {
        return Err(ffi::extra::ex_last_error());
    }
    Ok(Shape::from_shape(&r))
}

/// OCCT edges for one sketch curve.
fn curve_edges(sk: &crate::sketch::Sketch, fr: &Frame, id: crate::sketch::Id) -> R<Vec<Edge>> {
    use crate::sketch::Geom;
    let c = sk.curve(id).ok_or("a curve no longer exists")?;
    Ok(match &c.geom {
        Geom::Line { a, b } => vec![Edge::segment(fr.to_world(sk.pv(*a)), fr.to_world(sk.pv(*b)))],
        Geom::Circle { c, r } => vec![Edge::circle(fr.to_world(sk.pv(*c)), fr.normal(), *r)],
        Geom::Arc { c: cc, a, b } => {
            let pts = sk.tessellate(c, 1.0);
            let _ = cc;
            vec![Edge::arc(fr.to_world(sk.pv(*a)), fr.to_world(pts[pts.len() / 2]), fr.to_world(sk.pv(*b)))]
        }
        Geom::Spline { closed, .. } | Geom::Poly { closed, .. } => {
            let mut pts: Vec<DVec3> = sk.tessellate(c, 0.5).into_iter().map(|p| fr.to_world(p)).collect();
            if *closed {
                pts.pop();
            }
            vec![occ::spline_edge(&pts, *closed)?]
        }
        Geom::Ellipse { .. } => {
            let mut pts: Vec<DVec3> = sk.tessellate(c, 0.5).into_iter().map(|p| fr.to_world(p)).collect();
            pts.pop();
            vec![occ::spline_edge(&pts, true)?]
        }
        Geom::EllipticalArc { .. } => {
            let pts: Vec<DVec3> = sk.tessellate(c, 0.5).into_iter().map(|p| fr.to_world(p)).collect();
            vec![occ::spline_edge(&pts, false)?]
        }
    })
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
    (1..=map.Extent()).filter(|&i| map.FindFromIndex(i).Size() != 1).map(|i| Edge::from_edge(ffi::topo_ds::TopoDS::Edge(map.FindKey(i)))).collect()
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
    let planar = !tris.is_empty()
        && tris.iter().all(|t| {
            let n = (t[1] - t[0]).cross(t[2] - t[0]);
            n.length() < 1e-12 || n.normalize().dot(normal) > 0.9999
        });
    // nearest triangle centre to the centroid, so the point lies on the face
    let center = tris.iter().map(|t| (t[0] + t[1] + t[2]) / 3.0).min_by(|a, b| (*a - centroid).length().total_cmp(&(*b - centroid).length())).unwrap_or(centroid);
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
    let mut out = BodyMesh { bbox: [bb.min().into(), bb.max().into()], volume: volume(shape), area: surface_area(shape), centroid: centroid(shape).into(), ..Default::default() };
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
    // tight bounds from the triangulation (OCCT's box includes control points and tolerances)
    if !out.positions.is_empty() {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for v in out.positions.chunks(3) {
            for k in 0..3 {
                lo[k] = lo[k].min(v[k] as f64);
                hi[k] = hi[k].max(v[k] as f64);
            }
        }
        out.bbox = [lo, hi];
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
        let circular = ty == opencascade::primitives::EdgeType::Circle;
        let circle = if circular && pts.len() >= 3 { circle_from_points(&pts).map(|(c, n)| (c.into(), n.into(), (pts[0] - c).length())) } else { None };
        out.edges.push(EdgeInfo {
            pts: pts.iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect(),
            mid: mid.into(),
            linear: ty == opencascade::primitives::EdgeType::Line,
            circular,
            length,
            circle,
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

// ------------------------------------------------------------------ interference

/// Overlap volumes between assembly instances: (instance index a, b, volume mm³).
/// `items` are (studio, body, row-major 3x4 pose) per instance.
pub fn interference(items: &[(usize, BodyId, [f64; 12])]) -> Vec<(usize, usize, f64)> {
    let studios = STUDIOS.lock().map(|s| s.clone()).unwrap_or_default();
    let mut built: HashMap<usize, Vec<Body>> = HashMap::new();
    let mut shapes: Vec<Option<Shape>> = vec![];
    for (studio, body, m) in items {
        if !built.contains_key(studio) {
            let b = studios.get(*studio).map(build_bodies).unwrap_or_default();
            built.insert(*studio, b);
        }
        let s = built[studio].iter().find(|b| b.id == *body).and_then(|b| {
            let rot = DMat3::from_cols(DVec3::new(m[0], m[4], m[8]), DVec3::new(m[1], m[5], m[9]), DVec3::new(m[2], m[6], m[10]));
            occ::transform(&b.shape, rot, DVec3::new(m[3], m[7], m[11])).ok()
        });
        shapes.push(s);
    }
    let mut out = vec![];
    for i in 0..shapes.len() {
        for j in i + 1..shapes.len() {
            let (Some(a), Some(b)) = (&shapes[i], &shapes[j]) else { continue };
            if bbox_apart(a, b) {
                continue;
            }
            if let Ok(c) = occ::common(a, b) {
                let v = volume(&c);
                if v > 1e-6 {
                    out.push((i, j, v));
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------ drawings

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProjView {
    /// Polylines in the view's 2D coordinates (model mm).
    pub visible: Vec<Vec<[f64; 2]>>,
    pub hidden: Vec<Vec<[f64; 2]>>,
    /// Section hatching segments.
    #[serde(default)]
    pub hatch: Vec<[[f64; 2]; 2]>,
    pub bbox: [[f64; 2]; 2],
}

fn polylines_2d(s: &Shape) -> Vec<Vec<[f64; 2]>> {
    unique_edges(s).iter().map(|e| e.approximation_segments().map(|p| [p.x, p.y]).collect::<Vec<_>>()).filter(|p| p.len() >= 2).collect()
}

/// Hidden-line projections of a Part Studio's parts, one per (direction, up, section) entry.
/// Section views cut the parts through their centre, square to the view, and hatch the cut.
pub fn project(doc: &Document, views: &[([f64; 3], [f64; 3], bool)]) -> Vec<ProjView> {
    let bodies = build_bodies(doc);
    let shapes: Vec<&Shape> = bodies.iter().map(|b| &b.shape).collect();
    let Ok(all) = occ::compound(&shapes) else { return vec![ProjView::default(); views.len()] };
    views
        .iter()
        .map(|(d, u, section)| {
            let (d, u) = (DVec3::from(*d), DVec3::from(*u));
            let mut v = ProjView::default();
            let mut shape = dup(&all);
            if *section {
                let bb = opencascade::bounding_box::aabb(&all);
                let c = (bb.min() + bb.max()) / 2.0;
                let size = (bb.max() - bb.min()).length() * 2.0 + 10.0;
                // remove the half towards the viewer
                if let Ok(h) = half_space(c, d, size) {
                    if let Ok(cut) = occ::cut(&all, &h) {
                        shape = cut;
                    }
                }
                // hatch the faces lying on the cutting plane
                let vx = u.cross(d).normalize();
                let vy = d.cross(vx);
                let to2 = |p: DVec3| glam::DVec2::new(p.dot(vx), p.dot(vy));
                for f in unique_faces(&shape) {
                    let g = face_geom(&f);
                    if !(g.planar && g.normal.dot(d) > 0.999 && (g.center - c).dot(d).abs() < 1e-4 * size.max(1.0)) {
                        continue;
                    }
                    let tris: Vec<[glam::DVec2; 3]> = face_tris(&f).iter().map(|t| t.map(to2)).collect();
                    v.hatch.extend(hatch_triangles(&tris, 2.0));
                }
            }
            for kind in [0, 1, 2] {
                if let Ok(s) = occ::hlr(&shape, d, u, kind) {
                    v.visible.extend(polylines_2d(&s));
                }
            }
            for kind in [3, 4] {
                if let Ok(s) = occ::hlr(&shape, d, u, kind) {
                    v.hidden.extend(polylines_2d(&s));
                }
            }
            let mut lo = [f64::INFINITY; 2];
            let mut hi = [f64::NEG_INFINITY; 2];
            for p in v.visible.iter().flatten() {
                lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                hi = [hi[0].max(p[0]), hi[1].max(p[1])];
            }
            v.bbox = if lo[0].is_finite() { [lo, hi] } else { [[0.0; 2]; 2] };
            v
        })
        .collect()
}

/// 45° hatch segments (spacing in mm) covering a set of triangles.
fn hatch_triangles(tris: &[[glam::DVec2; 3]], spacing: f64) -> Vec<[[f64; 2]; 2]> {
    let mut out = vec![];
    // lines x - y = k
    for t in tris {
        let ks: Vec<f64> = t.iter().map(|p| p.x - p.y).collect();
        let (k0, k1) = (ks.iter().cloned().fold(f64::INFINITY, f64::min), ks.iter().cloned().fold(f64::NEG_INFINITY, f64::max));
        let mut k = (k0 / spacing).ceil() * spacing;
        while k <= k1 {
            let mut pts = vec![];
            for i in 0..3 {
                let (a, b) = (t[i], t[(i + 1) % 3]);
                let (fa, fb) = (a.x - a.y - k, b.x - b.y - k);
                if (fa <= 0.0) != (fb <= 0.0) {
                    let s = fa / (fa - fb);
                    pts.push(a.lerp(b, s));
                }
            }
            if pts.len() == 2 {
                out.push([[pts[0].x, pts[0].y], [pts[1].x, pts[1].y]]);
            }
            k += spacing;
        }
    }
    out
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    #[test]
    fn front_view_of_a_box() {
        let mut doc = Document::default();
        let mut s = crate::sketch::Sketch::default();
        let p: Vec<_> = [(0.0, 0.0), (40.0, 0.0), (40.0, 20.0), (0.0, 20.0)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        for i in 0..4 {
            s.add_curve(crate::sketch::Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false);
        }
        let sk = doc.insert("S".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Top }, sketch: s, frame: Frame::TOP });
        let mut e = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, depth, .. } = &mut e {
            profiles.push(Ref::Region { sketch: sk, point: [5.0, 5.0] });
            *depth = "10".into();
        }
        doc.insert("E".into(), e);
        let v = project(&doc, &[([0.0, -1.0, 0.0], [0.0, 0.0, 1.0], false)]);
        let sec = project(&doc, &[([0.0, -1.0, 0.0], [0.0, 0.0, 1.0], true)]);
        assert!(!sec[0].hatch.is_empty());
        let b = v[0].bbox;
        assert!((b[1][0] - b[0][0] - 40.0).abs() < 1e-6, "{b:?}");
        assert!((b[1][1] - b[0][1] - 10.0).abs() < 1e-6, "{b:?}");
        assert!(v[0].visible.len() >= 4);
    }
}

// ------------------------------------------------------------------ sheet metal

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FlatPattern {
    pub feature: FeatId,
    /// Outline polylines of the flat blank (mm).
    pub outline: Vec<Vec<[f64; 2]>>,
    /// Bend lines across the blank.
    pub bends: Vec<[[f64; 2]; 2]>,
    pub size: [f64; 2],
}

/// Fillet the corners of an open polyline: each corner gets an arc of the given radius.
/// Returns segments and, per corner, (turn angle, radius on the path side).
fn filleted_path(pts: &[DVec2], radii: &[f64]) -> R<(Vec<Seg>, Vec<f64>)> {
    let n = pts.len();
    let mut segs = vec![];
    let mut cur = pts[0];
    let mut angles = vec![];
    for i in 1..n - 1 {
        let (p, a, b) = (pts[i], pts[i - 1], pts[i + 1]);
        let u = (p - a).normalize();
        let v = (b - p).normalize();
        let turn = u.angle_to(v);
        let r = radii[i - 1];
        let d = r * (turn.abs() / 2.0).tan();
        if d > (p - a).length() * 0.999 || d > (b - p).length() * 0.999 {
            return Err("a segment is too short for its bend".into());
        }
        let t1 = p - u * d;
        let t2 = p + v * d;
        segs.push(Seg::Line { a: cur, b: t1 });
        if turn.abs() > 1e-9 {
            // centre is to the left for left turns
            let side = if turn > 0.0 { u.perp() } else { -u.perp() };
            let c = t1 + side * r;
            let (a0, a1) = ((t1 - c).to_angle(), (t2 - c).to_angle());
            let mut a1 = a1;
            if turn > 0.0 {
                while a1 < a0 {
                    a1 += std::f64::consts::TAU;
                }
            } else {
                while a1 > a0 {
                    a1 -= std::f64::consts::TAU;
                }
            }
            segs.push(Seg::Arc { c, r, t0: a0, t1: a1 });
        }
        angles.push(turn);
        cur = t2;
    }
    segs.push(Seg::Line { a: cur, b: pts[n - 1] });
    Ok((segs, angles))
}

/// Order selected sketch lines into a connected chain of points.
fn chain_points(sk: &crate::sketch::Sketch, ids: &[crate::sketch::Id]) -> R<Vec<DVec2>> {
    let mut lines: Vec<(crate::sketch::Id, crate::sketch::Id)> = vec![];
    for id in ids {
        match sk.curve(*id).map(|c| &c.geom) {
            Some(crate::sketch::Geom::Line { a, b }) => lines.push((*a, *b)),
            _ => return Err("sheet metal profiles are made of sketch lines".into()),
        }
    }
    if lines.is_empty() {
        return Err("select the profile lines".into());
    }
    // start at a point used once
    let count = |p: crate::sketch::Id| lines.iter().filter(|l| l.0 == p || l.1 == p).count();
    let start = lines.iter().flat_map(|l| [l.0, l.1]).find(|p| count(*p) == 1).ok_or("the profile must be an open chain")?;
    let mut order = vec![start];
    let mut used = vec![false; lines.len()];
    let mut cur = start;
    loop {
        let Some(i) = (0..lines.len()).find(|&i| !used[i] && (lines[i].0 == cur || lines[i].1 == cur)) else { break };
        used[i] = true;
        cur = if lines[i].0 == cur { lines[i].1 } else { lines[i].0 };
        order.push(cur);
    }
    if used.iter().any(|u| !u) {
        return Err("the profile lines must connect end to end".into());
    }
    Ok(order.iter().map(|p| sk.pv(*p)).collect())
}

impl Model<'_> {
    /// Build the folded sheet metal solid and its flat pattern.
    pub fn sheet_metal(&self, fid: FeatId, profile: &[Ref], t: f64, r: f64, k: f64, depth: f64, flip: bool) -> R<(Vec<Shape>, FlatPattern)> {
        if t <= 0.0 || r < 0.0 || depth <= 0.0 {
            return Err("thickness and depth must be positive".into());
        }
        // a region: flat plate
        if let Some(Ref::Region { .. }) = profile.first() {
            let faces = self.profile_faces(profile)?;
            let mut tools = vec![];
            let mut flat = FlatPattern { feature: fid, ..Default::default() };
            for (f, fr) in &faces {
                tools.push(occ::prism(&Shape::from(f), fr.normal() * if flip { -t } else { t })?);
                if let Ref::Region { sketch, point } = &profile[0] {
                    let (sk, _) = self.sketch(*sketch)?;
                    if let Some(reg) = find_regions(sk).into_iter().find(|g| g.contains(DVec2::from(*point))) {
                        flat.outline.push(reg.outer.poly.iter().map(|p| [p.x, p.y]).collect());
                        for h in &reg.holes {
                            flat.outline.push(h.poly.iter().map(|p| [p.x, p.y]).collect());
                        }
                    }
                }
            }
            let mut lo = [f64::INFINITY; 2];
            let mut hi = [f64::NEG_INFINITY; 2];
            for p in flat.outline.iter().flatten() {
                lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                hi = [hi[0].max(p[0]), hi[1].max(p[1])];
            }
            flat.size = [hi[0] - lo[0], hi[1] - lo[1]];
            return Ok((tools, flat));
        }
        let mut ids = vec![];
        let mut sketch_id = None;
        for p in profile {
            match p {
                Ref::SketchCurve { sketch, curve } => {
                    sketch_id = Some(*sketch);
                    ids.push(*curve);
                }
                _ => return Err("select sketch lines (or a region for a flat plate)".into()),
            }
        }
        let sid = sketch_id.ok_or("select the profile lines")?;
        let (sk, fr) = self.sketch(sid)?;
        let pts = chain_points(sk, &ids)?;
        // material on the left of the path (right when flipped)
        let side = if flip { -1.0 } else { 1.0 };
        // turning towards the material puts the path on the outside of the bend
        let turns: Vec<f64> = (1..pts.len() - 1).map(|i| (pts[i] - pts[i - 1]).angle_to(pts[i + 1] - pts[i])).collect();
        let path_r: Vec<f64> = turns.iter().map(|tn| if tn * side > 0.0 { r + t } else { r }).collect();
        let (path, angles) = filleted_path(&pts, &path_r)?;
        // offset path by t to the material side
        let off: Vec<Seg> = path
            .iter()
            .map(|s| match *s {
                Seg::Line { a, b } => {
                    let nrm = (b - a).normalize().perp() * side * t;
                    Seg::Line { a: a + nrm, b: b + nrm }
                }
                Seg::Arc { c, r: rr, t0, t1 } => {
                    // the material side is inside the arc for turns towards it
                    let towards = (t1 > t0) == (side > 0.0);
                    Seg::Arc { c, r: if towards { rr - t } else { rr + t }, t0, t1 }
                }
                ref other => other.clone(),
            })
            .collect();
        let mut outline = path.clone();
        outline.push(Seg::Line { a: path.last().unwrap().end(), b: off.last().unwrap().end() });
        outline.extend(off.iter().rev().map(|s| s.reversed()));
        outline.push(Seg::Line { a: off[0].start(), b: path[0].start() });
        let edges = seg_edges(&outline, &fr)?;
        let wire = occ::wire(&edges)?;
        let face = occ::face(&wire, &[])?;
        let solid = occ::prism(&Shape::from(&face), fr.normal() * depth)?;
        // flat pattern: straight lengths along the path side + bend allowances on the neutral layer
        let mut flat = FlatPattern { feature: fid, ..Default::default() };
        let mut x = 0.0;
        let mut bends = vec![];
        for s in &path {
            match *s {
                Seg::Line { a, b } => x += (b - a).length(),
                Seg::Arc { .. } => {}
                _ => {}
            }
        }
        let _ = x;
        // walk again to place bend lines
        let mut pos = 0.0;
        let mut ai = 0;
        for s in &path {
            match *s {
                Seg::Line { a, b } => pos += (b - a).length(),
                Seg::Arc { .. } => {
                    let theta = angles[ai].abs();
                    let inner = if angles[ai] * side > 0.0 { path_r[ai] - t } else { path_r[ai] };
                    let ba = theta * (inner + k * t);
                    bends.push(pos + ba / 2.0);
                    pos += ba;
                    ai += 1;
                }
                _ => {}
            }
        }
        flat.size = [pos, depth];
        flat.outline.push(vec![[0.0, 0.0], [pos, 0.0], [pos, depth], [0.0, depth], [0.0, 0.0]]);
        flat.bends = bends.iter().map(|b| [[*b, 0.0], [*b, depth]]).collect();
        Ok((vec![solid], flat))
    }
}

/// DXF of a flat pattern (outline on layer CUT, bend lines on layer BEND).
pub fn flat_dxf(f: &FlatPattern) -> String {
    let mut s = String::from("0\nSECTION\n2\nENTITIES\n");
    let line = |s: &mut String, layer: &str, a: [f64; 2], b: [f64; 2]| {
        s.push_str(&format!("0\nLINE\n8\n{layer}\n10\n{:.4}\n20\n{:.4}\n30\n0\n11\n{:.4}\n21\n{:.4}\n31\n0\n", a[0], a[1], b[0], b[1]));
    };
    for pl in &f.outline {
        for w in pl.windows(2) {
            line(&mut s, "CUT", w[0], w[1]);
        }
        if pl.len() > 2 && pl[0] != pl[pl.len() - 1] {
            line(&mut s, "CUT", pl[pl.len() - 1], pl[0]);
        }
    }
    for b in &f.bends {
        line(&mut s, "BEND", b[0], b[1]);
    }
    s.push_str("0\nENDSEC\n0\nEOF\n");
    s
}

#[cfg(test)]
mod sheet_metal_tests {
    use super::*;
    use crate::sketch::{Geom, Sketch};

    #[test]
    fn u_channel_volume_and_flat_length() {
        // U profile: up 20, across 30, down 20
        let mut s = Sketch::default();
        let p: Vec<_> = [(0.0, 20.0), (0.0, 0.0), (30.0, 0.0), (30.0, 20.0)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        let l: Vec<_> = (0..3).map(|i| s.add_curve(Geom::Line { a: p[i], b: p[i + 1] }, false)).collect();
        let mut doc = Document::default();
        let sk = doc.insert("S".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Front }, sketch: s, frame: Frame::FRONT });
        let mut sm = Op::new_default("sheetMetal").unwrap();
        if let Op::SheetMetal { profile, thickness, radius, k_factor, depth, .. } = &mut sm {
            for id in &l {
                profile.push(Ref::SketchCurve { sketch: sk, curve: *id });
            }
            *thickness = "2".into();
            *radius = "2".into();
            *k_factor = "0.5".into();
            *depth = "50".into();
        }
        let fid = doc.insert("SM".into(), sm);
        let r = rebuild(&doc, &[], |_| {});
        for f in &r.features {
            assert!(f.error.is_none(), "{:?}", f.error);
        }
        assert_eq!(r.bodies.len(), 1);
        let flat = r.flats.iter().find(|f| f.feature == fid).unwrap();
        // neutral length * thickness * depth = volume
        let want = flat.size[0] * 2.0 * 50.0;
        assert!((r.bodies[0].volume - want).abs() / want < 0.01, "{} vs {want}", r.bodies[0].volume);
        assert_eq!(flat.bends.len(), 2);
    }
}

#[cfg(test)]
mod face_tests {
    use super::*;
    use crate::sketch::{Geom, Sketch};

    #[test]
    fn move_face_and_rib() {
        let mut doc = Document::default();
        let mut s = Sketch::default();
        // L section in the Front plane, extruded 10 along -Y... the front plane normal is -Y
        let pts = [(0.0, 0.0), (40.0, 0.0), (40.0, 5.0), (5.0, 5.0), (5.0, 40.0), (0.0, 40.0)];
        let p: Vec<_> = pts.iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        for i in 0..6 {
            s.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 6] }, false);
        }
        let sk = doc.insert("S".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Front }, sketch: s, frame: Frame::FRONT });
        let mut e = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, depth, .. } = &mut e {
            profiles.push(Ref::Region { sketch: sk, point: [2.0, 2.0] });
            *depth = "20".into();
        }
        doc.insert("E".into(), e);
        let base = 20.0 * (40.0 * 5.0 + 35.0 * 5.0);
        // push the top of the vertical leg up by 5
        let mut mf = Op::new_default("moveFace").unwrap();
        if let Op::MoveFace { faces, distance } = &mut mf {
            faces.push(Ref::Face { point: [2.5, -10.0, 40.0], normal: [0.0, 0.0, 1.0] });
            *distance = "5".into();
        }
        doc.insert("MF".into(), mf);
        // rib across the inside corner, in a mid plane 10 mm behind the front
        let mut rs = Sketch::default();
        let a = rs.add_point(DVec2::new(25.0, 5.0));
        let b = rs.add_point(DVec2::new(5.0, 25.0));
        let l = rs.add_curve(Geom::Line { a, b }, false);
        // on the Front plane, i.e. the part's front face: half the rib thickness lands in the part's extent
        let rsk = doc.insert("RS".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Front }, sketch: rs, frame: Frame::FRONT });
        let mut rib = Op::new_default("rib").unwrap();
        if let Op::Rib { profile, thickness, flip } = &mut rib {
            profile.push(Ref::SketchCurve { sketch: rsk, curve: l });
            *thickness = "2".into();
            *flip = false;
        }
        doc.insert("Rib".into(), rib);
        let r = rebuild(&doc, &[], |_| {});
        for f in &r.features {
            assert!(f.error.is_none(), "{:?}", f.error);
        }
        let moved = base + 5.0 * 5.0 * 20.0;
        let rib_vol = 0.5 * 20.0 * 20.0 * 1.0;
        assert!((r.bodies[0].volume - (moved + rib_vol)).abs() < 1.0, "{} vs {}", r.bodies[0].volume, moved + rib_vol);
    }
}

#[cfg(test)]
mod derived_tests {
    use super::*;
    use crate::sketch::{Geom, Sketch};

    #[test]
    fn derive_parts_from_another_studio() {
        // studio 0: a 10 mm cube
        let mut a = Document::default();
        let mut s = Sketch::default();
        let p: Vec<_> = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        for i in 0..4 {
            s.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false);
        }
        let sk = a.insert("S".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Top }, sketch: s, frame: Frame::TOP });
        let mut e = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, .. } = &mut e {
            profiles.push(Ref::Region { sketch: sk, point: [5.0, 5.0] });
        }
        a.insert("E".into(), e);
        // studio 1 derives it and moves a copy
        let mut b = Document::default();
        let d = b.insert("D".into(), Op::Derived { studio: 0, bodies: vec![] });
        let mut t = Op::new_default("transform").unwrap();
        if let Op::Transform { bodies, direction, distance, copy, .. } = &mut t {
            bodies.push(Ref::Body { body: (d, 0) });
            *direction = Some(Ref::Axis { axis: 0 });
            *distance = "20".into();
            *copy = true;
        }
        b.insert("T".into(), t);
        *STUDIOS.lock().unwrap() = vec![a, b.clone()];
        let r = rebuild(&b, &[], |_| {});
        for f in &r.features {
            assert!(f.error.is_none(), "{:?}", f.error);
        }
        assert_eq!(r.bodies.len(), 2);
        assert!((r.bodies[0].volume - 1000.0).abs() < 1e-6);
        // a studio deriving from itself is reported, not looped forever
        let mut c = Document::default();
        c.insert("D".into(), Op::Derived { studio: 0, bodies: vec![] });
        *STUDIOS.lock().unwrap() = vec![c.clone()];
        let r = rebuild(&c, &[], |_| {});
        assert!(r.features[0].error.is_some());
    }

    #[test]
    fn interference_between_instances() {
        let mut a = Document::default();
        let mut s = Sketch::default();
        let p: Vec<_> = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        for i in 0..4 {
            s.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false);
        }
        let sk = a.insert("S".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Top }, sketch: s, frame: Frame::TOP });
        let mut e = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, .. } = &mut e {
            profiles.push(Ref::Region { sketch: sk, point: [5.0, 5.0] });
        }
        let eid = a.insert("E".into(), e);
        *STUDIOS.lock().unwrap() = vec![a];
        let at = |x: f64| [1.0, 0.0, 0.0, x, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let r = interference(&[(0, (eid, 0), at(0.0)), (0, (eid, 0), at(5.0)), (0, (eid, 0), at(30.0))]);
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].0, r[0].1), (0, 1));
        assert!((r[0].2 - 500.0).abs() < 1e-6, "{}", r[0].2);
    }
}

#[cfg(test)]
mod surface_tests {
    use super::*;
    use crate::sketch::{Geom, Sketch};

    #[test]
    fn surfaces_fill_knit_into_a_solid() {
        let mut doc = Document::default();
        let mut s = Sketch::default();
        let p: Vec<_> = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        let lines: Vec<_> = (0..4).map(|i| s.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false)).collect();
        let sk = doc.insert("S".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Top }, sketch: s, frame: Frame::TOP });
        let walls = doc.insert(
            "W".into(),
            Op::SurfaceExtrude { curves: lines.iter().map(|l| Ref::SketchCurve { sketch: sk, curve: *l }).collect(), depth: "10".into(), symmetric: false, opposite: false },
        );
        let edges = |z: f64| vec![[5.0, 0.0, z], [10.0, 5.0, z], [5.0, 10.0, z], [0.0, 5.0, z]].into_iter().map(|p| Ref::Edge { point: p }).collect::<Vec<_>>();
        let bottom = doc.insert("B".into(), Op::Fill { edges: edges(0.0) });
        let top = doc.insert("T".into(), Op::Fill { edges: edges(10.0) });
        doc.insert(
            "K".into(),
            Op::Knit { bodies: [walls, bottom, top].iter().map(|f| Ref::Body { body: (*f, 0) }).collect(), solid: true },
        );
        let r = rebuild(&doc, &[], |_| {});
        for f in &r.features {
            assert!(f.error.is_none(), "{:?}", f.error);
        }
        assert_eq!(r.bodies.len(), 1);
        assert!(!r.bodies[0].surface);
        assert!((r.bodies[0].volume - 1000.0).abs() < 1e-3, "{}", r.bodies[0].volume);
    }

    #[test]
    fn revolved_surface_thickened() {
        let mut doc = Document::default();
        let mut s = Sketch::default();
        let a = s.add_point(DVec2::new(10.0, 0.0));
        let b = s.add_point(DVec2::new(10.0, 20.0));
        let l = s.add_curve(Geom::Line { a, b }, false);
        let sk = doc.insert("S".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Front }, sketch: s, frame: Frame::FRONT });
        let rv = doc.insert("R".into(), Op::SurfaceRevolve { curves: vec![Ref::SketchCurve { sketch: sk, curve: l }], axis: Some(Ref::Axis { axis: 2 }), angle: "360".into() });
        let r = rebuild(&doc, &[], |_| {});
        assert!(r.bodies[0].surface, "a revolved line is a surface");
        doc.insert("T".into(), Op::Thicken { faces: vec![Ref::Body { body: (rv, 0) }], thickness: "1".into(), opposite: false, op: BoolOp::New });
        let r = rebuild(&doc, &[], |_| {});
        for f in &r.features {
            assert!(f.error.is_none(), "{:?}", f.error);
        }
        let v = r.bodies[0].volume;
        let tube_out = std::f64::consts::PI * (11.0f64.powi(2) - 100.0) * 20.0;
        let tube_in = std::f64::consts::PI * (100.0 - 81.0) * 20.0;
        assert!((v - tube_out).abs() < 1.0 || (v - tube_in).abs() < 1.0, "{v}");
    }
}

#[cfg(test)]
mod flange_tests {
    use super::*;
    use crate::sketch::{Geom, Sketch};

    #[test]
    fn plate_with_flange() {
        let mut doc = Document::default();
        let mut s = Sketch::default();
        let p: Vec<_> = [(0.0, 0.0), (40.0, 0.0), (40.0, 30.0), (0.0, 30.0)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        for i in 0..4 {
            s.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false);
        }
        let sk = doc.insert("S".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Top }, sketch: s, frame: Frame::TOP });
        let sm = doc.insert(
            "SM".into(),
            Op::SheetMetal { profile: vec![Ref::Region { sketch: sk, point: [5.0, 5.0] }], thickness: "2".into(), radius: "2".into(), k_factor: "0.5".into(), depth: "10".into(), flip: false, op: BoolOp::New },
        );
        doc.insert("F".into(), Op::Flange { edges: vec![Ref::Edge { point: [40.0, 15.0, 2.0] }], length: "10".into(), angle: "90".into(), flip: false, inset: "0".into() });
        let r = rebuild(&doc, &[], |_| {});
        for f in &r.features {
            assert!(f.error.is_none(), "{:?}", f.error);
        }
        let want = 40.0 * 30.0 * 2.0 + std::f64::consts::FRAC_PI_2 * 3.0 * 2.0 * 30.0 + 10.0 * 2.0 * 30.0;
        assert!((r.bodies[0].volume - want).abs() < 0.5, "{} vs {want}", r.bodies[0].volume);
        let flat = r.flats.iter().find(|f| f.feature == sm).unwrap();
        assert_eq!(flat.bends.len(), 1);
        assert!(r.bodies[0].bbox[1][2] > 13.9, "flange goes up: {:?}", r.bodies[0].bbox);
    }
}

#[cfg(test)]
mod feature_option_tests {
    use super::*;
    use crate::sketch::{Geom, Sketch};
    use std::f64::consts::PI;

    fn sketch_on(doc: &mut Document, plane: Ref, frame: Frame, f: impl FnOnce(&mut Sketch)) -> FeatId {
        let mut s = Sketch::default();
        f(&mut s);
        doc.insert("S".into(), Op::Sketch { plane, sketch: s, frame })
    }

    fn rect(s: &mut Sketch, x0: f64, y0: f64, w: f64, h: f64) {
        let p: Vec<_> = [(x0, y0), (x0 + w, y0), (x0 + w, y0 + h), (x0, y0 + h)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        for i in 0..4 {
            s.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false);
        }
    }

    fn extrude(doc: &mut Document, sk: FeatId, at: [f64; 2], f: impl FnOnce(&mut Op)) {
        let mut o = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, .. } = &mut o {
            profiles.push(Ref::Region { sketch: sk, point: at });
        }
        f(&mut o);
        doc.insert("E".into(), o);
    }

    fn run(doc: &Document) -> RebuildResult {
        let r = rebuild(doc, &[], |_| {});
        for f in &r.features {
            assert!(f.error.is_none(), "feature {} failed: {:?}", f.id, f.error);
        }
        r
    }

    fn top() -> Ref {
        Ref::OriginPlane { plane: OriginPlane::Top }
    }

    fn cube(doc: &mut Document, size: f64) {
        let s = sketch_on(doc, top(), Frame::TOP, |s| rect(s, 0.0, 0.0, size, size));
        extrude(doc, s, [1.0, 1.0], |o| {
            if let Op::Extrude { depth, .. } = o {
                *depth = size.to_string();
            }
        });
    }

    #[test]
    fn chamfer_types() {
        for (mode, want) in [("equal", 0.5 * 2.0 * 2.0 * 20.0), ("two", 0.5 * 2.0 * 4.0 * 20.0), ("angle", 0.5 * 2.0 * 2.0 * (60f64).to_radians().tan() * 20.0)] {
            let mut doc = Document::default();
            cube(&mut doc, 20.0);
            let mut c = Op::new_default("chamfer").unwrap();
            if let Op::Chamfer { edges, distance, mode: m, distance2, angle } = &mut c {
                edges.push(Ref::Edge { point: [10.0, 0.0, 20.0] });
                *distance = "2".into();
                *m = mode.into();
                *distance2 = "4".into();
                *angle = "60".into();
            }
            doc.insert("C".into(), c);
            let r = run(&doc);
            let removed = 8000.0 - r.bodies[0].volume;
            assert!((removed - want).abs() < 0.5, "{mode}: removed {removed} want {want}");
        }
    }

    #[test]
    fn extrude_up_to_next_and_vertex() {
        // plate 40x20x5, a separate block above it from z=15..25; extrude a square up from the plate's top to the next part
        let mut doc = Document::default();
        let s = sketch_on(&mut doc, top(), Frame::TOP, |s| rect(s, 0.0, 0.0, 40.0, 20.0));
        extrude(&mut doc, s, [1.0, 1.0], |o| {
            if let Op::Extrude { depth, .. } = o {
                *depth = "5".into();
            }
        });
        let s = sketch_on(&mut doc, top(), Frame::TOP, |s| rect(s, 0.0, 0.0, 40.0, 20.0));
        extrude(&mut doc, s, [1.0, 1.0], |o| {
            if let Op::Extrude { depth, depth2, second, opposite, op, .. } = o {
                // a 10 mm slab from z = 15 to 25: extrude 25 then cut the bottom 15 later is awkward; use second direction
                *depth = "25".into();
                *second = false;
                *opposite = false;
                *op = BoolOp::New;
                let _ = depth2;
            }
        });
        let s = sketch_on(&mut doc, top(), Frame::TOP, |s| rect(s, -1.0, -1.0, 42.0, 22.0));
        extrude(&mut doc, s, [-0.5, -0.5], |o| {
            if let Op::Extrude { depth, op, .. } = o {
                *depth = "15".into();
                *op = BoolOp::Remove;
            }
        });
        // now only a block z = 15..25 remains; put the plate back
        let s = sketch_on(&mut doc, top(), Frame::TOP, |s| rect(s, 0.0, 0.0, 40.0, 20.0));
        extrude(&mut doc, s, [1.0, 1.0], |o| {
            if let Op::Extrude { depth, op, .. } = o {
                *depth = "5".into();
                *op = BoolOp::New;
            }
        });
        let face = Ref::Face { point: [20.0, 10.0, 5.0], normal: [0.0, 0.0, 1.0] };
        let s = sketch_on(&mut doc, face, Frame::TOP, |s| rect(s, 15.0, 5.0, 10.0, 10.0));
        extrude(&mut doc, s, [20.0, 10.0], |o| {
            if let Op::Extrude { end, op, .. } = o {
                *end = EndType::UpToNext;
                *op = BoolOp::New;
            }
        });
        let r = run(&doc);
        let post = r.bodies.iter().find(|b| (b.volume - 1000.0).abs() < 1.0);
        assert!(post.is_some(), "volumes {:?}", r.bodies.iter().map(|b| b.volume).collect::<Vec<_>>());

        // up to vertex: a cube with a boss up to a point at z = 30
        let mut doc = Document::default();
        cube(&mut doc, 20.0);
        let face = Ref::Face { point: [10.0, 10.0, 20.0], normal: [0.0, 0.0, 1.0] };
        let s = sketch_on(&mut doc, face, Frame::TOP, |s| rect(s, 5.0, 5.0, 10.0, 10.0));
        extrude(&mut doc, s, [10.0, 10.0], |o| {
            if let Op::Extrude { end, up_to, op, .. } = o {
                *end = EndType::UpToVertex;
                *up_to = Some(Ref::Vertex { point: [50.0, 50.0, 30.0] });
                *op = BoolOp::Add;
            }
        });
        let r = run(&doc);
        assert!((r.bodies[0].volume - 9000.0).abs() < 1.0, "{}", r.bodies[0].volume);
    }

    #[test]
    fn extrude_draft_tapers_inward() {
        let mut doc = Document::default();
        let s = sketch_on(&mut doc, top(), Frame::TOP, |s| rect(s, 0.0, 0.0, 20.0, 20.0));
        extrude(&mut doc, s, [1.0, 1.0], |o| {
            if let Op::Extrude { depth, draft, .. } = o {
                *depth = "10".into();
                *draft = "5".into();
            }
        });
        let r = run(&doc);
        let t = 10.0 * (5f64).to_radians().tan();
        let top = 20.0 - 2.0 * t;
        // frustum volume
        let want = 10.0 / 3.0 * (400.0 + top * top + 20.0 * top);
        assert!((r.bodies[0].volume - want).abs() < 1.0, "{} want {want}", r.bodies[0].volume);
    }

    #[test]
    fn move_curved_face() {
        let mut doc = Document::default();
        let s = sketch_on(&mut doc, top(), Frame::TOP, |s| {
            let c = s.add_point(DVec2::ZERO);
            s.add_curve(Geom::Circle { c, r: 5.0 }, false);
        });
        extrude(&mut doc, s, [0.0, 0.0], |o| {
            if let Op::Extrude { depth, .. } = o {
                *depth = "10".into();
            }
        });
        let mut mf = Op::new_default("moveFace").unwrap();
        if let Op::MoveFace { faces, distance } = &mut mf {
            faces.push(Ref::Face { point: [5.0, 0.0, 5.0], normal: [1.0, 0.0, 0.0] });
            *distance = "1".into();
        }
        doc.insert("MF".into(), mf);
        let r = run(&doc);
        let want = PI * 36.0 * 10.0;
        assert!((r.bodies[0].volume - want).abs() < 2.0, "{} want {want}", r.bodies[0].volume);
    }
}

#[cfg(test)]
mod custom_tests {
    use super::*;
    use crate::script::{CustomDef, ParamDef};
    use std::collections::BTreeMap;

    #[test]
    fn scripted_features() {
        let mut doc = Document::default();
        doc.custom_features.push(CustomDef {
            name: "Holed block".into(),
            params: vec![ParamDef { name: "size".into(), kind: "length".into(), default: "10".into() }, ParamDef { name: "n".into(), kind: "count".into(), default: "2".into() }],
            selection: String::new(),
            source: "let b = cube(size, size, size);\nfor i in 0..n { b = b - cylinder(1, size).move(2.5 + i * 5, 5, 0); }\nprint(`holes: ${n}`);\nb".into(),
        });
        doc.custom_features.push(CustomDef { name: "Bosses".into(), params: vec![ParamDef { name: "h".into(), kind: "length".into(), default: "5".into() }], selection: "points".into(), source: "points.map(|p| cylinder(1, h).place(p, [0, 0, 1]))".into() });
        let mut values = BTreeMap::new();
        values.insert("size".to_string(), "20".to_string());
        doc.insert("C1".into(), Op::Custom { name: "Holed block".into(), values, refs: vec![], select: String::new(), op: BoolOp::New });
        doc.insert(
            "C2".into(),
            Op::Custom { name: "Bosses".into(), values: BTreeMap::new(), refs: vec![Ref::Vertex { point: [10.0, 10.0, 20.0] }], select: "points".into(), op: BoolOp::Add },
        );
        let r = rebuild(&doc, &[], |_| {});
        for f in &r.features {
            assert!(f.error.is_none(), "{:?}", f.error);
        }
        let pi = std::f64::consts::PI;
        let want = 8000.0 - 2.0 * pi * 20.0 + pi * 5.0;
        assert_eq!(r.bodies.len(), 1);
        assert!((r.bodies[0].volume - want).abs() < 0.5, "{} want {want}", r.bodies[0].volume);
        // script errors come back as feature errors
        doc.custom_features[0].source = "cube(1, 2)".into();
        let r = rebuild(&doc, &[], |_| {});
        assert!(r.features.iter().any(|f| f.error.as_deref().map(|e| e.contains("script")).unwrap_or(false)));
    }
}
