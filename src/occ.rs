//! Thin, exception-safe layer over the extra OpenCascade wrappers
//! (`crates/opencascade-sys/include/extra.hxx`). Failures come back as `Err`
//! with OCCT's message instead of aborting the process.

use cxx::UniquePtr;
use ffi::extra as ex;
use glam::{DMat3, DVec3};
use opencascade::primitives::{Edge, Face, Shape, Wire};
use opencascade_sys as ffi;

pub type R<T> = Result<T, String>;

fn take(p: UniquePtr<ffi::topo_ds::TopoDS_Shape>, what: &str) -> R<Shape> {
    if p.is_null() || p.IsNull() {
        let e = ex::ex_last_error();
        return Err(if e.is_empty() { format!("{what} failed") } else { e });
    }
    Ok(Shape::from_shape(&p))
}

fn list<'a>(shapes: impl IntoIterator<Item = &'a ffi::topo_ds::TopoDS_Shape>) -> UniquePtr<ffi::top_tools::TopTools_ListOfShape> {
    let mut l = ffi::top_tools::new_list_of_shape();
    for s in shapes {
        l.pin_mut().Append(s);
    }
    l
}

pub fn edge_shape(e: &Edge) -> &ffi::topo_ds::TopoDS_Shape {
    ffi::topo_ds::cast_edge_to_shape(&e.inner)
}

pub fn face_shape(f: &Face) -> &ffi::topo_ds::TopoDS_Shape {
    ffi::topo_ds::cast_face_to_shape(&f.inner)
}

pub fn wire_shape(w: &Wire) -> &ffi::topo_ds::TopoDS_Shape {
    ffi::topo_ds::cast_wire_to_shape(&w.inner)
}

pub fn fuse(a: &Shape, b: &Shape) -> R<Shape> {
    take(ex::ex_boolean(&a.inner, &b.inner, 0), "union")
}
pub fn cut(a: &Shape, b: &Shape) -> R<Shape> {
    take(ex::ex_boolean(&a.inner, &b.inner, 1), "subtract")
}
pub fn common(a: &Shape, b: &Shape) -> R<Shape> {
    take(ex::ex_boolean(&a.inner, &b.inner, 2), "intersect")
}

pub fn prism(profile: &Shape, v: DVec3) -> R<Shape> {
    take(ex::ex_prism(&profile.inner, v.x, v.y, v.z), "extrude")
}

pub fn revolve(profile: &Shape, o: DVec3, d: DVec3, angle_rad: f64) -> R<Shape> {
    take(ex::ex_revol(&profile.inner, o.x, o.y, o.z, d.x, d.y, d.z, angle_rad), "revolve")
}

pub fn fillet(s: &Shape, edges: &[Edge], r1: f64, r2: f64) -> R<Shape> {
    let l = list(edges.iter().map(edge_shape));
    take(ex::ex_fillet(&s.inner, &l, r1, r2), "fillet")
}

pub fn chamfer(s: &Shape, edges: &[Edge], d: f64) -> R<Shape> {
    let l = list(edges.iter().map(edge_shape));
    take(ex::ex_chamfer(&s.inner, &l, d), "chamfer")
}

/// Hollow `s`, removing `faces`; negative thickness keeps the outside surface.
pub fn shell(s: &Shape, faces: &[Face], t: f64) -> R<Shape> {
    let l = list(faces.iter().map(face_shape));
    take(ex::ex_shell(&s.inner, &l, t), "shell")
}

/// Hollow a solid and keep it closed: a cavity leaving walls `t` thick inside its surface
/// (t > 0), or walls `-t` thick grown around it (t < 0).
pub fn hollow(s: &Shape, t: f64) -> R<Shape> {
    take(ex::ex_hollow(&s.inner, t), "shell")
}

pub fn reversed(s: &Shape) -> R<Shape> {
    take(ex::ex_reversed(&s.inner), "reverse")
}

/// Check what an operation on solids made: an inside-out result is turned right way out, and
/// an invalid one or one without volume is an error (`hint` says what to try), so a failed
/// operation never passes for a part. Results without solids (surfaces, empty) pass as they are.
pub fn checked(s: Shape, what: &str, hint: &str) -> R<Shape> {
    if count(&s, SOLID) == 0 {
        return Ok(s);
    }
    let v = mass_props(&s, 0).map(|m| m.0).unwrap_or(0.0);
    if v.abs() <= 1e-9 {
        return Err(format!("{what} produced a solid with no volume{hint}"));
    }
    let s = if v < 0.0 { reversed(&s)? } else { s };
    if !is_valid(&s) {
        return Err(format!("{what} produced invalid geometry{hint}"));
    }
    Ok(s)
}

