//! The scripting interface beyond the basic shapes in `script`: building solids from profiles,
//! picking faces and edges by their geometry, queries (mass properties, point classification,
//! tessellation), vectors and matrices, files, and export. Custom features get all of it, and
//! `cad-rust-app script file.rhai` runs a script with no window (see `run_file`).

use std::cell::RefCell;
use std::rc::Rc;

use glam::{DMat3, DVec3};
use opencascade::primitives::{Edge, Face, Shape, Wire};
use rhai::{Array, Dynamic, Engine, EvalAltResult, ImmutableString, Map};

use crate::occ;
use crate::script::S;

type Res<T> = Result<T, Box<EvalAltResult>>;

fn err<T>(m: impl Into<String>) -> Res<T> {
    Err(m.into().into())
}

fn occ_err<T>(r: Result<T, String>) -> Res<T> {
    r.map_err(|e| e.into())
}

fn wrap(s: Shape) -> S {
    S(Rc::new(s))
}

pub fn num(d: &Dynamic) -> Res<f64> {
    if let Ok(f) = d.as_float() {
        return Ok(f);
    }
    if let Ok(i) = d.as_int() {
        return Ok(i as f64);
    }
    err(format!("expected a number, got {}", d.type_name()))
}

// ---------------------------------------------------------------- vectors and matrices

/// A 3-vector.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct V3(pub DVec3);

/// A 3 × 3 matrix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct M3(pub DMat3);

/// A vector from a Vec3 or an [x, y, z] / [x, y] array.
pub fn v3(d: &Dynamic) -> Res<DVec3> {
    if let Some(v) = d.clone().try_cast::<V3>() {
        return Ok(v.0);
    }
    let Ok(a) = d.clone().into_array() else {
        return err(format!("expected a vector [x, y, z], got {}", d.type_name()));
    };
    match a.len() {
        2 => Ok(DVec3::new(num(&a[0])?, num(&a[1])?, 0.0)),
        3 => Ok(DVec3::new(num(&a[0])?, num(&a[1])?, num(&a[2])?)),
        _ => err("expected a vector [x, y, z]"),
    }
}

fn v3s(a: &Array) -> Res<Vec<DVec3>> {
    a.iter().map(v3).collect()
}

fn dv(v: DVec3) -> Dynamic {
    Dynamic::from(V3(v))
}

fn m3_of(d: &Dynamic) -> Res<DMat3> {
    if let Some(m) = d.clone().try_cast::<M3>() {
        return Ok(m.0);
    }
    let Ok(rows) = d.clone().into_array() else { return err("expected a 3 × 3 matrix") };
    if rows.len() != 3 {
        return err("expected a 3 × 3 matrix");
    }
    let r: Vec<DVec3> = rows.iter().map(v3).collect::<Res<_>>()?;
    Ok(DMat3::from_cols(r[0], r[1], r[2]).transpose())
}

fn rows(m: DMat3) -> Array {
    let t = m.transpose();
    [t.x_axis, t.y_axis, t.z_axis].iter().map(|r| Dynamic::from_array(vec![r.x.into(), r.y.into(), r.z.into()])).collect()
}

/// Rotation by `deg` degrees about `axis`.
fn rotation(axis: DVec3, deg: f64) -> DMat3 {
    DMat3::from_axis_angle(axis.normalize_or(DVec3::Z), deg.to_radians())
}

fn register_linalg(e: &mut Engine) {
    e.register_type_with_name::<V3>("Vec3");
    e.register_type_with_name::<M3>("Mat3");
    e.register_fn("vec3", |x: Dynamic, y: Dynamic, z: Dynamic| -> Res<V3> { Ok(V3(DVec3::new(num(&x)?, num(&y)?, num(&z)?))) });
    e.register_fn("vec3", |a: Dynamic| -> Res<V3> { Ok(V3(v3(&a)?)) });
    e.register_get("x", |v: &mut V3| v.0.x);
    e.register_get("y", |v: &mut V3| v.0.y);
    e.register_get("z", |v: &mut V3| v.0.z);
    e.register_set("x", |v: &mut V3, x: f64| v.0.x = x);
    e.register_set("y", |v: &mut V3, x: f64| v.0.y = x);
    e.register_set("z", |v: &mut V3, x: f64| v.0.z = x);
    e.register_fn("+", |a: V3, b: V3| V3(a.0 + b.0));
    e.register_fn("-", |a: V3, b: V3| V3(a.0 - b.0));
    e.register_fn("-", |a: V3| V3(-a.0));
    e.register_fn("*", |a: V3, k: f64| V3(a.0 * k));
    e.register_fn("*", |k: f64, a: V3| V3(a.0 * k));
    e.register_fn("*", |a: V3, k: i64| V3(a.0 * k as f64));
    e.register_fn("/", |a: V3, k: f64| V3(a.0 / k));
    e.register_fn("==", |a: V3, b: V3| a == b);
    e.register_fn("dot", |a: Dynamic, b: Dynamic| -> Res<f64> { Ok(v3(&a)?.dot(v3(&b)?)) });
    e.register_fn("cross", |a: Dynamic, b: Dynamic| -> Res<V3> { Ok(V3(v3(&a)?.cross(v3(&b)?))) });
    e.register_fn("norm", |a: Dynamic| -> Res<f64> { Ok(v3(&a)?.length()) });
    e.register_fn("normalize", |a: Dynamic| -> Res<V3> {
        let v = v3(&a)?;
        if v.length() < 1e-300 {
            return err("normalize: zero vector");
        }
        Ok(V3(v.normalize()))
    });
    e.register_fn("distance", |a: Dynamic, b: Dynamic| -> Res<f64> { Ok(v3(&a)?.distance(v3(&b)?)) });
    e.register_fn("to_array", |v: &mut V3| -> Array { vec![v.0.x.into(), v.0.y.into(), v.0.z.into()] });
    e.register_fn("to_string", |v: &mut V3| format!("vec3({}, {}, {})", v.0.x, v.0.y, v.0.z));
    e.register_fn("to_debug", |v: &mut V3| format!("vec3({}, {}, {})", v.0.x, v.0.y, v.0.z));

    e.register_fn("mat3", |r: Dynamic| -> Res<M3> { Ok(M3(m3_of(&r)?)) });
    e.register_fn("identity", || M3(DMat3::IDENTITY));
    e.register_fn("rotation", |axis: Dynamic, deg: Dynamic| -> Res<M3> { Ok(M3(rotation(v3(&axis)?, num(&deg)?))) });
    e.register_fn("outer", |a: Dynamic, b: Dynamic| -> Res<M3> {
        let (a, b) = (v3(&a)?, v3(&b)?);
        Ok(M3(DMat3::from_cols(a * b.x, a * b.y, a * b.z)))
    });
    e.register_fn("transpose", |m: &mut M3| M3(m.0.transpose()));
    e.register_fn("inverse", |m: &mut M3| -> Res<M3> {
        if m.0.determinant().abs() < 1e-300 {
            return err("the matrix is singular");
        }
        Ok(M3(m.0.inverse()))
    });
    e.register_fn("det", |m: &mut M3| m.0.determinant());
    e.register_fn("trace", |m: &mut M3| m.0.x_axis.x + m.0.y_axis.y + m.0.z_axis.z);
    e.register_fn("get", |m: &mut M3, i: i64, j: i64| -> Res<f64> {
        if !(0..3).contains(&i) || !(0..3).contains(&j) {
            return err("matrix index out of range");
        }
        Ok(m.0.col(j as usize)[i as usize])
    });
    e.register_fn("rows", |m: &mut M3| rows(m.0));
    e.register_fn("*", |a: M3, b: M3| M3(a.0 * b.0));
    e.register_fn("*", |a: M3, v: V3| V3(a.0 * v.0));
    e.register_fn("*", |a: M3, k: f64| M3(a.0 * k));
    e.register_fn("+", |a: M3, b: M3| M3(a.0 + b.0));
    e.register_fn("-", |a: M3, b: M3| M3(a.0 - b.0));
    e.register_fn("to_string", |m: &mut M3| format!("mat3({:?})", m.0.transpose().to_cols_array_2d()));
    e.register_fn("to_debug", |m: &mut M3| format!("mat3({:?})", m.0.transpose().to_cols_array_2d()));

    // balance helpers: a point against a line through `a` along `dir`
    e.register_fn("line_distance", |p: Dynamic, a: Dynamic, dir: Dynamic| -> Res<f64> { Ok(perp(v3(&p)?, v3(&a)?, v3(&dir)?)?.length()) });
    e.register_fn("line_offset", |p: Dynamic, a: Dynamic, dir: Dynamic| -> Res<V3> { Ok(V3(perp(v3(&p)?, v3(&a)?, v3(&dir)?)?)) });
    e.register_fn("line_offset", |p: Dynamic, a: Dynamic, dir: Dynamic, u: Dynamic, v: Dynamic| -> Res<Array> {
        let d = perp(v3(&p)?, v3(&a)?, v3(&dir)?)?;
        Ok(vec![d.dot(v3(&u)?).into(), d.dot(v3(&v)?).into()])
    });
}

