//! Parametric documents from Rhai scripts (`rcad script`): the script builds a Part Studio's
//! feature tree (variables, dimensioned sketches, extrudes, fillets, …) through the same tools as
//! the MCP server, so what it saves opens in the editor with every feature and dimension editable.
//!
//! ```rhai
//! let doc = new_document();
//! doc.variable("w", 60);
//! let sk = doc.sketch("top");
//! sk.rect([0, 0], "w", "w / 2");                  // a centred rectangle, dimensioned
//! let body = doc.extrude(sk, 12);
//! doc.fillet(doc.edges(#{box: [[-100, -100, 11.9], [100, 100, 12.1]]}), 2);
//! doc.save("plate.json");
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use rhai::{Array, Dynamic, Engine, EvalAltResult, ImmutableString, Map};
use serde_json::{Value, json};

use crate::agent::{Session, failed};
use crate::script_api::{from_json, to_json};

type Res<T> = Result<T, Box<EvalAltResult>>;

/// A document being built: an agent session (one per `new_document` / `open_document`).
#[derive(Clone)]
pub struct Doc(Rc<RefCell<Session>>);

/// A sketch feature of a document.
#[derive(Clone)]
pub struct DocSketch {
    doc: Doc,
    pub id: i64,
}

impl Doc {
    /// Run a tool; a feature left failing is undone and becomes an error, as a script expects.
    fn call(&self, tool: &str, mut args: Value) -> Res<Value> {
        let modelling = matches!(tool, "add_feature" | "edit_feature" | "set_variable" | "sketch_add" | "sketch_constrain" | "create_sketch" | "delete_feature" | "set_part" | "set_parts");
        if modelling && args.get("rollback").is_none() {
            if let Some(o) = args.as_object_mut() {
                o.insert("rollback".into(), json!(true));
            }
        }
        let r = self.0.borrow_mut().call(tool, &args);
        match r {
            Ok((v, _)) if failed(&v) => {
                let e = &v["new_errors"][0];
                Err(format!("{tool}: {}: {}", e["name"].as_str().unwrap_or("feature"), e["error"].as_str().unwrap_or("failed")).into())
            }
            Ok((v, _)) => Ok(v),
            Err(e) => Err(format!("{tool}: {e}").into()),
        }
    }

    fn feature(&self, kind: &str, params: Value, name: Option<String>) -> Res<i64> {
        let mut a = json!({"kind": kind, "params": params});
        if let Some(n) = name {
            a["name"] = json!(n);
        }
        let v = self.call("add_feature", a)?;
        Ok(v["feature_id"].as_i64().unwrap_or(-1))
    }

    /// A length value: a number, or an expression the document evaluates (variables allowed).
    fn length(&self, v: &Dynamic) -> Res<(f64, String)> {
        let e = if let Some(s) = v.clone().try_cast::<ImmutableString>() {
            s.to_string()
        } else if let Ok(f) = v.as_float() {
            crate::expr::trim(f, 6)
        } else if let Ok(i) = v.as_int() {
            i.to_string()
        } else {
            return Err(format!("expected a size (number or expression), got {}", v.type_name()).into());
        };
        let s = self.0.borrow();
        let (vars, _) = s.doc.var_values();
        let env = crate::expr::Env { vars: &vars, units: s.doc.units };
        let mm = crate::expr::eval(&e, crate::expr::Kind::Length, &env).map_err(|err| format!("{e}: {err}"))?;
        Ok((mm, e))
    }
}

/// A script value as tool arguments: sketches become `{sketch: id}` (every region of the sketch).
fn arg(d: &Dynamic) -> Value {
    if let Some(s) = d.clone().try_cast::<DocSketch>() {
        return json!({"sketch": s.id});
    }
    if let Some(a) = d.clone().try_cast::<Array>() {
        return Value::Array(a.iter().map(arg).collect());
    }
    if let Some(m) = d.clone().try_cast::<Map>() {
        return Value::Object(m.iter().map(|(k, v)| (k.to_string(), arg(v))).collect());
    }
    to_json(d)
}