pub fn thicken(s: &Shape, t: f64) -> R<Shape> {
    take(ex::ex_thicken(&s.inner, t), "thicken")
}

pub fn draft(s: &Shape, faces: &[Face], pull: DVec3, angle_rad: f64, plane_p: DVec3, plane_n: DVec3) -> R<Shape> {
    let l = list(faces.iter().map(face_shape));
    take(ex::ex_draft(&s.inner, &l, pull.x, pull.y, pull.z, angle_rad, plane_p.x, plane_p.y, plane_p.z, plane_n.x, plane_n.y, plane_n.z), "draft")
}

pub fn sweep(profile: &Wire, spine: &Wire, frenet: bool) -> R<Shape> {
    take(ex::ex_sweep(wire_shape(profile), wire_shape(spine), frenet), "sweep")
}

pub fn loft(wires: &[Wire], ruled: bool) -> R<Shape> {
    let l = list(wires.iter().map(wire_shape));
    take(ex::ex_loft(&l, true, ruled), "loft")
}

/// Rigid transform x' = m x + t.
pub fn transform(s: &Shape, m: DMat3, t: DVec3) -> R<Shape> {
    let r = m.transpose(); // rows
    let v = [r.x_axis.x, r.x_axis.y, r.x_axis.z, t.x, r.y_axis.x, r.y_axis.y, r.y_axis.z, t.y, r.z_axis.x, r.z_axis.y, r.z_axis.z, t.z];
    take(ex::ex_transform(&s.inner, &v), "transform")
}

pub fn translate(s: &Shape, t: DVec3) -> R<Shape> {
    transform(s, DMat3::IDENTITY, t)
}

pub fn rotate(s: &Shape, o: DVec3, axis: DVec3, angle_rad: f64) -> R<Shape> {
    let m = DMat3::from_axis_angle(axis.normalize(), angle_rad);
    transform(s, m, o - m * o)
}

pub fn mirror(s: &Shape, p: DVec3, n: DVec3) -> R<Shape> {
    take(ex::ex_mirror(&s.inner, p.x, p.y, p.z, n.x, n.y, n.z), "mirror")
}

pub fn delete_faces(s: &Shape, faces: &[Face]) -> R<Shape> {
    let l = list(faces.iter().map(face_shape));
    take(ex::ex_delete_faces(&s.inner, &l), "delete face")
}

#[allow(clippy::too_many_arguments)]
pub fn helix(o: DVec3, axis: DVec3, xdir: DVec3, radius: f64, pitch: f64, turns: f64, left: bool) -> R<Shape> {
    take(ex::ex_helix(o.x, o.y, o.z, axis.x, axis.y, axis.z, xdir.x, xdir.y, xdir.z, radius, pitch, turns, left), "helix")
}

pub fn spline_edge(pts: &[DVec3], closed: bool) -> R<Edge> {
    let flat: Vec<f64> = pts.iter().flat_map(|p| [p.x, p.y, p.z]).collect();
    let s = take(ex::ex_spline(&flat, closed), "spline")?;
    Ok(Edge::from_edge(ffi::topo_ds::TopoDS::Edge(&s.inner)))
}

#[allow(clippy::too_many_arguments)]
pub fn ellipse_edge(c: DVec3, n: DVec3, xdir: DVec3, rmaj: f64, rmin: f64, a0: f64, a1: f64, full: bool) -> R<Edge> {
    let s = take(ex::ex_ellipse(c.x, c.y, c.z, n.x, n.y, n.z, xdir.x, xdir.y, xdir.z, rmaj, rmin, a0, a1, full), "ellipse")?;
    Ok(Edge::from_edge(ffi::topo_ds::TopoDS::Edge(&s.inner)))
}

pub fn wire(edges: &[Edge]) -> R<Wire> {
    let l = list(edges.iter().map(edge_shape));
    let s = take(ex::ex_wire(&l), "wire")?;
    Ok(Wire::from_wire(ffi::topo_ds::TopoDS::Wire(&s.inner)))
}

