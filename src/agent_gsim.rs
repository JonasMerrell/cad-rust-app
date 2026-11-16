//! Agent tool `simulate_gcode`: run a 3D printer's G-code (through its Klipper config and macros)
//! on the printer's own assembly and report collisions, near misses, hits on the part being
//! printed and axes running past their travel, with pictures of each (see `gsim`).

use std::collections::{BTreeMap, HashMap, HashSet};

use glam::DVec3;
use serde_json::{json, Value};

use super::{Session, R};
use crate::asm_anim::{self, Target};
use crate::assembly::{self, Drive, MateKind, Pose};
use crate::drawing_ui::DocCtx;
use crate::gsim::cfg::{Config, Machine};
use crate::gsim::collide::Aabb;
use crate::gsim::interp::{Interp, Kind};
use crate::gsim::sim::{self, AxisMap, Body, Group, Model, Options};
use crate::gsim::{plan, print::Print};

fn r3(v: [f64; 3]) -> [f64; 3] {
    v.map(|x| (x * 1000.0).round() / 1000.0)
}

fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn v3(v: &Value) -> Option<DVec3> {
    let a = v.as_array()?;
    Some(DVec3::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?))
}

/// Ray straight down from p onto triangles: the highest hit below p.
fn hit_below(g: &Group, p: DVec3) -> Option<f64> {
    let mut best: Option<f64> = None;
    for (t, _) in g.tris() {
        let (a, b, c) = (t[0], t[1], t[2]);
        // barycentric in plan
        let d = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
        if d.abs() < 1e-12 {
            continue;
        }
        let u = ((p.x - a.x) * (c.y - a.y) - (c.x - a.x) * (p.y - a.y)) / d;
        let v = ((b.x - a.x) * (p.y - a.y) - (p.x - a.x) * (b.y - a.y)) / d;
        if u < -1e-9 || v < -1e-9 || u + v > 1.0 + 1e-9 {
            continue;
        }
        let z = a.z + u * (b.z - a.z) + v * (c.z - a.z);
        if z <= p.z + 1e-6 && best.map(|bz| z > bz).unwrap_or(true) {
            best = Some(z);
        }
    }
    best
}

/// The beads printed before move `upto`, as one mesh in world coordinates (bed shifted by `shift`).
fn print_mesh(p: &Print, upto: usize, shift: DVec3) -> crate::kernel::BodyMesh {
    let mut m = crate::kernel::BodyMesh { name: "print".into(), ..Default::default() };
    // box faces of the 8 corners (p0/p1 × side ± × top/bottom)
    const F: [[u32; 4]; 6] = [[0, 2, 6, 4], [1, 5, 7, 3], [0, 4, 5, 1], [2, 3, 7, 6], [0, 1, 3, 2], [4, 6, 7, 5]];
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for b in p.beads.iter().filter(|b| (b.mv as usize) < upto) {
        let base = (m.positions.len() / 3) as u32;
        for q in &b.pts {
            let w = *q + shift;
            lo = lo.min(w);
            hi = hi.max(w);
            m.positions.extend([w.x as f32, w.y as f32, w.z as f32]);
        }
        for f in F {
            m.indices.extend([base + f[0], base + f[1], base + f[2], base + f[0], base + f[2], base + f[3]]);
        }
    }
    if lo.x.is_finite() {
        m.bbox = [lo.to_array(), hi.to_array()];
    }
    m
}

