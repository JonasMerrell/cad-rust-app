//! Custom features: small Rhai scripts that build solids, like Onshape's
//! FeatureScript. A definition lives in the document; each Custom feature
//! instance supplies parameter values and selections. Scripts run in the kernel
//! worker, so a bad script can at worst fail its own feature.

use std::cell::RefCell;
use std::rc::Rc;

use glam::DVec3;
use opencascade::primitives::Shape;
use rhai::{Array, Dynamic, Engine, EvalAltResult, Scope};
use serde::{Deserialize, Serialize};

use crate::occ;

/// A user-defined feature type.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct CustomDef {
    /// Name shown in the toolbar and used by Custom features to find it.
    pub name: String,
    #[serde(default)]
    pub params: Vec<ParamDef>,
    /// What the feature asks the user to select: "" (nothing), "points" or "faces".
    #[serde(default)]
    pub selection: String,
    pub source: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ParamDef {
    /// Variable name in the script.
    pub name: String,
    /// "length", "angle", "number", "count" or "bool"
    pub kind: String,
    pub default: String,
}

pub const PARAM_KINDS: &[(&str, &str)] = &[("length", "Length"), ("angle", "Angle (°)"), ("number", "Number"), ("count", "Count"), ("bool", "Yes / no")];

/// Starter script for a new definition.
pub const EXAMPLE: &str = r#"// Bosses: a cylinder at each selected point, standing on its face.
// Shapes: cube(w, d, h), cube_centered(w, d, h), cylinder(r, h), cone(r1, r2, h),
//   sphere(r), torus(R, r), prism([[x, y], ...], h), revolve([[r, z], ...], deg)
// Methods: .move(x, y, z), .rotate_x/y/z(deg), .rotate([ax, ay, az], deg),
//   .mirror([nx, ny, nz]), .scale(f), .fillet(r), .chamfer(d), .place(point, normal)
// a + b joins, a - b cuts, a & b intersects. Return one shape or an array.
let out = [];
for i in 0..points.len() {
    let boss = cylinder(diameter / 2, height).fillet(fillet);
    out.push(boss.place(points[i], normals[i]));
}
out
"#;

#[derive(Clone)]
pub struct S(pub Rc<Shape>);

type Res<T> = Result<T, Box<EvalAltResult>>;

fn num(d: &Dynamic) -> Res<f64> {
    if let Ok(f) = d.as_float() {
        return Ok(f);
    }
    if let Ok(i) = d.as_int() {
        return Ok(i as f64);
    }
    Err(format!("expected a number, got {}", d.type_name()).into())
}

fn vec3(d: &Dynamic) -> Res<DVec3> {
    let a = d.clone().into_array().map_err(|_| "expected [x, y, z]")?;
    if a.len() < 3 {
        return Err("expected [x, y, z]".into());
    }
    Ok(DVec3::new(num(&a[0])?, num(&a[1])?, num(&a[2])?))
}

fn pts2(d: &Array) -> Res<Vec<(f64, f64)>> {
    d.iter()
        .map(|p| {
            let a = p.clone().into_array().map_err(|_| "expected a list of [x, y] points")?;
            if a.len() < 2 {
                return Err("expected [x, y]".into());
            }
            Ok((num(&a[0])?, num(&a[1])?))
        })
        .collect()
}

fn occ_err<T>(r: Result<T, String>) -> Res<T> {
    r.map_err(|e| e.into())
}

fn wrap(s: Shape) -> S {
    S(Rc::new(s))
}

fn polygon_face(pts: &[DVec3]) -> Res<Shape> {
    if pts.len() < 3 {
        return Err("a profile needs at least 3 points".into());
    }
    let mut edges = vec![];
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        if (a - b).length() > 1e-9 {
            edges.push(opencascade::primitives::Edge::segment(a, b));
        }
    }
    let w = occ_err(occ::wire(&edges))?;
    Ok(Shape::from(occ_err(occ::face(&w, &[]))?))
}

fn rot(s: &S, axis: DVec3, deg: f64) -> Res<S> {
    Ok(wrap(occ_err(occ::rotate(&s.0, DVec3::ZERO, axis, deg.to_radians()))?))
}

fn all_edges(s: &Shape) -> Vec<opencascade::primitives::Edge> {
    crate::kernel::unique_edges(s)
}

fn engine(log: Rc<RefCell<Vec<String>>>) -> Engine {
    engine_with(log, false)
}

