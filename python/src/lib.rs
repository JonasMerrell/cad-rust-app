//! Python bindings to rCAD's OpenCascade kernel: `cadrust._core`.
//!
//! The low-level layer: shapes, the operations on them and the queries, with plain tuples and lists
//! in and out. `cadrust.freecad` builds a FreeCAD-style API (Vector, Placement, Part.makeBox, …) on it.
//! Failed operations raise `cadrust.OCCError` with OpenCascade's message.

use cxx::UniquePtr;
use opencascade_sys::extra as ex;
use opencascade_sys::top_tools::{TopTools_IndexedMapOfShape, TopTools_ListOfShape, new_list_of_shape};
use opencascade_sys::topo_ds::{TopoDS_Shape, TopoDS_Shape_to_owned};
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

mod fea;
mod render;
mod web;
// the app's finite-element mesher and solver, shared with the Simulation tab
#[allow(dead_code)]
#[path = "../../src/fea_mesh.rs"]
mod fea_mesh;
#[allow(dead_code)]
#[path = "../../src/fea_solve.rs"]
mod fea_solve;

#[path = "../../src/web_export.rs"]
mod web_export;

create_exception!(_core, OCCError, PyException, "An OpenCascade operation failed.");

type V3 = (f64, f64, f64);

/// An OpenCascade shape (TopoDS_Shape). Shapes are values: operations return new shapes.
#[pyclass(unsendable, module = "cadrust._core")]
pub struct Shape {
    inner: UniquePtr<TopoDS_Shape>,
}

impl Shape {
    fn of(s: &TopoDS_Shape) -> Shape {
        Shape { inner: TopoDS_Shape_to_owned(s) }
    }
    pub(crate) fn s(&self) -> &TopoDS_Shape {
        &self.inner
    }
}

pub(crate) fn fail(what: &str) -> PyErr {
    let e = ex::ex_last_error();
    OCCError::new_err(if e.is_empty() { format!("{what} failed") } else { e })
}

/// A shape result, or OCCError when OpenCascade gave back a null shape.
fn take(p: UniquePtr<TopoDS_Shape>, what: &str) -> PyResult<Shape> {
    if p.is_null() || p.IsNull() {
        return Err(fail(what));
    }
    Ok(Shape { inner: p })
}

fn list(shapes: &[PyRef<Shape>]) -> UniquePtr<TopTools_ListOfShape> {
    let mut l = new_list_of_shape();
    for s in shapes {
        l.pin_mut().Append(s.s());
    }
    l
}

fn from_map(m: &TopTools_IndexedMapOfShape) -> Vec<Shape> {
    (1..=m.Extent()).map(|i| Shape::of(m.FindKey(i))).collect()
}

fn flat(pts: &[V3]) -> Vec<f64> {
    pts.iter().flat_map(|p| [p.0, p.1, p.2]).collect()
}

fn triples(v: &[f64]) -> Vec<V3> {
    v.chunks_exact(3).map(|c| (c[0], c[1], c[2])).collect()
}

/// Row-major 3 × 4 matrix.
type M34 = [f64; 12];

#[pymethods]
impl Shape {
    /// An empty (null) shape.
    #[new]
    fn new() -> Shape {
        Shape { inner: ex::ex_null_shape() }
    }

    fn is_null(&self) -> bool {
        self.inner.is_null() || self.inner.IsNull()
    }

    /// TopAbs_ShapeEnum: 0 compound, 1 compsolid, 2 solid, 3 shell, 4 face, 5 wire, 6 edge, 7 vertex; -1 null.
    fn shape_type(&self) -> i32 {
        ex::ex_shape_type(self.s())
    }

    fn is_valid(&self) -> bool {
        ex::ex_is_valid(self.s())
    }

    fn is_same(&self, other: PyRef<Shape>) -> bool {
        ex::ex_is_same(self.s(), other.s())
    }

    /// Distinct sub-shapes of a kind (0 solid, 1 shell, 2 face, 3 wire, 4 edge, 5 vertex), in OCCT's map order.
    fn subshapes(&self, kind: i32) -> Vec<Shape> {
        from_map(&ex::ex_map(self.s(), kind))
    }

    /// A wire's edges (kind 4) or vertices (kind 5) in the order they connect.
    fn ordered(&self, kind: i32) -> Vec<Shape> {
        from_map(&ex::ex_ordered(self.s(), kind))
    }

    fn outer_wire(&self) -> PyResult<Shape> {
        take(ex::ex_outer_wire(self.s()), "outer wire")
    }

    /// The shapes directly inside this one (a compound's members).
    fn children(&self) -> Vec<Shape> {
        from_map(&ex::ex_children(self.s()))
    }