impl Session {
    pub(super) fn gsim_call(&mut self, a: &Value) -> R<(Value, Option<Vec<u8>>)> {
        let t_start = std::time::Instant::now();
        self.sync();
        // ---- the assembly
        let ai = match a.get("assembly") {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as usize,
            Some(Value::String(s)) => self.ws.assemblies.iter().position(|x| &x.name == s).ok_or(format!("no assembly named {s}"))?,
            _ => self.ws.assemblies.iter().position(|x| x.name.eq_ignore_ascii_case("printer")).ok_or("give assembly (index or name)")?,
        };
        if ai >= self.ws.assemblies.len() {
            return Err(format!("no assembly {ai}"));
        }
        // ---- the printer's config and the G-code
        let (cfg, cfg_files) = match (a.get("config").and_then(|v| v.as_str()), a.get("config_text").and_then(|v| v.as_str())) {
            (Some(p), _) => {
                let c = Config::load(std::path::Path::new(&self.input_path(p)))?;
                let f = c.files.clone();
                (c, f)
            }
            (None, Some(t)) => (Config::parse(t, None)?, vec![]),
            _ => (Config::default(), vec![]),
        };
        let mut machine = Machine::from_config(&cfg);
        let mut notes: Vec<String> = machine.notes.clone();
        if cfg_files.is_empty() && a.get("config_text").is_none() {
            notes.push("no printer.cfg given: Klipper's limits and macros aren't checked (give config)".into());
            for ax in &mut machine.axes {
                ax.min = -1e6;
                ax.max = 1e6;
            }
        }
        let gcode = match (a.get("gcode").and_then(|v| v.as_str()), a.get("gcode_text").and_then(|v| v.as_str())) {
            (Some(p), _) => {
                let path = self.input_path(p);
                std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?
            }
            (None, Some(t)) => t.to_string(),
            _ => return Err("give gcode (a file) or gcode_text".into()),
        };

        // ---- which mates are the axes
        self.build_for_assembly(ai)?;
        self.settle(ai);
        let orig = self.ws.assemblies[ai].clone();
        let mate_ref = |v: &Value| -> R<u64> {
            let m = match v {
                Value::Number(n) => orig.mates.iter().find(|m| Some(m.id) == n.as_u64()),
                Value::String(s) => orig.mates.iter().find(|m| &m.name == s || m.id.to_string() == *s),
                _ => None,
            };
            let m = m.ok_or(format!("no mate {v}"))?;
            if m.kind != MateKind::Slider && m.kind != MateKind::Cylindrical {
                return Err(format!("mate {} is a {} mate; axes are sliders", m.name, m.kind.label().to_lowercase()));
            }
            Ok(m.id)
        };
        let explicit = a.get("axes").and_then(|v| v.as_object());
        let mates: Vec<u64> = match explicit {
            Some(o) => ["x", "y", "z"].iter().map(|k| o.get(*k).ok_or(format!("axes: give x, y and z")).and_then(mate_ref)).collect::<R<_>>()?,
            None => {
                let s: Vec<u64> = orig.mates.iter().filter(|m| m.kind == MateKind::Slider && !m.suppressed).map(|m| m.id).collect();
                if s.len() != 3 {
                    return Err(format!(
                        "the assembly has {} slider mates ({}); say which drive X, Y and Z with axes: {{x, y, z}} (mate ids or names)",
                        s.len(),
                        orig.mates.iter().filter(|m| m.kind == MateKind::Slider).map(|m| format!("{} {}", m.id, m.name)).collect::<Vec<_>>().join(", ")
                    ));
                }
                s
            }
        };
        let mate = |id: u64| orig.mates.iter().find(|m| m.id == id).unwrap();
        let q_ref: Vec<f64> = mates.iter().map(|id| asm_anim::current_value(&orig, Target::Mate(*id)).ok_or(format!("mate {id} has no value (not placed?)"))).collect::<R<_>>()?;

        // ---- how each instance moves with each axis mate (finite differences through the solver)
        let pose_at = |s: &mut Session, q: &[f64]| -> (HashMap<u64, Pose>, Vec<u64>) {
            s.ws.assemblies[ai] = orig.clone();
            let drives: Vec<Drive> = mates.iter().zip(q).map(|(m, v)| Drive { mate: *m, angle: None, slide: Some(*v) }).collect();
            let failed = s.solve_driven(ai, &drives);
            (s.ws.assemblies[ai].instances.iter().map(|x| (x.id, x.pose)).collect(), failed)
        };
        let (base, failed) = pose_at(self, &q_ref);
        if !failed.is_empty() {
            notes.push(format!("mates {failed:?} don't solve at the current pose"));
        }
        let mut vel: HashMap<u64, [DVec3; 3]> = HashMap::new();
        let mut rotating: Vec<String> = vec![];
        for k in 0..3 {
            let lim = mate(mates[k]).limits;
            let d = if lim.map(|l| q_ref[k] + 1.0 > l[1]).unwrap_or(false) { -1.0 } else { 1.0 };
            let mut q = q_ref.clone();
            q[k] += d;
            let (p, _) = pose_at(self, &q);
            for x in &orig.instances {
                let (p0, p1) = (base[&x.id], p[&x.id]);
                let rot = p0.r.iter().zip(&p1.r).map(|(u, v)| (u - v).abs()).fold(0.0, f64::max);
                let e = vel.entry(x.id).or_insert([DVec3::ZERO; 3]);
                if rot > 1e-7 {
                    if !rotating.contains(&x.name) {
                        rotating.push(x.name.clone());
                    }
                    e[k] = DVec3::splat(f64::NAN);
                } else if !e[k].is_nan() {
                    e[k] = (DVec3::from(p1.t) - DVec3::from(p0.t)) / d;
                }
            }
        }
        // the assembly as it was, posed by the reference solve
        self.ws.assemblies[ai] = orig.clone();
        for x in &mut self.ws.assemblies[ai].instances {
            x.pose = base[&x.id];
        }
        let ref_asm = self.ws.assemblies[ai].clone();

        // ---- groups: instances that move alike (turning ones, the motors on belts, stay put)
        let mut group_of: HashMap<u64, usize> = HashMap::new();
        let mut gvel: Vec<[DVec3; 3]> = vec![[DVec3::ZERO; 3]];
        let mut ginst: Vec<Vec<u64>> = vec![vec![]];
        for x in &ref_asm.instances {
            let v = vel[&x.id];
            let v = if v.iter().any(|c| c.is_nan()) { [DVec3::ZERO; 3] } else { v };
            let v = v.map(|c| if c.length() < 1e-6 { DVec3::ZERO } else { c });
            let gi = match gvel.iter().position(|g| (0..3).all(|k| (g[k] - v[k]).length() < 1e-4)) {
                Some(i) => i,
                None => {
                    gvel.push(v);
                    ginst.push(vec![]);
                    gvel.len() - 1
                }
            };
            ginst[gi].push(x.id);
            group_of.insert(x.id, gi);
        }
        // ---- triangles of every visible part, at the reference pose
        let mut gtris: Vec<Vec<[DVec3; 3]>> = vec![vec![]; gvel.len()];
        let mut gbody: Vec<Vec<u32>> = vec![vec![]; gvel.len()];
        let mut bodies: Vec<Body> = vec![];
        let mut body_leaf: Vec<(u64, usize, crate::doc::BodyId, Pose)> = vec![];
        for x in ref_asm.instances.iter().filter(|x| !x.hidden) {
            let g = group_of[&x.id];
            for l in assembly::leaves(&self.ws.assemblies, x) {
                let Some(m) = self.mesh(l.studio, l.body) else { continue };
                let pose = x.pose.compose(&l.pose);
                let bi = bodies.len() as u32;
                let name = self.studio_doc(l.studio).and_then(|d| d.parts.get(&crate::doc::body_key(l.body))).and_then(|p| p.name.clone()).unwrap_or_else(|| m.name.clone());
                bodies.push(Body { name, instance: x.id, instance_name: x.name.clone(), group: g });
                body_leaf.push((x.id, l.studio, l.body, pose));
                let p = |i: u32| pose.apply(DVec3::new(m.positions[3 * i as usize] as f64, m.positions[3 * i as usize + 1] as f64, m.positions[3 * i as usize + 2] as f64));
                for t in m.indices.chunks(3) {
                    gtris[g].push([p(t[0]), p(t[1]), p(t[2])]);
                    gbody[g].push(bi);
                }
            }
        }
        let ntris: usize = gtris.iter().map(|t| t.len()).sum();
        let mut groups: Vec<Group> = vec![];
        for (g, (tris, body)) in gtris.into_iter().zip(gbody).enumerate() {
            let names: Vec<String> = ginst[g].iter().filter_map(|id| ref_asm.instance(*id)).map(|x| x.name.clone()).collect();
            let name = if g == 0 { "still parts".to_string() } else { names.join(" + ") };
            groups.push(Group::new(name, tris, body, gvel[g], ginst[g].clone()));
        }

        // ---- toolhead (moves with the most axes) and bed (moves with the others, or stands still)
        let cols = |g: usize| (0..3).filter(|k| groups[g].vel[*k].length() > 1e-6).count();
        let tool = (1..groups.len()).max_by_key(|g| (cols(*g), groups[*g].ntris())).ok_or("no part moves with the axis mates")?;
        if let Some(t) = a.get("toolhead").and_then(|v| v.as_u64()) {
            let _ = t;
        }
        let tool = match a.get("toolhead_instance").and_then(|v| v.as_u64()) {
            Some(id) => *group_of.get(&id).ok_or(format!("no instance {id}"))?,
            None => tool,
        };
        let tool_axes: Vec<usize> = (0..3).filter(|k| groups[tool].vel[*k].length() > 1e-6).collect();
        let area = |g: usize| {
            let b = groups[g].bounds;
            (b.hi.x - b.lo.x) * (b.hi.y - b.lo.y)
        };
        let bed = match a.get("bed_instance").and_then(|v| v.as_u64()) {
            Some(id) => *group_of.get(&id).ok_or(format!("no instance {id}"))?,
            None => (1..groups.len())
                .filter(|g| *g != tool && (0..3).all(|k| groups[*g].vel[k].length() < 1e-6 || !tool_axes.contains(&k)))
                .max_by(|x, y| area(*x).partial_cmp(&area(*y)).unwrap_or(std::cmp::Ordering::Equal))
                .unwrap_or(0),
        };

        // ---- the nozzle tip: lowest point of the toolhead, or given
        let (tip, tip_body) = match a.get("nozzle").and_then(v3) {
            Some(p) => {
                let mut best = (f64::INFINITY, 0u32);
                for (t, b) in groups[tool].tris() {
                    for q in t {
                        let d = (*q - p).length();
                        if d < best.0 {
                            best = (d, b);
                        }
                    }
                }
                (p, best.1)
            }
            None => {
                // the lowest point of a part called nozzle (or `nozzle_part`), else of the whole toolhead
                let want = a.get("nozzle_part").and_then(|v| v.as_str()).unwrap_or("nozzle").to_lowercase();
                let named: HashSet<u32> = bodies
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| b.group == tool && (b.name.to_lowercase().contains(&want) || b.instance_name.to_lowercase().contains(&want)))
                    .map(|(i, _)| i as u32)
                    .collect();
                if named.is_empty() {
                    notes.push(format!(
                        "no toolhead part is called \"{want}\", so the nozzle tip is the toolhead's lowest point (give nozzle_part or nozzle if that isn't the nozzle)"
                    ));
                }
                let mut best = (DVec3::splat(f64::INFINITY), 0u32);
                for (t, b) in groups[tool].tris() {
                    if !named.is_empty() && !named.contains(&b) {
                        continue;
                    }
                    for q in t {
                        if q.z < best.0.z {
                            best = (*q, b);
                        }
                    }
                }
                // a probe hanging lower than the hotend is worth saying: Z 0 then comes from the probe
                let tb = &bodies[best.1 as usize];
                let low = tb.name.to_lowercase();
                if named.is_empty() && ["beacon", "probe", "cartographer", "eddy"].iter().any(|k| low.contains(k)) {
                    let mut next = f64::INFINITY;
                    for (t, b) in groups[tool].tris() {
                        if bodies[b as usize].instance != tb.instance || bodies[b as usize].name != tb.name {
                            for q in t {
                                next = next.min(q.z);
                            }
                        }
                    }
                    notes.push(format!(
                        "the toolhead's lowest point is the probe \"{}\", {} mm below the next lowest part; in the model the probe sits below the nozzle, so Z 0 and the print checks use the probe as the tip",
                        tb.name,
                        r2(next - best.0.z)
                    ));
                }
                best
            }
        };