/// The script engine with every shape function; `stdout`: print goes to the terminal instead of `log`.
pub fn engine_with(log: Rc<RefCell<Vec<String>>>, stdout: bool) -> Engine {
    let mut e = Engine::new();
    e.set_max_operations(20_000_000);
    e.set_max_expr_depths(128, 64);
    e.set_max_array_size(100_000);
    e.register_type_with_name::<S>("Shape");
    if stdout {
        e.on_print(|s| println!("{s}"));
        e.on_debug(|s, _, _| eprintln!("{s}"));
    } else {
        let l = log.clone();
        e.on_print(move |s| l.borrow_mut().push(s.to_string()));
        let l = log;
        e.on_debug(move |s, _, _| l.borrow_mut().push(s.to_string()));
    }
    crate::script_api::register(&mut e);

    e.register_fn("cube", |w: Dynamic, d: Dynamic, h: Dynamic| -> Res<S> { Ok(wrap(Shape::box_with_dimensions(num(&w)?, num(&d)?, num(&h)?))) });
    e.register_fn("cube_centered", |w: Dynamic, d: Dynamic, h: Dynamic| -> Res<S> { Ok(wrap(Shape::box_centered(num(&w)?, num(&d)?, num(&h)?))) });
    e.register_fn("cylinder", |r: Dynamic, h: Dynamic| -> Res<S> {
        let (r, h) = (num(&r)?, num(&h)?);
        if r <= 0.0 || h <= 0.0 {
            return Err("cylinder needs a positive radius and height".into());
        }
        Ok(wrap(Shape::cylinder_radius_height(r, h)))
    });
    e.register_fn("cone", |r1: Dynamic, r2: Dynamic, h: Dynamic| -> Res<S> {
        Ok(wrap(Shape::cone().bottom_radius(num(&r1)?).top_radius(num(&r2)?).height(num(&h)?).build()))
    });
    e.register_fn("sphere", |r: Dynamic| -> Res<S> { Ok(wrap(Shape::sphere(num(&r)?).build())) });
    e.register_fn("torus", |r1: Dynamic, r2: Dynamic| -> Res<S> { Ok(wrap(Shape::torus().radius_1(num(&r1)?).radius_2(num(&r2)?).build())) });
    e.register_fn("prism", |pts: Array, h: Dynamic| -> Res<S> {
        let p: Vec<DVec3> = pts2(&pts)?.into_iter().map(|(x, y)| DVec3::new(x, y, 0.0)).collect();
        let f = polygon_face(&p)?;
        Ok(wrap(occ_err(occ::prism(&f, DVec3::Z * num(&h)?))?))
    });
    e.register_fn("revolve", |pts: Array, deg: Dynamic| -> Res<S> {
        // profile in the XZ plane as [radius, z] pairs, turned about Z
        let p: Vec<DVec3> = pts2(&pts)?.into_iter().map(|(r, z)| DVec3::new(r, 0.0, z)).collect();
        let f = polygon_face(&p)?;
        Ok(wrap(occ_err(occ::revolve(&f, DVec3::ZERO, DVec3::Z, num(&deg)?.to_radians()))?))
    });

    e.register_fn("move", |s: &mut S, x: Dynamic, y: Dynamic, z: Dynamic| -> Res<S> { Ok(wrap(occ_err(occ::translate(&s.0, DVec3::new(num(&x)?, num(&y)?, num(&z)?)))?)) });
    e.register_fn("move", |s: &mut S, v: Dynamic| -> Res<S> { Ok(wrap(occ_err(occ::translate(&s.0, vec3(&v)?))?)) });
    e.register_fn("rotate_x", |s: &mut S, d: Dynamic| -> Res<S> { rot(s, DVec3::X, num(&d)?) });
    e.register_fn("rotate_y", |s: &mut S, d: Dynamic| -> Res<S> { rot(s, DVec3::Y, num(&d)?) });
    e.register_fn("rotate_z", |s: &mut S, d: Dynamic| -> Res<S> { rot(s, DVec3::Z, num(&d)?) });
    e.register_fn("rotate", |s: &mut S, axis: Dynamic, d: Dynamic| -> Res<S> { rot(s, vec3(&axis)?.normalize_or(DVec3::Z), num(&d)?) });
    e.register_fn("mirror", |s: &mut S, n: Dynamic| -> Res<S> { Ok(wrap(occ_err(occ::mirror(&s.0, DVec3::ZERO, vec3(&n)?.normalize_or(DVec3::X)))?)) });
    e.register_fn("scale", |s: &mut S, f: Dynamic| -> Res<S> {
        let f = num(&f)?;
        if f <= 0.0 {
            return Err("scale must be positive".into());
        }
        Ok(wrap(occ_err(occ::transform(&s.0, glam::DMat3::from_diagonal(DVec3::splat(f)), DVec3::ZERO))?))
    });
    e.register_fn("fillet", |s: &mut S, r: Dynamic| -> Res<S> {
        let r = num(&r)?;
        if r <= 0.0 {
            return Ok(s.clone());
        }
        Ok(wrap(occ_err(occ::fillet(&s.0, &all_edges(&s.0), r, r))?))
    });
    e.register_fn("chamfer", |s: &mut S, d: Dynamic| -> Res<S> {
        let d = num(&d)?;
        if d <= 0.0 {
            return Ok(s.clone());
        }
        Ok(wrap(occ_err(occ::chamfer(&s.0, &all_edges(&s.0), d))?))
    });
    // move the shape's origin to `p` with its +Z along `n`
    e.register_fn("place", |s: &mut S, p: Dynamic, n: Dynamic| -> Res<S> {
        let (p, n) = (vec3(&p)?, vec3(&n)?.normalize_or(DVec3::Z));
        let m = if n.dot(DVec3::Z) > 1.0 - 1e-12 {
            glam::DMat3::IDENTITY
        } else if n.dot(DVec3::Z) < -1.0 + 1e-12 {
            glam::DMat3::from_rotation_x(std::f64::consts::PI)
        } else {
            let axis = DVec3::Z.cross(n).normalize();
            glam::DMat3::from_axis_angle(axis, DVec3::Z.angle_between(n))
        };
        Ok(wrap(occ_err(occ::transform(&s.0, m, p))?))
    });
    e.register_fn("+", |a: S, b: S| -> Res<S> { Ok(wrap(occ_err(occ::fuse(&a.0, &b.0))?)) });
    e.register_fn("-", |a: S, b: S| -> Res<S> { Ok(wrap(occ_err(occ::cut(&a.0, &b.0))?)) });
    e.register_fn("&", |a: S, b: S| -> Res<S> { Ok(wrap(occ_err(occ::common(&a.0, &b.0))?)) });
    e.register_fn("union", |a: S, b: S| -> Res<S> { Ok(wrap(occ_err(occ::fuse(&a.0, &b.0))?)) });
    e.register_fn("subtract", |a: S, b: S| -> Res<S> { Ok(wrap(occ_err(occ::cut(&a.0, &b.0))?)) });
    e.register_fn("intersect", |a: S, b: S| -> Res<S> { Ok(wrap(occ_err(occ::common(&a.0, &b.0))?)) });
    e.register_fn("volume", |s: &mut S| -> f64 { crate::kernel::volume(&s.0) });
    e.register_fn("deg", |d: Dynamic| -> Res<f64> { Ok(num(&d)?.to_radians()) });
    e
}

