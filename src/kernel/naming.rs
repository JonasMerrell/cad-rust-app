//! Persistent names for model faces and edges.
//!
//! A reference to a face or edge stores a point on it, and that point goes stale as soon as
//! an upstream parameter moves the geometry (a plate gets thicker, a sketch gets wider). So
//! each feature also records the faces it makes under a role: the "end" cap of an extrude,
//! the "side" made by sketch curve 7, the "fillet" made from its second edge (`Named`). A
//! reference keeps that role (`doc::FaceName`; an edge keeps the names of the two faces it
//! joins), and on a rebuild the name is looked up again: the faces of the model that lie on
//! the surface of the face the feature made this time, and overlap it, are the candidates,
//! and the stored point only chooses between them.
//!
//! Faces are recognised by their surface rather than through OCCT's modification history,
//! which the booleans, the face unification after them and most wrappers don't hand back;
//! surfaces survive all of those unchanged. Faces nothing named (imported parts, faces a
//! later feature reshaped) keep being found by their point alone.

use glam::{DVec2, DVec3};
use opencascade::primitives::{Edge, Face, Shape};

use super::{face_distance, face_geom, face_tris, polyline_distance, unique_edges, unique_faces, v3, Model, R};
use crate::doc::{FaceName, FeatId, Ref, RefUpdate, TopoName};
use crate::occ;

/// GeomAbs_SurfaceType values.
const PLANE: i32 = 0;
const CYLINDER: i32 = 1;
const SPHERE: i32 = 3;

/// Positions are compared to this (surfaces survive booleans exactly; unification refits planes to about 1e-7).
const TOL: f64 = 1e-5;

/// A face a feature made, kept so later references can find what became of it.
pub struct Named {
    pub name: FaceName,
    face: Face,
    surf: Surf,
}

/// The surface a face lies on, and a point inside the face.
#[derive(Clone, Copy, Debug)]
pub struct Surf {
    kind: i32,
    /// Plane: (point, normal, 0); cylinder, cone, sphere, torus: (location, axis, radius).
    axis: Option<(DVec3, DVec3, f64)>,
    /// A point inside the face, on its surface.
    pub at: DVec3,
    /// Outward normal at `at`.
    normal: DVec3,
}

fn tri_area(t: &[DVec3; 3]) -> f64 {
    (t[1] - t[0]).cross(t[2] - t[0]).length()
}

pub fn surf_of(f: &Face) -> Option<Surf> {
    // the middle of the largest triangle is well inside the face
    let tris = face_tris(f);
    let t = tris.iter().max_by(|a, b| tri_area(a).total_cmp(&tri_area(b)))?;
    let c = (t[0] + t[1] + t[2]) / 3.0;
    let q = occ::face_at(&Shape::from(f), c).ok()?;
    Some(Surf { kind: q.kind, axis: q.axis, at: q.point, normal: q.normal })
}

/// Whether `p` (on the face's surface) lies inside the face or on its boundary.
fn contains(f: &Face, p: DVec3) -> bool {
    let s = occ::face_shape(f);
    let uv = opencascade_sys::extra::ex_surface_uv(s, p.x, p.y, p.z);
    uv.len() == 2 && opencascade_sys::extra::ex_face_has_uv(s, uv[0], uv[1], 1e-7)
}

/// Whether two faces lie on the same surface.
fn same_surface(a: &Surf, fa: &Face, b: &Surf, fb: &Face) -> bool {
    if a.kind != b.kind {
        return false;
    }
    let parallel = |d1: DVec3, d2: DVec3| d1.dot(d2).abs() > 1.0 - 1e-9;
    match (a.kind, a.axis, b.axis) {
        (PLANE, Some((p1, d1, _)), Some((p2, d2, _))) => parallel(d1, d2) && (p2 - p1).dot(d1).abs() < TOL,
        (CYLINDER, Some((p1, d1, r1)), Some((p2, d2, r2))) => parallel(d1, d2) && (p2 - p1).cross(d1).length() < TOL && (r1 - r2).abs() < TOL,
        (SPHERE, Some((p1, _, r1)), Some((p2, _, r2))) => (p2 - p1).length() < TOL && (r1 - r2).abs() < TOL,
        _ => {
            // other surfaces: each face's inner point lies on the other's surface, with the same tangent plane
            let on = |s: &Surf, f: &Face| occ::face_at(&Shape::from(f), s.at).map(|q| (q.point - s.at).length() < TOL * 10.0 && parallel(q.normal, s.normal)).unwrap_or(false);
            on(a, fb) && on(b, fa)
        }
    }
}

