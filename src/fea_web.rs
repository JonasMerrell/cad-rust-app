//! A Simulation tab's result as a web page: one self-contained .html (three.js from a CDN) with the
//! part's outer surface coloured by a result field, a deformation scale, and the motion: each mode's
//! shape swinging for a modal study, the response playing over time for a transient one. Tapping the
//! part reads the value there.
//!
//! The solve runs here, once. The page gets the surface nodes and triangles of the mesh and, per
//! node, the fields and displacements (transient frames as 16-bit values against their peak, so a
//! long run stays a few MB).

use base64::Engine;
use glam::DVec3;
use serde_json::{json, Value};

use crate::fea::{self, Outcome, Solution, Study};

const TEMPLATE: &str = include_str!("fea_web.html");

/// Most frames of a transient run in the page.
const FRAMES: usize = 40;
/// Most modes in the page.
const MODES: usize = 12;

#[derive(Default)]
struct Blob(Vec<u8>);

impl Blob {
    fn put(&mut self, bytes: impl IntoIterator<Item = u8>, n: usize) -> Value {
        while self.0.len() % 4 != 0 {
            self.0.push(0);
        }
        let o = self.0.len();
        self.0.extend(bytes);
        json!([o, n])
    }

    fn f32s(&mut self, v: &[f32]) -> Value {
        self.put(v.iter().flat_map(|x| x.to_le_bytes()), v.len())
    }

    fn u32s(&mut self, v: &[u32]) -> Value {
        self.put(v.iter().flat_map(|x| x.to_le_bytes()), v.len())
    }

    fn i16s(&mut self, v: &[i16]) -> Value {
        self.put(v.iter().flat_map(|x| x.to_le_bytes()), v.len())
    }

    fn u16s(&mut self, v: &[u16]) -> Value {
        self.put(v.iter().flat_map(|x| x.to_le_bytes()), v.len())
    }
}

