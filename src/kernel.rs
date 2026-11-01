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

mod naming;

// ------------------------------------------------------------------ results

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FaceInfo {
    pub center: [f64; 3],
    pub normal: [f64; 3],
    pub planar: bool,
    pub area: f64,
    /// Persistent name of the face (the feature and role that made it), when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<crate::doc::FaceName>,
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
    /// Inertia tensor about the centroid for unit density (mm⁵), column-major.
    #[serde(default)]
    pub inertia: [f64; 9],
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
    /// Problems found in the finished parts (slivers, tiny faces, gaps, invalid geometry).
    #[serde(default)]
    pub issues: Vec<Issue>,
    /// Face and edge references with their persistent names and current points, for the
    /// editor to store in the document (`Document::apply_ref_updates`).
    #[serde(default)]
    pub refs: Vec<crate::doc::RefUpdate>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Issue {
    pub body: BodyId,
    /// "sliver", "small_face", "short_edge", "open", "invalid"
    pub kind: String,
    /// Where it is (a point on it).
    pub at: [f64; 3],
    /// Its size in mm (sliver width, face area, edge length).
    pub size: f64,
}

impl Issue {
    pub fn describe(&self) -> String {
        let n = |v: f64| crate::expr::trim(v, 4);
        match self.kind.as_str() {
            "sliver" => format!("Sliver face {} mm wide", n(self.size)),
            "small_face" => format!("Tiny face, {} mm²", n(self.size)),
            "short_edge" => format!("Tiny edge, {} mm long", n(self.size)),
            "mesh_gap" => "Meshes with gaps here (exported STL would have holes)".into(),
            "open" => "Not closed (has gaps)".into(),
            _ => "Invalid geometry".into(),
        }
    }
}

/// Look for geometry that usually means a modelling slip: faces or edges far smaller
/// than the part, thin slivers, gaps, and shapes OpenCascade reports as invalid.
/// `valid` is the B-rep check, done before meshing (meshing can upset it).
pub fn check_body(shape: &Shape, m: &BodyMesh, valid: bool) -> Vec<Issue> {
    let mut out = vec![];
    let id = m.id;
    // the display/export mesh must be closed: open edges mean holes in an exported STL
    if !m.surface {
        let open = mesh_open_edges(m);
        if let Some((a, b)) = open.first() {
            let at = [0, 1, 2].map(|k| (a[k] + b[k]) / 2.0);
            out.push(Issue { body: id, kind: "mesh_gap".into(), at, size: open.len() as f64 });
        }
    }
    // imported meshes are made of thin triangles on purpose
    if m.faces.len() >= 1500 {
        return out;
    }
    let size = (DVec3::from(m.bbox[1]) - DVec3::from(m.bbox[0])).length().max(1e-9);
    let small = (size * 1e-4).max(1e-3);
    for e in &m.edges {
        if e.length < small {
            out.push(Issue { body: id, kind: "short_edge".into(), at: e.mid, size: e.length });
        }
    }
    // per face: area and longest extent from its triangles
    let mut ext: Vec<(DVec3, DVec3)> = vec![(DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)); m.faces.len()];
    for (ti, t) in m.indices.chunks(3).enumerate() {
        let f = m.tri_face[ti] as usize;
        for &i in t {
            let p = DVec3::new(m.positions[3 * i as usize] as f64, m.positions[3 * i as usize + 1] as f64, m.positions[3 * i as usize + 2] as f64);
            ext[f].0 = ext[f].0.min(p);
            ext[f].1 = ext[f].1.max(p);
        }
    }
    for (fi, f) in m.faces.iter().enumerate() {
        let len = (ext[fi].1 - ext[fi].0).length();
        if !len.is_finite() || len <= 0.0 {
            continue;
        }
        if f.area < small * small {
            out.push(Issue { body: id, kind: "small_face".into(), at: f.center, size: f.area });
        } else {
            let width = f.area / len;
            if width < small * 2.0 && len > 20.0 * width {
                out.push(Issue { body: id, kind: "sliver".into(), at: f.center, size: width });
            }
        }
    }
    if !m.surface && !valid {
        // point at the faulty faces or edges themselves
        let spots = ffi::extra::ex_invalid_spots(&shape.inner);
        if spots.is_empty() {
            out.push(Issue { body: id, kind: "invalid".into(), at: m.centroid, size: 0.0 });
        }
        for p in spots.chunks(3).take(20) {
            out.push(Issue { body: id, kind: "invalid".into(), at: [p[0], p[1], p[2]], size: 0.0 });
        }
    }
    out
}

// ------------------------------------------------------------------ state

pub struct Body {
    pub id: BodyId,
    pub shape: Shape,
}