        // ---- Klipper axes onto mates: the tip's motion against the bed per unit of each mate
        let dirs: Vec<DVec3> = ["x", "y", "z"]
            .iter()
            .enumerate()
            .map(|(k, n)| a.get("directions").and_then(|d| d.get(*n)).and_then(v3).map(|v| v.normalize()).unwrap_or([DVec3::X, DVec3::Y, DVec3::Z][k]))
            .collect();
        let col: Vec<DVec3> = (0..3).map(|k| groups[tool].vel[k] - groups[bed].vel[k]).collect();
        let mut which = [usize::MAX; 3];
        for ax in 0..3 {
            if explicit.is_some() {
                which[ax] = ax;
                continue;
            }
            let (best, al) = (0..3)
                .map(|k| (k, if col[k].length() > 1e-9 { col[k].dot(dirs[ax]).abs() / col[k].length() } else { 0.0 }))
                .max_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
                .unwrap();
            if al < 0.9 {
                return Err(format!(
                    "no axis mate moves the nozzle along {} = {:?} relative to the bed; the mates move it along {}. Set directions (world direction of +X, +Y, +Z) or axes",
                    ["X", "Y", "Z"][ax],
                    dirs[ax].to_array(),
                    (0..3).map(|k| format!("{} {:?}", mate(mates[k]).name, r3(col[k].to_array()))).collect::<Vec<_>>().join("; ")
                ));
            }
            which[ax] = best;
        }
        if which[0] == which[1] || which[1] == which[2] || which[0] == which[2] {
            return Err("two Klipper axes map onto the same mate; give axes: {x, y, z} explicitly".into());
        }
        let mut axes: Vec<AxisMap> = vec![];
        let mut origin_notes = vec![];
        for ax in 0..3 {
            let k = which[ax];
            let m = mate(mates[k]);
            let along = col[k].dot(dirs[ax]);
            if along.abs() < 1e-9 {
                return Err(format!("mate {} doesn't move the nozzle along {}", m.name, ["X", "Y", "Z"][ax]));
            }
            let scale = 1.0 / along;
            let given = a.get("origin").and_then(|o| o.get(["x", "y", "z"][ax])).and_then(|v| v.as_f64());
            let offset = if let Some(o) = given {
                origin_notes.push(format!("{}: {} = {o} at {} 0 (given)", ["X", "Y", "Z"][ax], m.name, ["X", "Y", "Z"][ax]));
                o
            } else if ax < 2 {
                // Klipper's endstop position is where the axis stops against its end of travel
                let ka = &machine.axes[ax];
                let end = ka.endstop.unwrap_or(if ka.positive { ka.max } else { ka.min });
                let clear = a.get("endstop_clearance").and_then(|v| v.as_f64()).unwrap_or(0.0);
                match m.limits {
                    Some([lo, hi]) => {
                        let toward_min = !ka.positive;
                        let q_lim = if (scale > 0.0) == toward_min { lo } else { hi };
                        let inward = if toward_min { 1.0 } else { -1.0 };
                        let q_end = q_lim + scale * inward * clear;
                        origin_notes.push(format!(
                            "{}: homes to {} = {end} at {} {} ({}{})",
                            ["X", "Y", "Z"][ax],
                            ["X", "Y", "Z"][ax],
                            m.name,
                            r2(q_end),
                            if q_lim == lo { "its lower limit" } else { "its upper limit" },
                            if clear != 0.0 { format!(" less {clear} mm clearance") } else { String::new() }
                        ));
                        q_end - scale * end
                    }
                    None => {
                        let mid = (ka.min + ka.max) / 2.0;
                        origin_notes.push(format!("{}: {} has no limits, so the model's pose is taken as the middle of Klipper's range ({mid})", ["X", "Y", "Z"][ax], m.name));
                        q_ref[k] - scale * mid
                    }
                }
            } else {
                // Z 0: the nozzle tip touches the bed under it
                let bed_top = hit_below(&groups[bed], tip - DVec3::Z * 0.001);
                match bed_top {
                    Some(z) => {
                        let gap = (tip.z - z) / dirs[2].z.max(1e-9);
                        origin_notes.push(format!("Z: the nozzle is {} mm above the bed in the model; Z 0 = {} at {}", r2(gap), m.name, r2(q_ref[k] - scale * gap)));
                        q_ref[k] - scale * gap
                    }
                    None => {
                        return Err("nothing of the bed is under the nozzle in the model's pose, so Z 0 can't be found: move the toolhead over the bed or give origin: {z}".into())
                    }
                }
            };
            axes.push(AxisMap { mate: m.id, name: m.name.clone(), scale, offset, limits: m.limits });
        }
        // groups' velocities in Klipper's axis order
        for g in &mut groups {
            let v = g.vel;
            g.vel = [v[which[0]], v[which[1]], v[which[2]]];
        }
        let q_ref3 = [q_ref[which[0]], q_ref[which[1]], q_ref[which[2]]];
        let axes: [AxisMap; 3] = [axes[0].clone(), axes[1].clone(), axes[2].clone()];