/// The perpendicular from the line through `a` along `dir` to `p`.
fn perp(p: DVec3, a: DVec3, dir: DVec3) -> Res<DVec3> {
    if dir.length() < 1e-300 {
        return err("the line needs a direction");
    }
    let d = dir.normalize();
    let w = p - a;
    Ok(w - d * w.dot(d))
}

// ---------------------------------------------------------------- mass properties

/// Mass, centre of gravity and inertia tensor about the centre of gravity (g, mm, g·mm² for densities in g/mm³).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MassProps {
    pub mass: f64,
    pub cg: DVec3,
    pub inertia: DMat3,
}

impl MassProps {
    /// Inertia about a point `p` (parallel axis theorem).
    pub fn about(&self, p: DVec3) -> DMat3 {
        let d = self.cg - p;
        self.inertia + (DMat3::IDENTITY * d.length_squared() - DMat3::from_cols(d * d.x, d * d.y, d * d.z)) * self.mass
    }

    /// Moment of inertia about the line through `p` along `dir`.
    pub fn about_line(&self, p: DVec3, dir: DVec3) -> f64 {
        let u = dir.normalize_or(DVec3::Z);
        u.dot(self.about(p) * u)
    }

    pub fn combine(&self, o: &MassProps) -> MassProps {
        let mass = self.mass + o.mass;
        if mass <= 0.0 {
            return MassProps { mass: 0.0, cg: DVec3::ZERO, inertia: DMat3::ZERO };
        }
        let cg = (self.cg * self.mass + o.cg * o.mass) / mass;
        MassProps { mass, cg, inertia: self.about(cg) + o.about(cg) }
    }
}

/// Mass properties of a solid shape of uniform density.
pub fn mass_of(s: &Shape, density: f64) -> Result<MassProps, String> {
    let (v, c, i) = occ::mass_props(s, 0)?;
    Ok(MassProps { mass: v * density, cg: c, inertia: i * density })
}

fn register_mass(e: &mut Engine) {
    e.register_type_with_name::<MassProps>("MassProps");
    e.register_fn("mass_props", |s: S, density: Dynamic| -> Res<MassProps> { occ_err(mass_of(&s.0, num(&density)?)) });
    e.register_fn("mass_props", |mass: Dynamic, cg: Dynamic, inertia: Dynamic| -> Res<MassProps> { Ok(MassProps { mass: num(&mass)?, cg: v3(&cg)?, inertia: m3_of(&inertia)? }) });
    e.register_fn("point_mass", |mass: Dynamic, at: Dynamic| -> Res<MassProps> { Ok(MassProps { mass: num(&mass)?, cg: v3(&at)?, inertia: DMat3::ZERO }) });
    e.register_get("mass", |m: &mut MassProps| m.mass);
    e.register_get("cg", |m: &mut MassProps| V3(m.cg));
    e.register_get("inertia", |m: &mut MassProps| M3(m.inertia));
    e.register_fn("+", |a: MassProps, b: MassProps| a.combine(&b));
    e.register_fn("combine", |a: Array| -> Res<MassProps> {
        let mut acc = MassProps { mass: 0.0, cg: DVec3::ZERO, inertia: DMat3::ZERO };
        for x in &a {
            let Some(m) = x.clone().try_cast::<MassProps>() else { return err("combine: an array of MassProps") };
            acc = acc.combine(&m);
        }
        Ok(acc)
    });
    e.register_fn("translate", |m: &mut MassProps, v: Dynamic| -> Res<MassProps> { Ok(MassProps { cg: m.cg + v3(&v)?, ..*m }) });
    e.register_fn("rotate", |m: &mut MassProps, center: Dynamic, axis: Dynamic, deg: Dynamic| -> Res<MassProps> {
        let r = rotation(v3(&axis)?, num(&deg)?);
        let c = v3(&center)?;
        Ok(MassProps { mass: m.mass, cg: c + r * (m.cg - c), inertia: r * m.inertia * r.transpose() })
    });
    e.register_fn("scale_mass", |m: &mut MassProps, k: Dynamic| -> Res<MassProps> {
        let k = num(&k)?;
        Ok(MassProps { mass: m.mass * k, cg: m.cg, inertia: m.inertia * k })
    });
    e.register_fn("inertia_about", |m: &mut MassProps, p: Dynamic| -> Res<M3> { Ok(M3(m.about(v3(&p)?))) });
    e.register_fn("inertia_about", |m: &mut MassProps, p: Dynamic, dir: Dynamic| -> Res<f64> { Ok(m.about_line(v3(&p)?, v3(&dir)?)) });
    e.register_fn("to_map", |m: &mut MassProps| -> Map { mass_map(m) });
    e.register_fn("to_string", |m: &mut MassProps| format!("MassProps(mass {}, cg {:?})", m.mass, m.cg.to_array()));
    e.register_fn("to_debug", |m: &mut MassProps| format!("MassProps(mass {}, cg {:?})", m.mass, m.cg.to_array()));
}

fn mass_map(m: &MassProps) -> Map {
    let mut out = Map::new();
    out.insert("mass".into(), m.mass.into());
    out.insert("cg".into(), dv(m.cg));
    out.insert("inertia".into(), Dynamic::from(M3(m.inertia)));
    out
}

// ---------------------------------------------------------------- topology: faces and edges

/// The faces and edges of a shape, with which faces meet at each edge.
pub struct Topo {
    pub faces: Vec<Shape>,
    pub edges: Vec<Shape>,
    pub edge_faces: Vec<Vec<usize>>,
    pub face_edges: Vec<Vec<usize>>,
}

