//! Agent tools for Simulation tabs: linear elastic static, modal and transient studies.

use glam::DVec3;
use serde_json::{Value, json};

use super::{R, Session, round};
use crate::drawing_ui::DocCtx;
use crate::fea::{self, Analysis, FacePick, Load, LoadKind, Outcome, ProfileKind, Solution, Study, Support, SupportKind};

fn face_arg(v: &Value) -> R<FacePick> {
    let p = v.get("point").and_then(|p| p.as_array()).ok_or("each face needs point [x,y,z] (and normal), e.g. a face ref from list_geometry")?;
    let get = |a: &Vec<Value>, i: usize| a.get(i).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let point = [get(p, 0), get(p, 1), get(p, 2)];
    let normal = v.get("normal").and_then(|n| n.as_array()).map(|n| [get(n, 0), get(n, 1), get(n, 2)]).unwrap_or([0.0; 3]);
    Ok(FacePick { point, normal })
}

fn faces_arg(v: &Value) -> R<Vec<FacePick>> {
    match v.get("faces") {
        None => Ok(vec![]),
        Some(Value::Array(a)) => a.iter().map(face_arg).collect(),
        Some(_) => Err("faces: a list of face refs".into()),
    }
}

fn dir_arg(v: &Value) -> Option<[f64; 3]> {
    let d = v.get("direction")?;
    if let Some(s) = d.as_str() {
        return Some(match s.trim() {
            "+x" | "x" => [1.0, 0.0, 0.0],
            "-x" => [-1.0, 0.0, 0.0],
            "+y" | "y" => [0.0, 1.0, 0.0],
            "-y" => [0.0, -1.0, 0.0],
            "+z" | "z" => [0.0, 0.0, 1.0],
            _ => [0.0, 0.0, -1.0],
        });
    }
    let a = d.as_array()?;
    let v = DVec3::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?).normalize_or_zero();
    (v != DVec3::ZERO).then(|| v.to_array())
}