    fn count(&self, kind: i32) -> i32 {
        ex::ex_count(self.s(), kind)
    }

    // ------------------------------------------------------------ operations

    /// Boolean with tools: op 0 fuse, 1 cut, 2 common.
    #[pyo3(signature = (op, tools, fuzzy=0.0, parallel=true))]
    fn boolean(&self, op: i32, tools: Vec<PyRef<Shape>>, fuzzy: f64, parallel: bool) -> PyResult<Shape> {
        let mut args = new_list_of_shape();
        args.pin_mut().Append(self.s());
        take(ex::ex_boolean_opts(&args, &list(&tools), op, fuzzy, parallel), ["fuse", "cut", "common"][op.clamp(0, 2) as usize])
    }

    /// Merge coplanar faces and collinear edges (remove splitter).
    fn unify(&self) -> PyResult<Shape> {
        take(ex::ex_unify(self.s()), "remove splitter")
    }

    fn fillet(&self, edges: Vec<PyRef<Shape>>, r1: f64, r2: f64) -> PyResult<Shape> {
        take(ex::ex_fillet(self.s(), &list(&edges), r1, r2), "fillet")
    }

    fn chamfer(&self, edges: Vec<PyRef<Shape>>, d: f64) -> PyResult<Shape> {
        take(ex::ex_chamfer_faces(self.s(), &list(&edges), d), "chamfer")
    }

    fn extrude(&self, v: V3) -> PyResult<Shape> {
        take(ex::ex_prism(self.s(), v.0, v.1, v.2), "extrude")
    }

    /// Revolve about the axis through `o` along `d`, by `angle` degrees.
    fn revolve(&self, o: V3, d: V3, angle: f64) -> PyResult<Shape> {
        take(ex::ex_revol(self.s(), o.0, o.1, o.2, d.0, d.1, d.2, angle.to_radians()), "revolve")
    }

    /// The same shape at a new location (rigid transform; the geometry is shared).
    fn moved(&self, m: M34) -> PyResult<Shape> {
        take(ex::ex_moved(self.s(), &m), "move")
    }

    /// The shape with its geometry transformed (rigid).
    fn transformed(&self, m: M34) -> PyResult<Shape> {
        take(ex::ex_transform(self.s(), &m), "transform")
    }

    /// The shape with a general affine transform (scaling allowed).
    fn gtransformed(&self, m: M34) -> PyResult<Shape> {
        take(ex::ex_gtransform(self.s(), &m), "transform")
    }

    fn mirrored(&self, p: V3, n: V3) -> PyResult<Shape> {
        take(ex::ex_mirror(self.s(), p.0, p.1, p.2, n.0, n.1, n.2), "mirror")
    }

    /// A deep copy (new geometry).
    fn copy(&self) -> PyResult<Shape> {
        take(ex::ex_copy(self.s()), "copy")
    }

    /// 2D offset of a planar face: out for d > 0, in for d < 0; corners rounded (`arc`) or extended.
    fn offset2d(&self, d: f64, arc: bool) -> PyResult<Shape> {
        take(ex::ex_offset2d(self.s(), d, arc), "2D offset")
    }

    fn fix(&self) -> PyResult<Shape> {
        take(ex::ex_fixed(self.s()), "fix")
    }

    // ------------------------------------------------------------ queries

    /// (xmin, ymin, zmin, xmax, ymax, zmax), or None for an empty shape. `optimal` uses the exact geometry,
    /// otherwise the box FreeCAD reports (tolerances included, no gap).
    #[pyo3(signature = (optimal=false))]
    fn bbox(&self, optimal: bool) -> Option<(f64, f64, f64, f64, f64, f64)> {
        let b = if optimal { ex::ex_bbox(self.s(), true) } else { ex::ex_bbox_tight(self.s()) };
        (b.len() == 6).then(|| (b[0], b[1], b[2], b[3], b[4], b[5]))
    }

    /// (mass, centre) for kind 0 volume, 1 area, 2 length.
    fn props(&self, kind: i32) -> PyResult<(f64, V3)> {
        let v = ex::ex_mass_props(self.s(), kind);
        if v.len() < 4 {
            return Err(fail("mass properties"));
        }
        Ok((v[0], (v[1], v[2], v[3])))
    }

    /// Inertia tensor (row-major 3 × 3, about the centre) for unit density.
    fn inertia(&self) -> PyResult<Vec<f64>> {
        let v = ex::ex_mass_props(self.s(), 0);
        if v.len() < 13 {
            return Err(fail("mass properties"));
        }
        Ok(v[4..13].to_vec())
    }

