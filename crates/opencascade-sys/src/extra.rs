//! Exception-safe OpenCascade operations added for rCAD (see include/extra.hxx).
pub use inner::*;

#[cxx::bridge]
mod inner {
    unsafe extern "C++" {
        include!("opencascade-sys/include/extra.hxx");

        type TopoDS_Shape = crate::topo_ds::TopoDS_Shape;
        type TopTools_ListOfShape = crate::top_tools::TopTools_ListOfShape;
        type TopTools_IndexedMapOfShape = crate::top_tools::TopTools_IndexedMapOfShape;

        pub fn ex_last_error() -> String;
        pub fn ex_fixed(s: &TopoDS_Shape) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_unify(s: &TopoDS_Shape) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_mesh(s: &TopoDS_Shape, linear: f64, angular: f64) -> bool;
        pub fn ex_mesh_careful(s: &TopoDS_Shape, linear: f64, angular: f64, level: i32) -> bool;
        pub fn ex_feature_edges(s: &TopoDS_Shape, angle: f64, faceted_from: i32) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_invalid_spots(s: &TopoDS_Shape) -> Vec<f64>;
        pub fn ex_heal(s: &TopoDS_Shape, tol: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_mesh_solid(pts: &[f64], faces: &[u32]) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_boolean(a: &TopoDS_Shape, b: &TopoDS_Shape, op: i32) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_prism(profile: &TopoDS_Shape, dx: f64, dy: f64, dz: f64) -> UniquePtr<TopoDS_Shape>;
        #[allow(clippy::too_many_arguments)]
        pub fn ex_revol(profile: &TopoDS_Shape, ox: f64, oy: f64, oz: f64, dx: f64, dy: f64, dz: f64, angle: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_fillet(s: &TopoDS_Shape, edges: &TopTools_ListOfShape, r1: f64, r2: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_chamfer(s: &TopoDS_Shape, edges: &TopTools_ListOfShape, d: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_shell(s: &TopoDS_Shape, faces: &TopTools_ListOfShape, t: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_thicken(s: &TopoDS_Shape, t: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_reversed(s: &TopoDS_Shape) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_hollow(s: &TopoDS_Shape, t: f64) -> UniquePtr<TopoDS_Shape>;
        #[allow(clippy::too_many_arguments)]
        pub fn ex_draft(s: &TopoDS_Shape, faces: &TopTools_ListOfShape, dx: f64, dy: f64, dz: f64, angle: f64, px: f64, py: f64, pz: f64, nx: f64, ny: f64, nz: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_sweep(profile_wire: &TopoDS_Shape, spine: &TopoDS_Shape, frenet: bool) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_loft(wires: &TopTools_ListOfShape, solid: bool, ruled: bool) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_transform(s: &TopoDS_Shape, m: &[f64]) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_mirror(s: &TopoDS_Shape, px: f64, py: f64, pz: f64, nx: f64, ny: f64, nz: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_delete_faces(s: &TopoDS_Shape, faces: &TopTools_ListOfShape) -> UniquePtr<TopoDS_Shape>;
        #[allow(clippy::too_many_arguments)]
        pub fn ex_helix(ox: f64, oy: f64, oz: f64, dx: f64, dy: f64, dz: f64, xx: f64, xy: f64, xz: f64, radius: f64, pitch: f64, turns: f64, left: bool) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_spline(pts: &[f64], closed: bool) -> UniquePtr<TopoDS_Shape>;
        #[allow(clippy::too_many_arguments)]
        pub fn ex_ellipse(cx: f64, cy: f64, cz: f64, nx: f64, ny: f64, nz: f64, xx: f64, xy: f64, xz: f64, rmaj: f64, rmin: f64, a0: f64, a1: f64, full: bool) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_wire(edges: &TopTools_ListOfShape) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_face(outer: &TopoDS_Shape, holes: &TopTools_ListOfShape) -> UniquePtr<TopoDS_Shape>;
        #[allow(clippy::too_many_arguments)]
        pub fn ex_hlr(s: &TopoDS_Shape, dx: f64, dy: f64, dz: f64, ux: f64, uy: f64, uz: f64, kind: i32) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_is_valid(s: &TopoDS_Shape) -> bool;
        pub fn ex_compound(shapes: &TopTools_ListOfShape) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_chamfer2(s: &TopoDS_Shape, edges: &TopTools_ListOfShape, d1: f64, d2: f64, mode: i32) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_offset_faces(s: &TopoDS_Shape, faces: &TopTools_ListOfShape, d: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_sew(shapes: &TopTools_ListOfShape, tol: f64, solid: bool) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_fill(edges: &TopTools_ListOfShape) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_offset_shape(s: &TopoDS_Shape, d: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_mass_props(s: &TopoDS_Shape, kind: i32) -> Vec<f64>;
        pub fn ex_classify(s: &TopoDS_Shape, pts: &[f64], tol: f64) -> Vec<i32>;
        pub fn ex_count(s: &TopoDS_Shape, kind: i32) -> i32;
        pub fn ex_sub(s: &TopoDS_Shape, kind: i32, i: i32) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_edge_faces(s: &TopoDS_Shape) -> Vec<i32>;
        pub fn ex_face_at(f: &TopoDS_Shape, x: f64, y: f64, z: f64) -> Vec<f64>;
        pub fn ex_edge_info(e: &TopoDS_Shape) -> Vec<f64>;
        pub fn ex_edge_points(e: &TopoDS_Shape, deflection: f64) -> Vec<f64>;
        pub fn ex_is_same(a: &TopoDS_Shape, b: &TopoDS_Shape) -> bool;
        pub fn ex_bbox(s: &TopoDS_Shape, optimal: bool) -> Vec<f64>;
        pub fn ex_boolean_n(args: &TopTools_ListOfShape, tools: &TopTools_ListOfShape, op: i32, fuzzy: f64, unify: bool) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_offset2d(f: &TopoDS_Shape, d: f64, arc: bool) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_face_towards(f: &TopoDS_Shape, nx: f64, ny: f64, nz: f64) -> UniquePtr<TopoDS_Shape>;

        // scripting from Python
        #[allow(clippy::too_many_arguments)]
        pub fn ex_make_box(px: f64, py: f64, pz: f64, dx: f64, dy: f64, dz: f64, l: f64, w: f64, h: f64) -> UniquePtr<TopoDS_Shape>;
        #[allow(clippy::too_many_arguments)]
        pub fn ex_make_cylinder(px: f64, py: f64, pz: f64, dx: f64, dy: f64, dz: f64, r: f64, h: f64, angle: f64) -> UniquePtr<TopoDS_Shape>;
        #[allow(clippy::too_many_arguments)]
        pub fn ex_make_cone(px: f64, py: f64, pz: f64, dx: f64, dy: f64, dz: f64, r1: f64, r2: f64, h: f64, angle: f64) -> UniquePtr<TopoDS_Shape>;
        #[allow(clippy::too_many_arguments)]
        pub fn ex_make_sphere(px: f64, py: f64, pz: f64, dx: f64, dy: f64, dz: f64, r: f64, a1: f64, a2: f64, a3: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_vertex(x: f64, y: f64, z: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_polygon(pts: &[f64], close: bool) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_segment(ax: f64, ay: f64, az: f64, bx: f64, by: f64, bz: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_arc3(p: &[f64]) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_wire_from(parts: &TopTools_ListOfShape) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_face_from_wire(outer: &TopoDS_Shape, holes: &TopTools_ListOfShape) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_map(s: &TopoDS_Shape, kind: i32) -> UniquePtr<TopTools_IndexedMapOfShape>;
        pub fn ex_children(s: &TopoDS_Shape) -> UniquePtr<TopTools_IndexedMapOfShape>;
        pub fn ex_shape_type(s: &TopoDS_Shape) -> i32;
        pub fn ex_copy(s: &TopoDS_Shape) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_moved(s: &TopoDS_Shape, m: &[f64]) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_gtransform(s: &TopoDS_Shape, m: &[f64]) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_bbox_tight(s: &TopoDS_Shape) -> Vec<f64>;
        pub fn ex_vertex_point(v: &TopoDS_Shape) -> Vec<f64>;
        pub fn ex_distance(a: &TopoDS_Shape, b: &TopoDS_Shape) -> Vec<f64>;
        pub fn ex_slice(s: &TopoDS_Shape, nx: f64, ny: f64, nz: f64, d: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_discretize(e: &TopoDS_Shape, mode: i32, value: f64) -> Vec<f64>;
        pub fn ex_surface_uv(f: &TopoDS_Shape, x: f64, y: f64, z: f64) -> Vec<f64>;
        pub fn ex_face_has_uv(f: &TopoDS_Shape, u: f64, v: f64, tol: f64) -> bool;
        pub fn ex_triangulate(s: &TopoDS_Shape, linear: f64, angular: f64, relative: bool) -> Vec<f64>;
        pub fn ex_loft2(wires: &TopTools_ListOfShape, solid: bool, ruled: bool, check: bool) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_boolean_opts(args: &TopTools_ListOfShape, tools: &TopTools_ListOfShape, op: i32, fuzzy: f64, parallel: bool) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_chamfer_faces(s: &TopoDS_Shape, edges: &TopTools_ListOfShape, d: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_null_shape() -> UniquePtr<TopoDS_Shape>;
        pub fn ex_catch_signals();
        pub fn ex_common_volumes(a: &TopTools_ListOfShape, b: &TopTools_ListOfShape, threads: i32) -> Vec<f64>;
        pub fn ex_messages_to_stderr();
        pub fn ex_write_step_assembly(shapes: &TopTools_ListOfShape, parents: &[i32], names: &[String], colors: &[f64], path: &str) -> bool;
        pub fn ex_edge_polylines(s: &TopoDS_Shape, deflection: f64) -> Vec<f64>;
        pub fn ex_outer_wire(f: &TopoDS_Shape) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_ordered(w: &TopoDS_Shape, kind: i32) -> UniquePtr<TopTools_IndexedMapOfShape>;
        pub fn ex_write_stl(s: &TopoDS_Shape, path: &str, linear: f64, angular: f64) -> bool;
        pub fn ex_step_parts(path: &str, deflection: f64, names: &mut Vec<String>) -> Vec<f64>;
    }
}