        let mut model = Model { groups, bodies, axes, q_ref: q_ref3, tool, bed, tip, tip_body, skip: HashSet::new() };
        // ---- parts that ride on each other wherever the axes are (rails and carriages)
        let ride = a.get("riding_gap").and_then(|v| v.as_f64()).unwrap_or(0.2);
        let shifted = |k: usize, d: f64| -> f64 {
            let q = q_ref3[k];
            let (lo, hi) = model.axes[k].limits.map(|l| (l[0], l[1])).unwrap_or((q - 1e3, q + 1e3));
            if q + d <= hi {
                q + d
            } else {
                (q - d).max(lo)
            }
        };
        let q2 = [shifted(0, 17.0), shifted(1, 23.0), shifted(2, 11.0)];
        let t1 = model.touching(q_ref3, ride);
        let t2 = model.touching(q2, ride);
        model.skip = t1.intersection(&t2).copied().collect();
        let pose_hits: Vec<(u32, u32)> = t1.difference(&t2).copied().collect();

        if let Some(p) = a.get("dump_model").and_then(|v| v.as_str()) {
            model.save(p)?;
        }
        // ---- run the G-code
        let mut it = Interp::new(&machine);
        if let Some(h) = a.get("assume_homed").and_then(v3) {
            it.assume_homed(h.to_array());
        }
        it.run_file(&gcode);
        let total = plan::plan(&machine, &mut it.moves);
        let mut o = Options { nozzle: machine.nozzle_diameter, filament: machine.filament_diameter, ..Options::default() };
        if let Some(v) = a.get("clearance").and_then(|v| v.as_f64()) {
            o.crash = v.max(0.0);
        }
        if let Some(v) = a.get("near_miss").and_then(|v| v.as_f64()) {
            o.near = v.max(0.0);
        }
        if let Some(v) = a.get("check_print").and_then(|v| v.as_bool()) {
            o.print = v;
        }
        if let Some(v) = a.get("print_allowance").and_then(|v| v.as_f64()) {
            o.print_allow = v.max(0.0);
        }
        if let Some(v) = a.get("max_events").and_then(|v| v.as_u64()) {
            o.max_events = v.clamp(1, 100_000) as usize;
        }
        let n_moves = it.moves.len();
        let run = sim::run(&model, &it.moves, &it.srcs, &o, &mut |i, n| {
            if n > 20_000 && i % 20_000 == 0 {
                eprintln!("simulate_gcode: move {i} of {n}");
            }
        });