    fn point(&self) -> PyResult<V3> {
        let v = ex::ex_vertex_point(self.s());
        if v.len() < 3 {
            return Err(fail("vertex"));
        }
        Ok((v[0], v[1], v[2]))
    }

    /// An edge: (curve type, length, start, middle, end, circle) where circle is (centre, axis, radius)
    /// or None. Curve types follow GeomAbs_CurveType: 0 line, 1 circle, 2 ellipse, … 6 B-spline.
    fn edge_info(&self) -> PyResult<(i32, f64, V3, V3, V3, Option<(V3, V3, f64)>)> {
        let v = ex::ex_edge_info(self.s());
        if v.len() < 11 {
            return Err(fail("edge query"));
        }
        let c = (v.len() >= 18).then(|| ((v[11], v[12], v[13]), (v[14], v[15], v[16]), v[17]));
        Ok((v[0] as i32, v[1], (v[2], v[3], v[4]), (v[5], v[6], v[7]), (v[8], v[9], v[10]), c))
    }

    /// A face near point p: (surface type, normal, nearest point, axis) where axis is (location,
    /// direction, radius) for planes, cylinders, cones, spheres and tori. Types follow GeomAbs_SurfaceType:
    /// 0 plane, 1 cylinder, 2 cone, 3 sphere, 4 torus, … 6 B-spline.
    fn face_info(&self, p: V3) -> PyResult<(i32, V3, V3, Option<(V3, V3, f64)>)> {
        let v = ex::ex_face_at(self.s(), p.0, p.1, p.2);
        if v.len() < 7 {
            return Err(fail("face query"));
        }
        let ax = (v.len() >= 14).then(|| ((v[7], v[8], v[9]), (v[10], v[11], v[12]), v[13]));
        Ok((v[0] as i32, (v[1], v[2], v[3]), (v[4], v[5], v[6]), ax))
    }

    /// (u, v) on the face's surface nearest p.
    fn surface_uv(&self, p: V3) -> PyResult<(f64, f64)> {
        let v = ex::ex_surface_uv(self.s(), p.0, p.1, p.2);
        if v.len() < 2 {
            return Err(fail("surface parameter"));
        }
        Ok((v[0], v[1]))
    }

    /// Whether (u, v) is inside the face or on its boundary.
    #[pyo3(signature = (u, v, tol=1e-7))]
    fn has_uv(&self, u: f64, v: f64, tol: f64) -> bool {
        ex::ex_face_has_uv(self.s(), u, v, tol)
    }

    /// Classify points against a solid: 0 in, 1 out, 2 on (within tol), 3 unknown.
    fn classify(&self, pts: Vec<V3>, tol: f64) -> Vec<i32> {
        ex::ex_classify(self.s(), &flat(&pts), tol)
    }

    /// (distance, [(point on self, point on other), …]).
    fn distance(&self, other: PyRef<Shape>) -> PyResult<(f64, Vec<(V3, V3)>)> {
        let v = ex::ex_distance(self.s(), other.s());
        if v.is_empty() {
            return Err(fail("distance"));
        }
        let sols = v[1..].chunks_exact(6).map(|c| ((c[0], c[1], c[2]), (c[3], c[4], c[5]))).collect();
        Ok((v[0], sols))
    }

    /// Section with the plane n·x = d, as a compound of wires.
    fn slice(&self, n: V3, d: f64) -> PyResult<Shape> {
        take(ex::ex_slice(self.s(), n.0, n.1, n.2, d), "slice")
    }

    /// Points along an edge: mode 0 `value` points evenly by length, 1 uniform deflection, 2 quasi-uniform.
    fn discretize(&self, mode: i32, value: f64) -> Vec<V3> {
        triples(&ex::ex_discretize(self.s(), mode, value))
    }

    /// Points along an edge within `deflection` of it (quasi-uniform).
    fn edge_points(&self, deflection: f64) -> Vec<V3> {
        triples(&ex::ex_edge_points(self.s(), deflection))
    }

    /// The edges as polylines within `deflection` mm (seams and degenerate edges left out).
    fn edge_polylines(&self, deflection: f64) -> Vec<Vec<V3>> {
        let e = ex::ex_edge_polylines(self.s(), deflection);
        let mut out = vec![];
        let mut i = 1;
        while i < e.len() {
            let n = e[i] as usize;
            out.push(triples(&e[i + 1..i + 1 + 3 * n]));
            i += 1 + 3 * n;
        }
        out
    }