/// A parameter value handed to the script.
pub enum Value {
    Num(f64),
    Int(i64),
    Bool(bool),
}

/// Run a definition. `selections` are (point, normal) pairs in world mm.
/// Returns the solids and the script's printed lines.
pub fn run(def: &CustomDef, values: Vec<(String, Value)>, selections: &[(DVec3, DVec3)]) -> Result<(Vec<Shape>, Vec<String>), String> {
    let log = Rc::new(RefCell::new(vec![]));
    let e = engine(log.clone());
    let mut scope = Scope::new();
    for (k, v) in values {
        match v {
            Value::Num(f) => scope.push(k, f),
            Value::Int(i) => scope.push(k, i),
            Value::Bool(b) => scope.push(k, b),
        };
    }
    let v3 = |p: DVec3| -> Dynamic { Dynamic::from_array(vec![Dynamic::from_float(p.x), Dynamic::from_float(p.y), Dynamic::from_float(p.z)]) };
    scope.push("points", selections.iter().map(|s| v3(s.0)).collect::<Array>());
    scope.push("normals", selections.iter().map(|s| v3(s.1)).collect::<Array>());
    let out: Dynamic = e.eval_with_scope(&mut scope, &def.source).map_err(|err| format!("script: {err}"))?;
    let mut shapes = vec![];
    collect(&out, &mut shapes)?;
    let lines = log.borrow().clone();
    Ok((shapes, lines))
}

fn collect(d: &Dynamic, out: &mut Vec<Shape>) -> Result<(), String> {
    if let Some(s) = d.clone().try_cast::<S>() {
        out.push(Rc::try_unwrap(s.0).unwrap_or_else(|rc| (*rc).clone_shape()));
        return Ok(());
    }
    if let Ok(a) = d.clone().into_array() {
        for x in &a {
            collect(x, out)?;
        }
        return Ok(());
    }
    if d.is_unit() {
        return Ok(());
    }
    Err(format!("the script must return a shape or an array of shapes, not {}", d.type_name()))
}

trait CloneShape {
    fn clone_shape(&self) -> Shape;
}

impl CloneShape for Shape {
    fn clone_shape(&self) -> Shape {
        // a zero translation copies the topology
        occ::translate(self, DVec3::ZERO).unwrap_or_else(|_| Shape::empty())
    }
}
