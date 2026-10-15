//! Exception-safe OpenCascade operations added for cad-rust-app (see include/extra.hxx).
pub use inner::*;

#[cxx::bridge]
mod inner {
    unsafe extern "C++" {
        include!("opencascade-sys/include/extra.hxx");

        type TopoDS_Shape = crate::topo_ds::TopoDS_Shape;
        type TopTools_ListOfShape = crate::top_tools::TopTools_ListOfShape;

        pub fn ex_last_error() -> String;
        pub fn ex_fixed(s: &TopoDS_Shape) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_unify(s: &TopoDS_Shape) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_boolean(a: &TopoDS_Shape, b: &TopoDS_Shape, op: i32) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_prism(profile: &TopoDS_Shape, dx: f64, dy: f64, dz: f64) -> UniquePtr<TopoDS_Shape>;
        #[allow(clippy::too_many_arguments)]
        pub fn ex_revol(profile: &TopoDS_Shape, ox: f64, oy: f64, oz: f64, dx: f64, dy: f64, dz: f64, angle: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_fillet(s: &TopoDS_Shape, edges: &TopTools_ListOfShape, r1: f64, r2: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_chamfer(s: &TopoDS_Shape, edges: &TopTools_ListOfShape, d: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_shell(s: &TopoDS_Shape, faces: &TopTools_ListOfShape, t: f64) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_thicken(s: &TopoDS_Shape, t: f64) -> UniquePtr<TopoDS_Shape>;
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
        pub fn ex_sew(shapes: &TopTools_ListOfShape, tol: f64, solid: bool) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_fill(edges: &TopTools_ListOfShape) -> UniquePtr<TopoDS_Shape>;
        pub fn ex_offset_shape(s: &TopoDS_Shape, d: f64) -> UniquePtr<TopoDS_Shape>;
    }
}