impl Topo {
    pub fn of(s: &Rc<Shape>) -> Topo {
        let faces: Vec<Shape> = crate::kernel::sub_shapes(s, occ::FACE);
        let edges: Vec<Shape> = crate::kernel::sub_shapes(s, occ::EDGE);
        let mut edge_faces = occ::edge_faces(s);
        edge_faces.resize(edges.len(), vec![]);
        let mut face_edges = vec![vec![]; faces.len()];
        for (ei, fs) in edge_faces.iter().enumerate() {
            for f in fs {
                if let Some(x) = face_edges.get_mut(*f) {
                    x.push(ei);
                }
            }
        }
        Topo { faces, edges, edge_faces, face_edges }
    }
}

#[derive(Clone)]
pub struct FaceRef {
    pub topo: Rc<Topo>,
    pub i: usize,
}

#[derive(Clone)]
pub struct EdgeRef {
    pub topo: Rc<Topo>,
    pub i: usize,
}

impl FaceRef {
    fn shape(&self) -> &Shape {
        &self.topo.faces[self.i]
    }
    fn face(&self) -> Face {
        Face::from_face(opencascade_sys::topo_ds::TopoDS::Face(&self.shape().inner))
    }
}

impl EdgeRef {
    fn shape(&self) -> &Shape {
        &self.topo.edges[self.i]
    }
    fn edge(&self) -> Edge {
        Edge::from_edge(opencascade_sys::topo_ds::TopoDS::Edge(&self.shape().inner))
    }
}

const SURFACES: [&str; 11] = ["plane", "cylinder", "cone", "sphere", "torus", "bezier", "bspline", "revolution", "extrusion", "offset", "other"];
const FILLET_HINT: &str = " (radius too large for the part?)";
const CURVES: [&str; 9] = ["line", "circle", "ellipse", "hyperbola", "parabola", "bezier", "bspline", "offset", "other"];