    /// Triangulate: (vertices [x, y, z, …], triangles [i, j, k, …], face number per triangle). Angular
    /// deflection in radians.
    #[pyo3(signature = (linear, angular, relative=false))]
    fn triangulate(&self, linear: f64, angular: f64, relative: bool) -> PyResult<(Vec<f64>, Vec<u32>, Vec<u32>)> {
        let v = ex::ex_triangulate(self.s(), linear, angular, relative);
        if v.len() < 2 {
            return Err(fail("triangulate"));
        }
        let (nv, nt) = (v[0] as usize, v[1] as usize);
        let verts = v[2..2 + 3 * nv].to_vec();
        let tris = v[2 + 3 * nv..2 + 3 * nv + 3 * nt].iter().map(|x| *x as u32).collect();
        let faces = v[2 + 3 * nv + 3 * nt..].iter().map(|x| *x as u32).collect();
        Ok((verts, tris, faces))
    }

    /// Tessellate for rendering (deflections: linear mm, angular radians), with the edges as polylines.
    #[pyo3(signature = (linear, angular, edges=true))]
    fn mesh(&self, py: Python<'_>, linear: f64, angular: f64, edges: bool) -> PyResult<render::Mesh> {
        let v = ex::ex_triangulate(self.s(), linear, angular, false);
        if v.len() < 2 {
            return Err(fail("triangulate"));
        }
        let (nv, nt) = (v[0] as usize, v[1] as usize);
        let tris: Vec<u32> = v[2 + 3 * nv..2 + 3 * nv + 3 * nt].iter().map(|x| *x as u32).collect();
        let faces: Vec<u32> = v[2 + 3 * nv + 3 * nt..].iter().map(|x| *x as u32).collect();
        let mut lines = vec![];
        if edges {
            let e = ex::ex_edge_polylines(self.s(), linear);
            let mut i = 1;
            while i < e.len() {
                let n = e[i] as usize;
                lines.push(e[i + 1..i + 1 + 3 * n].chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect());
                i += 1 + 3 * n;
            }
        }
        let _ = py;
        Ok(render::Mesh::build(&v[2..2 + 3 * nv], &tris, &faces, lines))
    }

    /// Write a binary STL (deflections: linear mm, angular radians).
    fn write_stl(&self, path: &str, linear: f64, angular: f64) -> PyResult<()> {
        if ex::ex_write_stl(self.s(), path, linear, angular) { Ok(()) } else { Err(fail("STL export")) }
    }

    fn __repr__(&self) -> String {
        let t = ["Compound", "CompSolid", "Solid", "Shell", "Face", "Wire", "Edge", "Vertex", "Shape"];
        let k = self.shape_type();
        format!("<Shape {}>", if k < 0 { "null" } else { t[k as usize] })
    }
}

// ---------------------------------------------------------------- construction

#[pyfunction]
fn make_box(p: V3, d: V3, l: f64, w: f64, h: f64) -> PyResult<Shape> {
    take(ex::ex_make_box(p.0, p.1, p.2, d.0, d.1, d.2, l, w, h), "box")
}

#[pyfunction]
#[pyo3(signature = (p, d, r, h, angle=360.0))]
fn make_cylinder(p: V3, d: V3, r: f64, h: f64, angle: f64) -> PyResult<Shape> {
    take(ex::ex_make_cylinder(p.0, p.1, p.2, d.0, d.1, d.2, r, h, angle), "cylinder")
}

#[pyfunction]
#[pyo3(signature = (p, d, r1, r2, h, angle=360.0))]
fn make_cone(p: V3, d: V3, r1: f64, r2: f64, h: f64, angle: f64) -> PyResult<Shape> {
    take(ex::ex_make_cone(p.0, p.1, p.2, d.0, d.1, d.2, r1, r2, h, angle), "cone")
}

#[pyfunction]
#[pyo3(signature = (p, d, r, a1=-90.0, a2=90.0, a3=360.0))]
fn make_sphere(p: V3, d: V3, r: f64, a1: f64, a2: f64, a3: f64) -> PyResult<Shape> {
    take(ex::ex_make_sphere(p.0, p.1, p.2, d.0, d.1, d.2, r, a1, a2, a3), "sphere")
}

#[pyfunction]
fn vertex(p: V3) -> PyResult<Shape> {
    take(ex::ex_vertex(p.0, p.1, p.2), "vertex")
}

#[pyfunction]
#[pyo3(signature = (pts, close=false))]
fn polygon(pts: Vec<V3>, close: bool) -> PyResult<Shape> {
    take(ex::ex_polygon(&flat(&pts), close), "polygon")
}

#[pyfunction]
fn segment(a: V3, b: V3) -> PyResult<Shape> {
    take(ex::ex_segment(a.0, a.1, a.2, b.0, b.1, b.2), "line segment")
}