        // ---- pictures of the worst moments
        let mut renders = vec![];
        let max_r = a.get("renders").and_then(|v| v.as_u64()).unwrap_or(4) as usize;
        let dir = a.get("render_dir").and_then(|v| v.as_str()).unwrap_or("gcode-sim");
        let order = |k: &str| match k {
            "collision" => 0,
            "print_collision" => 1,
            "travel_limit" => 2,
            "near_miss" => 3,
            _ => 4,
        };
        let mut idx: Vec<usize> = (0..run.events.len()).filter(|i| run.events[*i].kind != "nozzle_on_bed").collect();
        idx.sort_by_key(|i| (order(run.events[*i].kind), *i));
        let hl_a = [0xe5, 0x33, 0x33];
        let hl_b = [0xff, 0xa0, 0x1a];
        let print_col = [0x3c, 0xb3, 0x71];
        for (n, ei) in idx.into_iter().take(max_r).enumerate() {
            let e = &run.events[ei];
            let mv = &it.moves[e.worst_move];
            let k = [mv.from[0] + (mv.to[0] - mv.from[0]) * e.worst_t, mv.from[1] + (mv.to[1] - mv.from[1]) * e.worst_t, mv.from[2] + (mv.to[2] - mv.from[2]) * e.worst_t];
            let q = model.q(k);
            let shifts: Vec<DVec3> = (0..model.groups.len()).map(|g| model.shift(g, q)).collect();
            let (ka, kb) = e.parts;
            let pm = print_mesh(&run.print, e.worst_move, shifts[model.bed]);
            let mut items: Vec<(&crate::kernel::BodyMesh, Pose, [u8; 3])> = vec![];
            let at = if e.kind == "travel_limit" { model.tip + shifts[model.tool] } else { DVec3::from(e.at) };
            let half = a.get("render_zoom").and_then(|v| v.as_f64()).unwrap_or(if e.kind == "print_collision" { 25.0 } else { 70.0 });
            let fit_box = Aabb { lo: at - DVec3::splat(half), hi: at + DVec3::splat(half) };
            let (mut centre_a, mut n_a) = (DVec3::ZERO, 0.0);
            for (bi, (inst, studio, body, pose)) in body_leaf.iter().enumerate() {
                let Some(m) = self.mesh(*studio, *body) else { continue };
                let g = model.bodies[bi].group;
                let mut p = *pose;
                p.t = (DVec3::from(p.t) + shifts[g]).into();
                // only what's near, and none of the big still panels that would hide it
                let involved = bi as u32 == ka || bi as u32 == kb;
                let mut bb = Aabb::EMPTY;
                for c in 0..8 {
                    bb.add(p.apply(DVec3::new(m.bbox[c & 1][0], m.bbox[(c >> 1) & 1][1], m.bbox[(c >> 2) & 1][2])));
                }
                let size = bb.hi - bb.lo;
                let big = size.max_element() > 3.0 * half && size.min_element() < 0.3 * half;
                if !involved && (!bb.swept(DVec3::ZERO, DVec3::ZERO, 0.0).overlaps(&fit_box) || (g == 0 && big)) {
                    continue;
                }
                if bi as u32 == ka {
                    centre_a += (bb.lo + bb.hi) / 2.0;
                    n_a += 1.0;
                }
                let col = if bi as u32 == ka {
                    hl_a
                } else if bi as u32 == kb {
                    hl_b
                } else {
                    self.studio_doc(*studio).and_then(|d| d.parts.get(&crate::doc::body_key(*body))).and_then(|p| p.color).unwrap_or([0x9a, 0xa9, 0xba])
                };
                let _ = inst;
                items.push((m, p, col));
            }
            if !pm.positions.is_empty() {
                items.push((&pm, Pose::default(), print_col));
            }
            let fit = Some((at - DVec3::splat(half), at + DVec3::splat(half)));
            // unless a view is asked for, look from the side of the first part, so the part it
            // meets doesn't hide the contact
            let view = a.get("view").and_then(|v| v.as_str());
            let eye = match view {
                None if n_a > 0.0 => {
                    // from the side (looking down on a contact under the toolhead hides it)
                    let d = centre_a / n_a - at;
                    let flat = DVec3::new(d.x, d.y, 0.0);
                    let flat = if flat.length() < 1e-3 { DVec3::new(1.0, -1.0, 0.0).normalize() } else { flat.normalize() };
                    // from above: the bed and the print are under most contacts
                    Some((flat + DVec3::new(0.0, 0.0, 0.6)).normalize().to_array())
                }
                _ => None,
            };
            let png = super::raster_scene_in(&items, &[], view.unwrap_or("iso"), eye, 900, 700, fit)?;
            let path = format!("{dir}/event-{:02}-{}-line{}.png", n + 1, e.kind, e.line);
            super::write_out(&path, &png)?;
            renders.push(json!({"event": ei, "path": path}));
        }