/// Apply the fields of create/edit_simulation to a study.
fn apply(s: &mut Study, a: &Value) -> R<()> {
    if let Some(n) = a.get("name").and_then(|v| v.as_str()) {
        s.name = n.to_string();
    }
    if let Some(n) = a.get("studio").and_then(|v| v.as_u64()) {
        s.studio = n as usize;
    }
    if let Some(b) = a.get("bodies") {
        s.bodies = serde_json::from_value(b.clone()).map_err(|_| "bodies: [[feature, index], …]")?;
    }
    if let Some(k) = a.get("analysis").and_then(|v| v.as_str()) {
        s.analysis = Analysis::from_key(k).ok_or("analysis: static, modal or transient")?;
    }
    if let Some(m) = a.get("material").and_then(|v| v.as_str()) {
        if !m.is_empty() && fea::elastic(m).is_none() {
            return Err(format!("material must be one of {} (or \"\" for the part's own)", fea::MATERIALS.iter().map(|x| x.0).collect::<Vec<_>>().join(", ")));
        }
        s.material = fea::MATERIALS.iter().find(|x| x.0.eq_ignore_ascii_case(m)).map(|x| x.0.to_string()).unwrap_or_default();
    }
    match a.get("mesh_size") {
        Some(Value::Null) => s.mesh_size = None,
        Some(v) => s.mesh_size = Some(v.as_f64().filter(|x| *x > 0.0).ok_or("mesh_size: element size in mm (null for auto)")?),
        None => {}
    }
    if let Some(n) = a.get("modes").and_then(|v| v.as_u64()) {
        s.modes = (n as usize).clamp(1, 50);
    }
    if let Some(m) = a.get("loss_factors") {
        let m: std::collections::BTreeMap<String, f64> = serde_json::from_value(m.clone()).map_err(|_| "loss_factors: {material: η, …}")?;
        for k in m.keys() {
            if fea::elastic(k).is_none() {
                return Err(format!("loss_factors: no material {k}"));
            }
        }
        s.loss_factors = m;
    }
    if let Some(t) = a.get("transient") {
        let tr = &mut s.transient;
        if let Some(p) = t.get("profile").and_then(|v| v.as_str()) {
            tr.profile = ProfileKind::ALL.into_iter().find(|k| k.key() == p).ok_or("profile: step, ramp, pulse or sine")?;
        }
        if let Some(v) = t.get("time").and_then(|v| v.as_f64()) {
            tr.time = v.max(1e-9);
        }
        if let Some(v) = t.get("frequency").and_then(|v| v.as_f64()) {
            tr.frequency = v.max(1e-6);
        }
        match t.get("duration") {
            Some(Value::Null) => tr.duration = None,
            Some(v) => tr.duration = v.as_f64().filter(|x| *x > 0.0),
            None => {}
        }
        if let Some(v) = t.get("steps").and_then(|v| v.as_u64()) {
            tr.steps = (v as usize).clamp(10, 20000);
        }
        if let Some(v) = t.get("damping").and_then(|v| v.as_f64()) {
            tr.damping = v.clamp(0.0, 1.0);
        }
    }
    if let Some(list) = a.get("supports") {
        let list = list.as_array().ok_or("supports: a list")?;
        s.supports.clear();
        for x in list {
            let kind = match x.get("kind").and_then(|v| v.as_str()).unwrap_or("fixed") {
                "fixed" => SupportKind::Fixed,
                "roller" => SupportKind::Roller,
                k => return Err(format!("support kind {k}: fixed or roller")),
            };
            let id = s.add_support(kind);
            let sp: &mut Support = s.support_mut(id).unwrap();
            sp.faces = faces_arg(x)?;
            if let Some(n) = x.get("name").and_then(|v| v.as_str()) {
                sp.name = n.into();
            }
        }
    }
    if let Some(list) = a.get("loads") {
        let list = list.as_array().ok_or("loads: a list")?;
        s.loads.clear();
        for x in list {
            let kind = match x.get("kind").and_then(|v| v.as_str()).unwrap_or("force") {
                "force" => LoadKind::Force,
                "pressure" => LoadKind::Pressure,
                "gravity" => LoadKind::Gravity,
                k => return Err(format!("load kind {k}: force, pressure or gravity")),
            };
            let id = s.add_load(kind);
            let l: &mut Load = s.load_mut(id).unwrap();
            l.faces = faces_arg(x)?;
            if let Some(v) = x.get("value").and_then(|v| v.as_f64()) {
                l.value = v;
            }
            if let Some(d) = dir_arg(x) {
                l.direction = d;
            }
            if let Some(n) = x.get("normal").and_then(|v| v.as_bool()) {
                l.normal = n;
            }
            if let Some(p) = x.get("at") {
                l.at = Some(serde_json::from_value(p.clone()).map_err(|_| "at: [x, y, z]")?);
                l.radius = Some(x.get("radius").and_then(|v| v.as_f64()).filter(|r| *r > 0.0).ok_or("give radius (mm) with at")?);
            }
            if let Some(n) = x.get("name").and_then(|v| v.as_str()) {
                l.name = n.into();
            }
        }
    }
    Ok(())
}

fn v3(v: DVec3) -> Value {
    json!([round(v.x), round(v.y), round(v.z)])
}

fn sig(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return 0.0;
    }
    let k = 10f64.powi(4 - v.abs().log10().ceil() as i32);
    (v * k).round() / k
}