/// Whether two faces lie on the same surface and overlap there.
fn overlap(a: &Surf, fa: &Face, b: &Surf, fb: &Face) -> bool {
    same_surface(a, fa, b, fb) && (contains(fb, a.at) || contains(fa, b.at))
}

/// The edge's middle as the display mesh measures it (the editor finds picked edges by this point).
pub fn edge_mid(e: &Edge) -> DVec3 {
    let pts: Vec<DVec3> = e.approximation_segments().collect();
    super::polyline_mid(&pts)
}

fn edge_distance(e: &Edge, p: DVec3) -> f64 {
    let pts: Vec<DVec3> = e.approximation_segments().collect();
    polyline_distance(&pts, p)
}

/// The sketch curve nearest a point of the sketch plane (not construction geometry).
fn nearest_curve(sk: &crate::sketch::Sketch, p: DVec2) -> Option<u64> {
    sk.curves.iter().filter(|c| !c.construction).map(|c| (sk.curve_distance(c, p), c.id)).min_by(|a, b| a.0.total_cmp(&b.0)).map(|x| x.1 as u64)
}

fn fmt_point(p: DVec3) -> String {
    format!("[{:.3}, {:.3}, {:.3}]", p.x, p.y, p.z)
}

impl Model<'_> {
    fn add_named(&mut self, fid: FeatId, face: &Face, role: &str, n: u64) {
        if let Some(surf) = surf_of(face) {
            self.named.push(Named { name: FaceName { feature: fid, role: role.into(), n }, face: Face::from_face(&face.inner), surf });
        }
    }

    /// Name the faces of an extruded tool: the caps across `dir` are "start" and "end",
    /// the rest are "side" faces named after the sketch curve they were swept from.
    pub(super) fn name_prism(&mut self, fid: FeatId, tool: &Shape, dir: DVec3, sketch: Option<FeatId>) {
        let sk = sketch.and_then(|id| self.sketch(id).ok().map(|(s, fr)| (s.clone(), fr)));
        let mut caps = vec![];
        for f in unique_faces(tool) {
            let Some(s) = surf_of(&f) else { continue };
            if s.kind == PLANE && s.normal.dot(dir).abs() > 1.0 - 1e-6 {
                caps.push((s.at.dot(dir), f));
            } else if let Some((sk, fr)) = &sk {
                if let Some(c) = nearest_curve(sk, fr.to_local(s.at)) {
                    self.add_named(fid, &f, "side", c);
                }
            }
        }
        let lo = caps.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
        let hi = caps.iter().map(|c| c.0).fold(f64::NEG_INFINITY, f64::max);
        if hi - lo > TOL {
            for (d, f) in &caps {
                if (d - lo).abs() < TOL {
                    self.add_named(fid, f, "start", 0);
                } else if (d - hi).abs() < TOL {
                    self.add_named(fid, f, "end", 0);
                }
            }
        }
    }

    pub(super) fn name_extruded(&mut self, fid: FeatId, tools: &[Shape], faces: &[(Face, crate::sketch::Frame)], sketches: &[Option<FeatId>], sign: f64) {
        for ((t, (_, fr)), sk) in tools.iter().zip(faces).zip(sketches) {
            self.name_prism(fid, t, fr.normal() * sign, *sk);
        }
    }

    /// Name the faces of a revolved tool: "side" faces after the sketch curve that swept them
    /// (matched by distance from the axis and height along it), and for a partial revolve the
    /// "start" and "end" caps. `at` is a point of the profile and `start` the angle the profile
    /// was turned by before revolving (symmetric revolves).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn name_revolve(&mut self, fid: FeatId, tool: &Shape, o: DVec3, dir: DVec3, sketch: FeatId, at: DVec3, start: f64) {
        let Some((sk, fr)) = self.sketch(sketch).ok().map(|(s, fr)| (s.clone(), fr)) else { return };
        let dir = dir.normalize();
        let rh = |p: DVec3| {
            let h = (p - o).dot(dir);
            DVec2::new(((p - o) - dir * h).length(), h)
        };
        // each curve as a polyline in (radius, height)
        let curves: Vec<(u64, Vec<DVec2>)> =
            sk.curves.iter().filter(|c| !c.construction).map(|c| (c.id as u64, sk.tessellate(c, 0.2).into_iter().map(|p| rh(fr.to_world(p))).collect())).collect();
        // angles about the axis are measured from the profile
        let x0 = {
            let v = at - o;
            let v = v - dir * v.dot(dir);
            if v.length() > 1e-9 {
                v.normalize()
            } else {
                dir.any_orthonormal_vector()
            }
        };
        let y0 = dir.cross(x0);
        let off_start = |t: f64| {
            let d = (t - start).rem_euclid(std::f64::consts::TAU);
            d.min(std::f64::consts::TAU - d)
        };
        let mut caps = vec![];
        for f in unique_faces(tool) {
            let Some(s) = surf_of(&f) else { continue };
            if s.kind == PLANE && s.normal.dot(dir).abs() < 1e-6 && (s.at - o).dot(s.normal).abs() < TOL {
                let v = s.at - o;
                caps.push((off_start(v.dot(y0).atan2(v.dot(x0))), f));
                continue;
            }
            let p = rh(s.at);
            let best = curves
                .iter()
                .map(|(id, pts)| (pts.windows(2).map(|w| crate::sketch::seg_dist(p, w[0], w[1])).fold(f64::INFINITY, f64::min), *id))
                .min_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((_, id)) = best {
                self.add_named(fid, &f, "side", id);
            }
        }
        if caps.len() >= 2 {
            // the cap where the profile started, and the one it turned to
            caps.sort_by(|a, b| a.0.total_cmp(&b.0));
            let last = caps.len() - 1;
            self.add_named(fid, &caps[0].1, "start", 0);
            self.add_named(fid, &caps[last].1, "end", 0);
        }
    }

    /// Name the round wall of a hole tool (radius `r` about the line through `p` along `dir`).
    pub(super) fn name_hole(&mut self, fid: FeatId, tool: &Shape, p: DVec3, dir: DVec3, r: f64, n: u64) {
        for f in unique_faces(tool) {
            let Some(s) = surf_of(&f) else { continue };
            if let (CYLINDER, Some((c, d, rr))) = (s.kind, s.axis) {
                if (rr - r).abs() < TOL && d.dot(dir).abs() > 1.0 - 1e-9 && (c - p).cross(dir).length() < TOL {
                    self.add_named(fid, &f, "wall", n);
                }
            }
        }
    }

    /// Name the faces an edge operation (fillet, chamfer) added to a body: those on none of
    /// the surfaces the body had before, each after the nearest of the edges it was made from
    /// (`edges` holds each edge's index in the feature's list).
    pub(super) fn name_edge_faces(&mut self, fid: FeatId, role: &str, before: &Shape, after: &Shape, edges: &[(usize, Edge)]) {
        let old: Vec<(Surf, Face)> = unique_faces(before).into_iter().filter_map(|f| surf_of(&f).map(|s| (s, f))).collect();
        for f in unique_faces(after) {
            let Some(s) = surf_of(&f) else { continue };
            if old.iter().any(|(o, of)| same_surface(&s, &f, o, of)) {
                continue;
            }
            if let Some((i, _)) = edges.iter().map(|(i, e)| (*i, edge_distance(e, s.at))).min_by(|a, b| a.1.total_cmp(&b.1)) {
                self.add_named(fid, &f, role, i as u64);
            }
        }
    }

    /// "Extrude 1 end face", "Extrude 1 side face (sketch curve 7)".
    pub(super) fn describe_name(&self, n: &FaceName) -> String {
        let f = self.doc.feature(n.feature).map(|f| f.name.clone()).unwrap_or_else(|| format!("feature {}", n.feature));
        match n.role.as_str() {
            "side" => format!("{f} side face (sketch curve {})", n.n),
            "fillet" | "chamfer" => format!("{f} face from its edge {}", n.n + 1),
            "wall" => format!("{f} wall (sketch point {})", n.n),
            r => format!("{f} {r} face"),
        }
    }

    /// The model faces a name refers to now, as (body, face index, face). `None` when the
    /// feature that made it is no longer in the document (the point is all there is to go on).
    fn named_faces(&self, name: &FaceName) -> R<Option<Vec<(usize, usize, Face)>>> {
        let made: Vec<&Named> = self.named.iter().filter(|x| x.name == *name).collect();
        if made.is_empty() {
            return match self.doc.feature(name.feature) {
                None => Ok(None),
                Some(f) if f.suppressed => Err(format!("{} is suppressed", f.name)),
                Some(_) => Err(format!("{} no longer exists", self.describe_name(name))),
            };
        }
        let mut out = vec![];
        for (bi, b) in self.bodies.iter().enumerate() {
            for (fi, f) in unique_faces(&b.shape).into_iter().enumerate() {
                let Some(s) = surf_of(&f) else { continue };
                if made.iter().any(|m| overlap(&s, &f, &m.surf, &m.face)) {
                    out.push((bi, fi, f));
                }
            }
        }
        if out.is_empty() {
            return Err(format!("{} no longer exists", self.describe_name(name)));
        }
        Ok(Some(out))
    }

    /// The first name (in feature order) of the faces a model face lies on, if any.
    fn face_name_of(&self, f: &Face) -> Option<FaceName> {
        let s = surf_of(f)?;
        self.named.iter().find(|m| overlap(&s, f, &m.surf, &m.face)).map(|m| m.name.clone())
    }

    /// Names of the two faces edge `e` of body `bi` joins, when both have one.
    fn edge_name_of(&self, bi: usize, e: &Edge) -> Option<TopoName> {
        let shape = &self.bodies[bi].shape;
        let k = unique_edges(shape).iter().position(|x| opencascade_sys::extra::ex_is_same(occ::edge_shape(x), occ::edge_shape(e)))?;
        let faces = unique_faces(shape);
        let adj = occ::edge_faces(shape);
        let fs = adj.get(k)?;
        if fs.len() != 2 {
            return None;
        }
        let a = self.face_name_of(&faces[fs[0]])?;
        let b = self.face_name_of(&faces[fs[1]])?;
        (a != b).then(|| TopoName(vec![a, b]))
    }

    /// The live face a face reference points at: by its name when it has one, else by its point.
    pub(super) fn find_face_ref(&self, r: &Ref) -> R<(Face, usize)> {
        let Ref::Face { point, normal, name } = r else { return Err("select a face".into()) };
        let p = v3(*point);
        let found = match name.0.first() {
            Some(n) => match self.named_faces(n)? {
                Some(c) => c.into_iter().map(|(bi, _, f)| (face_distance(&f, p), f, bi)).min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, f, bi)| (f, bi)),
                None => None,
            },
            None => None,
        };
        let (face, bi) = match found {
            Some(x) => x,
            None => self.find_face(p, v3(*normal))?,
        };
        self.note_face(r, &face);
        Ok((face, bi))
    }

    /// The live edge an edge reference points at: by the names of its two faces when it has
    /// them, else by its point.
    pub(super) fn find_edge_ref(&self, r: &Ref) -> R<(Edge, usize)> {
        let Ref::Edge { point, name } = r else { return Err("select an edge".into()) };
        let p = v3(*point);
        let mut found = None;
        if let [a, b] = &name.0[..] {
            if let (Some(fa), Some(fb)) = (self.named_faces(a)?, self.named_faces(b)?) {
                let mut best: Option<(f64, Edge, usize)> = None;
                for (bi, body) in self.bodies.iter().enumerate() {
                    let (ia, ib): (Vec<usize>, Vec<usize>) = (fa.iter().filter(|x| x.0 == bi).map(|x| x.1).collect(), fb.iter().filter(|x| x.0 == bi).map(|x| x.1).collect());
                    if ia.is_empty() || ib.is_empty() {
                        continue;
                    }
                    let edges = unique_edges(&body.shape);
                    for (k, fs) in occ::edge_faces(&body.shape).iter().enumerate() {
                        let joins = fs.iter().any(|f| ia.contains(f)) && fs.iter().any(|f| ib.contains(f));
                        if joins && k < edges.len() {
                            let d = edge_distance(&edges[k], p);
                            if best.as_ref().map(|b| d < b.0).unwrap_or(true) {
                                best = Some((d, Edge::from_edge(&edges[k].inner), bi));
                            }
                        }
                    }
                }
                match best {
                    Some((_, e, bi)) => found = Some((e, bi)),
                    None => return Err(format!("the edge between the {} and the {} no longer exists", self.describe_name(a), self.describe_name(b))),
                }
            }
        }
        let (edge, bi) = match found {
            Some(x) => x,
            None => self.find_edge(p).map_err(|e| format!("{e} (it was near {})", fmt_point(p)))?,
        };
        self.note_edge(r, &edge, bi);
        Ok((edge, bi))
    }

    /// Remember a resolved face reference with its name and current point, to store back in the document.
    fn note_face(&self, r: &Ref, face: &Face) {
        let Ref::Face { point, normal, name } = r else { return };
        if self.current.get() == 0 {
            return;
        }
        let name = if name.is_empty() { self.face_name_of(face).map(|n| TopoName(vec![n])).unwrap_or_default() } else { name.clone() };
        if name.is_empty() {
            return;
        }
        let p = v3(*point);
        let g = face_geom(face);
        let (np, nn) =
            if g.planar { (p - g.normal * (p - g.center).dot(g.normal), g.normal) } else { (occ::face_at(&Shape::from(face), p).map(|q| q.point).unwrap_or(p), v3(*normal)) };
        let new = Ref::Face { point: np.into(), normal: nn.into(), name };
        self.note(r, new, (np - p).length() > 1e-7 || (nn - v3(*normal)).length() > 1e-7);
    }

    fn note_edge(&self, r: &Ref, e: &Edge, bi: usize) {
        let Ref::Edge { point, name } = r else { return };
        if self.current.get() == 0 {
            return;
        }
        let name = if name.is_empty() { self.edge_name_of(bi, e).unwrap_or_default() } else { name.clone() };
        if name.is_empty() {
            return;
        }
        let np = edge_mid(e);
        let new = Ref::Edge { point: np.into(), name };
        self.note(r, new, (np - v3(*point)).length() > 1e-7);
    }

    fn note(&self, old: &Ref, new: Ref, moved: bool) {
        let named_now =
            matches!((old, &new), (Ref::Face { name: a, .. }, Ref::Face { name: b, .. }) | (Ref::Edge { name: a, .. }, Ref::Edge { name: b, .. }) if a.is_empty() && !b.is_empty());
        if moved || named_now {
            self.updates.borrow_mut().push(RefUpdate { feature: self.current.get(), old: old.clone(), new });
        }
    }

    /// Whether a point is inside one of the parts.
    pub(super) fn inside_material(&self, p: DVec3) -> bool {
        self.bodies.iter().any(|b| occ::classify(&b.shape, &[p], 1e-7).map(|v| v[0] == 0).unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use glam::DVec2;

    use super::super::{rebuild, RebuildResult};
    use crate::doc::{BoolOp, Document, EndType, FeatId, Op, OriginPlane, Ref, Variable};
    use crate::sketch::{Frame, Geom, Sketch};

    /// Rebuild, then store frames and reference names as the editor does.
    fn build(doc: &mut Document) -> RebuildResult {
        let r = rebuild(doc, &[], |_| {});
        for f in &mut doc.features {
            if let (Op::Sketch { frame, .. }, Some(fr)) = (&mut f.op, r.frames.get(&f.id)) {
                *frame = *fr;
            }
        }
        doc.apply_ref_updates(&r.refs);
        r
    }

    fn errors(r: &RebuildResult) -> Vec<String> {
        r.features.iter().filter_map(|f| f.error.clone()).collect()
    }

    fn rect(s: &mut Sketch, w: f64, h: f64) {
        let p: Vec<_> = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        for i in 0..4 {
            s.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false);
        }
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

    /// A 60 × 40 plate `t` thick with a sketch on its top face and a 10 mm hole cut through all.
    fn plate_with_hole(t: &str) -> (Document, FeatId) {
        let mut doc = Document::default();
        doc.variables.push(Variable { name: "t".into(), expr: t.into() });
        let mut s = Sketch::default();
        rect(&mut s, 60.0, 40.0);
        let s1 = doc.insert("Sketch 1".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Top }, sketch: s, frame: Frame::TOP });
        extrude(&mut doc, s1, [5.0, 5.0], "t", EndType::Blind, BoolOp::New);
        let mut s = Sketch::default();
        let c = s.add_point(DVec2::new(30.0, 20.0));
        s.add_curve(Geom::Circle { c, r: 5.0 }, false);
        let top = Ref::Face { point: [10.0, 10.0, 20.0], normal: [0.0, 0.0, 1.0], name: Default::default() };
        let s2 = doc.insert("Sketch 2".into(), Op::Sketch { plane: top, sketch: s, frame: Frame::TOP });
        extrude(&mut doc, s2, [30.0, 20.0], "10", EndType::ThroughAll, BoolOp::Remove);
        (doc, s2)
    }

    #[test]
    fn sketch_on_a_face_follows_it_when_the_plate_gets_thicker() {
        let (mut doc, s2) = plate_with_hole("20");
        let r = build(&mut doc);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));
        // the kernel named the face the sketch is on
        match &doc.feature(s2).unwrap().op {
            Op::Sketch { plane: Ref::Face { name, .. }, .. } => assert_eq!(name.0.first().map(|n| n.role.as_str()), Some("end"), "{name:?}"),
            _ => unreachable!(),
        }
        doc.variables[0].expr = "30".into();
        let r = build(&mut doc);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));
        assert!((r.frames[&s2].origin[2] - 30.0).abs() < 1e-9, "{:?}", r.frames[&s2]);
        let want = 60.0 * 40.0 * 30.0 - std::f64::consts::PI * 25.0 * 30.0;
        assert!((r.bodies[0].volume - want).abs() < 1e-3, "{} vs {want}: the hole is not through", r.bodies[0].volume);
    }

    #[test]
    fn a_face_reference_that_went_stale_is_an_error_not_the_old_plane() {
        // an old document: the sketch's face is referred to by its point only, and the plate is now 30 thick
        let (mut doc, s2) = plate_with_hole("30");
        let r = build(&mut doc);
        let err = r.features.iter().find(|f| f.id == s2).and_then(|f| f.error.clone()).unwrap_or_default();
        assert!(err.contains("sketch plane could not be found"), "{err}");
    }

    #[test]
    fn through_all_from_inside_the_part_cuts_both_ways() {
        let mut doc = Document::default();
        let mut s = Sketch::default();
        rect(&mut s, 60.0, 40.0);
        let s1 = doc.insert("Sketch 1".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Top }, sketch: s, frame: Frame::TOP });
        extrude(&mut doc, s1, [5.0, 5.0], "30", EndType::Blind, BoolOp::New);
        let mut pl = Op::new_default("plane").unwrap();
        if let Op::Plane { base, offset, .. } = &mut pl {
            *base = Some(Ref::OriginPlane { plane: OriginPlane::Top });
            *offset = "20".into();
        }
        let p = doc.insert("Plane".into(), pl);
        let mut s = Sketch::default();
        let c = s.add_point(DVec2::new(30.0, 20.0));
        s.add_curve(Geom::Circle { c, r: 5.0 }, false);
        let s2 = doc.insert("Sketch 2".into(), Op::Sketch { plane: Ref::FeaturePlane { feature: p }, sketch: s, frame: Frame::TOP });
        extrude(&mut doc, s2, [30.0, 20.0], "10", EndType::ThroughAll, BoolOp::Remove);
        let r = build(&mut doc);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));
        let want = 60.0 * 40.0 * 30.0 - std::f64::consts::PI * 25.0 * 30.0;
        assert!((r.bodies[0].volume - want).abs() < 1e-3, "{} vs {want}", r.bodies[0].volume);
    }

    /// An 80 × 50 × 10 block with its four vertical edges filleted (r 5), picked by point.
    fn filleted_block(extra: Option<[f64; 3]>) -> (Document, FeatId, FeatId) {
        let mut doc = Document::default();
        let mut s = Sketch::default();
        rect(&mut s, 80.0, 50.0);
        let s1 = doc.insert("Sketch 1".into(), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Top }, sketch: s, frame: Frame::TOP });
        extrude(&mut doc, s1, [5.0, 5.0], "10", EndType::Blind, BoolOp::New);
        let mut f = Op::new_default("fillet").unwrap();
        if let Op::Fillet { edges, radius, .. } = &mut f {
            for p in [[0.0, 0.0, 5.0], [80.0, 0.0, 5.0], [80.0, 50.0, 5.0], [0.0, 50.0, 5.0]].into_iter().chain(extra) {
                edges.push(Ref::Edge { point: p, name: Default::default() });
            }
            *radius = "5".into();
        }
        let fid = doc.insert("Fillet 1".into(), f);
        (doc, s1, fid)
    }

    /// Move the rectangle's right side to x = `w`, as the sketch solver does when its width changes.
    fn set_width(doc: &mut Document, sk: FeatId, w: f64) {
        if let Some(Op::Sketch { sketch, .. }) = doc.feature_mut(sk).map(|f| &mut f.op) {
            for p in &mut sketch.points {
                if p.x > 1.0 {
                    p.x = w;
                }
            }
        }
    }

    #[test]
    fn fillets_survive_a_change_of_width() {
        let (mut doc, s1, _) = filleted_block(None);
        let r = build(&mut doc);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));
        set_width(&mut doc, s1, 100.0);
        let r = build(&mut doc);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));
        let corner = 25.0 - std::f64::consts::PI * 25.0 / 4.0;
        let want = (100.0 * 50.0 - 4.0 * corner) * 10.0;
        assert!((r.bodies[0].volume - want).abs() < 1e-3, "{} vs {want}", r.bodies[0].volume);
        // and back again, through the names stored on the way
        set_width(&mut doc, s1, 60.0);
        let r = build(&mut doc);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));
        assert!((r.bodies[0].volume - (60.0 * 50.0 - 4.0 * corner) * 10.0).abs() < 1e-3);
    }

    #[test]
    fn a_missing_edge_is_reported_and_the_others_still_get_filleted() {
        let (mut doc, _, fid) = filleted_block(Some([200.0, 200.0, 5.0]));
        let r = build(&mut doc);
        let err = r.features.iter().find(|f| f.id == fid).and_then(|f| f.error.clone()).unwrap_or_default();
        assert!(err.contains("1 of 5 edges") && err.contains("edge 5"), "{err}");
        let corner = 25.0 - std::f64::consts::PI * 25.0 / 4.0;
        let want = (80.0 * 50.0 - 4.0 * corner) * 10.0;
        assert!((r.bodies[0].volume - want).abs() < 1e-3, "{} vs {want}", r.bodies[0].volume);
    }

    #[test]
    fn edges_of_fillet_faces_keep_their_names() {
        // chamfer the top edge of a fillet face, then make the block wider
        let (mut doc, s1, _) = filleted_block(None);
        let mut c = Op::new_default("chamfer").unwrap();
        if let Op::Chamfer { edges, distance, .. } = &mut c {
            // the top edge of the fillet at (80, 50): a quarter circle about (75, 45)
            let a = std::f64::consts::FRAC_PI_4;
            edges.push(Ref::Edge { point: [75.0 + 5.0 * a.cos(), 45.0 + 5.0 * a.sin(), 10.0], name: Default::default() });
            *distance = "1".into();
        }
        let cid = doc.insert("Chamfer".into(), c);
        let r = build(&mut doc);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));
        match &doc.feature(cid).unwrap().op {
            Op::Chamfer { edges, .. } => match &edges[0] {
                Ref::Edge { name, .. } => assert!(name.0.iter().any(|n| n.role == "fillet"), "{name:?}"),
                _ => unreachable!(),
            },
            _ => unreachable!(),
        }
        let v0 = r.bodies[0].volume;
        set_width(&mut doc, s1, 100.0);
        let r = build(&mut doc);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));
        // 20 × 50 × 10 more, less the 1 mm chamfer (½ mm² section) along the 40 mm the top outline
        // grew (the chamfer runs on round the tangent outline)
        let want = v0 + 20.0 * 50.0 * 10.0 - 0.5 * 40.0;
        assert!((r.bodies[0].volume - want).abs() < 1e-3, "{} vs {want}", r.bodies[0].volume);
    }

    #[test]
    fn names_are_kept_out_of_old_documents_and_comparisons() {
        let plain = r#"{"kind":"edge","point":[1.0,2.0,3.0]}"#;
        let r: Ref = serde_json::from_str(plain).unwrap();
        assert_eq!(serde_json::to_string(&r).unwrap(), plain);
        let named: Ref = serde_json::from_str(r#"{"kind":"edge","point":[1.0,2.0,3.0],"name":[{"feature":2,"role":"end"},{"feature":2,"role":"side","n":4}]}"#).unwrap();
        assert_eq!(named, r);
    }
}