        // ---- the report
        let bname = |b: u32| -> String {
            let bd = &model.bodies[b as usize];
            if bd.name.is_empty() {
                bd.instance_name.clone()
            } else {
                format!("{} ({})", bd.name, bd.instance_name)
            }
        };
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for e in &run.events {
            *counts.entry(e.kind).or_default() += 1;
        }
        let issues_by_kind = it.issues.iter().fold(BTreeMap::<&str, usize>::new(), |mut m, i| {
            *m.entry(i.kind).or_default() += 1;
            m
        });
        let events: Vec<Value> = run
            .events
            .iter()
            .map(|e| {
                let mut v = json!({
                    "kind": e.kind,
                    "line": e.line,
                    "gcode": e.text,
                    "a": e.a,
                    "b": e.b,
                    "closest_mm": r2(e.dist),
                    "toolhead": r3(e.toolhead),
                    "time_s": r2(e.time_s),
                    "moves": e.moves,
                });
                if e.lines.len() > 1 {
                    v["lines"] = json!(e.lines);
                }
                if !e.via.is_empty() {
                    v["via"] = json!(e.via);
                }
                if e.last_line != e.line {
                    v["last_line"] = json!(e.last_line);
                }
                if e.kind == "travel_limit" {
                    v["past_limit_mm"] = json!(r2(e.dist));
                    v.as_object_mut().unwrap().remove("closest_mm");
                } else {
                    v["at"] = json!(r3(e.at));
                }
                v
            })
            .collect();
        let kinds = it.moves.iter().fold(BTreeMap::<String, (usize, f64)>::new(), |mut m, mv| {
            let e = m.entry(format!("{:?}", mv.kind).to_lowercase()).or_default();
            e.0 += 1;
            e.1 += mv.len();
            m
        });
        let ext = &run.extents;
        let fin = |v: [f64; 3]| if v[0].is_finite() { json!(r3(v)) } else { Value::Null };
        let mut out = json!({
            "summary": {
                "verdict": if counts.get("collision").copied().unwrap_or(0) + counts.get("print_collision").copied().unwrap_or(0) + counts.get("travel_limit").copied().unwrap_or(0) == 0 && !it.issues.iter().any(|i| i.kind == "out_of_range" || i.kind == "macro_error") { "clear" } else { "problems" },
                "events": counts,
                "klipper_issues": issues_by_kind,
                "moves": n_moves,
                "checked_moves": run.checked_moves,
                "print_time_s": r2(total),
                "print_time": format!("{}h {:02}m {:02}s", (total / 3600.0) as u64, ((total % 3600.0) / 60.0) as u64, (total % 60.0) as u64),
                "beads": run.print.beads.len(),
                "filament_volume_mm3": r2(run.print.volume),
                "aborted": it.aborted,
                "run_s": r2(t_start.elapsed().as_secs_f64()),
            },
            "events": events,
            "klipper_issues": it.issues.iter().take(200).map(|i| json!({"kind": i.kind, "line": i.line, "via": i.via, "gcode": i.text, "message": i.message})).collect::<Vec<_>>(),
            "renders": renders,
            "machine": {
                "kinematics": machine.kinematics,
                "config_files": cfg_files,
                "limits": machine.axes.iter().map(|x| [x.min, x.max]).collect::<Vec<_>>(),
                "probe": machine.probe.as_ref().map(|p| json!({"section": p.section, "offset": p.offset})),
            },
            "mapping": {
                "axes": model.axes.iter().enumerate().map(|(k, x)| {
                    let axis = ["X", "Y", "Z"][k];
                    let per_mm = (x.scale * 1000.0).round() / 1000.0;
                    let range = x.limits.map(|l| {
                        let (p, q) = ((l[0] - x.offset) / x.scale, (l[1] - x.offset) / x.scale);
                        [r2(p.min(q)), r2(p.max(q))]
                    });
                    json!({"klipper": axis, "mate": x.mate, "name": x.name, "mate_per_mm": per_mm, "mate_at_0": r2(x.offset), "limits": x.limits, "klipper_range_in_limits": range})
                }).collect::<Vec<_>>(),
                "origin": origin_notes,
                "nozzle_tip": r3(tip.to_array()),
                "nozzle_body": bname(tip_body),
                "toolhead_group": model.groups[tool].name,
                "bed_group": model.groups[bed].name,
                "groups": model.groups.iter().map(|g| json!({"name": g.name, "triangles": g.ntris(), "parts": g.parts.len(), "per_mm_of": g.vel.iter().map(|v| r3(v.to_array())).collect::<Vec<_>>()})).collect::<Vec<_>>(),
                "turning_parts_held_still": rotating,
                "riding_pairs_ignored": model.skip.iter().take(60).map(|(x, y)| format!("{} / {}", bname(*x), bname(*y))).collect::<Vec<_>>(),
                "riding_pairs_count": model.skip.len(),
                "touching_in_model_pose_only": pose_hits.iter().take(20).map(|(x, y)| format!("{} / {}", bname(*x), bname(*y))).collect::<Vec<_>>(),
                "triangles": ntris,
            },
            "extents": {"toolhead_min": fin(ext.toolhead_min), "toolhead_max": fin(ext.toolhead_max), "mate_min": fin(ext.mate_min), "mate_max": fin(ext.mate_max)},
            "commands": it.commands.iter().map(|(k, (n, h))| (k.clone(), json!(format!("{n}× {h}")))).collect::<serde_json::Map<_, _>>(),
            "move_kinds": kinds.iter().map(|(k, (n, d))| (k.clone(), json!({"moves": n, "mm": r2(*d)}))).collect::<serde_json::Map<_, _>>(),
            "notes": notes,
        });
        if run.dropped > 0 {
            out["summary"]["events_not_listed"] = json!(run.dropped);
        }
        if let Some(p) = a.get("report").and_then(|v| v.as_str()) {
            super::write_out(p, serde_json::to_string_pretty(&out).unwrap_or_default())?;
        }
        // leave the assembly where it was
        self.ws.assemblies[ai] = orig;
        let _ = Kind::Travel;
        Ok((out, None))
    }
}