pub fn face(outer: &Wire, holes: &[Wire]) -> R<Face> {
    let l = list(holes.iter().map(wire_shape));
    let s = take(ex::ex_face(wire_shape(outer), &l), "face")?;
    s.faces().next().ok_or_else(|| "could not make a face from the profile".to_string())
}

/// Hidden-line projection along `dir` with `up` towards +Y of the result.
/// kind: 0 visible sharp, 1 visible smooth, 2 visible outline, 3 hidden sharp, 4 hidden outline.
pub fn hlr(s: &Shape, dir: DVec3, up: DVec3, kind: i32) -> R<Shape> {
    take(ex::ex_hlr(&s.inner, dir.x, dir.y, dir.z, up.x, up.y, up.z, kind), "projection")
}

pub fn compound(shapes: &[&Shape]) -> R<Shape> {
    let l = list(shapes.iter().map(|s| &*s.inner));
    take(ex::ex_compound(&l), "compound")
}

/// mode 0: two distances; mode 1: distance and angle (radians).
pub fn chamfer2(s: &Shape, edges: &[Edge], d1: f64, d2: f64, mode: i32) -> R<Shape> {
    let l = list(edges.iter().map(edge_shape));
    take(ex::ex_chamfer2(&s.inner, &l, d1, d2, mode), "chamfer")
}

pub fn offset_faces(s: &Shape, faces: &[Face], d: f64) -> R<Shape> {
    let l = list(faces.iter().map(face_shape));
    take(ex::ex_offset_faces(&s.inner, &l, d), "move face")
}

/// Drop existing triangulations so the next mesh uses fresh settings.
pub fn clean_mesh(s: &Shape) {
    let _ = ex::ex_mesh_careful(&s.inner, 1e9, 1.0, 0);
}

pub fn heal(s: &Shape, tol: f64) -> R<Shape> {
    take(ex::ex_heal(&s.inner, tol), "heal")
}

/// A solid (or shell) from triangles: shared vertices `pts`, index triples `tris`.
/// With `merge`, coplanar neighbouring triangles become one flat face.
pub fn mesh_solid(pts: &[f64], tris: &[u32], merge: bool) -> R<Shape> {
    let spec = crate::import::face_spec(pts, tris, merge);
    take(ex::ex_mesh_solid(pts, &spec), "mesh import")
}

pub fn sew(shapes: &[&Shape], solid: bool) -> R<Shape> {
    let l = list(shapes.iter().map(|s| &*s.inner));
    take(ex::ex_sew(&l, 1e-4, solid), "knit")
}

pub fn fill(edges: &[Edge]) -> R<Shape> {
    let l = list(edges.iter().map(edge_shape));
    take(ex::ex_fill(&l), "fill")
}

pub fn offset_shape(s: &Shape, d: f64) -> R<Shape> {
    take(ex::ex_offset_shape(&s.inner, d), "offset")
}

pub fn unify(s: &Shape) -> R<Shape> {
    take(ex::ex_unify(&s.inner), "unify")
}

pub fn is_valid(s: &Shape) -> bool {
    ex::ex_is_valid(&s.inner)
}

// ---------------------------------------------------------------- queries for scripts

fn last_err(what: &str) -> String {
    let e = ex::ex_last_error();
    if e.is_empty() { format!("{what} failed") } else { e }
}

/// Mass properties of a shape as a uniform volume (`kind` 0), surface (1) or wire (2):
/// (measure, centre, inertia about the centre for unit density).
pub fn mass_props(s: &Shape, kind: i32) -> R<(f64, DVec3, DMat3)> {
    let v = ex::ex_mass_props(&s.inner, kind);
    if v.len() < 13 {
        return Err(last_err("mass properties"));
    }
    let m = DMat3::from_cols_array(&[v[4], v[7], v[10], v[5], v[8], v[11], v[6], v[9], v[12]]);
    Ok((v[0], DVec3::new(v[1], v[2], v[3]), m))
}

/// Inside (0), outside (1), on (2) or unknown (3) for each point.
pub fn classify(s: &Shape, pts: &[DVec3], tol: f64) -> R<Vec<i32>> {
    let flat: Vec<f64> = pts.iter().flat_map(|p| p.to_array()).collect();
    let v = ex::ex_classify(&s.inner, &flat, tol);
    if v.len() != pts.len() {
        return Err(last_err("classify"));
    }
    Ok(v.into_iter().collect())
}