fn summary(s: &Study, o: &Outcome) -> Value {
    let m = &o.mesh;
    let mut v = json!({
        "analysis": s.analysis.key(),
        "material": o.material,
        "material_props": {"E_mpa": o.mat.e, "poisson": o.mat.nu, "density_g_cm3": o.mat.rho * 1e9, "yield_mpa": o.mat.yield_mpa, "loss_factor": o.mat.eta},
        "mass_kg": sig(o.mass * 1e3),
        "mesh": {"nodes": m.nodes.len(), "elements": m.tets.len(), "element": "10-node tetrahedron", "element_size_mm": round(s.mesh_size.unwrap_or(0.0)), "smallest_dihedral_deg": round(m.min_dihedral)},
        "warnings": o.warnings,
        "units": "mm, N, MPa, s",
    });
    match &o.solution {
        Solution::Static { u, stress, reactions } => {
            let vm: Vec<f32> = stress.iter().map(crate::fea_solve::von_mises).collect();
            let (k, max_vm) = vm.iter().enumerate().fold((0, 0.0f32), |a, (i, x)| if *x > a.1 { (i, *x) } else { a });
            let (kd, max_d) = u.chunks(3).enumerate().map(|(i, c)| (i, (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt())).fold((0, 0.0), |a, b| if b.1 > a.1 { b } else { a });
            v["max_von_mises_mpa"] = json!(sig(max_vm as f64));
            v["max_stress_at"] = v3(m.nodes[k]);
            v["max_displacement_mm"] = json!(sig(max_d));
            v["max_displacement_at"] = v3(m.nodes[kd]);
            v["displacement_there"] = v3(DVec3::new(u[3 * kd], u[3 * kd + 1], u[3 * kd + 2]));
            v["factor_of_safety"] = json!(sig(o.mat.yield_mpa / (max_vm as f64).max(1e-12)));
            v["reactions"] = json!(
                reactions
                    .iter()
                    .map(|(id, r)| json!({"support": s.supports.iter().find(|x| x.id == *id).map(|x| x.name.clone()), "force_n": v3(*r), "magnitude_n": sig(r.length())}))
                    .collect::<Vec<_>>()
            );
        }
        Solution::Modal { modes } => {
            v["modes"] = json!(
                modes
                    .iter()
                    .enumerate()
                    .map(|(i, m)| {
                        let mut j = json!({"mode": i + 1, "frequency_hz": sig(m.freq), "mass_participation_xyz": m.participation.map(|x| round(x))});
                        // with damping: the modal loss factor and the time for the mode to die down to 1/1000 (−60 dB)
                        if m.loss > 0.0 && m.freq > 1.0 {
                            j["loss_factor"] = json!(sig(m.loss));
                            j["decay_60db_s"] = json!(sig(2.0 * 1000f64.ln() / (m.loss * std::f64::consts::TAU * m.freq)));
                        }
                        j
                    })
                    .collect::<Vec<_>>()
            );
        }
        Solution::Transient(t) => {
            let (ip, peak) = t.max_disp.iter().copied().enumerate().fold((0, 0.0), |a, b| if b.1 > a.1 { b } else { a });
            v["lowest_frequency_hz"] = json!(sig(t.f1));
            v["duration_s"] = json!(sig(t.duration));
            v["time_step_s"] = json!(sig(t.dt));
            v["rayleigh_alpha_beta"] = json!([sig(t.rayleigh.0), sig(t.rayleigh.1)]);
            v["peak_displacement_mm"] = json!(sig(peak));
            v["peak_at_s"] = json!(sig(t.times[ip]));
            v["final_displacement_mm"] = json!(sig(t.max_disp.last().copied().unwrap_or(0.0)));
            v["decay_to_10pct_s"] = json!(t.decay.map(sig));
            if t.free {
                v["rigid_motion_removed"] = json!(true);
            }
            // a coarse history: 40 samples of the largest displacement
            let n = t.times.len();
            let step = (n / 40).max(1);
            v["history"] = json!((0..n).step_by(step).map(|i| [sig(t.times[i]), sig(t.max_disp[i])]).collect::<Vec<_>>());
            // peak stress over the kept frames
            let mut best = (0.0f32, 0.0);
            for (tt, u) in &t.frames {
                let vm = crate::fea_solve::nodal_stress(m, &o.mat, &o.body_mats, u).iter().map(crate::fea_solve::von_mises).fold(0.0f32, f32::max);
                if vm > best.0 {
                    best = (vm, *tt);
                }
            }
            v["peak_von_mises_mpa"] = json!(sig(best.0 as f64));
            v["peak_stress_at_s"] = json!(sig(best.1));
        }
    }
    v
}

impl Session {
    fn sim_idx(&self, a: &Value) -> R<usize> {
        let i = a.get("simulation").and_then(|v| v.as_u64()).ok_or("give simulation (the Simulation tab's index from create_simulation)")? as usize;
        if i >= self.ws.sims.len() {
            return Err(format!("no Simulation tab {i}"));
        }
        Ok(i)
    }

    fn sim_solve(&mut self, si: usize) -> R<(Study, Outcome)> {
        self.sync();
        let s = self.ws.sims[si].clone();
        self.ensure_built(s.studio)?;
        let input = fea::input(self.bodies(s.studio), self.studio_doc(s.studio), &s.bodies);
        if input.tris.is_empty() {
            return Err(format!("Part Studio {} has no parts to analyse", s.studio));
        }
        let mut st = s.clone();
        st.mesh_size = Some(s.mesh_size.unwrap_or_else(|| input.auto_size()));
        let o = fea::run(&st, &input, None, &|_, _| {}, &|| false)?;
        Ok((st, o))
    }

    pub(super) fn fea_call(&mut self, name: &str, a: &Value) -> R<(Value, Option<Vec<u8>>)> {
        let v = match name {
            "create_simulation" => {
                self.sync();
                let n = self.ws.sims.len() + 1;
                let mut s = Study { name: format!("Simulation {n}"), studio: self.cur, ..Default::default() };
                apply(&mut s, a)?;
                if s.studio >= self.ws.studios.len() {
                    return Err(format!("no Part Studio {}", s.studio));
                }
                self.ws.sims.push(s.clone());
                json!({"simulation": self.ws.sims.len() - 1, "study": s, "hint": "run solve_simulation for results"})
            }
            "edit_simulation" => {
                let si = self.sim_idx(a)?;
                let mut s = self.ws.sims[si].clone();
                apply(&mut s, a)?;
                self.ws.sims[si] = s.clone();
                json!({"simulation": si, "study": s})
            }
            "solve_simulation" => {
                let si = self.sim_idx(a)?;
                let (s, o) = self.sim_solve(si)?;
                let mut v = summary(&s, &o);
                v["simulation"] = json!(si);
                if a.get("render").and_then(|v| v.as_bool()).unwrap_or(false) {
                    let png = self.render_outcome(&o, a)?;
                    return Ok((v, Some(png)));
                }
                v
            }
            "render_simulation" => {
                let si = self.sim_idx(a)?;
                let (_, o) = self.sim_solve(si)?;
                let png = self.render_outcome(&o, a)?;
                if let Some(p) = a.get("path").and_then(|p| p.as_str()) {
                    std::fs::write(p, &png).map_err(|e| e.to_string())?;
                }
                return Ok((json!({"simulation": si, "saved": a.get("path")}), Some(png)));
            }
            _ => return Err(format!("unknown tool {name}")),
        };
        Ok((v, None))
    }

    /// Colour map on the deformed mesh: field (stress, displacement, safety), mode (1-based), time (s), deform (× auto).
    fn render_outcome(&self, o: &Outcome, a: &Value) -> R<Vec<u8>> {
        let field = a.get("field").and_then(|v| v.as_str()).unwrap_or("stress");
        let deform = a.get("deform").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let (u, stress): (Vec<f64>, Option<Vec<[f32; 6]>>) = match &o.solution {
            Solution::Static { u, stress, .. } => (u.clone(), Some(stress.clone())),
            Solution::Modal { modes } => {
                let k = a.get("mode").and_then(|v| v.as_u64()).unwrap_or(1).max(1) as usize - 1;
                let m = modes.get(k).ok_or_else(|| format!("only {} modes", modes.len()))?;
                let peak = fea::max_mag(&m.shape).max(1e-300);
                (m.shape.iter().map(|x| x / peak).collect(), None)
            }
            Solution::Transient(t) => {
                let at = a.get("time").and_then(|v| v.as_f64());
                let fr = match at {
                    Some(tt) => t.frames.iter().min_by(|x, y| (x.0 - tt).abs().total_cmp(&(y.0 - tt).abs())).unwrap(),
                    // the frame with the largest displacement
                    None => t.frames.iter().max_by(|x, y| fea::max_mag(&x.1).total_cmp(&fea::max_mag(&y.1))).unwrap(),
                };
                (fr.1.clone(), Some(crate::fea_solve::nodal_stress(&o.mesh, &o.mat, &o.body_mats, &fr.1)))
            }
        };
        let m = &o.mesh;
        let disp: Vec<DVec3> = u.chunks(3).map(|c| DVec3::new(c[0], c[1], c[2])).collect();
        let vals: Vec<f64> = match (field, &stress) {
            ("stress", Some(s)) => s.iter().map(|x| crate::fea_solve::von_mises(x) as f64).collect(),
            ("safety", Some(s)) => s.iter().map(|x| (o.mat.yield_mpa / (crate::fea_solve::von_mises(x) as f64).max(1e-9)).min(10.0)).collect(),
            _ => disp.iter().map(|d| d.length()).collect(),
        };
        let (lo, hi) = vals.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(*v), b.max(*v)));
        // safety on a fixed scale (red at 1 or below, blue at 10 or more), so a part that is safe everywhere
        // doesn't come out red
        let (lo, hi) = if field == "safety" { (1.0, 10.0) } else { (lo.min(0.0), hi) };
        let (blo, bhi) = m.nodes.iter().fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)), |(a, b), p| (a.min(*p), b.max(*p)));
        let peak = fea::max_mag(&u).max(1e-300);
        let k = deform * (bhi - blo).length() * 0.05 / peak;
        // one mesh per colour band
        const BANDS: usize = 32;
        let mut meshes: Vec<crate::kernel::BodyMesh> = (0..BANDS).map(|_| Default::default()).collect();
        for (f, _) in &m.faces {
            for st in [[0, 3, 5], [3, 1, 4], [5, 4, 2], [3, 4, 5]] {
                let ids = st.map(|i| f[i] as usize);
                let p = ids.map(|i| m.nodes[i] + disp[i] * k);
                let val = ids.iter().map(|i| vals[*i]).sum::<f64>() / 3.0;
                let mut t = ((val - lo) / (hi - lo).max(1e-300)).clamp(0.0, 1.0);
                if field == "safety" {
                    t = 1.0 - t;
                }
                let band = ((t * BANDS as f64) as usize).min(BANDS - 1);
                let mm = &mut meshes[band];
                let n = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
                for q in p {
                    let i = mm.positions.len() as u32 / 3;
                    mm.positions.extend([q.x as f32, q.y as f32, q.z as f32]);
                    mm.normals.extend([n.x as f32, n.y as f32, n.z as f32]);
                    mm.indices.push(i);
                }
                mm.tri_face.push(0);
            }
        }
        for mm in meshes.iter_mut() {
            let mut lo = [f64::INFINITY; 3];
            let mut hi = [f64::NEG_INFINITY; 3];
            for p in mm.positions.chunks(3) {
                for c in 0..3 {
                    lo[c] = lo[c].min(p[c] as f64);
                    hi[c] = hi[c].max(p[c] as f64);
                }
            }
            mm.bbox = [lo, hi];
        }
        let items: Vec<(&crate::kernel::BodyMesh, crate::assembly::Pose, [u8; 3])> = meshes
            .iter()
            .enumerate()
            .filter(|(_, mm)| !mm.indices.is_empty())
            .map(|(b, mm)| {
                let c = crate::fea_ui::colormap((b as f32 + 0.5) / BANDS as f32);
                (mm, crate::assembly::Pose::default(), [c.r(), c.g(), c.b()])
            })
            .collect();
        let view = a.get("view").and_then(|v| v.as_str()).unwrap_or("iso");
        let w = a.get("width").and_then(|v| v.as_u64()).unwrap_or(900).clamp(64, 4096) as u32;
        let h = a.get("height").and_then(|v| v.as_u64()).unwrap_or(700).clamp(64, 4096) as u32;
        let dir = a.get("direction").and_then(|d| d.as_array()).and_then(|d| Some([d.first()?.as_f64()?, d.get(1)?.as_f64()?, d.get(2)?.as_f64()?]));
        super::raster_scene(&items, &[], view, dir, w, h)
    }
}