fn kind_name(list: &[&'static str], k: i32) -> &'static str {
    list.get(k.max(0) as usize).copied().unwrap_or("other")
}

fn faces_of(t: &Rc<Topo>, ids: &[usize]) -> Array {
    ids.iter().map(|i| Dynamic::from(FaceRef { topo: t.clone(), i: *i })).collect()
}

fn edges_of(t: &Rc<Topo>, ids: &[usize]) -> Array {
    ids.iter().map(|i| Dynamic::from(EdgeRef { topo: t.clone(), i: *i })).collect()
}

/// Edges out of an array of Edge values (or one Edge).
fn edge_list(d: &Dynamic) -> Res<Vec<Edge>> {
    if let Some(e) = d.clone().try_cast::<EdgeRef>() {
        return Ok(vec![e.edge()]);
    }
    let Ok(a) = d.clone().into_array() else { return err("expected an array of edges") };
    a.iter().map(|x| x.clone().try_cast::<EdgeRef>().map(|e| e.edge()).ok_or_else(|| "expected an array of edges".into())).collect()
}

fn face_list(d: &Dynamic) -> Res<Vec<Face>> {
    if let Some(f) = d.clone().try_cast::<FaceRef>() {
        return Ok(vec![f.face()]);
    }
    let Ok(a) = d.clone().into_array() else { return err("expected an array of faces") };
    a.iter().map(|x| x.clone().try_cast::<FaceRef>().map(|f| f.face()).ok_or_else(|| "expected an array of faces".into())).collect()
}

/// A shape from a Shape, Face or Edge value.
fn any_shape(d: &Dynamic) -> Res<Rc<Shape>> {
    if let Some(s) = d.clone().try_cast::<S>() {
        return Ok(s.0);
    }
    if let Some(f) = d.clone().try_cast::<FaceRef>() {
        return Ok(Rc::new(kdup(f.shape())));
    }
    if let Some(e) = d.clone().try_cast::<EdgeRef>() {
        return Ok(Rc::new(kdup(e.shape())));
    }
    err(format!("expected a shape, face or edge, got {}", d.type_name()))
}

fn kdup(s: &Shape) -> Shape {
    Shape::from_shape(&s.inner)
}

fn shapes(a: &Array) -> Res<Vec<Rc<Shape>>> {
    a.iter().map(any_shape).collect()
}

fn register_topology(e: &mut Engine) {
    e.register_type_with_name::<FaceRef>("Face");
    e.register_type_with_name::<EdgeRef>("Edge");
    e.register_fn("faces", |s: &mut S| -> Array {
        let t = Rc::new(Topo::of(&s.0));
        let ids: Vec<usize> = (0..t.faces.len()).collect();
        faces_of(&t, &ids)
    });
    e.register_fn("edges", |s: &mut S| -> Array {
        let t = Rc::new(Topo::of(&s.0));
        let ids: Vec<usize> = (0..t.edges.len()).collect();
        edges_of(&t, &ids)
    });
    e.register_fn("faces", |e: &mut EdgeRef| faces_of(&e.topo, &e.topo.edge_faces[e.i]));
    e.register_fn("edges", |f: &mut FaceRef| edges_of(&f.topo, &f.topo.face_edges[f.i]));
    e.register_fn("shape", |f: &mut FaceRef| wrap(kdup(f.shape())));
    e.register_fn("shape", |e: &mut EdgeRef| wrap(kdup(e.shape())));
    e.register_fn("index", |f: &mut FaceRef| f.i as i64);
    e.register_fn("index", |e: &mut EdgeRef| e.i as i64);
    e.register_fn("kind", |f: &mut FaceRef| -> Res<String> { Ok(kind_name(&SURFACES, occ_err(occ::face_at(f.shape(), DVec3::ZERO))?.kind).into()) });
    e.register_fn("kind", |e: &mut EdgeRef| -> Res<String> { Ok(kind_name(&CURVES, occ_err(occ::edge_info(e.shape()))?.kind).into()) });
    e.register_fn("normal", |f: &mut FaceRef, p: Dynamic| -> Res<V3> { Ok(V3(occ_err(occ::face_at(f.shape(), v3(&p)?))?.normal)) });
    e.register_fn("normal", |f: &mut FaceRef| -> Res<V3> {
        let c = f.face().center_of_mass();
        Ok(V3(occ_err(occ::face_at(f.shape(), c))?.normal))
    });
    e.register_fn("closest", |f: &mut FaceRef, p: Dynamic| -> Res<V3> { Ok(V3(occ_err(occ::face_at(f.shape(), v3(&p)?))?.point)) });
    e.register_fn("center", |f: &mut FaceRef| V3(f.face().center_of_mass()));
    e.register_fn("area", |f: &mut FaceRef| f.face().surface_area());
    // plane, cylinder, cone, sphere, torus: {point, axis, radius}
    e.register_fn("axis", |f: &mut FaceRef| -> Res<Dynamic> {
        let q = occ_err(occ::face_at(f.shape(), DVec3::ZERO))?;
        Ok(match q.axis {
            Some((p, d, r)) => {
                let mut m = Map::new();
                m.insert("point".into(), dv(p));
                m.insert("axis".into(), dv(d));
                m.insert("radius".into(), r.into());
                m.into()
            }
            None => Dynamic::UNIT,
        })
    });
    e.register_fn("length", |e: &mut EdgeRef| -> Res<f64> { Ok(occ_err(occ::edge_info(e.shape()))?.length) });
    e.register_fn("start", |e: &mut EdgeRef| -> Res<V3> { Ok(V3(occ_err(occ::edge_info(e.shape()))?.start)) });
    e.register_fn("end", |e: &mut EdgeRef| -> Res<V3> { Ok(V3(occ_err(occ::edge_info(e.shape()))?.end)) });
    e.register_fn("mid", |e: &mut EdgeRef| -> Res<V3> { Ok(V3(occ_err(occ::edge_info(e.shape()))?.mid)) });
    e.register_fn("circle", |e: &mut EdgeRef| -> Res<Dynamic> {
        Ok(match occ_err(occ::edge_info(e.shape()))?.circle {
            Some((c, a, r)) => {
                let mut m = Map::new();
                m.insert("center".into(), dv(c));
                m.insert("axis".into(), dv(a));
                m.insert("radius".into(), r.into());
                m.into()
            }
            None => Dynamic::UNIT,
        })
    });
    e.register_fn("points", |e: &mut EdgeRef, deflection: Dynamic| -> Res<Array> { Ok(occ::edge_points(e.shape(), num(&deflection)?).into_iter().map(dv).collect()) });
    e.register_fn("same", |a: Dynamic, b: Dynamic| -> Res<bool> { Ok(occ::is_same(&*any_shape(&a)?, &*any_shape(&b)?)) });
    e.register_fn("==", |a: FaceRef, b: FaceRef| occ::is_same(a.shape(), b.shape()));
    e.register_fn("==", |a: EdgeRef, b: EdgeRef| occ::is_same(a.shape(), b.shape()));
    e.register_fn("to_string", |f: &mut FaceRef| format!("Face#{}", f.i));
    e.register_fn("to_debug", |f: &mut FaceRef| format!("Face#{}", f.i));
    e.register_fn("to_string", |e: &mut EdgeRef| format!("Edge#{}", e.i));
    e.register_fn("to_debug", |e: &mut EdgeRef| format!("Edge#{}", e.i));

    // fillets and chamfers on chosen edges, all in one operation
    e.register_fn("fillet", |s: &mut S, edges: Array, r: Dynamic| -> Res<S> {
        let es = edge_list(&Dynamic::from_array(edges))?;
        let r = num(&r)?;
        if es.is_empty() {
            return Ok(s.clone());
        }
        Ok(wrap(occ_err(occ::fillet(&s.0, &es, r, r).and_then(|x| occ::checked(x, "fillet", FILLET_HINT)))?))
    });
    e.register_fn("fillet", |s: &mut S, edges: Array, r1: Dynamic, r2: Dynamic| -> Res<S> {
        let es = edge_list(&Dynamic::from_array(edges))?;
        if es.is_empty() {
            return Ok(s.clone());
        }
        Ok(wrap(occ_err(occ::fillet(&s.0, &es, num(&r1)?, num(&r2)?).and_then(|x| occ::checked(x, "fillet", FILLET_HINT)))?))
    });
    e.register_fn("chamfer", |s: &mut S, edges: Array, d: Dynamic| -> Res<S> {
        let es = edge_list(&Dynamic::from_array(edges))?;
        if es.is_empty() {
            return Ok(s.clone());
        }
        Ok(wrap(occ_err(occ::chamfer(&s.0, &es, num(&d)?).and_then(|x| occ::checked(x, "chamfer", " (distance too large for the part?)")))?))
    });
    // hollow out, removing the given faces (none: a closed hollow), walls `t` thick inside the
    // surface (negative: grown outside it)
    e.register_fn("shell", |s: &mut S, faces: Array, t: Dynamic| -> Res<S> {
        let fs = face_list(&Dynamic::from_array(faces))?;
        let t = num(&t)?;
        if t == 0.0 {
            return err("shell thickness must not be 0");
        }
        let r = if fs.is_empty() { occ::hollow(&s.0, t) } else { occ::shell(&s.0, &fs, -t) };
        Ok(wrap(occ_err(r.and_then(|x| occ::checked(x, "shell", " (thickness too large for the part?)")))?))
    });
}

// ---------------------------------------------------------------- building shapes

fn polygon_wire(pts: &[DVec3], closed: bool) -> Res<Wire> {
    if pts.len() < 2 {
        return err("a wire needs at least 2 points");
    }
    let mut edges = vec![];
    let n = if closed { pts.len() } else { pts.len() - 1 };
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        if (a - b).length() > 1e-9 {
            edges.push(Edge::segment(a, b));
        }
    }
    occ_err(occ::wire(&edges))
}

/// The wire of a value: a wire shape, or a face's outer wire.
fn wire_of(d: &Dynamic) -> Res<Wire> {
    let s = any_shape(d)?;
    if let Some(w) = s.as_wire() {
        return Ok(w);
    }
    if let Some(f) = s.as_face() {
        return Ok(f.outer_wire());
    }
    err("expected a wire or a face")
}

fn planar_face(outer: &Wire, holes: &[Wire]) -> Res<Shape> {
    Ok(Shape::from(occ_err(occ::face(outer, holes))?))
}

/// Faces of a shape turned to face along `n` (planar regions come back "facing up").
fn faces_towards(s: &Shape, n: DVec3) -> Res<Array> {
    let mut out = vec![];
    for f in crate::kernel::sub_shapes(s, occ::FACE) {
        out.push(Dynamic::from(wrap(occ_err(occ::face_towards(&f, n))?)));
    }
    Ok(out)
}

fn region_normal(a: &[Rc<Shape>]) -> DVec3 {
    a.first().and_then(|s| s.as_face()).map(|f| f.normal_at_center()).unwrap_or(DVec3::Z)
}

/// Boolean of planar regions: one face per connected piece, all facing the first region's way.
fn region_op(a: &[Rc<Shape>], b: &[Rc<Shape>], op: i32) -> Res<Array> {
    let n = region_normal(a);
    if a.is_empty() {
        return Ok(vec![]);
    }
    let r = if op == 0 {
        let all: Vec<&Shape> = a.iter().chain(b).map(|s| &**s).collect();
        if all.len() == 1 { kdup(all[0]) } else { occ_err(occ::boolean_n(&all[..1], &all[1..], 0, 0.0, true))? }
    } else {
        let args: Vec<&Shape> = a.iter().map(|s| &**s).collect();
        let tools: Vec<&Shape> = b.iter().map(|s| &**s).collect();
        let base = if args.len() == 1 { kdup(args[0]) } else { occ_err(occ::boolean_n(&args[..1], &args[1..], 0, 0.0, true))? };
        if tools.is_empty() {
            if op == 2 {
                return Ok(vec![]);
            }
            base
        } else {
            occ_err(occ::boolean_n(&[&base], &tools, op, 0.0, true))?
        }
    };
    faces_towards(&r, n)
}

fn shape_list(d: &Dynamic) -> Res<Vec<Rc<Shape>>> {
    match d.clone().into_array() {
        Ok(a) => shapes(&a),
        Err(_) => Ok(vec![any_shape(d)?]),
    }
}

fn register_build(e: &mut Engine) {
    e.register_fn("block", |lo: Dynamic, hi: Dynamic| -> Res<S> {
        let (a, b) = (v3(&lo)?, v3(&hi)?);
        let (a, b) = (a.min(b), a.max(b));
        if (b - a).min_element() <= 0.0 {
            return err("block needs a positive size");
        }
        Ok(wrap(Shape::box_from_corners(a, b)))
    });
    e.register_fn("cylinder", |base: Dynamic, axis: Dynamic, r: Dynamic, h: Dynamic| -> Res<S> {
        let (r, h) = (num(&r)?, num(&h)?);
        if r <= 0.0 || h <= 0.0 {
            return err("cylinder needs a positive radius and height");
        }
        Ok(wrap(Shape::cylinder(v3(&base)?, r, v3(&axis)?.normalize_or(DVec3::Z), h)))
    });
    e.register_fn("sphere_at", |c: Dynamic, r: Dynamic| -> Res<S> { Ok(wrap(Shape::sphere(num(&r)?).at(v3(&c)?).build())) });
    e.register_fn("cone", |base: Dynamic, axis: Dynamic, r1: Dynamic, r2: Dynamic, h: Dynamic| -> Res<S> {
        let c = Shape::cone().bottom_radius(num(&r1)?).top_radius(num(&r2)?).height(num(&h)?).build();
        let n = v3(&axis)?.normalize_or(DVec3::Z);
        Ok(wrap(occ_err(occ::transform(&c, z_to(n), v3(&base)?))?))
    });
    // wires and planar faces
    e.register_fn("wire", |pts: Array, closed: bool| -> Res<S> { Ok(wrap(Shape::from(polygon_wire(&v3s(&pts)?, closed)?))) });
    e.register_fn("arc", |a: Dynamic, b: Dynamic, c: Dynamic| -> Res<S> { Ok(wrap(Shape::from(occ_err(occ::wire(&[Edge::arc(v3(&a)?, v3(&b)?, v3(&c)?)]))?))) });
    e.register_fn("join", |parts: Array| -> Res<S> {
        // a wire from wires and edges laid end to end
        let mut edges = vec![];
        for p in &parts {
            let s = any_shape(p)?;
            edges.extend(crate::kernel::unique_edges(&s));
        }
        Ok(wrap(Shape::from(occ_err(occ::wire(&edges))?)))
    });
    e.register_fn("polygon", |pts: Array| -> Res<S> { Ok(wrap(planar_face(&polygon_wire(&v3s(&pts)?, true)?, &[])?)) });
    e.register_fn("circle", |c: Dynamic, r: Dynamic| -> Res<S> {
        let w = occ_err(occ::wire(&[Edge::circle(v3(&c)?, DVec3::Z, num(&r)?)]))?;
        Ok(wrap(planar_face(&w, &[])?))
    });
    e.register_fn("circle", |c: Dynamic, n: Dynamic, r: Dynamic| -> Res<S> {
        let w = occ_err(occ::wire(&[Edge::circle(v3(&c)?, v3(&n)?.normalize_or(DVec3::Z), num(&r)?)]))?;
        Ok(wrap(planar_face(&w, &[])?))
    });
    e.register_fn("face", |outer: Dynamic| -> Res<S> { Ok(wrap(planar_face(&wire_of(&outer)?, &[])?)) });
    e.register_fn("face", |outer: Dynamic, holes: Array| -> Res<S> {
        let hs: Vec<Wire> = holes.iter().map(wire_of).collect::<Res<_>>()?;
        Ok(wrap(planar_face(&wire_of(&outer)?, &hs)?))
    });
    e.register_fn("rounded_rect", |center: Dynamic, w: Dynamic, h: Dynamic, r: Dynamic| -> Res<S> {
        let (c, w, h, r) = (v3(&center)?, num(&w)?, num(&h)?, num(&r)?);
        let base = planar_face(&polygon_wire(&[c + DVec3::new(-w / 2.0, -h / 2.0, 0.0), c + DVec3::new(w / 2.0, -h / 2.0, 0.0), c + DVec3::new(w / 2.0, h / 2.0, 0.0), c + DVec3::new(-w / 2.0, h / 2.0, 0.0)], true)?, &[])?;
        if r <= 0.0 {
            return Ok(wrap(base));
        }
        // round the corners: in by r and back out with arcs
        let a = occ_err(occ::offset2d(&base, -r, true))?;
        let f = crate::kernel::sub_shapes(&a, occ::FACE).into_iter().next().ok_or("rounded_rect: the radius is too large")?;
        let b = occ_err(occ::offset2d(&f, r, true))?;
        Ok(wrap(crate::kernel::sub_shapes(&b, occ::FACE).into_iter().next().ok_or("rounded_rect failed")?))
    });
    // 2D regions
    e.register_fn("offset2d", |f: Dynamic, d: Dynamic| -> Res<Array> {
        let s = any_shape(&f)?;
        let n = s.as_face().map(|f| f.normal_at_center()).unwrap_or(DVec3::Z);
        let r = occ_err(occ::offset2d(&s, num(&d)?, true))?;
        faces_towards(&r, n)
    });
    e.register_fn("offset2d", |f: Dynamic, d: Dynamic, arcs: bool| -> Res<Array> {
        let s = any_shape(&f)?;
        let n = s.as_face().map(|f| f.normal_at_center()).unwrap_or(DVec3::Z);
        let r = occ_err(occ::offset2d(&s, num(&d)?, arcs))?;
        faces_towards(&r, n)
    });
    e.register_fn("region_union", |a: Dynamic| -> Res<Array> { region_op(&shape_list(&a)?, &[], 0) });
    e.register_fn("region_union", |a: Dynamic, b: Dynamic| -> Res<Array> { region_op(&shape_list(&a)?, &shape_list(&b)?, 0) });
    e.register_fn("region_cut", |a: Dynamic, b: Dynamic| -> Res<Array> { region_op(&shape_list(&a)?, &shape_list(&b)?, 1) });
    e.register_fn("region_common", |a: Dynamic, b: Dynamic| -> Res<Array> { region_op(&shape_list(&a)?, &shape_list(&b)?, 2) });
    // solids from profiles
    e.register_fn("extrude", |f: Dynamic, v: Dynamic| -> Res<S> { Ok(wrap(occ_err(occ::prism(&*any_shape(&f)?, v3(&v)?))?)) });
    e.register_fn("loft", |profiles: Array, ruled: bool| -> Res<S> {
        let ws: Vec<Wire> = profiles.iter().map(wire_of).collect::<Res<_>>()?;
        if ws.len() < 2 {
            return err("loft needs at least 2 profiles");
        }
        Ok(wrap(occ_err(occ::loft(&ws, ruled))?))
    });
    e.register_fn("revolve", |f: Dynamic, origin: Dynamic, axis: Dynamic, deg: Dynamic| -> Res<S> {
        Ok(wrap(occ_err(occ::revolve(&*any_shape(&f)?, v3(&origin)?, v3(&axis)?.normalize_or(DVec3::Z), num(&deg)?.to_radians()))?))
    });
    // booleans with many arguments; `fuzzy` (mm) treats gaps smaller than it as touching
    let fuse = |items: &[Rc<Shape>], fuzzy: f64| -> Res<S> {
        match items.len() {
            0 => err("fuse needs shapes"),
            1 => Ok(S(items[0].clone())),
            _ => {
                let all: Vec<&Shape> = items.iter().map(|s| &**s).collect();
                Ok(wrap(occ_err(occ::boolean_n(&all[..1], &all[1..], 0, fuzzy, false).and_then(|x| occ::checked(x, "fuse", "")))?))
            }
        }
    };
    e.register_fn("fuse", move |a: Array| -> Res<S> { fuse(&shapes(&a)?, 0.0) });
    e.register_fn("fuse", move |a: Array, fuzzy: Dynamic| -> Res<S> { fuse(&shapes(&a)?, num(&fuzzy)?) });
    let bop = |s: &S, tools: &Dynamic, op: i32, fuzzy: f64| -> Res<S> {
        let t = shape_list(tools)?;
        if t.is_empty() {
            return Ok(if op == 1 { s.clone() } else { wrap(Shape::empty()) });
        }
        let tr: Vec<&Shape> = t.iter().map(|x| &**x).collect();
        Ok(wrap(occ_err(occ::boolean_n(&[&s.0], &tr, op, fuzzy, false).and_then(|x| occ::checked(x, ["fuse", "cut", "common"][op as usize], "")))?))
    };
    e.register_fn("cut", move |s: &mut S, tools: Dynamic| -> Res<S> { bop(s, &tools, 1, 0.0) });
    e.register_fn("cut", move |s: &mut S, tools: Dynamic, fuzzy: Dynamic| -> Res<S> { bop(s, &tools, 1, num(&fuzzy)?) });
    e.register_fn("common", move |s: &mut S, tools: Dynamic| -> Res<S> { bop(s, &tools, 2, 0.0) });
    e.register_fn("common", move |s: &mut S, tools: Dynamic, fuzzy: Dynamic| -> Res<S> { bop(s, &tools, 2, num(&fuzzy)?) });
    e.register_fn("unify", |s: &mut S| -> Res<S> { Ok(wrap(occ_err(occ::unify(&s.0))?)) });
    // placement
    e.register_fn("translate", |s: &mut S, v: Dynamic| -> Res<S> { Ok(wrap(occ_err(occ::translate(&s.0, v3(&v)?))?)) });
    e.register_fn("rotate", |s: &mut S, point: Dynamic, axis: Dynamic, deg: Dynamic| -> Res<S> {
        Ok(wrap(occ_err(occ::rotate(&s.0, v3(&point)?, v3(&axis)?.normalize_or(DVec3::Z), num(&deg)?.to_radians()))?))
    });
    e.register_fn("mirror", |s: &mut S, point: Dynamic, normal: Dynamic| -> Res<S> { Ok(wrap(occ_err(occ::mirror(&s.0, v3(&point)?, v3(&normal)?.normalize_or(DVec3::X)))?)) });
    e.register_fn("transform", |s: &mut S, m: Dynamic, t: Dynamic| -> Res<S> { Ok(wrap(occ_err(occ::transform(&s.0, m3_of(&m)?, v3(&t)?))?)) });
    e.register_fn("copy", |s: &mut S| -> Res<S> { Ok(wrap(occ_err(occ::translate(&s.0, DVec3::ZERO))?)) });
    e.register_fn("compound", |a: Array| -> Res<S> {
        let v = shapes(&a)?;
        let r: Vec<&Shape> = v.iter().map(|x| &**x).collect();
        Ok(wrap(occ_err(occ::compound(&r))?))
    });
    e.register_fn("solids", |s: &mut S| -> Array { crate::kernel::solids_of(&s.0).into_iter().map(|x| Dynamic::from(wrap(x))).collect() });
}

/// Rotation taking +Z to `n`.
fn z_to(n: DVec3) -> DMat3 {
    if n.dot(DVec3::Z) > 1.0 - 1e-12 {
        DMat3::IDENTITY
    } else if n.dot(DVec3::Z) < -1.0 + 1e-12 {
        DMat3::from_rotation_x(std::f64::consts::PI)
    } else {
        DMat3::from_axis_angle(DVec3::Z.cross(n).normalize(), DVec3::Z.angle_between(n))
    }
}

// ---------------------------------------------------------------- queries

/// A cheap fingerprint of a shape's geometry (volume, area, centre, bounding box, counts), equal for
/// equal shapes across runs, for caching results between rebuilds.
pub fn shape_hash(s: &Shape) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let q = |v: f64| (v * 1e6).round() as i64;
    for k in [occ::FACE, occ::EDGE, occ::VERTEX, occ::SOLID] {
        occ::count(s, k).hash(&mut h);
    }
    if let Ok((v, c, _)) = occ::mass_props(s, 0) {
        q(v).hash(&mut h);
        c.to_array().map(q).hash(&mut h);
    }
    if let Ok((a, c, _)) = occ::mass_props(s, 1) {
        q(a).hash(&mut h);
        c.to_array().map(q).hash(&mut h);
    }
    if let Some((lo, hi)) = occ::bbox(s, true) {
        lo.to_array().map(q).hash(&mut h);
        hi.to_array().map(q).hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

fn bbox_map(lo: DVec3, hi: DVec3) -> Map {
    let mut m = Map::new();
    m.insert("min".into(), dv(lo));
    m.insert("max".into(), dv(hi));
    m.insert("size".into(), dv(hi - lo));
    m.insert("center".into(), dv((lo + hi) / 2.0));
    m
}

/// Triangles of a shape: flat vertex array (x, y, z, …), index triples, and the face of each triangle.
pub fn tessellate(s: &Shape, linear: f64, angular_deg: f64) -> Result<(Vec<DVec3>, Vec<[u32; 3]>, Vec<u32>), String> {
    occ::clean_mesh(s);
    if !opencascade_sys::extra::ex_mesh(&s.inner, linear.max(1e-4), angular_deg.to_radians().max(1e-3)) {
        return Err("meshing failed".into());
    }
    let mut verts = vec![];
    let mut tris = vec![];
    let mut face = vec![];
    for (fi, f) in crate::kernel::unique_faces(s).iter().enumerate() {
        for t in crate::kernel::face_tris(f) {
            let b = verts.len() as u32;
            verts.extend_from_slice(&t);
            tris.push([b, b + 1, b + 2]);
            face.push(fi as u32);
        }
    }
    Ok((verts, tris, face))
}

fn register_queries(e: &mut Engine) {
    e.register_fn("area", |s: &mut S| crate::kernel::surface_area(&s.0));
    e.register_fn("length", |s: &mut S| -> Res<f64> { Ok(occ_err(occ::mass_props(&s.0, 2))?.0) });
    e.register_fn("centroid", |s: &mut S| -> Res<V3> { Ok(V3(occ_err(occ::mass_props(&s.0, 0))?.1)) });
    e.register_fn("bbox", |s: &mut S| -> Res<Map> {
        let (lo, hi) = occ::bbox(&s.0, true).ok_or("the shape is empty")?;
        Ok(bbox_map(lo, hi))
    });
    e.register_fn("is_valid", |s: &mut S| occ::is_valid(&s.0));
    e.register_fn("solid_count", |s: &mut S| occ::count(&s.0, occ::SOLID) as i64);
    e.register_fn("face_count", |s: &mut S| occ::count(&s.0, occ::FACE) as i64);
    e.register_fn("edge_count", |s: &mut S| occ::count(&s.0, occ::EDGE) as i64);
    e.register_fn("is_empty", |s: &mut S| occ::count(&s.0, occ::FACE) == 0 && occ::count(&s.0, occ::EDGE) == 0);
    e.register_fn("hash", |s: &mut S| shape_hash(&s.0));
    // "in", "out", "on" for each point
    let names = |v: Vec<i32>| -> Array { v.into_iter().map(|c| Dynamic::from(["in", "out", "on", "unknown"][c.clamp(0, 3) as usize])).collect() };
    e.register_fn("classify", move |s: &mut S, pts: Array, tol: Dynamic| -> Res<Array> { Ok(names(occ_err(occ::classify(&s.0, &v3s(&pts)?, num(&tol)?))?)) });
    e.register_fn("classify", move |s: &mut S, p: V3, tol: Dynamic| -> Res<String> {
        let v = occ_err(occ::classify(&s.0, &[p.0], num(&tol)?))?;
        Ok(["in", "out", "on", "unknown"][v[0].clamp(0, 3) as usize].into())
    });
    e.register_fn("contains", |s: &mut S, p: Dynamic| -> Res<bool> { Ok(occ_err(occ::classify(&s.0, &[v3(&p)?], 1e-7))?[0] == 0) });
    e.register_fn("tessellate", |s: &mut S, linear: Dynamic, angular: Dynamic| -> Res<Map> {
        let (v, t, f) = occ_err(tessellate(&s.0, num(&linear)?, num(&angular)?))?;
        let mut m = Map::new();
        m.insert("vertices".into(), v.iter().flat_map(|p| p.to_array()).map(Dynamic::from_float).collect::<Array>().into());
        m.insert("indices".into(), t.iter().flatten().map(|i| Dynamic::from_int(*i as i64)).collect::<Array>().into());
        m.insert("faces".into(), f.iter().map(|i| Dynamic::from_int(*i as i64)).collect::<Array>().into());
        Ok(m)
    });
}

// ---------------------------------------------------------------- data in and out

/// A JSON value as a script value.
pub fn from_json(v: &serde_json::Value) -> Dynamic {
    match v {
        serde_json::Value::Null => Dynamic::UNIT,
        serde_json::Value::Bool(b) => (*b).into(),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n.as_f64().unwrap_or(0.0).into(),
        },
        serde_json::Value::String(s) => s.clone().into(),
        serde_json::Value::Array(a) => a.iter().map(from_json).collect::<Array>().into(),
        serde_json::Value::Object(o) => o.iter().map(|(k, v)| (k.as_str().into(), from_json(v))).collect::<Map>().into(),
    }
}

/// A script value as JSON: vectors become [x, y, z], matrices rows, mass properties a map.
pub fn to_json(d: &Dynamic) -> serde_json::Value {
    use serde_json::{Value, json};
    if d.is_unit() {
        return Value::Null;
    }
    if let Ok(b) = d.as_bool() {
        return json!(b);
    }
    if let Ok(i) = d.as_int() {
        return json!(i);
    }
    if let Ok(f) = d.as_float() {
        return if f.is_finite() { json!(f) } else { Value::Null };
    }
    if let Some(s) = d.clone().try_cast::<ImmutableString>() {
        return json!(s.as_str());
    }
    if let Some(v) = d.clone().try_cast::<V3>() {
        return json!(v.0.to_array());
    }
    if let Some(m) = d.clone().try_cast::<M3>() {
        return json!(m.0.transpose().to_cols_array_2d());
    }
    if let Some(m) = d.clone().try_cast::<MassProps>() {
        return to_json(&Dynamic::from_map(mass_map(&m)));
    }
    if let Some(s) = d.clone().try_cast::<S>() {
        return json!(format!("Shape {}", shape_hash(&s.0)));
    }
    if let Ok(a) = d.clone().into_array() {
        return Value::Array(a.iter().map(to_json).collect());
    }
    if let Some(m) = d.clone().try_cast::<Map>() {
        return Value::Object(m.iter().map(|(k, v)| (k.to_string(), to_json(v))).collect());
    }
    json!(d.to_string())
}

fn register_files(e: &mut Engine) {
    e.register_fn("parse_json", |s: &str| -> Res<Dynamic> { Ok(from_json(&serde_json::from_str(s).map_err(|e| format!("JSON: {e}"))?)) });
    e.register_fn("to_json", |d: Dynamic| -> String { serde_json::to_string(&to_json(&d)).unwrap_or_default() });
    e.register_fn("read_text", |p: &str| -> Res<String> { std::fs::read_to_string(p).map_err(|e| format!("can't open {p}: {e}").into()) });
    e.register_fn("write_text", |p: &str, s: &str| -> Res<()> { std::fs::write(p, s).map_err(|e| format!("can't write {p}: {e}").into()) });
    e.register_fn("read_json", |p: &str| -> Res<Dynamic> {
        let t = std::fs::read_to_string(p).map_err(|e| format!("can't open {p}: {e}"))?;
        Ok(from_json(&serde_json::from_str(&t).map_err(|e| format!("{p}: {e}"))?))
    });
    e.register_fn("read_yaml", |p: &str| -> Res<Dynamic> {
        let t = std::fs::read_to_string(p).map_err(|e| format!("can't open {p}: {e}"))?;
        let v: serde_json::Value = serde_yaml::from_str(&t).map_err(|e| format!("{p}: {e}"))?;
        Ok(from_json(&v))
    });
    e.register_fn("parse_yaml", |s: &str| -> Res<Dynamic> {
        let v: serde_json::Value = serde_yaml::from_str(s).map_err(|e| format!("YAML: {e}"))?;
        Ok(from_json(&v))
    });
    e.register_fn("write_json", |p: &str, d: Dynamic| -> Res<()> {
        let s = serde_json::to_string_pretty(&to_json(&d)).unwrap_or_default();
        std::fs::write(p, s).map_err(|e| format!("can't write {p}: {e}").into())
    });
    // STEP of one shape or several (each a separate body), STL of one shape
    e.register_fn("export_step", |x: Dynamic, p: &str| -> Res<()> {
        let v = shape_list(&x)?;
        Shape::write_all_step(v.iter().map(|s| &**s), p).map_err(|e| format!("can't write {p}: {e:?}").into())
    });
    e.register_fn("export_stl", |x: Dynamic, p: &str| -> Res<()> {
        let v = shape_list(&x)?;
        let r: Vec<&Shape> = v.iter().map(|s| &**s).collect();
        let c = if r.len() == 1 { kdup(r[0]) } else { occ_err(occ::compound(&r))? };
        c.write_stl_with_tolerance(p, 0.01).map_err(|e| format!("can't write {p}: {e:?}").into())
    });
    e.register_fn("export_stl", |x: Dynamic, p: &str, deflection: Dynamic| -> Res<()> {
        let v = shape_list(&x)?;
        let r: Vec<&Shape> = v.iter().map(|s| &**s).collect();
        let c = if r.len() == 1 { kdup(r[0]) } else { occ_err(occ::compound(&r))? };
        c.write_stl_with_tolerance(p, num(&deflection)?).map_err(|e| format!("can't write {p}: {e:?}").into())
    });
    e.register_fn("import_step", |p: &str| -> Res<S> { Ok(wrap(Shape::read_step(p).map_err(|e| format!("can't open {p}: {e:?}"))?)) });
    e.register_fn("time", || -> f64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0) });
}