#[pyfunction]
fn arc3(a: V3, b: V3, c: V3) -> PyResult<Shape> {
    take(ex::ex_arc3(&flat(&[a, b, c])), "arc")
}

/// Wire from edges and wires, in order.
#[pyfunction]
fn wire(parts: Vec<PyRef<Shape>>) -> PyResult<Shape> {
    take(ex::ex_wire_from(&list(&parts)), "wire")
}

/// Face bounded by a closed wire, with holes.
#[pyfunction]
#[pyo3(signature = (outer, holes=vec![]))]
fn face(outer: PyRef<Shape>, holes: Vec<PyRef<Shape>>) -> PyResult<Shape> {
    take(ex::ex_face_from_wire(outer.s(), &list(&holes)), "face")
}

#[pyfunction]
fn compound(shapes: Vec<PyRef<Shape>>) -> PyResult<Shape> {
    take(ex::ex_compound(&list(&shapes)), "compound")
}

#[pyfunction]
#[pyo3(signature = (sections, solid=true, ruled=false, check=false))]
fn loft(sections: Vec<PyRef<Shape>>, solid: bool, ruled: bool, check: bool) -> PyResult<Shape> {
    take(ex::ex_loft2(&list(&sections), solid, ruled, check), "loft")
}

/// Volume shared by each pair of shapes (a[i], b[i]), computed on several threads: interference checks over
/// many parts. A failed boolean gives -1.
#[pyfunction]
#[pyo3(signature = (a, b, threads=0))]
fn common_volumes(py: Python<'_>, a: Vec<PyRef<Shape>>, b: Vec<PyRef<Shape>>, threads: i32) -> PyResult<Vec<f64>> {
    if a.len() != b.len() {
        return Err(pyo3::exceptions::PyValueError::new_err("common_volumes: a and b differ in length"));
    }
    let (la, lb) = (list(&a), list(&b));
    struct Lists(UniquePtr<TopTools_ListOfShape>, UniquePtr<TopTools_ListOfShape>);
    unsafe impl Send for Lists {}
    let l = Lists(la, lb);
    Ok(py.detach(move || {
        let l = l;
        ex::ex_common_volumes(&l.0, &l.1, threads)
    }))
}

/// Write an assembly to STEP with names, colours and structure. nodes: (shape or None for an assembly,
/// parent index or -1, name, (r, g, b) or None); parents come before their children.
#[pyfunction]
#[allow(clippy::type_complexity)]
fn write_step(nodes: Vec<(Option<PyRef<Shape>>, i32, String, Option<V3>)>, path: &str) -> PyResult<()> {
    let mut shapes = new_list_of_shape();
    let null = ex::ex_null_shape();
    let mut parents = vec![];
    let mut names = vec![];
    let mut colors = vec![];
    for (s, p, n, c) in &nodes {
        shapes.pin_mut().Append(match s {
            Some(s) => s.s(),
            None => &null,
        });
        parents.push(*p);
        names.push(n.clone());
        let c = c.unwrap_or((-1.0, -1.0, -1.0));
        colors.extend([c.0, c.1, c.2]);
    }
    if ex::ex_write_step_assembly(&shapes, &parents, &names, &colors, path) { Ok(()) } else { Err(fail("STEP export")) }
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("OCCError", m.py().get_type::<OCCError>())?;
    m.add_class::<Shape>()?;
    m.add_class::<render::Mesh>()?;
    m.add_class::<fea::FeModel>()?;
    m.add_class::<web::WebMesh>()?;
    m.add_function(wrap_pyfunction!(render::render_meshes, m)?)?;
    for f in [
        wrap_pyfunction!(make_box, m)?,
        wrap_pyfunction!(make_cylinder, m)?,
        wrap_pyfunction!(make_cone, m)?,
        wrap_pyfunction!(make_sphere, m)?,
        wrap_pyfunction!(vertex, m)?,
        wrap_pyfunction!(polygon, m)?,
        wrap_pyfunction!(segment, m)?,
        wrap_pyfunction!(arc3, m)?,
        wrap_pyfunction!(wire, m)?,
        wrap_pyfunction!(face, m)?,
        wrap_pyfunction!(compound, m)?,
        wrap_pyfunction!(loft, m)?,
        wrap_pyfunction!(write_step, m)?,
        wrap_pyfunction!(common_volumes, m)?,
        wrap_pyfunction!(web::web_mesh, m)?,
        wrap_pyfunction!(web::web_write, m)?,
    ] {
        m.add_function(f)?;
    }
    Ok(())
}