/// Sub-shape kinds for `count` and `sub`.
pub const SOLID: i32 = 0;
pub const FACE: i32 = 2;
pub const WIRE: i32 = 3;
pub const EDGE: i32 = 4;
pub const VERTEX: i32 = 5;

pub fn count(s: &Shape, kind: i32) -> usize {
    ex::ex_count(&s.inner, kind).max(0) as usize
}

/// For each edge (in `sub_shapes` order) the faces around it (in `sub_shapes` order).
pub fn edge_faces(s: &Shape) -> Vec<Vec<usize>> {
    let v = ex::ex_edge_faces(&s.inner);
    let mut out = vec![];
    let mut i = 0;
    while i < v.len() {
        let n = v[i] as usize;
        out.push(v[i + 1..(i + 1 + n).min(v.len())].iter().map(|x| *x as usize).collect());
        i += 1 + n;
    }
    out
}

/// What a face query returns: surface type, outward normal and nearest point, and the axis/plane when there is one.
pub struct FaceAt {
    pub kind: i32,
    pub normal: DVec3,
    pub point: DVec3,
    pub axis: Option<(DVec3, DVec3, f64)>,
}

pub fn face_at(f: &Shape, p: DVec3) -> R<FaceAt> {
    let v = ex::ex_face_at(&f.inner, p.x, p.y, p.z);
    if v.len() < 7 {
        return Err(last_err("face query"));
    }
    let axis = (v.len() >= 14).then(|| (DVec3::new(v[7], v[8], v[9]), DVec3::new(v[10], v[11], v[12]), v[13]));
    Ok(FaceAt { kind: v[0] as i32, normal: DVec3::new(v[1], v[2], v[3]), point: DVec3::new(v[4], v[5], v[6]), axis })
}

pub struct EdgeInfo {
    pub kind: i32,
    pub length: f64,
    pub start: DVec3,
    pub mid: DVec3,
    pub end: DVec3,
    pub circle: Option<(DVec3, DVec3, f64)>,
}

pub fn edge_info(e: &Shape) -> R<EdgeInfo> {
    let v = ex::ex_edge_info(&e.inner);
    if v.len() < 11 {
        return Err(last_err("edge query"));
    }
    let p = |i: usize| DVec3::new(v[i], v[i + 1], v[i + 2]);
    let circle = (v.len() >= 18).then(|| (p(11), p(14), v[17]));
    Ok(EdgeInfo { kind: v[0] as i32, length: v[1], start: p(2), mid: p(5), end: p(8), circle })
}

pub fn edge_points(e: &Shape, deflection: f64) -> Vec<DVec3> {
    ex::ex_edge_points(&e.inner, deflection).chunks(3).map(|c| DVec3::new(c[0], c[1], c[2])).collect()
}

pub fn is_same(a: &Shape, b: &Shape) -> bool {
    ex::ex_is_same(&a.inner, &b.inner)
}

pub fn bbox(s: &Shape, optimal: bool) -> Option<(DVec3, DVec3)> {
    let v = ex::ex_bbox(&s.inner, optimal);
    (v.len() == 6).then(|| (DVec3::new(v[0], v[1], v[2]), DVec3::new(v[3], v[4], v[5])))
}

/// op 0 fuse, 1 cut, 2 common of all `args` with all `tools`.
pub fn boolean_n(args: &[&Shape], tools: &[&Shape], op: i32, fuzzy: f64, unify: bool) -> R<Shape> {
    let a = list(args.iter().map(|s| &*s.inner));
    let t = list(tools.iter().map(|s| &*s.inner));
    take(ex::ex_boolean_n(&a, &t, op, fuzzy, unify), ["fuse", "cut", "common"][op.clamp(0, 2) as usize])
}

/// 2D offset of a planar face: a compound of faces (empty when it vanishes).
pub fn offset2d(f: &Shape, d: f64, arc: bool) -> R<Shape> {
    let p = ex::ex_offset2d(&f.inner, d, arc);
    if p.is_null() {
        return Err(last_err("2D offset"));
    }
    if p.IsNull() {
        return Err(last_err("2D offset"));
    }
    Ok(Shape::from_shape(&p))
}

pub fn face_towards(f: &Shape, n: DVec3) -> R<Shape> {
    take(ex::ex_face_towards(&f.inner, n.x, n.y, n.z), "orient face")
}