pub struct Model<'a> {
    pub doc: &'a Document,
    pub vars: crate::expr::Vars,
    pub bodies: Vec<Body>,
    pub frames: HashMap<FeatId, Frame>,
    /// Wire results (helices).
    pub curves: Vec<(FeatId, Shape)>,
    /// Tool bodies and operation of each feature, for feature patterns.
    pub tools: HashMap<FeatId, (Vec<Shape>, BoolOp)>,
    pub flats: Vec<FlatPattern>,
    /// Sheet metal bodies: thickness, bend radius, K-factor, the feature with its flat pattern.
    pub sheet: HashMap<BodyId, SheetInfo>,
    /// Faces each feature made, by role, for persistent references (see `naming`).
    named: Vec<naming::Named>,
    /// The feature being built (0: none), and the references it resolved that should be stored back.
    current: std::cell::Cell<FeatId>,
    updates: std::cell::RefCell<Vec<crate::doc::RefUpdate>>,
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
        Model {
            doc,
            vars,
            bodies: vec![],
            frames: HashMap::new(),
            curves: vec![],
            tools: HashMap::new(),
            flats: vec![],
            sheet: HashMap::new(),
            named: vec![],
            current: Default::default(),
            updates: Default::default(),
        }
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
            Ref::FeaturePlane { feature } => self.frames.get(feature).copied().ok_or_else(|| self.missing(*feature, "the referenced plane is missing")),
            Ref::Face { .. } => {
                let (face, _) = self.find_face_ref(r)?;
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
            Ref::Body { body } => self.bodies.iter().position(|b| b.id == *body).ok_or_else(|| self.missing(body.0, "a referenced body no longer exists")),
            Ref::Face { .. } => Ok(self.find_face_ref(r)?.1),
            Ref::Edge { .. } => Ok(self.find_edge_ref(r)?.1),
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
            Ref::Edge { .. } => {
                let (e, _) = self.find_edge_ref(r)?;
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
        match self.doc.feature(id).map(|f| (&f.op, &f.name)) {
            Some((Op::Sketch { sketch, .. }, name)) => Ok((sketch, self.frames.get(&id).copied().ok_or_else(|| self.missing(id, &format!("the plane of {name} could not be found")))?)),
            _ => Err("the referenced sketch no longer exists".into()),
        }
    }

    /// Why something feature `id` made is missing: say so plainly when that feature comes
    /// later in the list than the one being built (it was moved above what it uses).
    fn missing(&self, id: FeatId, otherwise: &str) -> String {
        let (Some(at), Some(cur)) = (self.doc.index_of(id), self.doc.index_of(self.current.get())) else { return otherwise.into() };
        if at > cur {
            let name = |i: usize| self.doc.features[i].name.clone();
            return format!("this feature uses {}, which comes after it in the list: move {} below {}", name(at), name(cur), name(at));
        }
        match self.doc.features.get(at) {
            Some(f) if f.suppressed => format!("{} is suppressed", f.name),
            _ => otherwise.into(),
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
                Ref::Face { .. } => {
                    let (f, _) = self.find_face_ref(r)?;
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
        // e.g. an extrude up to a face behind its profile: an error, not a feature that adds nothing
        if unique_faces(&tool).is_empty() {
            return Err("the feature made nothing (is the face or point it goes up to on the other side?)".into());
        }
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
                let mut out: Vec<Body> = vec![];
                let before: Vec<BodyId> = self.bodies.iter().map(|b| b.id).collect();
                for b in std::mem::take(&mut self.bodies) {
                    // a cut that misses leaves the body as it was (checked by the cut itself:
                    // a separate overlap test costs as much as the cut on large parts)
                    if op == BoolOp::Remove && bbox_apart(&b.shape, &tool) {
                        out.push(b);
                        continue;
                    }
                    let r = if op == BoolOp::Remove { occ::cut(&b.shape, &tool)? } else { occ::common(&b.shape, &tool)? };
                    let mut k = 0;
                    for (i, s) in solids_of(&r).into_iter().enumerate() {
                        // pieces after the first: b + 1000, + 2000, ..., skipping ids a piece of an
                        // earlier split already has (a body split twice would otherwise repeat one)
                        let id = if i == 0 {
                            b.id
                        } else {
                            loop {
                                k += 1;
                                let id = (b.id.0, b.id.1 + 1000 * k);
                                if !before.contains(&id) && !out.iter().any(|o| o.id == id) {
                                    break id;
                                }
                            }
                        };
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
            Op::Sketch { plane, .. } => {
                // no falling back to where the plane was: the sketch would silently stay behind
                let fr = self.frame_of(plane).map_err(|e| format!("the sketch plane could not be found: {e}"))?;
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
                let sketches: Vec<Option<FeatId>> = profiles.iter().map(|r| if let Ref::Region { sketch, .. } = r { Some(*sketch) } else { None }).collect();
                let mut sign = if *opposite { -1.0 } else { 1.0 };
                let d = match end {
                    EndType::ThroughAll | EndType::UpToFace => self.model_size() * 2.0,
                    EndType::UpToNext => self.up_to_next(&faces, sign, *op)?,
                    EndType::UpToVertex => {
                        let r = up_to.as_ref().ok_or("select the vertex or point to extrude up to")?;
                        let p = self.point_of(r)?;
                        let (_, fr) = &faces[0];
                        // toward the point, on whichever side of the profile it lies
                        let along = (p - DVec3::from(fr.origin)).dot(fr.normal() * sign);
                        if along < 0.0 {
                            sign = -sign;
                        }
                        along.abs()
                    }
                    _ => self.len(depth)?,
                };
                if d <= 0.0 {
                    return Err("depth must be positive".into());
                }
                let d2 = if *second { self.len(depth2)? } else { 0.0 };
                let draft_a = self.ang(draft)?.to_radians();
                // `both`: through all, in both directions
                let build = |sign: f64, both: bool| -> R<Vec<Shape>> {
                    let mut tools = vec![];
                    for (face, fr) in &faces {
                        let n = fr.normal() * sign;
                        let fs = Shape::from(face);
                        let solid = match end {
                            EndType::Symmetric => occ::prism(&occ::translate(&fs, -n * (d / 2.0))?, n * d)?,
                            _ if both => occ::prism(&occ::translate(&fs, -n * d)?, n * (2.0 * d))?,
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
                // A through-all cut from a profile with material behind it (in the middle of the part,
                // or on a face that has since moved into it) would end in a blind pocket: cut both ways.
                let both = *end == EndType::ThroughAll
                    && *op == BoolOp::Remove
                    && !*second
                    && faces.iter().any(|(f, fr)| naming::surf_of(f).map(|s| self.inside_material(s.at - fr.normal() * sign * 1e-2)).unwrap_or(false));
                let tools = build(sign, both)?;
                // Cutting away from the material does nothing: go into it instead, as the original does.
                let cutting = matches!(op, BoolOp::Remove | BoolOp::Intersect);
                let may_flip = cutting && !both && !matches!(end, EndType::Symmetric | EndType::UpToNext | EndType::UpToVertex) && !*second;
                if !may_flip {
                    self.name_extruded(fid, &tools, &faces, &sketches, sign);
                    return self.apply(fid, tools, *op);
                }
                // cut, and if that removed nothing, cut the other way instead (one boolean in the usual case)
                let flipped = build(-sign, false)?;
                // a tool that came out empty (up to a face behind the profile) cuts the other way
                let empty = |ts: &[Shape]| ts.iter().all(|t| unique_faces(t).is_empty());
                if empty(&tools) && !empty(&flipped) {
                    self.name_extruded(fid, &flipped, &faces, &sketches, -sign);
                    return self.apply(fid, flipped, *op);
                }
                let before: Vec<Body> = self.bodies.iter().map(|b| Body { id: b.id, shape: dup(&b.shape) }).collect();
                let vol = |bs: &[Body]| bs.iter().map(|b| volume(&b.shape)).sum::<f64>();
                let v0 = vol(&before);
                let named = self.named.len();
                self.name_extruded(fid, &tools, &faces, &sketches, sign);
                self.apply(fid, tools, *op)?;
                if (vol(&self.bodies) - v0).abs() <= 1e-9 * v0.abs().max(1.0) && self.bodies.len() == before.len() {
                    self.bodies = before;
                    self.named.truncate(named);
                    self.name_extruded(fid, &flipped, &faces, &sketches, -sign);
                    return self.apply(fid, flipped, *op);
                }
                Ok(())
            }
            Op::Revolve { profiles, axis, mode, angle, op } => {
                let faces = self.profile_faces(profiles)?;
                let axis = axis.as_ref().ok_or("select an axis")?;
                let (o, dir) = self.axis_of(axis)?;
                let a = if mode == "full" { 360.0 } else { self.ang(angle)? };
                let start = if mode == "symmetric" { -(a / 2.0).to_radians() } else { 0.0 };
                let mut tools = vec![];
                for ((face, _), r) in faces.iter().zip(profiles) {
                    let fs = Shape::from(face);
                    let fs = if mode == "symmetric" { occ::rotate(&fs, o, dir, start)? } else { fs };
                    let tool = occ::revolve(&fs, o, dir, a.to_radians())?;
                    if let (Ref::Region { sketch, .. }, Some(s)) = (r, naming::surf_of(face)) {
                        self.name_revolve(fid, &tool, o, dir, *sketch, s.at, start);
                    }
                    tools.push(tool);
                }
                self.apply(fid, tools, *op)
            }
            Op::Sweep { profiles, path, op } => {
                let faces = self.profile_faces(profiles)?;
                let spine = self.path_wire(path)?;
                let mut tools = vec![];
                for (face, _) in &faces {
                    // the outer boundary swept, less each hole swept the same way
                    const HINT: &str = " (the path may turn back on itself or too sharply for the profile)";
                    let sweep = |w: &Wire| occ::sweep(w, &spine, false).map_err(|e| format!("{e}{HINT}"));
                    let mut s = sweep(&face.outer_wire())?;
                    for h in hole_wires(face) {
                        s = occ::cut(&s, &sweep(&h)?)?;
                    }
                    tools.push(occ::checked(s, "the sweep", HINT)?);
                }
                self.apply(fid, tools, *op)
            }
            Op::Loft { profiles, ruled, op } => {
                let faces = self.profile_faces(profiles)?;
                if faces.len() < 2 {
                    return Err("select two or more profiles".into());
                }
                let wires: Vec<Wire> = faces.iter().map(|(f, _)| f.outer_wire()).collect();
                let mut s = occ::loft(&wires, *ruled)?;
                // holes: loft each through the matching hole of every profile and cut it out
                let holes: Vec<Vec<Wire>> = faces.iter().map(|(f, _)| hole_wires(f)).collect();
                let nh = holes[0].len();
                if holes.iter().any(|h| h.len() != nh) {
                    return Err(format!("the profiles have different numbers of holes ({}): a loft needs the same holes in every profile", holes.iter().map(|h| h.len().to_string()).collect::<Vec<_>>().join(", ")));
                }
                for k in 0..nh {
                    let chain = matching_holes(&wires, &holes, k);
                    s = occ::cut(&s, &occ::loft(&chain, *ruled)?)?;
                }
                let s = occ::checked(s, "the loft", " (profiles twisted or crossing?)")?;
                self.apply(fid, vec![s], *op)
            }
            Op::Fillet { edges, radius, variable, end_radius } => {
                let r = self.len(radius)?;
                if r <= 0.0 {
                    return Err("radius must be positive".into());
                }
                let r2 = if *variable { self.len(end_radius)? } else { r };
                if r2 <= 0.0 {
                    return Err("end radius must be positive".into());
                }
                self.edge_op(fid, "fillet", edges, |shape, es| occ::fillet(shape, es, r, r2).and_then(|s| occ::checked(s, "the fillet", " (radius too large?)")))
            }
            Op::Chamfer { edges, distance, mode, distance2, angle } => {
                let d = self.len(distance)?;
                if d <= 0.0 {
                    return Err("distance must be positive".into());
                }
                match mode.as_str() {
                    "two" => {
                        let d2 = self.len(distance2)?;
                        self.edge_op(fid, "chamfer", edges, |shape, es| occ::chamfer2(shape, es, d, d2, 0))
                    }
                    "angle" => {
                        let a = self.ang(angle)?.to_radians();
                        self.edge_op(fid, "chamfer", edges, |shape, es| occ::chamfer2(shape, es, d, a, 1))
                    }
                    _ => self.edge_op(fid, "chamfer", edges, |shape, es| occ::chamfer(shape, es, d)),
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
                        Ref::Edge { .. } => es.push(self.find_edge_ref(r)?.0),
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
            Op::Heal { bodies, tolerance } => {
                let tol = self.len(tolerance)?;
                if tol <= 0.0 {
                    return Err("tolerance must be positive".into());
                }
                let idx: Vec<usize> = if bodies.is_empty() { (0..self.bodies.len()).collect() } else { bodies.iter().map(|r| self.body_index(r)).collect::<R<_>>()? };
                if idx.is_empty() {
                    return Err("there are no parts to heal".into());
                }
                for i in idx {
                    let healed = occ::heal(&self.bodies[i].shape, tol)?;
                    let solids = solids_of(&healed);
                    self.bodies[i].shape = if solids.len() == 1 { solids.into_iter().next().unwrap() } else { healed };
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
                    let (e, bi) = self.find_edge_ref(r)?;
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
                let ids: Vec<BodyId> = faces.iter().filter_map(|r| self.find_face_ref(r).ok().map(|(_, bi)| self.bodies[bi].id)).collect();
                self.face_op(faces, |shape, fs| occ::shell(shape, fs, -t))?;
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
                if !(dia > 0.0) {
                    return Err("diameter must be positive".into());
                }
                let size = self.model_size();
                let depth = if *through { size * 2.0 } else { self.len(depth)? };
                if !(depth > 0.0) {
                    return Err("depth must be positive".into());
                }
                let mut tools = vec![];
                for (k, r) in points.iter().enumerate() {
                    let (p, n) = self.hole_point(r)?;
                    let down = -n;
                    // start slightly above the surface so the top face is cut cleanly; a through hole
                    // from inside the part (its face has moved into the material) goes out both ways
                    let back = if *through && self.inside_material(p - down * 0.01) { size * 2.0 } else { 0.01 };
                    let start = p - down * back;
                    let mut tool = Shape::cylinder(start, dia / 2.0, down, depth + back);
                    let id = if let Ref::SketchPoint { point, .. } = r { *point as u64 } else { k as u64 };
                    self.name_hole(fid, &tool, p, down, dia / 2.0, id);
                    match hole_type {
                        HoleType::Simple => {}
                        HoleType::Counterbore => {
                            let cd = self.len(cb_diameter)?;
                            let ch = self.len(cb_depth)?;
                            if !(cd > dia) || !(ch > 0.0) {
                                return Err("the counterbore must be wider than the hole and have a positive depth".into());
                            }
                            let cb = Shape::cylinder(p - down * size, cd / 2.0, down, size + ch);
                            tool = occ::fuse(&tool, &cb)?;
                        }
                        HoleType::Countersink => {
                            let cd = self.len(cs_diameter)?;
                            let ca = self.ang(cs_angle)?.to_radians();
                            if !(cd > dia) || !(ca > 0.0 && ca < std::f64::consts::PI) {
                                return Err("the countersink must be wider than the hole, with an angle between 0° and 180°".into());
                            }
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
            Op::CurvePattern { kind, bodies, path, count, align } => {
                let n = self.count(count)?;
                let pts = self.path_points(path)?;
                // arc-length samples: copies at equal spacing from the start to the end (or around, if closed)
                let mut acc = vec![0.0];
                for w in pts.windows(2) {
                    acc.push(acc.last().unwrap() + (w[1] - w[0]).length());
                }
                let total = *acc.last().unwrap();
                if total < 1e-9 {
                    return Err("the path has no length".into());
                }
                let closed = (pts[0] - *pts.last().unwrap()).length() < 1e-6 * total.max(1.0);
                let at = |s: f64| -> (DVec3, DVec3) {
                    let i = acc.partition_point(|a| *a < s).clamp(1, pts.len() - 1);
                    let (a, b) = (pts[i - 1], pts[i]);
                    let seg = (acc[i] - acc[i - 1]).max(1e-12);
                    let t = ((s - acc[i - 1]) / seg).clamp(0.0, 1.0);
                    (a.lerp(b, t), (b - a).normalize_or_zero())
                };
                let step = if closed { total / n as f64 } else { total / (n.max(2) - 1) as f64 };
                let (p0, t0) = at(0.0);
                let mut xf = vec![];
                for i in 1..n {
                    let (p, t) = at(step * i as f64);
                    let m = if *align { rotation_between(t0, t) } else { DMat3::IDENTITY };
                    // move p0 to p, turning about p
                    xf.push((m, p - m * p0));
                }
                self.pattern(fid, kind, bodies, &xf)
            }
            Op::PointPattern { kind, bodies, points } => {
                let pts: Vec<DVec3> = points.iter().map(|r| self.point_of(r)).collect::<R<_>>()?;
                if pts.len() < 2 {
                    return Err("select at least two sketch points: the first is where the original sits".into());
                }
                let xf: Vec<(DMat3, DVec3)> = pts[1..].iter().map(|p| (DMat3::IDENTITY, *p - pts[0])).collect();
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
                        let (tools, op) = self.tools.get(&fref).ok_or_else(|| self.missing(fref, "that feature has nothing to mirror"))?;
                        let op = *op;
                        let t: Vec<Shape> = tools.iter().map(|t| occ::mirror(t, p, n)).collect::<R<_>>()?;
                        self.apply_tools(fid, t, op)?;
                    }
                    return Ok(());
                }
                // a mirror image that doesn't touch the original is a part of its own
                let mut k = 0;
                for bi in self.pattern_bodies(bodies)? {
                    let m = occ::mirror(&self.bodies[bi].shape, p, n)?;
                    let mut pieces = join_touching(vec![dup(&self.bodies[bi].shape), m])?.into_iter();
                    self.bodies[bi].shape = pieces.next().ok_or("the mirror made nothing")?;
                    for s in pieces {
                        self.bodies.push(Body { id: (fid, k), shape: s });
                        k += 1;
                    }
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
                // a subtraction or intersection that leaves nothing of the part is an error, not an empty part
                if *kind != BooleanKind::Union && solids_of(&shape).is_empty() && !solids_of(&self.bodies[t].shape).is_empty() {
                    return Err(match kind {
                        BooleanKind::Subtract => "the tools cover the whole part: subtracting them leaves nothing".into(),
                        _ => "the parts don't overlap: their intersection is empty".into(),
                    });
                }
                // a union of parts that don't touch would make one part of separate solids: keep them apart
                let pieces = if *kind == BooleanKind::Union { solids_of(&shape) } else { vec![] };
                if pieces.len() > solids_of(&self.bodies[t].shape).len() {
                    return Err("the parts don't touch, so they can't be joined into one part (they are left as they were)".into());
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
            Op::Import { step, format, data, scale, merge, .. } => {
                let scale = self.num(scale)?;
                if !(scale > 0.0) {
                    return Err("scale must be positive".into());
                }
                let shape = imported_shape(step, format, data, scale, *merge)?;
                let solids = solids_of(&shape);
                if solids.is_empty() {
                    // an open mesh or surface model: keep it as a surface body
                    self.add_surface(fid, shape);
                    return Ok(());
                }
                for (i, so) in solids.into_iter().enumerate() {
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
            Ref::Vertex { point } => Ok(v3(*point)),
            // edges and faces are looked up again (by name), not taken at the point stored when
            // they were picked: that goes stale when what is upstream changes
            Ref::Edge { .. } => Ok(naming::edge_mid(&self.find_edge_ref(r)?.0)),
            Ref::Face { point, .. } => {
                let (f, _) = self.find_face_ref(r)?;
                let (p, g) = (v3(*point), face_geom(&f));
                Ok(if g.planar { p - g.normal * (p - g.center).dot(g.normal) } else { occ::face_at(&Shape::from(&f), p).map(|q| q.point).unwrap_or(p) })
            }
            _ => Err("select points".into()),
        }
    }

    /// Start point and tangent of a curve-like reference.
    fn tangent_of(&self, r: &Ref) -> R<(DVec3, DVec3)> {
        let pts: Vec<DVec3> = match r {
            Ref::Edge { .. } => self.find_edge_ref(r)?.0.approximation_segments().collect(),
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

    /// Points along a chain of curves and edges, joined end to end.
    fn path_points(&self, refs: &[Ref]) -> R<Vec<DVec3>> {
        let mut out: Vec<DVec3> = vec![];
        for r in refs {
            let mut pts: Vec<DVec3> = match r {
                Ref::Edge { .. } => self.find_edge_ref(r)?.0.approximation_segments().collect(),
                Ref::Curve { feature } => self.curve_points(*feature)?,
                Ref::SketchCurve { sketch, curve } => {
                    let (sk, fr) = self.sketch(*sketch)?;
                    let c = sk.curve(*curve).ok_or("the curve no longer exists")?;
                    sk.tessellate(c, 0.2).into_iter().map(|p| fr.to_world(p)).collect()
                }
                _ => return Err("select curves or edges for the path".into()),
            };
            if let Some(last) = out.last().copied() {
                // follow on from the nearer end
                if (pts[pts.len() - 1] - last).length() < (pts[0] - last).length() {
                    pts.reverse();
                }
                if (pts[0] - last).length() < 1e-6 {
                    pts.remove(0);
                }
            }
            out.extend(pts);
        }
        if out.len() < 2 {
            return Err("select a path".into());
        }
        Ok(out)
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
                let (tools, op) = self.tools.get(&fref).ok_or_else(|| self.missing(fref, "that feature has nothing to pattern"))?;
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
        // copies that touch the original or each other are joined to it; the others are parts of their own
        let mut k = 0;
        for bi in self.pattern_bodies(refs)? {
            let base = dup(&self.bodies[bi].shape);
            let mut copies = vec![dup(&base)];
            for (m, t) in xf {
                copies.push(occ::transform(&base, *m, *t)?);
            }
            let mut pieces = join_touching(copies)?.into_iter();
            self.bodies[bi].shape = pieces.next().ok_or("the pattern made nothing")?;
            for s in pieces {
                self.bodies.push(Body { id: (fid, k), shape: s });
                k += 1;
            }
        }
        Ok(())
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
                Ref::Edge { .. } => edges.push(self.find_edge_ref(r)?.0),
                _ => return Err("select sketch curves, edges or a helix for the path".into()),
            }
        }
        if edges.is_empty() {
            return Err("select a path".into());
        }
        Ok(Wire::from_unordered_edges(edges.iter(), opencascade::primitives::EdgeConnection::default()))
    }

    /// Apply an edge operation (fillet, chamfer) to the referenced edges, body by body. Edges
    /// that can't be found are left out and named in the error, so one stale reference doesn't
    /// take the others with it.
    fn edge_op(&mut self, fid: FeatId, role: &str, refs: &[Ref], f: impl Fn(&Shape, &[Edge]) -> R<Shape>) -> R<()> {
        if refs.is_empty() {
            return Err("select edges".into());
        }
        let mut by_body: HashMap<usize, Vec<(usize, Edge)>> = HashMap::new();
        let mut keys: HashMap<usize, u64> = HashMap::new();
        let mut missing = vec![];
        for (i, r) in refs.iter().enumerate() {
            if !matches!(r, Ref::Edge { .. }) {
                return Err("select edges".into());
            }
            match self.find_edge_ref(r) {
                Ok((e, bi)) => {
                    keys.insert(i, self.edge_key(r, bi, &e, i));
                    by_body.entry(bi).or_default().push((i, e))
                }
                Err(e) => missing.push(format!("edge {}: {e}", i + 1)),
            }
        }
        let mut bodies: Vec<(usize, Vec<(usize, Edge)>)> = by_body.into_iter().collect();
        bodies.sort_by_key(|b| b.0);
        for (bi, list) in bodies {
            let es: Vec<Edge> = list.iter().map(|(_, e)| Edge::from_edge(&e.inner)).collect();
            match f(&self.bodies[bi].shape, &es) {
                Ok(s) => {
                    let before = std::mem::replace(&mut self.bodies[bi].shape, s);
                    let after = dup(&self.bodies[bi].shape);
                    let keyed: Vec<(usize, u64, Edge)> = list.iter().map(|(i, e)| (*i, keys[i], Edge::from_edge(&e.inner))).collect();
                    self.name_edge_faces(fid, role, &before, &after, &keyed);
                }
                Err(e) => {
                    // find the edges that fail on their own, to say which ones
                    let bad: Vec<String> = list
                        .iter()
                        .filter(|(_, edge)| f(&self.bodies[bi].shape, std::slice::from_ref(edge)).is_err())
                        .map(|(i, edge)| {
                            let m = naming::edge_mid(edge);
                            format!("edge {} near [{:.3}, {:.3}, {:.3}]", i + 1, m.x, m.y, m.z)
                        })
                        .collect();
                    return Err(if bad.is_empty() { format!("{e} (each edge works alone; try fewer edges at once)") } else { format!("{e}; failing: {}", bad.join(", ")) });
                }
            }
        }
        if !missing.is_empty() {
            let done = refs.len() - missing.len();
            return Err(format!("{} of {} edges could not be found and were left out ({} done): {}", missing.len(), refs.len(), done, missing.join("; ")));
        }
        Ok(())
    }

    /// Apply a face operation to the referenced faces, body by body; faces that can't be
    /// found are left out and named in the error.
    fn face_op(&mut self, refs: &[Ref], f: impl Fn(&Shape, &[Face]) -> R<Shape>) -> R<()> {
        if refs.is_empty() {
            return Err("select faces".into());
        }
        let mut by_body: HashMap<usize, Vec<Face>> = HashMap::new();
        let mut missing = vec![];
        for (i, r) in refs.iter().enumerate() {
            match self.find_face_ref(r) {
                Ok((face, bi)) => by_body.entry(bi).or_default().push(face),
                Err(e) => missing.push(format!("face {}: {e}", i + 1)),
            }
        }
        for (bi, fs) in by_body {
            self.bodies[bi].shape = f(&self.bodies[bi].shape, &fs)?;
        }
        if !missing.is_empty() {
            return Err(format!("{} of {} faces could not be found and were left out: {}", missing.len(), refs.len(), missing.join("; ")));
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
            Some("the kernel crashed on this feature; it stays skipped until you change it or what it depends on".to_string())
        } else {
            progress(f.id);
            let t = std::time::Instant::now();
            m.current.set(f.id);
            let e = m.run(f.id).err();
            m.current.set(0);
            if std::env::var("CAD_TIME").is_ok() {
                eprintln!("feature {} ({}) {:?}", f.id, f.op.kind(), t.elapsed());
            }
            e
        };
        out.features.push(FeatureStatus { id: f.id, error });
    }
    out.frames.extend(m.frames.iter().map(|(k, v)| (*k, *v)));
    // sketches whose plane is gone still show where they were (they carry the error)
    for f in &doc.features {
        if let Op::Sketch { frame, .. } = &f.op {
            out.frames.entry(f.id).or_insert(*frame);
        }
    }
    out.refs = m.updates.take();
    for (i, b) in m.bodies.iter().enumerate() {
        let valid = unique_faces(&b.shape).len() >= 1500 || occ::is_valid(&b.shape);
        if let Ok(mut mesh) = mesh_body(&b.shape) {
            mesh.surface = solids_of(&b.shape).is_empty();
            mesh.id = b.id;
            out.issues.extend(check_body(&b.shape, &mesh, valid));
            mesh.name = if mesh.surface { format!("Surface {}", i + 1) } else { format!("Part {}", i + 1) };
            // face names, for what refers to the part from outside the Part Studio (mate connectors)
            if mesh.faces.len() <= 600 {
                for (f, info) in unique_faces(&b.shape).iter().zip(mesh.faces.iter_mut()) {
                    info.name = m.face_name_of(f);
                }
            }
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
        // "stl@0.05": STL with that mesh tolerance (mm)
        f if f.starts_with("stl@") => {
            let tol: f64 = f[4..].parse().map_err(|_| "bad STL tolerance")?;
            occ::clean_mesh(&shape);
            shape.write_stl_with_tolerance(path, tol.clamp(0.001, 1.0)).map_err(|e| format!("{e:?}"))
        }
        "iges" => shape.write_iges(path).map_err(|e| format!("{e:?}")),
        _ => Err("unknown format".into()),
    }
}

// ------------------------------------------------------------------ import

/// Imported files are parsed once per worker process and reused on every rebuild.
static IMPORTS: std::sync::Mutex<Option<HashMap<u64, Shape>>> = std::sync::Mutex::new(None);

fn imported_shape(step: &str, format: &str, data: &str, scale: f64, merge: bool) -> R<Shape> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (step, format, data, scale.to_bits(), merge).hash(&mut h);
    let key = h.finish();
    if let Some(s) = IMPORTS.lock().ok().and_then(|m| m.as_ref().and_then(|m| m.get(&key)).map(dup)) {
        return Ok(s);
    }
    let bytes = if data.is_empty() { step.as_bytes().to_vec() } else { crate::import::unpack(data)? };
    let format = if !format.is_empty() {
        format.to_string()
    } else {
        // older documents: STEP or IGES text (IGES has a section letter in column 73)
        let iges = String::from_utf8_lossy(&bytes).lines().next().map(|l| l.len() >= 73 && l.as_bytes()[72] == b'S').unwrap_or(false);
        if iges { "iges" } else { "step" }.to_string()
    };
    let shape = match format.as_str() {
        "step" | "iges" => {
            let path = import_temp_file(&bytes, if format == "iges" { "igs" } else { "step" })?;
            let s = if format == "iges" { Shape::read_iges(&path) } else { Shape::read_step(&path) }.map_err(|e| format!("could not read the {} file: {e:?}", format.to_uppercase()));
            let _ = std::fs::remove_file(&path);
            let s = s?;
            if (scale - 1.0).abs() > 1e-12 { occ::transform(&s, DMat3::from_diagonal(DVec3::splat(scale)), DVec3::ZERO)? } else { s }
        }
        "stl" | "obj" => {
            let m = crate::import::read_mesh(&format, &bytes, scale)?;
            occ::mesh_solid(&m.pts, &m.tris, merge)?
        }
        f => return Err(format!("can't import {f} files")),
    };
    if let Ok(mut m) = IMPORTS.lock() {
        let m = m.get_or_insert_with(HashMap::new);
        if m.len() > 16 {
            m.clear();
        }
        m.insert(key, dup(&shape));
    }
    Ok(shape)
}

/// Write `bytes` to a new file of its own in the temp folder for OCCT to read: created
/// exclusively under a random name, so it never follows a link planted there or meets another
/// reader's file.
fn import_temp_file(bytes: &[u8], ext: &str) -> R<std::path::PathBuf> {
    use std::hash::{BuildHasher, Hasher};
    use std::io::Write;
    let mut last = String::new();
    for _ in 0..16 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        let path = std::env::temp_dir().join(format!("cad-rust-import-{}-{:016x}.{ext}", std::process::id(), h.finish()));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(bytes) {
                    let _ = std::fs::remove_file(&path);
                    return Err(e.to_string());
                }
                return Ok(path);
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(format!("could not make a temporary file for the import: {last}"))
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
    if tools.len() == 1 {
        return Ok(tools.remove(0));
    }
    // all in one boolean: fusing one at a time takes time quadratic in the number of tools
    let all: Vec<&Shape> = tools.iter().collect();
    if let Ok(s) = occ::boolean_n(&all[..1], &all[1..], 0, 0.0, true) {
        if solids_of(&s).len() <= tools.len() && occ::is_valid(&s) {
            return Ok(s);
        }
    }
    let mut acc = tools.remove(0);
    for t in tools {
        acc = occ::fuse(&acc, &t)?;
    }
    Ok(acc)
}

/// Group shapes that touch or overlap and fuse each group (in one boolean); the group with the
/// first shape comes first, with the solid holding it first.
fn join_touching(shapes: Vec<Shape>) -> R<Vec<Shape>> {
    let n = shapes.len();
    let boxes: Vec<_> = shapes.iter().map(opencascade::bounding_box::aabb).collect();
    let tol = 1e-6;
    let apart = |a: usize, b: usize| boxes[a].min().cmpgt(boxes[b].max() + tol).any() || boxes[b].min().cmpgt(boxes[a].max() + tol).any();
    // union-find over boxes that overlap (sort-and-sweep along x keeps large patterns fast)
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| boxes[a].min().x.total_cmp(&boxes[b].min().x));
    for (k, &a) in order.iter().enumerate() {
        for &b in &order[k + 1..] {
            if boxes[b].min().x > boxes[a].max().x + tol {
                break;
            }
            if !apart(a, b) {
                let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
                parent[ra] = rb;
            }
        }
    }
    let mut groups: Vec<Vec<usize>> = vec![];
    let mut group_of: HashMap<usize, usize> = HashMap::new();
    for i in 0..n {
        let r = root(&mut parent, i);
        let g = *group_of.entry(r).or_insert_with(|| {
            groups.push(vec![]);
            groups.len() - 1
        });
        groups[g].push(i);
    }
    let mut slots: Vec<Option<Shape>> = shapes.into_iter().map(Some).collect();
    let mut out = vec![];
    for g in groups {
        if g.len() == 1 {
            out.push(slots[g[0]].take().unwrap());
            continue;
        }
        let first = g[0];
        let probe = centroid(slots[first].as_ref().unwrap());
        let members: Vec<Shape> = g.iter().map(|&i| slots[i].take().unwrap()).collect();
        let mut solids = solids_of(&fuse_all(members)?);
        // the solid holding the group's first shape leads
        if let Some(i) = solids.iter().position(|s| occ::classify(s, &[probe], 1e-6).map(|v| v[0] != 1).unwrap_or(false)) {
            let s = solids.remove(i);
            solids.insert(0, s);
        }
        out.extend(solids);
    }
    Ok(out)
}

/// The holes (inner boundaries) of a face.
fn hole_wires(face: &Face) -> Vec<Wire> {
    let outer = face.outer_wire();
    sub_shapes(&Shape::from(face), occ::WIRE)
        .into_iter()
        .filter(|w| !ffi::extra::ex_is_same(&w.inner, occ::wire_shape(&outer)))
        .map(|w| Wire::from_wire(ffi::topo_ds::TopoDS::Wire(&w.inner)))
        .collect()
}

fn wire_center(w: &Wire) -> DVec3 {
    let b = opencascade::bounding_box::aabb(&Shape::from(w));
    (b.min() + b.max()) / 2.0
}

/// Hole `k` of the first profile and, in each other profile, the hole sitting at the same
/// place relative to its outer boundary.
fn matching_holes(outers: &[Wire], holes: &[Vec<Wire>], k: usize) -> Vec<Wire> {
    let rel = |i: usize, w: &Wire| wire_center(w) - wire_center(&outers[i]);
    let want = rel(0, &holes[0][k]);
    holes
        .iter()
        .enumerate()
        .map(|(i, hs)| {
            let best = if i == 0 { k } else { (0..hs.len()).min_by(|&a, &b| (rel(i, &hs[a]) - want).length().total_cmp(&(rel(i, &hs[b]) - want).length())).unwrap_or(0) };
            Wire::from_wire(&hs[best].inner)
        })
        .collect()
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

/// Rotation turning direction `a` onto `b` (the shortest way).
fn rotation_between(a: DVec3, b: DVec3) -> DMat3 {
    let (a, b) = (a.normalize_or_zero(), b.normalize_or_zero());
    let c = a.cross(b);
    if c.length() < 1e-12 {
        if a.dot(b) > 0.0 {
            return DMat3::IDENTITY;
        }
        // opposite: half a turn about any perpendicular
        let p = if a.x.abs() < 0.9 { a.cross(DVec3::X) } else { a.cross(DVec3::Y) }.normalize();
        return DMat3::from_axis_angle(p, std::f64::consts::PI);
    }
    DMat3::from_axis_angle(c.normalize(), a.angle_between(b))
}

pub fn volume(s: &Shape) -> f64 {
    // faceted shapes (imported meshes): add up the triangles, far faster than exact integration
    let faces = unique_faces(s);
    if faces.len() >= 1500 {
        return faces.iter().flat_map(face_tris).map(|[a, b, c]| a.dot(b.cross(c)) / 6.0).sum();
    }
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

/// Distinct sub-shapes of a kind (occ::SOLID, FACE, EDGE, …) in a stable order (the one `occ::edge_faces` uses).
pub fn sub_shapes(s: &Shape, kind: i32) -> Vec<Shape> {
    use ffi::top_abs::TopAbs_ShapeEnum as K;
    let k = match kind {
        occ::SOLID => K::TopAbs_SOLID,
        occ::FACE => K::TopAbs_FACE,
        occ::WIRE => K::TopAbs_WIRE,
        occ::EDGE => K::TopAbs_EDGE,
        _ => K::TopAbs_VERTEX,
    };
    explore(s, k).iter().map(|x| Shape::from_shape(x)).collect()
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
    let m = mesh_body_level(shape, 0)?;
    if m.faces.len() >= 1500 || m.surface || mesh_open_edges(&m).is_empty() {
        return Ok(m);
    }
    // the mesher left gaps (it struggles with tiny edges): try more careful settings
    for level in 1..=3 {
        if let Ok(m2) = mesh_body_level(shape, level) {
            if mesh_open_edges(&m2).is_empty() {
                return Ok(m2);
            }
        }
    }
    Ok(m)
}

/// Edges of a mesh used by only one triangle (or unevenly), as welded vertex pairs.
pub fn mesh_open_edges(m: &BodyMesh) -> Vec<([f64; 3], [f64; 3])> {
    let key = |i: u32| {
        let p = &m.positions[3 * i as usize..3 * i as usize + 3];
        [(p[0] as f64 * 1e4).round() as i64, (p[1] as f64 * 1e4).round() as i64, (p[2] as f64 * 1e4).round() as i64]
    };
    let mut count: HashMap<([i64; 3], [i64; 3]), i32> = HashMap::new();
    for t in m.indices.chunks(3) {
        for k in 0..3 {
            let (a, b) = (key(t[k]), key(t[(k + 1) % 3]));
            if a != b {
                *count.entry((a, b)).or_default() += 1;
                *count.entry((b, a)).or_default() -= 1;
            }
        }
    }
    let f = |a: [i64; 3]| a.map(|v| v as f64 / 1e4);
    count.into_iter().filter(|(_, v)| *v > 0).map(|((a, b), _)| (f(a), f(b))).collect()
}

fn mesh_body_level(shape: &Shape, level: i32) -> R<BodyMesh> {
    let bb = opencascade::bounding_box::aabb(shape);
    let diag = (bb.max() - bb.min()).length().max(1.0);
    let tol = (diag * 0.0008).clamp(0.005, 0.5);
    let tt = std::time::Instant::now();
    let dbg = std::env::var("CAD_TIME").is_ok();
    // the angular limit keeps small and large curved faces smooth alike (~8.6°)
    let meshed = if level == 0 { ffi::extra::ex_mesh(&shape.inner, tol, 0.15) } else { ffi::extra::ex_mesh_careful(&shape.inner, tol, 0.15, level) };
    if !meshed {
        return Err("meshing failed".into());
    }
    if dbg { eprintln!("  mesh {:?}", tt.elapsed()); }
    let nfaces = unique_faces(shape).len();
    let faceted = nfaces >= 1500;
    // faceted shapes: mass properties come from the (exact) triangles below, much faster
    let mut out = if faceted {
        BodyMesh { bbox: [bb.min().into(), bb.max().into()], ..Default::default() }
    } else {
        let inertia = occ::mass_props(shape, 0).map(|m| m.2.to_cols_array()).unwrap_or_default();
        BodyMesh { bbox: [bb.min().into(), bb.max().into()], volume: volume(shape), area: surface_area(shape), centroid: centroid(shape).into(), inertia, ..Default::default() }
    };
    if dbg { eprintln!("  props {:?}", tt.elapsed()); }
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
        let g = face_geom(&face);
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
        if g.planar {
            // flat faces: one normal, so thin slivers and fans shade like the rest of the face
            normals.iter_mut().for_each(|n| *n = g.normal);
        } else {
            // nodes repeated along a seam get the same normal, so the seam doesn't show as a band
            let key = |p: DVec3| ((p.x * 1e4).round() as i64, (p.y * 1e4).round() as i64, (p.z * 1e4).round() as i64);
            let mut sum: HashMap<(i64, i64, i64), DVec3> = HashMap::new();
            for (v, n) in verts.iter().zip(&normals) {
                *sum.entry(key(*v)).or_default() += *n;
            }
            for (v, n) in verts.iter().zip(normals.iter_mut()) {
                *n = sum[&key(*v)];
            }
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
        out.faces.push(FaceInfo { center: g.center.into(), normal: g.normal.into(), planar: g.planar, area: g.area, name: None });
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
    if dbg { eprintln!("  faces {:?}", tt.elapsed()); }
    // faceted shapes (imported meshes) only show edges where the surface turns by 20° or more
    let shown = match ffi::extra::ex_feature_edges(&shape.inner, 20f64.to_radians(), 1500) {
        p if !p.is_null() && !p.IsNull() => unique_edges(&Shape::from_shape(&p)),
        _ => visible_edges(shape),
    };
    if faceted {
        let p = |i: u32| DVec3::new(out.positions[3 * i as usize] as f64, out.positions[3 * i as usize + 1] as f64, out.positions[3 * i as usize + 2] as f64);
        let (mut vol, mut area, mut c) = (0.0, 0.0, DVec3::ZERO);
        for t in out.indices.chunks(3) {
            let (a, b, cc) = (p(t[0]), p(t[1]), p(t[2]));
            let v = a.dot(b.cross(cc)) / 6.0;
            vol += v;
            c += (a + b + cc) / 4.0 * v;
            area += (b - a).cross(cc - a).length() / 2.0;
        }
        out.volume = vol;
        out.area = area;
        out.centroid = if vol.abs() > 1e-12 { (c / vol).into() } else { [0.0; 3] };
    }
    for e in shown {
        // straight edges need just their ends (fast for faceted shapes with many edges)
        let pts: Vec<DVec3> = if faceted && e_is_line(&e) { vec![e.start_point(), e.end_point()] } else { e.approximation_segments().collect() };
        if pts.len() < 2 {
            continue;
        }
        let length: f64 = pts.windows(2).map(|w| (w[1] - w[0]).length()).sum();
        // references to the edge keep this point
        let mid = polyline_mid(&pts);
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

/// The middle of a polyline by arc length.
pub fn polyline_mid(pts: &[DVec3]) -> DVec3 {
    let length: f64 = pts.windows(2).map(|w| (w[1] - w[0]).length()).sum();
    let mut acc = 0.0;
    for w in pts.windows(2) {
        let l = (w[1] - w[0]).length();
        if acc + l >= length / 2.0 {
            return w[0].lerp(w[1], ((length / 2.0 - acc) / l.max(1e-12)).clamp(0.0, 1.0));
        }
        acc += l;
    }
    pts.first().copied().unwrap_or(DVec3::ZERO)
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
        let top = Ref::Face { point: [20.0, 10.0, 10.0], normal: [0.0, 0.0, 1.0], name: Default::default() };
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
            edges.push(Ref::Edge { point: [20.0, 0.0, 10.0], name: Default::default() });
            *radius = "3".into();
        }
        doc.insert("Fillet".into(), f);
        let mut sh = Op::new_default("shell").unwrap();
        if let Op::Shell { faces, thickness } = &mut sh {
            faces.push(Ref::Face { point: [30.0, 15.0, 0.0], normal: [0.0, 0.0, -1.0], name: Default::default() });
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
        // the spaced copies are parts of their own; the mirror image touches the original and joins it
        assert_eq!(r.bodies.len(), 3);
        assert!((r.bodies[0].volume - 2000.0).abs() < 1e-3, "{}", r.bodies[0].volume);
        assert!(r.bodies[1..].iter().all(|b| (b.volume - 1000.0).abs() < 1e-3));
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
    fn bodies_split_twice_keep_distinct_ids() {
        let mut doc = Document::default();
        let s = sketch_rect(&mut doc, 0.0, 0.0, 60.0, 40.0);
        extrude(&mut doc, s, [5.0, 5.0], "10", BoolOp::New);
        // a slot across splits the plate in two, then one along splits both halves
        let c1 = sketch_rect(&mut doc, 28.0, -5.0, 4.0, 50.0);
        extrude(&mut doc, c1, [30.0, 0.0], "10", BoolOp::Remove);
        let c2 = sketch_rect(&mut doc, -5.0, 18.0, 70.0, 4.0);
        extrude(&mut doc, c2, [0.0, 20.0], "10", BoolOp::Remove);
        let r = run(&doc);
        assert_eq!(r.bodies.len(), 4);
        let ids: std::collections::HashSet<BodyId> = r.bodies.iter().map(|b| b.id).collect();
        assert_eq!(ids.len(), 4, "{:?}", r.bodies.iter().map(|b| b.id).collect::<Vec<_>>());
    }

    fn error_of(r: &RebuildResult, id: FeatId) -> String {
        r.features.iter().find(|f| f.id == id).and_then(|f| f.error.clone()).unwrap_or_default()
    }

    #[test]
    fn booleans_and_extrudes_that_make_nothing_say_so() {
        let mut doc = Document::default();
        let s = sketch_rect(&mut doc, 0.0, 0.0, 10.0, 10.0);
        let small = extrude(&mut doc, s, [5.0, 5.0], "10", BoolOp::New);
        let s = sketch_rect(&mut doc, -5.0, -5.0, 20.0, 20.0);
        let big = extrude(&mut doc, s, [-4.0, -4.0], "20", BoolOp::New);
        let mut b = Op::new_default("boolean").unwrap();
        if let Op::Boolean { kind, target, tools, .. } = &mut b {
            *kind = BooleanKind::Subtract;
            *target = Some(Ref::Body { body: (small, 0) });
            tools.push(Ref::Body { body: (big, 0) });
        }
        let bid = doc.insert("B".into(), b);
        let r = rebuild(&doc, &[], |_| {});
        assert!(error_of(&r, bid).contains("leaves nothing"), "{:?}", error_of(&r, bid));

        // up to a face behind the profile: a block at z -20..-15, extruded to from the top plane
        let mut doc = Document::default();
        let mut pl = Op::new_default("plane").unwrap();
        if let Op::Plane { base, offset, .. } = &mut pl {
            *base = Some(Ref::OriginPlane { plane: OriginPlane::Top });
            *offset = "-20".into();
        }
        let p = doc.insert("P".into(), pl);
        let mut sk = Sketch::default();
        let q: Vec<_> = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)].iter().map(|&(x, y)| sk.add_point(DVec2::new(x, y))).collect();
        for i in 0..4 {
            sk.add_curve(Geom::Line { a: q[i], b: q[(i + 1) % 4] }, false);
        }
        let low = doc.insert("S".into(), Op::Sketch { plane: Ref::FeaturePlane { feature: p }, sketch: sk, frame: Frame::TOP });
        extrude(&mut doc, low, [5.0, 5.0], "5", BoolOp::New);
        let s = sketch_rect(&mut doc, 0.0, 0.0, 10.0, 10.0);
        let e = extrude(&mut doc, s, [5.0, 5.0], "10", BoolOp::New);
        if let Some(Op::Extrude { end, up_to, .. }) = doc.feature_mut(e).map(|f| &mut f.op) {
            *end = EndType::UpToFace;
            *up_to = Some(Ref::Face { point: [5.0, 5.0, -15.0], normal: [0.0, 0.0, 1.0], name: Default::default() });
        }
        let r = rebuild(&doc, &[], |_| {});
        assert!(error_of(&r, e).contains("made nothing"), "{:?}", error_of(&r, e));
    }

    #[test]
    fn up_to_a_vertex_below_the_profile_extrudes_down() {
        let mut doc = Document::default();
        let s = sketch_rect(&mut doc, 0.0, 0.0, 10.0, 10.0);
        let e = extrude(&mut doc, s, [5.0, 5.0], "1", BoolOp::New);
        if let Some(Op::Extrude { end, up_to, .. }) = doc.feature_mut(e).map(|f| &mut f.op) {
            *end = EndType::UpToVertex;
            *up_to = Some(Ref::Vertex { point: [20.0, 20.0, -7.0] });
        }
        let r = run(&doc);
        assert!((r.bodies[0].volume - 700.0).abs() < 1e-6, "{}", r.bodies[0].volume);
        assert!((r.bodies[0].centroid[2] + 3.5).abs() < 1e-6, "{:?}", r.bodies[0].centroid);
    }

    #[test]
    fn draft_thicken_delete_face() {
        let mut doc = Document::default();
        let s = sketch_rect(&mut doc, 0.0, 0.0, 20.0, 20.0);
        extrude(&mut doc, s, [5.0, 5.0], "20", BoolOp::New);
        let mut d = Op::new_default("draft").unwrap();
        if let Op::Draft { faces, neutral, angle } = &mut d {
            faces.push(Ref::Face { point: [10.0, 0.0, 10.0], normal: [0.0, -1.0, 0.0], name: Default::default() });
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
            faces.push(Ref::Face { point: [25.0, 20.0, 2.5], normal: [0.0; 3], name: Default::default() });
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
            *base = Some(Ref::Face { point: [50.0, 10.0, 10.0], normal: [0.0, 0.0, 1.0], name: Default::default() });
            refs.push(Ref::Face { point: [50.0, 10.0, 0.0], normal: [0.0, 0.0, -1.0], name: Default::default() });
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
    /// Visible tangent edges (where faces join smoothly, e.g. fillets), drawn thin.
    #[serde(default)]
    pub smooth: Vec<Vec<[f64; 2]>>,
    /// Every model edge projected, visible or not, with its identity, so dimensions can be
    /// attached to model geometry and follow it when the model changes.
    #[serde(default)]
    pub edges: Vec<ProjEdge>,
    /// Holes made by Hole features, for callouts.
    #[serde(default)]
    pub holes: Vec<ProjHole>,
    /// Section views: where the cutting plane went (p·dir, mm).
    #[serde(default)]
    pub cut: Option<f64>,
    /// Assembly views: a point on each item (its centroid), for balloons.
    #[serde(default)]
    pub items: Vec<[f64; 2]>,
}

/// A model edge seen in a view.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ProjEdge {
    /// The part: its body id in a Part Studio view, (item index, 0) in an assembly view.
    pub part: BodyId,
    /// Index of the edge in the part (OpenCascade's edge order, stable while the topology is).
    pub index: u32,
    /// 0 straight, 1 circle or arc, 2 other.
    pub kind: u8,
    pub pts: Vec<[f64; 2]>,
    /// A circle or arc seen square on: centre x, y and radius in the view.
    #[serde(default)]
    pub circle: Option<[f64; 3]>,
    /// A circle seen edge on: its centre and its axis direction in the view.
    #[serde(default)]
    pub axis: Option<[f64; 4]>,
}

/// A hole from a Hole feature, as seen in a view.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ProjHole {
    pub feature: FeatId,
    /// Where the hole starts, in the view.
    pub at: [f64; 2],
    /// The hole's axis is along the view direction (it shows as circles).
    pub square: bool,
    pub dia: f64,
    pub through: bool,
    pub depth: f64,
    /// 0 simple, 1 counterbore, 2 countersink.
    pub kind: u8,
    /// Counterbore or countersink diameter.
    pub dia2: f64,
    /// Counterbore depth, or countersink angle in degrees.
    pub v2: f64,
}

/// One projection to make: a view direction (towards the viewer) and up, and what to show.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ProjReq {
    pub dir: [f64; 3],
    pub up: [f64; 3],
    #[serde(default)]
    pub section: bool,
    /// Section cutting plane as p·dir (mm); None cuts through the middle.
    #[serde(default)]
    pub cut: Option<f64>,
    /// Only these parts of the Part Studio (all when empty).
    #[serde(default)]
    pub bodies: Vec<BodyId>,
    /// An assembly: (Part Studio, body, row-major 3x4 pose) per part, from the workspace's
    /// Part Studios (see STUDIOS); the document is not used.
    #[serde(default)]
    pub items: Vec<(usize, BodyId, [f64; 12])>,
}

impl From<([f64; 3], [f64; 3], bool)> for ProjReq {
    fn from((dir, up, section): ([f64; 3], [f64; 3], bool)) -> Self {
        ProjReq { dir, up, section, ..Default::default() }
    }
}

/// A hole of a Hole feature in model coordinates.
struct HoleSpec {
    feature: FeatId,
    p: DVec3,
    down: DVec3,
    dia: f64,
    through: bool,
    depth: f64,
    kind: u8,
    dia2: f64,
    v2: f64,
}

/// Build the document, noting the holes its Hole features make.
fn build_with_holes(doc: &Document) -> (Vec<Body>, Vec<HoleSpec>) {
    let mut m = Model::new(doc);
    let mut holes = vec![];
    let end = doc.rollback_index();
    for (i, f) in doc.features.iter().enumerate() {
        if i >= end || f.suppressed || m.run(f.id).is_err() {
            continue;
        }
        if let Op::Hole { points, hole_type, diameter, depth, through, cb_diameter, cb_depth, cs_diameter, cs_angle, .. } = &f.op {
            let (kind, dia2, v2) = match hole_type {
                HoleType::Simple => (0, 0.0, 0.0),
                HoleType::Counterbore => (1, m.len(cb_diameter).unwrap_or(0.0), m.len(cb_depth).unwrap_or(0.0)),
                HoleType::Countersink => (2, m.len(cs_diameter).unwrap_or(0.0), m.ang(cs_angle).unwrap_or(90.0)),
            };
            let (Ok(dia), d) = (m.len(diameter), if *through { 0.0 } else { m.len(depth).unwrap_or(0.0) }) else { continue };
            for r in points {
                if let Ok((p, n)) = m.hole_point(r) {
                    holes.push(HoleSpec { feature: f.id, p, down: -n, dia, through: *through, depth: d, kind, dia2, v2 });
                }
            }
        }
    }
    (m.bodies, holes)
}

fn polylines_2d(s: &Shape) -> Vec<Vec<[f64; 2]>> {
    unique_edges(s).iter().map(|e| e.approximation_segments().map(|p| [p.x, p.y]).collect::<Vec<_>>()).filter(|p| p.len() >= 2).collect()
}

/// Hidden-line projections of a Part Studio's parts (or an assembly's), one per request.
/// Section views cut the parts square to the view (through the middle unless a cut is given)
/// and hatch the cut.
pub fn project(doc: &Document, views: &[ProjReq]) -> Vec<ProjView> {
    let mut built: Option<(Vec<Body>, Vec<HoleSpec>)> = None;
    views
        .iter()
        .map(|req| {
            // the parts to show, each with an identity for dimension anchors
            let mut parts: Vec<(BodyId, Shape)> = vec![];
            let mut holes: Vec<(DVec3, DVec3, &HoleSpec)> = vec![];
            if req.items.is_empty() {
                let (bodies, specs) = built.get_or_insert_with(|| build_with_holes(doc));
                for b in bodies.iter().filter(|b| req.bodies.is_empty() || req.bodies.contains(&b.id)) {
                    parts.push((b.id, dup(&b.shape)));
                }
                // holes in the parts shown
                for h in specs.iter() {
                    if req.bodies.is_empty() || bodies.iter().filter(|b| req.bodies.contains(&b.id)).any(|b| aabb_contains(&b.shape, h.p, 0.5)) {
                        holes.push((h.p, h.down, h));
                    }
                }
            } else {
                let studios = STUDIOS.lock().map(|s| s.clone()).unwrap_or_default();
                let mut cache: HashMap<usize, Vec<Body>> = HashMap::new();
                for (k, (studio, body, m)) in req.items.iter().enumerate() {
                    let bs = cache.entry(*studio).or_insert_with(|| studios.get(*studio).map(build_bodies).unwrap_or_default());
                    let Some(b) = bs.iter().find(|b| b.id == *body) else { continue };
                    let rot = DMat3::from_cols(DVec3::new(m[0], m[4], m[8]), DVec3::new(m[1], m[5], m[9]), DVec3::new(m[2], m[6], m[10]));
                    if let Ok(s) = occ::transform(&b.shape, rot, DVec3::new(m[3], m[7], m[11])) {
                        parts.push(((k as u64, 0), s));
                    }
                }
            }
            project_parts(parts, &holes, req)
        })
        .collect()
}

fn aabb_contains(s: &Shape, p: DVec3, margin: f64) -> bool {
    let bb = opencascade::bounding_box::aabb(s);
    (p - bb.min()).min_element() > -margin && (bb.max() - p).min_element() > -margin
}

fn project_parts(parts: Vec<(BodyId, Shape)>, holes: &[(DVec3, DVec3, &HoleSpec)], req: &ProjReq) -> ProjView {
    let (d, u) = (DVec3::from(req.dir).normalize_or(DVec3::Z), DVec3::from(req.up).normalize_or(DVec3::Y));
    let vx = u.cross(d).normalize_or(DVec3::X);
    let vy = d.cross(vx);
    let to2 = |p: DVec3| [p.dot(vx), p.dot(vy)];
    let mut v = ProjView::default();
    let shapes: Vec<&Shape> = parts.iter().map(|p| &p.1).collect();
    let Ok(all) = occ::compound(&shapes) else { return v };
    // the item centroids (bounding box centres) for balloons
    if !req.items.is_empty() {
        v.items = parts.iter().map(|(_, s)| {
            let bb = opencascade::bounding_box::aabb(s);
            to2((bb.min() + bb.max()) / 2.0)
        }).collect();
    }
    let mut parts = parts;
    if req.section {
        let bb = opencascade::bounding_box::aabb(&all);
        let size = (bb.max() - bb.min()).length() * 2.0 + 10.0;
        let mid = (bb.min() + bb.max()) / 2.0;
        let cut = req.cut.unwrap_or(mid.dot(d));
        let c = mid + d * (cut - mid.dot(d));
        v.cut = Some(cut);
        // remove the half towards the viewer, part by part so each keeps its identity
        if let Ok(h) = half_space(c, d, size) {
            for p in parts.iter_mut() {
                if let Ok(cut) = occ::cut(&p.1, &h) {
                    p.1 = cut;
                }
            }
        }
        // hatch the faces lying on the cutting plane
        for (_, s) in &parts {
            for f in unique_faces(s) {
                let g = face_geom(&f);
                if !(g.planar && g.normal.dot(d) > 0.999 && (g.center - c).dot(d).abs() < 1e-4 * size.max(1.0)) {
                    continue;
                }
                let tris: Vec<[glam::DVec2; 3]> = face_tris(&f).iter().map(|t| t.map(|p| glam::DVec2::from(to2(p)))).collect();
                v.hatch.extend(hatch_triangles(&tris, 2.0));
            }
        }
    }
    parts.retain(|p| !unique_faces(&p.1).is_empty());
    let shapes: Vec<&Shape> = parts.iter().map(|p| &p.1).collect();
    let Ok(shape) = occ::compound(&shapes) else { return v };
    // one hidden-line pass over every part together, so parts hide each other
    if let Ok(s) = occ::hlr(&shape, d, u, 0) {
        v.visible.extend(polylines_2d(&s));
    }
    if let Ok(s) = occ::hlr(&shape, d, u, 2) {
        v.visible.extend(polylines_2d(&s));
    }
    if let Ok(s) = occ::hlr(&shape, d, u, 1) {
        v.smooth.extend(polylines_2d(&s));
    }
    for kind in [3, 4] {
        if let Ok(s) = occ::hlr(&shape, d, u, kind) {
            v.hidden.extend(polylines_2d(&s));
        }
    }
    // a hidden edge lying under a visible one is not shown
    v.hidden = drop_covered(&v.hidden, &v.visible, 0.02);
    // model edges with their identity
    for (part, s) in &parts {
        for (i, e) in unique_edges(s).iter().enumerate() {
            let pts3: Vec<DVec3> = e.approximation_segments().collect();
            if pts3.len() < 2 {
                continue;
            }
            let ty = e.edge_type();
            let kind = match ty {
                opencascade::primitives::EdgeType::Line => 0,
                opencascade::primitives::EdgeType::Circle => 1,
                _ => 2,
            };
            let mut pe = ProjEdge { part: *part, index: i as u32, kind, pts: pts3.iter().map(|p| to2(*p)).collect(), circle: None, axis: None };
            if kind == 1 && pts3.len() >= 3 {
                if let Some((c, n)) = circle_from_points(&pts3) {
                    let r = (pts3[0] - c).length();
                    let c2 = to2(c);
                    if n.dot(d).abs() > 0.999 {
                        pe.circle = Some([c2[0], c2[1], r]);
                    } else if n.dot(d).abs() < 1e-3 {
                        let a = to2(n);
                        pe.axis = Some([c2[0], c2[1], a[0], a[1]]);
                    }
                }
            }
            v.edges.push(pe);
        }
    }
    for (p, down, h) in holes {
        v.holes.push(ProjHole {
            feature: h.feature,
            at: to2(*p),
            square: down.dot(d).abs() > 0.999,
            dia: h.dia,
            through: h.through,
            depth: h.depth,
            kind: h.kind,
            dia2: h.dia2,
            v2: h.v2,
        });
    }
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for p in v.visible.iter().chain(v.smooth.iter()).flatten() {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    v.bbox = if lo[0].is_finite() { [lo, hi] } else { [[0.0; 2]; 2] };
    v
}

/// The parts of `lines` not lying (within `tol`) on any of `cover`, as polylines.
fn drop_covered(lines: &[Vec<[f64; 2]>], cover: &[Vec<[f64; 2]>], tol: f64) -> Vec<Vec<[f64; 2]>> {
    use glam::DVec2;
    // bucket the covering segments on a grid
    let cell = 2.0;
    let key = |p: DVec2| ((p.x / cell).floor() as i64, (p.y / cell).floor() as i64);
    let mut grid: HashMap<(i64, i64), Vec<(DVec2, DVec2)>> = HashMap::new();
    for pl in cover {
        for w in pl.windows(2) {
            let (a, b) = (DVec2::from(w[0]), DVec2::from(w[1]));
            let (k0, k1) = (key(a.min(b) - DVec2::splat(tol)), key(a.max(b) + DVec2::splat(tol)));
            if (k1.0 - k0.0 + 1) * (k1.1 - k0.1 + 1) > 4096 {
                // a very long segment: put it in the cells along it
                let n = ((b - a).length() / cell).ceil() as usize + 1;
                for i in 0..=n {
                    let p = a.lerp(b, i as f64 / n as f64);
                    grid.entry(key(p)).or_default().push((a, b));
                }
                continue;
            }
            for x in k0.0..=k1.0 {
                for y in k0.1..=k1.1 {
                    grid.entry((x, y)).or_default().push((a, b));
                }
            }
        }
    }
    let covered = |p: DVec2| -> bool {
        grid.get(&key(p)).map(|segs| {
            segs.iter().any(|(a, b)| {
                let d = *b - *a;
                let t = if d.length_squared() < 1e-18 { 0.0 } else { ((p - *a).dot(d) / d.length_squared()).clamp(0.0, 1.0) };
                (*a + d * t - p).length() < tol
            })
        }).unwrap_or(false)
    };
    let mut out = vec![];
    for pl in lines {
        let mut run: Vec<[f64; 2]> = vec![];
        for w in pl.windows(2) {
            let (a, b) = (DVec2::from(w[0]), DVec2::from(w[1]));
            // sample the segment; keep the uncovered stretches
            let n = (((b - a).length() / 0.25).ceil() as usize).clamp(1, 400);
            for i in 0..n {
                let (t0, t1) = (i as f64 / n as f64, (i + 1) as f64 / n as f64);
                let (p0, p1) = (a.lerp(b, t0), a.lerp(b, t1));
                let hide = covered(p0) && covered(p1) && covered((p0 + p1) / 2.0);
                if hide {
                    if run.len() >= 2 {
                        out.push(std::mem::take(&mut run));
                    }
                    run.clear();
                    continue;
                }
                if run.last().map(|l| DVec2::from(*l).distance(p0) > 1e-12).unwrap_or(true) {
                    if run.len() >= 2 {
                        out.push(std::mem::take(&mut run));
                    }
                    run = vec![p0.into()];
                }
                run.push(p1.into());
            }
        }
        if run.len() >= 2 {
            out.push(run);
        }
    }
    // join the samples of each straight stretch back into one segment
    out.into_iter()
        .map(|pl| {
            let mut s: Vec<[f64; 2]> = vec![pl[0]];
            for i in 1..pl.len() {
                if s.len() >= 2 && i + 1 <= pl.len() {
                    let (a, b, c) = (DVec2::from(s[s.len() - 2]), DVec2::from(s[s.len() - 1]), DVec2::from(pl[i]));
                    if (b - a).perp_dot(c - b).abs() < 1e-9 * (1.0 + (b - a).length() * (c - b).length()) && (b - a).dot(c - b) > 0.0 {
                        *s.last_mut().unwrap() = pl[i];
                        continue;
                    }
                }
                s.push(pl[i]);
            }
            s
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
        let v = project(&doc, &[([0.0, -1.0, 0.0], [0.0, 0.0, 1.0], false).into()]);
        let sec = project(&doc, &[([0.0, -1.0, 0.0], [0.0, 0.0, 1.0], true).into()]);
        assert!(!sec[0].hatch.is_empty());
        let b = v[0].bbox;
        assert!((b[1][0] - b[0][0] - 40.0).abs() < 1e-6, "{b:?}");
        assert!((b[1][1] - b[0][1] - 10.0).abs() < 1e-6, "{b:?}");
        assert!(v[0].visible.len() >= 4);
        // every model edge is listed with its identity; the box has 12
        assert_eq!(v[0].edges.len(), 12);
        // a cut placed off centre
        let off = project(&doc, &[ProjReq { dir: [0.0, -1.0, 0.0], up: [0.0, 0.0, 1.0], section: true, cut: Some(-5.0), ..Default::default() }]);
        assert_eq!(off[0].cut, Some(-5.0));
        // the back edges of the box lie under the front ones: no hidden lines are left
        assert!(v[0].hidden.iter().all(|pl| pl.len() < 2), "{:?}", v[0].hidden);
    }

    #[test]
    fn hidden_segments_under_visible_ones_are_dropped() {
        let vis = vec![vec![[0.0, 0.0], [10.0, 0.0]]];
        let hid = vec![vec![[5.0, 0.0], [20.0, 0.0]], vec![[0.0, 5.0], [10.0, 5.0]]];
        let out = drop_covered(&hid, &vis, 0.02);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!((out[0][0][0] - 10.0).abs() < 0.3 && (out[0][out[0].len() - 1][0] - 20.0).abs() < 1e-9, "{out:?}");
        assert_eq!(out[0].len(), 2, "one straight stretch: {out:?}");
        assert_eq!(out[1], vec![[0.0, 5.0], [10.0, 5.0]]);
    }

    #[test]
    fn parts_hide_each_other_and_holes_are_reported() {
        // a 40x10x20 block in front of a lower 40x10x10 block behind it, seen from the front
        let mut doc = Document::default();
        let mut s = crate::sketch::Sketch::default();
        for (x0, y0, y1) in [(0.0, 0.0, 10.0), (0.0, 20.0, 30.0)] {
            let p: Vec<_> = [(x0, y0), (x0 + 40.0, y0), (x0 + 40.0, y1), (x0, y1)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
            for i in 0..4 {
                s.add_curve(crate::sketch::Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false);
            }
        }
        let sk = doc.insert("S".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Top }, sketch: s, frame: Frame::TOP });
        let mut e = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, depth, .. } = &mut e {
            profiles.push(Ref::Region { sketch: sk, point: [5.0, 5.0] });
            *depth = "20".into();
        }
        doc.insert("E".into(), e);
        let mut e = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, depth, .. } = &mut e {
            profiles.push(Ref::Region { sketch: sk, point: [5.0, 25.0] });
            *depth = "10".into();
        }
        doc.insert("E2".into(), e);
        let v = &project(&doc, &[([0.0, -1.0, 0.0], [0.0, 0.0, 1.0], false).into()])[0];
        // the back block's top edge (z = 10) is behind the front block: hidden, not visible
        let at10 = |pls: &Vec<Vec<[f64; 2]>>| pls.iter().any(|pl| pl.len() >= 2 && pl.iter().all(|p| (p[1] - 10.0).abs() < 1e-6) && (pl[0][0] - pl[pl.len() - 1][0]).abs() > 30.0);
        assert!(!at10(&v.visible), "{:?}", v.visible);
        assert!(at10(&v.hidden), "{:?}", v.hidden);
        // one part only
        let body = build_bodies(&doc)[0].id;
        let one = &project(&doc, &[ProjReq { dir: [0.0, -1.0, 0.0], up: [0.0, 0.0, 1.0], bodies: vec![body], ..Default::default() }])[0];
        assert!(one.edges.iter().all(|e| e.part == body));
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
            faces.push(Ref::Face { point: [2.5, -10.0, 40.0], normal: [0.0, 0.0, 1.0], name: Default::default() });
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
        let edges = |z: f64| vec![[5.0, 0.0, z], [10.0, 5.0, z], [5.0, 10.0, z], [0.0, 5.0, z]].into_iter().map(|p| Ref::Edge { point: p, name: Default::default() }).collect::<Vec<_>>();
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
        doc.insert("F".into(), Op::Flange { edges: vec![Ref::Edge { point: [40.0, 15.0, 2.0], name: Default::default() }], length: "10".into(), angle: "90".into(), flip: false, inset: "0".into() });
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
                edges.push(Ref::Edge { point: [10.0, 0.0, 20.0], name: Default::default() });
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
        let face = Ref::Face { point: [20.0, 10.0, 5.0], normal: [0.0, 0.0, 1.0], name: Default::default() };
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
        let face = Ref::Face { point: [10.0, 10.0, 20.0], normal: [0.0, 0.0, 1.0], name: Default::default() };
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
            faces.push(Ref::Face { point: [5.0, 0.0, 5.0], normal: [1.0, 0.0, 0.0], name: Default::default() });
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

#[cfg(test)]
mod validity_probe {
    use super::*;
    #[test]
    #[ignore]
    fn probe_file() {
        let path = std::env::var("PROBE").unwrap();
        let ws = crate::workspace::Workspace::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
        let bodies = build_bodies(&ws.studios[0].doc);
        for b in &bodies {
            eprintln!("body {:?} valid={} faces={}", b.id, occ::is_valid(&b.shape), unique_faces(&b.shape).len());
        }
    }

    #[test]
    #[ignore]
    fn probe_steps() {
        // validity after each feature
        let path = std::env::var("PROBE").unwrap();
        let ws = crate::workspace::Workspace::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut doc = ws.studios[0].doc.clone();
        for n in 1..=doc.features.len() {
            doc.rollback = Some(n);
            let bodies = build_bodies(&doc);
            let f = &doc.features[n - 1];
            eprintln!("{} {}: {:?}", f.name, f.op.kind(), bodies.iter().map(|b| (occ::is_valid(&b.shape), unique_faces(&b.shape).len(), (volume(&b.shape) * 10.0).round() / 10.0)).collect::<Vec<_>>());
        }
        doc.rollback = None;
        let bodies = build_bodies(&doc);
        let b = &bodies[0];
        eprintln!("before mesh valid={}", occ::is_valid(&b.shape));
        for f in unique_faces(&b.shape) {
            let g = face_geom(&f);
            let es = f.edges().count();
            eprintln!("face planar={} area={:.3} n={:?} edges={} center={:?}", g.planar, g.area, g.normal.to_array().map(|v| (v * 1000.0).round() / 1000.0), es, g.center.to_array().map(|v| (v * 100.0).round() / 100.0));
        }
        let m = mesh_body(&b.shape).unwrap();
        eprintln!("after mesh valid={}", occ::is_valid(&b.shape));
        let _ = m;
    }

    #[test]
    #[ignore]
    fn probe_step() {
        // read a STEP back like a slicer would, mesh it and write an STL to check
        let path = std::env::var("PROBE").unwrap();
        let s = Shape::read_step(&path).unwrap();
        eprintln!("valid={} faces={} vol={}", occ::is_valid(&s), unique_faces(&s).len(), volume(&s));
        s.write_stl_with_tolerance(format!("{path}.stl"), 0.05).unwrap();
    }
}

#[cfg(test)]
mod check_tests {
    use super::*;
    use crate::sketch::{Geom, Sketch};

    fn rect(s: &mut Sketch, x0: f64, y0: f64, x1: f64, y1: f64) {
        let p: Vec<_> = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        for i in 0..4 {
            s.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false);
        }
    }

    #[test]
    fn finds_tiny_edges_and_heals_them() {
        let mut doc = Document::default();
        let top = Ref::OriginPlane { plane: OriginPlane::Top };
        let mut s = Sketch::default();
        rect(&mut s, 0.0, 0.0, 20.0, 20.0);
        let s1 = doc.insert("S".into(), Op::Sketch { plane: top.clone(), sketch: s, frame: Frame::TOP });
        let mut e = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, op, .. } = &mut e {
            profiles.push(Ref::Region { sketch: s1, point: [1.0, 1.0] });
            *op = BoolOp::New;
        }
        doc.insert("E".into(), e);
        // a second block 3 µm short of lining up with the first
        let mut s = Sketch::default();
        rect(&mut s, 19.997, 10.0, 30.0, 30.0);
        let s2 = doc.insert("S2".into(), Op::Sketch { plane: top, sketch: s, frame: Frame::TOP });
        let mut e = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, depth, op, .. } = &mut e {
            profiles.push(Ref::Region { sketch: s2, point: [25.0, 25.0] });
            *depth = "5".into();
            *op = BoolOp::Add;
        }
        doc.insert("E2".into(), e);
        let r = rebuild(&doc, &[], |_| {});
        assert!(r.issues.iter().any(|i| i.kind == "short_edge"), "{:?}", r.issues);
        doc.insert("H".into(), Op::Heal { bodies: vec![], tolerance: "0.01".into() });
        let r = rebuild(&doc, &[], |_| {});
        assert!(r.features.iter().all(|f| f.error.is_none()));
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        assert_eq!(r.bodies.len(), 1);
    }
}

#[cfg(test)]
mod sweep_pattern_tests {
    use super::*;
    use crate::doc::{Document, Op, OriginPlane, Ref};
    use crate::sketch::{Geom, Sketch};

    fn sketch(doc: &mut Document, plane: OriginPlane, f: impl FnOnce(&mut Sketch)) -> FeatId {
        let mut s = Sketch::default();
        f(&mut s);
        doc.insert("Sketch".into(), Op::Sketch { plane: Ref::OriginPlane { plane }, sketch: s, frame: plane.frame() })
    }

    fn polyline(s: &mut Sketch, pts: &[(f64, f64)]) {
        let p: Vec<_> = pts.iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        for w in p.windows(2) {
            s.add_curve(Geom::Line { a: w[0], b: w[1] }, false);
        }
    }

    fn sweep(doc: &mut Document, profile: FeatId, at: [f64; 2], path: FeatId) {
        let curves: Vec<Ref> = match &doc.feature(path).unwrap().op {
            Op::Sketch { sketch, .. } => sketch.curves.iter().map(|c| Ref::SketchCurve { sketch: path, curve: c.id }).collect(),
            _ => unreachable!(),
        };
        doc.insert("Sweep".into(), Op::Sweep { profiles: vec![Ref::Region { sketch: profile, point: at }], path: curves, op: BoolOp::New });
    }

    fn ok(r: &RebuildResult) {
        for f in &r.features {
            assert!(f.error.is_none(), "feature {} failed: {:?}", f.id, f.error);
        }
    }

    #[test]
    fn sweep_keeps_the_holes_of_the_profile() {
        let mut doc = Document::default();
        let ring = sketch(&mut doc, OriginPlane::Top, |s| {
            let c = s.add_point(DVec2::ZERO);
            s.add_curve(Geom::Circle { c, r: 5.0 }, false);
            s.add_curve(Geom::Circle { c, r: 4.0 }, false);
        });
        let path = sketch(&mut doc, OriginPlane::Front, |s| polyline(s, &[(0.0, 0.0), (0.0, 50.0)]));
        sweep(&mut doc, ring, [4.5, 0.0], path);
        let r = rebuild(&doc, &[], |_| {});
        ok(&r);
        let want = std::f64::consts::PI * (25.0 - 16.0) * 50.0;
        assert!((r.bodies[0].volume - want).abs() < 1e-3 * want, "{} vs {want}", r.bodies[0].volume);
    }

    #[test]
    fn sweep_turns_the_profile_round_sharp_corners() {
        let mut doc = Document::default();
        let disc = sketch(&mut doc, OriginPlane::Top, |s| {
            let c = s.add_point(DVec2::ZERO);
            s.add_curve(Geom::Circle { c, r: 5.0 }, false);
        });
        // up 50, then 45° over: the second leg must keep its full section, not a sheared one
        let path = sketch(&mut doc, OriginPlane::Front, |s| polyline(s, &[(0.0, 0.0), (0.0, 50.0), (30.0, 80.0)]));
        sweep(&mut doc, disc, [0.0, 0.0], path);
        let r = rebuild(&doc, &[], |_| {});
        ok(&r);
        let along = std::f64::consts::PI * 25.0 * (50.0 + 30.0 * 2f64.sqrt());
        let v = r.bodies[0].volume;
        assert!(v > 0.97 * along && v < 1.03 * along, "{v} vs {along}");
        assert!(r.issues.is_empty(), "{:?}", r.issues);
    }

    fn cube_pattern(spacing: &str, count: &str) -> RebuildResult {
        let mut doc = Document::default();
        let s = sketch(&mut doc, OriginPlane::Top, |s| polyline(s, &[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0), (0.0, 0.0)]));
        let mut e = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, op, .. } = &mut e {
            profiles.push(Ref::Region { sketch: s, point: [5.0, 5.0] });
            *op = BoolOp::New;
        }
        let e = doc.insert("Extrude".into(), e);
        let mut lp = Op::new_default("linearPattern").unwrap();
        if let Op::LinearPattern { bodies, direction, spacing: sp, count: n, .. } = &mut lp {
            bodies.push(Ref::Body { body: (e, 0) });
            *direction = Some(Ref::Axis { axis: 0 });
            *sp = spacing.into();
            *n = count.into();
        }
        doc.insert("Pattern".into(), lp);
        rebuild(&doc, &[], |_| {})
    }

    #[test]
    fn part_pattern_copies_are_parts_unless_they_touch() {
        let r = cube_pattern("20", "4");
        ok(&r);
        assert_eq!(r.bodies.len(), 4);
        assert!(r.bodies.iter().all(|b| (b.volume - 1000.0).abs() < 1e-6));
        // touching copies make one part
        let r = cube_pattern("10", "4");
        ok(&r);
        assert_eq!(r.bodies.len(), 1);
        assert!((r.bodies[0].volume - 4000.0).abs() < 1e-6, "{}", r.bodies[0].volume);
        // many copies stay fast (they used to be fused one by one)
        let t = std::time::Instant::now();
        let r = cube_pattern("20", "300");
        ok(&r);
        assert_eq!(r.bodies.len(), 300);
        assert!(t.elapsed().as_secs_f64() < 60.0, "{:?}", t.elapsed());
    }

    #[test]
    fn hole_sizes_are_checked() {
        for (dia, depth, through) in [("-3", "10", true), ("0", "10", true), ("5", "0", false)] {
            let mut doc = Document::default();
            let s = sketch(&mut doc, OriginPlane::Top, |s| {
                polyline(s, &[(0.0, 0.0), (20.0, 0.0), (20.0, 20.0), (0.0, 20.0), (0.0, 0.0)]);
                s.add_point(DVec2::new(10.0, 10.0));
            });
            let mut e = Op::new_default("extrude").unwrap();
            if let Op::Extrude { profiles, op, .. } = &mut e {
                profiles.push(Ref::Region { sketch: s, point: [5.0, 5.0] });
                *op = BoolOp::New;
            }
            doc.insert("Extrude".into(), e);
            let pt = match &doc.feature(s).unwrap().op {
                Op::Sketch { sketch, .. } => sketch.points.last().unwrap().id,
                _ => unreachable!(),
            };
            let mut h = Op::new_default("hole").unwrap();
            if let Op::Hole { points, diameter, depth: d, through: t, .. } = &mut h {
                points.push(Ref::SketchPoint { sketch: s, point: pt });
                *diameter = dia.into();
                *d = depth.into();
                *t = through;
            }
            let hid = doc.insert("Hole".into(), h);
            let r = rebuild(&doc, &[], |_| {});
            let err = r.features.iter().find(|f| f.id == hid).and_then(|f| f.error.clone());
            assert!(err.as_deref().map(|e| e.contains("must be positive")).unwrap_or(false), "{dia} {depth}: {err:?}");
        }
    }

    #[test]
    fn union_of_parts_that_dont_touch_says_so() {
        let mut doc = Document::default();
        let s = sketch(&mut doc, OriginPlane::Top, |s| {
            polyline(s, &[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0), (0.0, 0.0)]);
            polyline(s, &[(20.0, 0.0), (30.0, 0.0), (30.0, 10.0), (20.0, 10.0), (20.0, 0.0)]);
        });
        let mut e = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, op, .. } = &mut e {
            profiles.push(Ref::Region { sketch: s, point: [5.0, 5.0] });
            profiles.push(Ref::Region { sketch: s, point: [25.0, 5.0] });
            *op = BoolOp::New;
        }
        let e = doc.insert("Extrude".into(), e);
        let b = doc.insert("Union".into(), Op::Boolean { kind: BooleanKind::Union, target: Some(Ref::Body { body: (e, 0) }), tools: vec![Ref::Body { body: (e, 1) }], keep_tools: false });
        let r = rebuild(&doc, &[], |_| {});
        let err = r.features.iter().find(|f| f.id == b).and_then(|f| f.error.clone()).unwrap_or_default();
        assert!(err.contains("don't touch"), "{err}");
        assert_eq!(r.bodies.len(), 2);
    }

    #[test]
    fn pattern_above_its_seed_says_so() {
        let mut doc = Document::default();
        let s = sketch(&mut doc, OriginPlane::Top, |s| polyline(s, &[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0), (0.0, 0.0)]));
        let mut lp = Op::new_default("linearPattern").unwrap();
        let e_id = doc.next_id + 2;
        if let Op::LinearPattern { kind, bodies, direction, .. } = &mut lp {
            *kind = "feature".into();
            bodies.push(Ref::Feature { feature: e_id });
            *direction = Some(Ref::Axis { axis: 0 });
        }
        let p = doc.insert("Pattern".into(), lp);
        let mut e = Op::new_default("extrude").unwrap();
        if let Op::Extrude { profiles, op, .. } = &mut e {
            profiles.push(Ref::Region { sketch: s, point: [5.0, 5.0] });
            *op = BoolOp::New;
        }
        assert_eq!(doc.insert("Extrude".into(), e), e_id);
        let r = rebuild(&doc, &[], |_| {});
        let err = r.features.iter().find(|f| f.id == p).and_then(|f| f.error.clone()).unwrap_or_default();
        assert!(err.contains("comes after it") && err.contains("move Pattern below Extrude"), "{err}");
    }
}
