//! Exact geometry queries for the agent tools, run in the kernel worker on the Part Studio's
//! solids: mass properties, point classification and ray casting (the same OpenCascade calls
//! the Rhai `mass_props`, `classify` and `ray_cast` use).

use glam::DVec3;
use serde_json::{json, Value};

use crate::doc::{BodyId, Document};

/// Names of OCCT's point states, in its order.
const CLASSES: [&str; 4] = ["in", "out", "on", "unknown"];

fn v3(v: &Value) -> Result<DVec3, String> {
    let a = v.as_array().filter(|a| a.len() == 3).ok_or("points and directions are [x, y, z]")?;
    let f = |i: usize| a[i].as_f64().ok_or("points and directions are [x, y, z] numbers");
    Ok(DVec3::new(f(0)?, f(1)?, f(2)?))
}

fn body_of(v: &Value) -> Option<BodyId> {
    let a = v.as_array()?;
    Some((a.first()?.as_u64()?, a.get(1)?.as_u64()? as u32))
}

/// Run query `q` on the solids of `doc`:
/// * `{"mass": [{"body": [f, i], "density": g/mm³}, …]}`: volume, centre of gravity and inertia per body;
/// * `{"classify": [[x, y, z], …], "tolerance": mm}`: for each point, in / on / out of each body;
/// * `{"ray": {"origins": [[x, y, z], …], "direction": [x, y, z]}}`: surface crossings along each ray.
pub fn run(doc: &Document, q: &Value) -> Result<Value, String> {
    let bodies = crate::kernel::build_bodies(doc);
    let want: Option<Vec<BodyId>> = q.get("bodies").and_then(|b| b.as_array()).map(|b| b.iter().filter_map(body_of).collect());
    let picked: Vec<&crate::kernel::Body> = bodies.iter().filter(|b| want.as_ref().map(|w| w.contains(&b.id)).unwrap_or(true)).collect();
    if let Some(list) = q.get("mass").and_then(|m| m.as_array()) {
        let mut out = vec![];
        for item in list {
            let id = item.get("body").and_then(body_of).ok_or("mass: give body")?;
            let density = item.get("density").and_then(|d| d.as_f64()).unwrap_or(0.0);
            let b = bodies.iter().find(|b| b.id == id).ok_or_else(|| format!("no part {id:?}"))?;
            let (vol, cg, inertia) = crate::occ::mass_props(&b.shape, 0)?;
            let i = inertia * density;
            out.push(json!({"body": [id.0, id.1], "volume": vol, "cg": cg.to_array(), "mass": vol * density, "inertia": [i.row(0).to_array(), i.row(1).to_array(), i.row(2).to_array()]}));
        }
        return Ok(json!({"mass": out}));
    }
    if let Some(list) = q.get("classify").and_then(|p| p.as_array()) {
        let pts: Vec<DVec3> = list.iter().map(v3).collect::<Result<_, _>>()?;
        let tol = q.get("tolerance").and_then(|t| t.as_f64()).unwrap_or(1e-4);
        let mut per_body = vec![];
        for b in &picked {
            per_body.push((b.id, crate::occ::classify(&b.shape, &pts, tol)?));
        }
        let out: Vec<Value> = (0..pts.len())
            .map(|k| {
                // inside wins over on, on over out, across the parts
                let mut best: (i32, Option<BodyId>) = (1, None);
                for (id, c) in &per_body {
                    let rank = |c: i32| match c {
                        0 => 0,
                        2 => 1,
                        _ => 2,
                    };
                    if rank(c[k]) < rank(best.0) {
                        best = (c[k], Some(*id));
                    }
                }
                json!({"point": pts[k].to_array(), "result": CLASSES[best.0.clamp(0, 3) as usize], "body": best.1.map(|b| json!([b.0, b.1]))})
            })
            .collect();
        return Ok(json!({"points": out}));
    }
    if let Some(r) = q.get("ray") {
        let d = v3(r.get("direction").ok_or("give direction")?)?.normalize_or_zero();
        if d == DVec3::ZERO {
            return Err("direction: not zero".into());
        }
        let origins: Vec<DVec3> = r.get("origins").and_then(|o| o.as_array()).ok_or("give origins")?.iter().map(v3).collect::<Result<_, _>>()?;
        let mut tris = vec![];
        let mut owner = vec![];
        for b in &picked {
            let (v, t, _) = crate::script_api::tessellate(&b.shape, 0.01, 15.0)?;
            for x in t {
                tris.push([v[x[0] as usize], v[x[1] as usize], v[x[2] as usize]]);
                owner.push(b.id);
            }
        }
        let g = crate::script_num::RayGrid::new(tris, d);
        let out: Vec<Value> = origins
            .iter()
            .map(|o| {
                let hits: Vec<Value> = g
                    .hits(*o, 0.0, None)
                    .into_iter()
                    .map(|(t, ti)| json!({"distance": t, "point": (*o + d * t).to_array(), "body": [owner[ti as usize].0, owner[ti as usize].1]}))
                    .collect();
                json!({"origin": o.to_array(), "hits": hits})
            })
            .collect();
        return Ok(json!({"rays": out}));
    }
    Err("unknown query".into())
}