/// A list of refs: an array of refs or of list_geometry entries (their `ref` is taken), or one.
fn refs(d: &Dynamic) -> Value {
    let one = |v: Value| if v.get("ref").is_some() { v["ref"].clone() } else { v };
    match arg(d) {
        Value::Array(a) => Value::Array(a.into_iter().map(one).collect()),
        v => Value::Array(vec![one(v)]),
    }
}

fn plane(d: &Dynamic) -> Value {
    match arg(d) {
        Value::String(s) => json!(s.to_lowercase()),
        v if v.get("ref").is_some() => v["ref"].clone(),
        // a plane feature's id
        Value::Number(n) => json!({"kind": "feature_plane", "feature": n}),
        v => v,
    }
}

fn pt(d: &Dynamic) -> Res<Value> {
    let v = crate::script_api::v3(d)?;
    Ok(json!([v.x, v.y]))
}

/// Geometry from list_geometry, flattened over the parts: `what` is "faces" or "edges".
fn geometry(doc: &Doc, what: &str, filter: Value) -> Res<Array> {
    let mut a = filter;
    if !a.is_object() {
        a = json!({});
    }
    a["type"] = json!(what);
    let v = doc.call("list_geometry", a)?;
    Ok(v["bodies"].as_array().into_iter().flatten().flat_map(|b| b[what].as_array().cloned().unwrap_or_default()).map(|x| from_json(&x)).collect())
}

/// Extrudes and revolves join the part once there is one (as the editor's custom features do),
/// unless the script says otherwise with `op`.
fn default_op(d: &Doc, params: &mut Value) {
    if params.get("op").is_none() {
        let any = !d.0.borrow().result.bodies.is_empty();
        params["op"] = json!(if any { "add" } else { "new" });
    }
}

impl DocSketch {
    fn add(&self, item: Value) -> Res<Dynamic> {
        let v = self.doc.call("sketch_add", json!({"sketch_id": self.id, "items": [item]}))?;
        Ok(from_json(&v["created"][0]))
    }
}

