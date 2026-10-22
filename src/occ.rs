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