fn sig(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return "0".into();
    }
    let d = (3 - 1 - x.abs().log10().floor() as i32).max(0) as usize;
    format!("{x:.d$}")
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The page for a solved study.
pub fn page(study: &Study, o: &Outcome) -> Result<String, String> {
    let m = &o.mesh;
    // the surface: every node of the boundary triangles, renumbered
    let mut map = vec![u32::MAX; m.nodes.len()];
    let mut ids: Vec<usize> = Vec::new();
    for (f, _) in &m.faces {
        for &n in f {
            let n = n as usize;
            if map[n] == u32::MAX {
                map[n] = ids.len() as u32;
                ids.push(n);
            }
        }
    }
    if ids.is_empty() {
        return Err("the simulation's mesh has no surface".into());
    }
    let mut tris: Vec<u32> = Vec::with_capacity(m.faces.len() * 12);
    for (f, _) in &m.faces {
        // a 6-node triangle as four flat ones, as the editor draws it
        for st in [[0, 3, 5], [3, 1, 4], [5, 4, 2], [3, 4, 5]] {
            tris.extend(st.map(|i| map[f[i] as usize]));
        }
    }
    let pos: Vec<f32> = ids.iter().flat_map(|&i| [m.nodes[i].x as f32, m.nodes[i].y as f32, m.nodes[i].z as f32]).collect();
    let (lo, hi) = ids.iter().fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)), |(a, b), &i| (a.min(m.nodes[i]), b.max(m.nodes[i])));
    let size = hi - lo;
    let on_surface = |u: &[f64]| -> Vec<f32> { ids.iter().flat_map(|&i| [u[3 * i] as f32, u[3 * i + 1] as f32, u[3 * i + 2] as f32]).collect() };
    let scalar = |v: &[f64]| -> Vec<f32> { ids.iter().map(|&i| v[i] as f32).collect() };
    let range = |v: &[f32]| v.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), x| (a.min(*x), b.max(*x)));
    let mut blob = Blob::default();
    let field = |blob: &mut Blob, name: &str, unit: &str, v: Vec<f32>, rev: bool, from_zero: bool| -> Value {
        let (lo, hi) = range(&v);
        let lo = if from_zero { lo.min(0.0) } else { lo };
        json!({"name": name, "unit": unit, "lo": lo, "hi": hi, "rev": rev, "data": blob.f32s(&v)})
    };

    let mut sim = json!({
        "title": study.name,
        "material": o.material,
        "size": [size.x, size.y, size.z],
        "center": [(lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0, (lo.z + hi.z) / 2.0],
        "pos": blob.f32s(&pos),
        "tris": blob.u32s(&tris),
        "warnings": o.warnings,
    });
    let diag = size.length();
    match &o.solution {
        Solution::Static { u, stress, .. } => {
            let peak = fea::max_mag(u).max(1e-300);
            let vm: Vec<f64> = stress.iter().map(|s| crate::fea_solve::von_mises(s) as f64).collect();
            let p1: Vec<f64> = stress.iter().map(|s| crate::fea_solve::principal(s)[0] as f64).collect();
            let p3: Vec<f64> = stress.iter().map(|s| crate::fea_solve::principal(s)[2] as f64).collect();
            let safety: Vec<f64> = fea::node_safety(o, u, stress).into_iter().map(|v| v.min(10.0)).collect();
            let mag: Vec<f64> = u.chunks(3).map(|c| DVec3::new(c[0], c[1], c[2]).length()).collect();
            let mut safety_field = field(&mut blob, "Factor of safety", "", scalar(&safety), true, false);
            // a fixed scale, red at 1 or below and blue at 10 or more, as the pictures use
            safety_field["lo"] = json!(1.0);
            safety_field["hi"] = json!(10.0);
            sim["kind"] = json!("static");
            sim["deform"] = json!(diag * 0.05 / peak);
            sim["peak_mm"] = json!(peak);
            sim["disp"] = blob.f32s(&on_surface(u));
            sim["fields"] = json!([
                field(&mut blob, "Stress", "MPa", scalar(&vm), false, true),
                field(&mut blob, "Displacement", "mm", scalar(&mag), false, true),
                safety_field,
                field(&mut blob, "Max principal", "MPa", scalar(&p1), false, false),
                field(&mut blob, "Min principal", "MPa", scalar(&p3), false, false),
            ]);
        }
        Solution::Modal { modes } => {
            sim["kind"] = json!("modal");
            sim["deform"] = json!(diag * 0.05);
            let list: Vec<Value> = modes
                .iter()
                .take(MODES)
                .enumerate()
                .map(|(k, md)| {
                    let peak = fea::max_mag(&md.shape).max(1e-300);
                    let shape: Vec<f64> = md.shape.iter().map(|x| x / peak).collect();
                    json!({"mode": k + 1, "freq": md.freq, "loss": md.loss, "participation": md.participation, "shape": blob.f32s(&on_surface(&shape))})
                })
                .collect();
            sim["modes"] = json!(list);
        }
        Solution::Transient(t) => {
            // frames spread over the run, always with the one of largest displacement
            let n = t.frames.len();
            if n == 0 {
                return Err("the transient run kept no frames".into());
            }
            let biggest = (0..n).max_by(|&a, &b| fea::max_mag(&t.frames[a].1).total_cmp(&fea::max_mag(&t.frames[b].1))).unwrap_or(0);
            let mut pick: Vec<usize> = (0..FRAMES.min(n)).map(|k| if FRAMES >= n { k } else { k * (n - 1) / (FRAMES - 1) }).collect();
            if !pick.contains(&biggest) {
                pick.push(biggest);
                pick.sort_unstable();
            }
            let disps: Vec<Vec<f32>> = pick.iter().map(|&k| on_surface(&t.frames[k].1)).collect();
            let vms: Vec<Vec<f32>> = pick
                .iter()
                .map(|&k| {
                    let s = crate::fea_solve::nodal_stress(&o.mesh, &o.mat, &o.body_mats, &t.frames[k].1);
                    ids.iter().map(|&i| crate::fea_solve::von_mises(&s[i])).collect()
                })
                .collect();
            let dmax = disps.iter().flatten().fold(0f32, |a, x| a.max(x.abs())).max(1e-30);
            let vmax = vms.iter().flatten().fold(0f32, |a, x| a.max(*x)).max(1e-30);
            let peak = t.max_disp.iter().copied().fold(0.0, f64::max).max(1e-300);
            sim["kind"] = json!("transient");
            sim["deform"] = json!(diag * 0.05 / peak);
            sim["peak_mm"] = json!(peak);
            sim["frames"] = json!({
                "times": pick.iter().map(|&k| t.frames[k].0).collect::<Vec<_>>(),
                "disp_scale": dmax / 32767.0,
                "vm_scale": vmax / 65535.0,
                "vm_max": vmax,
                "disp": disps.iter().map(|d| {
                    let q: Vec<i16> = d.iter().map(|x| (x / dmax * 32767.0).round() as i16).collect();
                    blob.i16s(&q)
                }).collect::<Vec<_>>(),
                "vm": vms.iter().map(|v| {
                    let q: Vec<u16> = v.iter().map(|x| (x / vmax * 65535.0).round() as u16).collect();
                    blob.u16s(&q)
                }).collect::<Vec<_>>(),
            });
        }
    }
    let what = match &o.solution {
        Solution::Static { .. } => "static",
        Solution::Modal { .. } => "modal",
        Solution::Transient(_) => "transient",
    };
    let title = format!("{} · {what} · {}", study.name, o.material);
    let note = format!("part {} × {} × {} mm", sig(size.x), sig(size.y), sig(size.z));
    sim["note"] = json!(note);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&blob.0);
    let data = format!("window.SIM = {}; window.BLOB64 = \"{b64}\";", serde_json::to_string(&sim).map_err(|e| e.to_string())?.replace("</", "<\\/"));
    Ok(TEMPLATE.replacen("__TITLE__", &html_escape(&title), 1).replacen("/*__DATA__*/", &data, 1))
}