pub fn register(e: &mut Engine) {
    e.register_type_with_name::<Doc>("Document");
    e.register_type_with_name::<DocSketch>("Sketch");

    e.register_fn("new_document", || -> Doc { Doc(Rc::new(RefCell::new(Session::new()))) });
    e.register_fn("open_document", |path: &str| -> Res<Doc> {
        let d = Doc(Rc::new(RefCell::new(Session::new())));
        d.call("open_document", json!({"path": path}))?;
        Ok(d)
    });
    // any MCP tool, by name, with its arguments as a map. Rhai keeps `call` for function pointers, so
    // `d.call(…)` never reaches a registered function: the name is `tool`.
    e.register_fn("tool", |d: &mut Doc, tool: &str, args: Map| -> Res<Dynamic> { Ok(from_json(&d.call(tool, arg(&Dynamic::from_map(args)))?)) });
    e.register_fn("tool", |d: &mut Doc, tool: &str| -> Res<Dynamic> { Ok(from_json(&d.call(tool, json!({}))?)) });
    e.register_fn("describe", |d: &mut Doc| -> Res<Dynamic> { Ok(from_json(&d.call("describe", json!({}))?)) });
    e.register_fn("parts", |d: &mut Doc| -> Res<Dynamic> { Ok(from_json(&d.call("describe", json!({}))?["parts"])) });
    e.register_fn("variable", |d: &mut Doc, name: &str, v: Dynamic| -> Res<()> {
        d.call("set_variable", json!({"name": name, "expr": arg(&v)}))?;
        Ok(())
    });
    e.register_fn("save", |d: &mut Doc, path: &str| -> Res<()> {
        d.call("save_document", json!({"path": path}))?;
        Ok(())
    });
    e.register_fn("export_file", |d: &mut Doc, path: &str| -> Res<()> {
        let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
        let format = match ext.as_str() {
            "stp" | "step" => "step",
            "igs" | "iges" => "iges",
            "stl" | "obj" => ext.as_str(),
            _ => return Err(format!("{path}: export writes .step, .iges, .stl or .obj").into()),
        };
        d.call("export", json!({"format": format, "path": path}))?;
        Ok(())
    });
    e.register_fn("render", |d: &mut Doc, path: &str| -> Res<()> {
        d.call("render", json!({"path": path}))?;
        Ok(())
    });
    e.register_fn("render", |d: &mut Doc, path: &str, view: &str| -> Res<()> {
        d.call("render", json!({"path": path, "view": view}))?;
        Ok(())
    });

    // sketches
    let sketch = |d: &mut Doc, p: Dynamic, name: Option<String>| -> Res<DocSketch> {
        let mut a = json!({"plane": plane(&p)});
        if let Some(n) = name {
            a["name"] = json!(n);
        }
        let v = d.call("create_sketch", a)?;
        Ok(DocSketch { doc: d.clone(), id: v["sketch_id"].as_i64().unwrap_or(-1) })
    };
    e.register_fn("sketch", move |d: &mut Doc, p: Dynamic| sketch(d, p, None));
    e.register_fn("sketch", move |d: &mut Doc, p: Dynamic, name: &str| sketch(d, p, Some(name.to_string())));
    e.register_get("id", |s: &mut DocSketch| s.id);
    // any sketch_add item (see AGENTS.md): #{type: "arc", center, start, end, dims: …}
    e.register_fn("add", |s: &mut DocSketch, item: Map| s.add(arg(&Dynamic::from_map(item))));
    // a rectangle centred on `center`, its width and height dimensioned (numbers or expressions)
    e.register_fn("rect", |s: &mut DocSketch, center: Dynamic, w: Dynamic, h: Dynamic| -> Res<Dynamic> {
        let (c, (wm, we), (hm, he)) = (pt(&center)?, s.doc.length(&w)?, s.doc.length(&h)?);
        let (cx, cy) = (c[0].as_f64().unwrap_or(0.0), c[1].as_f64().unwrap_or(0.0));
        s.add(json!({"type": "center_rect", "center": c, "corner": [cx + wm / 2.0, cy + hm / 2.0], "dims": {"width": we, "height": he}}))
    });
    // a rectangle between two corners, dimensioned as drawn
    e.register_fn("rect_corners", |s: &mut DocSketch, a: Dynamic, b: Dynamic| -> Res<Dynamic> { s.add(json!({"type": "rect", "corner1": pt(&a)?, "corner2": pt(&b)?, "dims": true})) });
    // a circle with its diameter dimensioned
    e.register_fn("circle", |s: &mut DocSketch, center: Dynamic, dia: Dynamic| -> Res<Dynamic> {
        let (mm, ex) = s.doc.length(&dia)?;
        s.add(json!({"type": "circle", "center": pt(&center)?, "radius": mm / 2.0, "dims": {"diameter": ex}}))
    });
    e.register_fn("line", |s: &mut DocSketch, a: Dynamic, b: Dynamic| -> Res<Dynamic> { s.add(json!({"type": "line", "from": pt(&a)?, "to": pt(&b)?})) });
    e.register_fn("arc", |s: &mut DocSketch, c: Dynamic, a: Dynamic, b: Dynamic| -> Res<Dynamic> { s.add(json!({"type": "arc", "center": pt(&c)?, "start": pt(&a)?, "end": pt(&b)?, "dims": true})) });
    e.register_fn("polyline", |s: &mut DocSketch, pts: Array, closed: bool| -> Res<Dynamic> {
        let pts: Vec<Value> = pts.iter().map(pt).collect::<Res<_>>()?;
        s.add(json!({"type": "polyline", "points": pts, "closed": closed}))
    });
    // a closed outline with every side dimensioned as drawn
    e.register_fn("polygon", |s: &mut DocSketch, pts: Array| -> Res<Dynamic> {
        let pts: Vec<Value> = pts.iter().map(pt).collect::<Res<_>>()?;
        s.add(json!({"type": "polyline", "points": pts, "closed": true, "dims": true}))
    });
    e.register_fn("constrain", |s: &mut DocSketch, c: Dynamic| -> Res<()> {
        let list = match arg(&c) {
            Value::Array(a) => a,
            v => vec![v],
        };
        s.doc.call("sketch_constrain", json!({"sketch_id": s.id, "constraints": list}))?;
        Ok(())
    });
    e.register_fn("info", |s: &mut DocSketch| -> Res<Dynamic> { Ok(from_json(&s.doc.call("sketch_info", json!({"sketch_id": s.id}))?)) });
    // region refs, for extruding one region of a sketch: doc.extrude(sk.regions()[0], 5)
    e.register_fn("regions", |s: &mut DocSketch| -> Res<Array> {
        let v = s.doc.call("sketch_info", json!({"sketch_id": s.id}))?;
        Ok(v["regions"].as_array().into_iter().flatten().map(|r| from_json(&r["ref"])).collect())
    });

    // features: each returns the new feature's id
    e.register_fn("feature", |d: &mut Doc, kind: &str, params: Map| d.feature(kind, arg(&Dynamic::from_map(params)), None));
    e.register_fn("feature", |d: &mut Doc, kind: &str, params: Map, name: &str| d.feature(kind, arg(&Dynamic::from_map(params)), Some(name.to_string())));
    e.register_fn("edit", |d: &mut Doc, id: i64, params: Map| -> Res<()> {
        d.call("edit_feature", json!({"id": id, "params": arg(&Dynamic::from_map(params))}))?;
        Ok(())
    });
    let profiles = |p: &Dynamic| match arg(p) {
        Value::Array(a) => Value::Array(a),
        v => json!([v]),
    };
    let extrude = move |d: &mut Doc, p: Dynamic, depth: Dynamic, opts: Map| -> Res<i64> {
        let mut params = arg(&Dynamic::from_map(opts));
        params["profiles"] = profiles(&p);
        params["depth"] = arg(&depth);
        default_op(d, &mut params);
        d.feature("extrude", params, None)
    };
    e.register_fn("extrude", move |d: &mut Doc, p: Dynamic, depth: Dynamic| extrude(d, p, depth, Map::new()));
    e.register_fn("extrude", extrude);
    let revolve = move |d: &mut Doc, p: Dynamic, axis: Dynamic, angle: Dynamic, opts: Map| -> Res<i64> {
        let mut params = arg(&Dynamic::from_map(opts));
        params["profiles"] = profiles(&p);
        params["axis"] = match arg(&axis) {
            Value::String(s) => json!({"kind": "axis", "axis": match s.to_lowercase().as_str() { "x" => 0, "y" => 1, _ => 2 }}),
            v if v.get("ref").is_some() => v["ref"].clone(),
            v => v,
        };
        params["angle"] = arg(&angle);
        if params.get("mode").is_none() {
            let full = matches!(&params["angle"], Value::Number(n) if (n.as_f64().unwrap_or(0.0) - 360.0).abs() < 1e-9);
            params["mode"] = json!(if full { "full" } else { "one" });
        }
        default_op(d, &mut params);
        d.feature("revolve", params, None)
    };
    e.register_fn("revolve", move |d: &mut Doc, p: Dynamic, axis: Dynamic, angle: Dynamic| revolve(d, p, axis, angle, Map::new()));
    e.register_fn("revolve", revolve);
    e.register_fn("fillet", |d: &mut Doc, edges: Dynamic, r: Dynamic| d.feature("fillet", json!({"edges": refs(&edges), "radius": arg(&r)}), None));
    e.register_fn("chamfer", |d: &mut Doc, edges: Dynamic, dist: Dynamic| d.feature("chamfer", json!({"edges": refs(&edges), "distance": arg(&dist)}), None));
    e.register_fn("shell", |d: &mut Doc, faces: Dynamic, t: Dynamic| d.feature("shell", json!({"faces": refs(&faces), "thickness": arg(&t)}), None));

    // faces and edges of the built parts, as list_geometry gives them (each has a `ref`)
    e.register_fn("faces", |d: &mut Doc| geometry(d, "faces", json!({})));
    e.register_fn("faces", |d: &mut Doc, f: Map| geometry(d, "faces", arg(&Dynamic::from_map(f))));
    e.register_fn("edges", |d: &mut Doc| geometry(d, "edges", json!({})));
    e.register_fn("edges", |d: &mut Doc, f: Map| geometry(d, "edges", arg(&Dynamic::from_map(f))));
}