pub fn tools() -> Vec<Value> {
    let n = |d: &str| json!({"type": "number", "description": d});
    let s = |d: &str| json!({"type": "string", "description": d});
    let p3 = |d: &str| json!({"type": "array", "items": {"type": "number"}, "description": d});
    vec![json!({
        "name": "simulate_gcode",
        "description": "Run a 3D printer's G-code on the printer's own assembly, so changes to the machine or the slicing can be checked before the real printer runs them. The G-code goes through the Klipper config (printer.cfg and its includes): its macros (Jinja: set/if/for, params, printer.* state, SET_GCODE_VARIABLE, SAVE/RESTORE_GCODE_STATE), homing (endstops, safe_z_home, Beacon/Cartographer/probe Z homing, homing_override), Z_TILT_ADJUST/QUAD_GANTRY_LEVEL, BED_MESH_CALIBRATE with the probe offset, Trad Rack toolchanges (pre_unload_gcode/post_load_gcode), G2/G3 arcs, offsets, Klipper's soft limits, and the planner's timing. The assembly's three slider mates are the axes: each Klipper axis is matched to the mate that moves the nozzle that way relative to the bed (directions), X and Y home against the end of their mate's travel (origin overrides it), Z 0 is where the nozzle tip touches the bed. Every move is swept exactly (no sampling) and reports: collision (moving parts touching, e.g. the toolhead into the frame or gantry into a panel), print_collision (the toolhead or gantry running into what's been printed: the bead model is built from the extrusion moves; ordinary layers sit under the nozzle, non-planar or badly ordered moves don't), travel_limit (a mate driven past its limits), near_miss (closer than near_miss mm), nozzle_on_bed (contact probing), plus Klipper issues (Move out of range, moves before homing, macro errors, unknown commands). Parts that touch everywhere along their travel (rails and their carriages) are ignored. Renders PNGs of the worst events with the parts involved in red/orange and the print in green.",
        "inputSchema": {"type": "object", "properties": {
            "assembly": {"type": ["integer", "string"], "description": "the printer assembly (index or name; default the one named Printer)"},
            "gcode": s("G-code file to run"),
            "gcode_text": s("or the G-code itself"),
            "config": s("Klipper printer.cfg (its [include]s are read too)"),
            "config_text": s("or the config itself"),
            "axes": {"type": "object", "description": "which mates drive Klipper X, Y and Z (ids or names), when the assembly doesn't have exactly three sliders", "properties": {"x": {"type": ["integer", "string"], "description": "mate"}, "y": {"type": ["integer", "string"], "description": "mate"}, "z": {"type": ["integer", "string"], "description": "mate"}}, "additionalProperties": false},
            "directions": {"type": "object", "description": "world direction the nozzle moves relative to the bed for +X, +Y and +Z (default the world axes)", "properties": {"x": p3("[x, y, z]"), "y": p3("[x, y, z]"), "z": p3("[x, y, z]")}, "additionalProperties": false},
            "origin": {"type": "object", "description": "mate value (mm) at Klipper 0 for an axis, instead of finding it (X/Y: homing against the mate's limit; Z: the nozzle touching the bed)", "properties": {"x": n("mm"), "y": n("mm"), "z": n("mm")}, "additionalProperties": false},
            "endstop_clearance": n("X/Y home this far short of the end of their travel (mm, default 0)"),
            "nozzle": p3("the nozzle tip (world mm, the assembly as it stands); default the lowest point of the nozzle part"),
            "nozzle_part": s("name (or part of one) of the toolhead part whose lowest point is the nozzle tip (default \"nozzle\"; with no such part, the toolhead's lowest point)"),
            "toolhead_instance": {"type": "integer", "description": "an instance that moves with the toolhead (default: whatever moves with the most axes)"},
            "bed_instance": {"type": "integer", "description": "an instance that moves with the bed (default: the largest that moves with the other axes, or the frame)"},
            "assume_homed": p3("start homed at this toolhead position (for files that don't home)"),
            "clearance": n("closer than this is a collision, mm (default 0.05: mesh tolerance)"),
            "near_miss": n("also report parts coming closer than this, mm (default 0: off)"),
            "riding_gap": n("parts closer than this both where the model stands and with every axis moved are taken to ride on each other and ignored, mm (default 0.2)"),
            "check_print": {"type": "boolean", "description": "check against the printed part (default true)"},
            "print_allowance": n("bead tops sit this far below the nozzle that laid them, so the nozzle riding on its own layer isn't a hit, mm (default 0.1)"),
            "max_events": {"type": "integer", "description": "list at most this many events (default 200)"},
            "renders": {"type": "integer", "description": "PNGs of the first events, worst kinds first (default 4, 0 for none)"},
            "render_dir": s("folder for them (default gcode-sim)"),
            "render_zoom": n("half the size of the box each picture frames around the contact, mm (default 70, or 25 for print collisions)"),
            "view": s("render view (default iso; as in render_assembly)"),
            "report": s("also write the whole result as JSON to this file"),
            "dump_model": s("also save the machine's moving groups (triangles and how they move) to this file, for debugging"),
        }, "additionalProperties": false},
    })]
}