/// Add the whole API to an engine.
pub fn register(e: &mut Engine) {
    register_linalg(e);
    register_mass(e);
    register_topology(e);
    register_build(e);
    register_queries(e);
    register_files(e);
    crate::script_num::register(e);
}

// ---------------------------------------------------------------- headless runner

/// Run a script file with no window: `args` become the ARGS array, `print` goes to stdout, and the
/// script's final value is printed as JSON. Errors (including OCCT's) come back with the line.
pub fn run_file(path: &str, args: &[String]) -> Result<serde_json::Value, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("can't open {path}: {e}"))?;
    run_source(&src, args, Some(std::path::Path::new(path)))
}

pub fn run_source(src: &str, args: &[String], path: Option<&std::path::Path>) -> Result<serde_json::Value, String> {
    let log = Rc::new(RefCell::new(vec![]));
    let mut e = crate::script::engine_with(log, true);
    e.set_max_operations(0);
    e.set_max_array_size(0);
    if let Some(dir) = path.and_then(|p| p.parent()) {
        let mut r = rhai::module_resolvers::FileModuleResolver::new_with_path(dir);
        r.enable_cache(true);
        e.set_module_resolver(r);
    }
    let mut scope = rhai::Scope::new();
    scope.push("ARGS", args.iter().map(|a| Dynamic::from(a.clone())).collect::<Array>());
    let out: Dynamic = e.eval_with_scope(&mut scope, src).map_err(|err| format!("{}{err}", path.map(|p| format!("{}: ", p.display())).unwrap_or_default()))?;
    Ok(to_json(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval<T: Clone + 'static>(src: &str) -> T {
        let e = crate::script::engine_with(Rc::new(RefCell::new(vec![])), false);
        e.eval::<T>(src).unwrap_or_else(|err| panic!("{err}"))
    }

    #[test]
    fn box_mass_properties_match_the_formulas() {
        // 40 × 20 × 10 block of density 2.7e-3 from (5, 0, 0)
        let m = mass_of(&Shape::box_from_corners(DVec3::new(5.0, 0.0, 0.0), DVec3::new(45.0, 20.0, 10.0)), 2.7e-3).unwrap();
        let mass = 40.0 * 20.0 * 10.0 * 2.7e-3;
        assert!((m.mass - mass).abs() / mass < 1e-12);
        assert!(m.cg.distance(DVec3::new(25.0, 10.0, 5.0)) < 1e-9);
        let ixx = mass / 12.0 * (20.0f64.powi(2) + 10.0f64.powi(2));
        let izz = mass / 12.0 * (40.0f64.powi(2) + 20.0f64.powi(2));
        assert!((m.inertia.x_axis.x - ixx).abs() / ixx < 1e-9, "{} {ixx}", m.inertia.x_axis.x);
        assert!((m.inertia.z_axis.z - izz).abs() / izz < 1e-9);
        assert!(m.inertia.y_axis.x.abs() < 1e-6 * izz);
        // two halves combined with the parallel axis theorem give the whole block
        let a = mass_of(&Shape::box_from_corners(DVec3::new(5.0, 0.0, 0.0), DVec3::new(25.0, 20.0, 10.0)), 2.7e-3).unwrap();
        let b = mass_of(&Shape::box_from_corners(DVec3::new(25.0, 0.0, 0.0), DVec3::new(45.0, 20.0, 10.0)), 2.7e-3).unwrap();
        let c = a.combine(&b);
        assert!((c.mass - m.mass).abs() < 1e-9);
        assert!((c.inertia - m.inertia).abs().to_cols_array().iter().all(|v| *v < 1e-6 * izz));
        // about a line through the corner along Z
        let i0 = m.about_line(DVec3::new(5.0, 0.0, 0.0), DVec3::Z);
        assert!((i0 - (izz + mass * (20.0f64.powi(2) + 10.0f64.powi(2)))).abs() / i0 < 1e-9);
    }

    #[test]
    fn selective_fillet_rounds_only_the_top_edges() {
        let v: f64 = eval(
            r#"
            let b = block([0, 0, 0], [40, 20, 10]);
            let picks = [];
            for e in b.edges() {
                let f = e.faces();
                let m = e.mid();
                if (f[0].normal(m).z > 0.6 || f[1].normal(m).z > 0.6) && m.z > 9.9 { picks.push(e); }
            }
            if picks.len() != 4 { throw "expected 4 top edges"; }
            b.fillet(picks, 2).volume()
            "#,
        );
        // the four top edges lose (1 − π/4) r² along their length; the corners are blended together
        let plain = 40.0 * 20.0 * 10.0;
        assert!(v < plain - 0.8 * (1.0 - std::f64::consts::FRAC_PI_4) * 4.0 * 120.0 && v > plain - 1.2 * (1.0 - std::f64::consts::FRAC_PI_4) * 4.0 * 120.0, "{v}");
    }

    #[test]
    fn shell_hollows_inward_and_bad_results_throw() {
        // a closed hollow keeps the outside: walls 1 thick inside a 10 mm cube
        let v: f64 = eval(r#"block([0, 0, 0], [10, 10, 10]).shell([], 1).volume()"#);
        assert!((v - (1000.0 - 512.0)).abs() < 1e-6, "{v}");
        // negative: walls grown outside it
        let v: f64 = eval(r#"block([0, 0, 0], [10, 10, 10]).shell([], -1).volume()"#);
        assert!((v - (1728.0 - 1000.0)).abs() < 1e-6, "{v}");
        // open at the top
        let v: f64 = eval(r#"let b = block([0, 0, 0], [10, 10, 10]); let top = []; for f in b.faces() { if f.center().z > 9.9 { top.push(f); } } b.shell(top, 1).volume()"#);
        assert!((v - (1000.0 - 8.0 * 8.0 * 9.0)).abs() < 1e-6, "{v}");
        // a fillet larger than the part throws instead of returning an invalid shape
        let msg: String = eval(r#"let m = "no error"; try { let b = block([0, 0, 0], [10, 10, 10]); b.fillet(b.edges(), 6); } catch (e) { m = `${e}`; } m"#);
        assert!(msg.contains("fillet"), "{msg}");
    }

    #[test]
    fn classify_points_and_catch_errors() {
        let a: Array = eval(r#"block([0, 0, 0], [10, 10, 10]).classify([[5, 5, 5], [20, 5, 5], [10, 5, 5]], 1e-6)"#);
        let s: Vec<String> = a.into_iter().map(|x| x.into_string().unwrap()).collect();
        assert_eq!(s, vec!["in", "out", "on"]);
        let msg: String = eval(r#"let m = "no error"; try { block([0, 0, 0], [10, 10, 10]).fillet([], 0); cylinder(vec3(0, 0, 0), vec3(0, 0, 1), -1, 5); } catch (e) { m = `${e}`; } m"#);
        assert!(msg.contains("positive radius"), "{msg}");
    }

    #[test]
    fn offsets_and_regions() {
        let n: i64 = eval("region_union([circle([0, 0], 10), circle([19, 0], 10), circle([60, 0], 10)]).len()");
        assert_eq!(n, 2);
        let a: f64 = eval("let f = offset2d(rounded_rect([0, 0], 40, 30, 5), 3)[0]; f.area()");
        // a rounded rectangle grown by 3: the sides move out and the corner radius grows to 8
        let expect = (46.0 * 36.0) - (4.0 - std::f64::consts::PI) * 64.0;
        assert!((a - expect).abs() < 1e-6, "{a} {expect}");
        let up: f64 = eval("region_cut(circle([0, 0], 10), circle([0, 0], 4))[0].faces()[0].normal().z");
        assert!(up > 0.999);
    }
}