pub fn tools() -> Vec<Value> {
    let obj = |props: Value, req: &[&str]| json!({"type": "object", "properties": props, "required": req});
    let fields = json!({
        "simulation": {"type": "integer"}, "name": {"type": "string"}, "studio": {"type": "integer"}, "bodies": {"type": "array"},
        "analysis": {"type": "string"}, "material": {"type": "string"}, "loss_factors": {"type": "object"}, "mesh_size": {}, "modes": {"type": "integer"},
        "transient": {"type": "object"}, "supports": {"type": "array"}, "loads": {"type": "array"}
    });
    vec![
        json!({"name": "create_simulation", "description": "Add a Simulation tab: linear elastic finite element analysis of a Part Studio's parts (auto-meshed with 10-node tetrahedra; parts that touch are bonded). Units mm, N, MPa, s. Fields: name, studio (default the active one), bodies [[f,i],…] (default every visible part), analysis (static: deflection and stress; modal: natural frequencies and mode shapes; transient: response over time), material (applies to every part; \"\" = each part's own material from set_part, and parts of different materials are meshed separately and bonded where they touch: Steel, Stainless steel, Stainless steel 303, Aluminium 6061, Aluminium 7075, Brass, Copper, Titanium, Tungsten alloy, Urethane foam, PLA, ABS, PETG, Nylon, Oak, Pine; \"\" = the part's material, steel when unset), mesh_size (element size mm, null = auto), modes (modal: how many, default 6), transient {profile: step | ramp | pulse (half-sine impact) | sine (shaking), time (ramp/pulse length s), frequency (sine Hz), duration (s, null = five periods of the lowest natural frequency), steps (default 400), damping (fraction of critical, default 0.02)}, loss_factors {material: η} (replace the library's structural loss factors; each material's η damps transient runs and modal results report each mode's loss factor and −60 dB decay time), supports [{kind: fixed | roller (slides on its faces, also symmetry planes), faces: [face refs {point, normal} from list_geometry], name}], loads [{kind: force (total N spread over the faces) | pressure (MPa into the faces) | gravity (value in g on the whole part), value, direction ([x,y,z] or \"-z\" etc., default -z), normal (force pushes into the faces instead), faces, at [x,y,z] + radius (only the part of the faces within radius of that point, e.g. an impact at the face centre), name}]. Static needs at least one support and one load; modal and transient work with or without supports (a free transient reports the vibration with the rigid motion taken out). Returns the simulation index.", "inputSchema": obj(fields.clone(), &[])}),
        json!({"name": "edit_simulation", "description": "Change a Simulation tab (same fields as create_simulation; supports and loads replace the whole lists).", "inputSchema": obj(fields, &["simulation"])}),
        json!({"name": "solve_simulation", "description": "Mesh and solve a Simulation tab. Static: largest von Mises stress and where, largest displacement, factor of safety (yield / max stress), reaction force of each support. Modal: natural frequencies with effective mass shares along x, y, z. Transient: lowest natural frequency, time step, peak displacement and when, peak stress, and a displacement history. render:true also returns a PNG (as render_simulation).", "inputSchema": obj(json!({"simulation": {"type": "integer"}, "render": {"type": "boolean"}, "field": {"type": "string"}, "mode": {"type": "integer"}, "time": {"type": "number"}, "view": {"type": "string"}, "width": {"type": "integer"}, "height": {"type": "integer"}}), &["simulation"])}),
        json!({"name": "render_simulation", "description": "PNG of a Simulation tab's result as a colour map (blue low → red high) on the exaggerated deformed shape: field stress (von Mises, default), displacement, or safety (red where the factor of safety is low); mode (modal, 1-based), time (transient s; default the moment of largest displacement), deform (× the automatic exaggeration, 0 = undeformed), view/direction/width/height/path as in render.", "inputSchema": obj(json!({"simulation": {"type": "integer"}, "field": {"type": "string"}, "mode": {"type": "integer"}, "time": {"type": "number"}, "deform": {"type": "number"}, "view": {"type": "string"}, "direction": {"type": "array"}, "width": {"type": "integer"}, "height": {"type": "integer"}, "path": {"type": "string"}}), &["simulation"])}),
    ]
}
