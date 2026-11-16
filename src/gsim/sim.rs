//! The machine running the G-code: the assembly's moving groups follow the axis mates, which
//! follow the toolhead position of each move, and every move is checked for parts of the
//! machine running into each other, into the part being printed, and for axes running past
//! their travel.

use std::collections::{HashMap, HashSet};

use glam::DVec3;
use serde::{Deserialize, Serialize};

use super::collide::{sweep, Aabb, Bvh, Near};
use super::interp::{Kind, Move, Src};
use super::print::{Print, PrintNear};

/// A set of parts that move together: world translation per unit of each axis mate.
pub struct Group {
    pub name: String,
    /// One BVH per body, so pairs of bodies that ride on each other are skipped whole.
    pub parts: Vec<Bvh>,
    pub bounds: Aabb,
    pub vel: [DVec3; 3],
    #[allow(dead_code)]
    pub instances: Vec<u64>,
}

impl Group {
    pub fn new(name: String, tris: Vec<[DVec3; 3]>, body: Vec<u32>, vel: [DVec3; 3], instances: Vec<u64>) -> Group {
        let mut by: std::collections::BTreeMap<u32, Vec<[DVec3; 3]>> = Default::default();
        for (t, b) in tris.into_iter().zip(body) {
            by.entry(b).or_default().push(t);
        }
        let parts: Vec<Bvh> = by
            .into_iter()
            .map(|(b, t)| {
                let n = t.len();
                Bvh::new(t, vec![b; n])
            })
            .collect();
        let mut bounds = Aabb::EMPTY;
        for p in &parts {
            let b = p.bounds();
            bounds.add(b.lo);
            bounds.add(b.hi);
        }
        Group { name, parts, bounds, vel, instances }
    }
    pub fn tris(&self) -> impl Iterator<Item = (&[DVec3; 3], u32)> {
        self.parts.iter().flat_map(|p| p.tris.iter().zip(p.body.iter().copied()))
    }
    pub fn ntris(&self) -> usize {
        self.parts.iter().map(|p| p.tris.len()).sum()
    }
}

/// Group a moving from r0 to r1 relative to group b: body pairs within reach.
fn pair_sweep(a: &Group, b: &Group, r0: DVec3, r1: DVec3, reach: f64, skip: &dyn Fn(u32, u32) -> bool, out: &mut Vec<Near>) {
    if !a.bounds.swept(r0, r1, reach).overlaps(&b.bounds) {
        return;
    }
    for pa in &a.parts {
        let sa = pa.bounds().swept(r0, r1, reach);
        if !sa.overlaps(&b.bounds) {
            continue;
        }
        let ba = pa.body[0];
        for pb in &b.parts {
            if !sa.overlaps(&pb.bounds()) || skip(ba, pb.body[0]) {
                continue;
            }
            sweep(pa, pb, r0, r1, reach, &|_, _| false, out);
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Body {
    pub name: String,
    pub instance: u64,
    pub instance_name: String,
    pub group: usize,
}

/// One Klipper axis driven by one mate: mate value = offset + scale × toolhead position.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AxisMap {
    pub mate: u64,
    pub name: String,
    pub scale: f64,
    pub offset: f64,
    pub limits: Option<[f64; 2]>,
}

impl AxisMap {
    pub fn q(&self, k: f64) -> f64 {
        self.offset + self.scale * k
    }
}

pub struct Model {
    pub groups: Vec<Group>,
    pub bodies: Vec<Body>,
    /// Klipper X, Y, Z.
    pub axes: [AxisMap; 3],
    /// Mate values at the reference pose (the triangles' pose).
    pub q_ref: [f64; 3],
    pub tool: usize,
    pub bed: usize,
    /// Nozzle tip at the reference pose (world).
    pub tip: DVec3,
    pub tip_body: u32,
    /// Body pairs that touch wherever the axes are (rails and their carriages): not collisions.
    pub skip: HashSet<(u32, u32)>,
}

impl Model {
    /// Write the model (its triangles and how they move) to a file, to run G-code on it later
    /// without the CAD: a JSON header, then each group's triangles (f32) and body ids.
    pub fn save(&self, path: &str) -> Result<(), String> {
        let head = serde_json::json!({
            "bodies": self.bodies, "axes": self.axes, "q_ref": self.q_ref, "tool": self.tool, "bed": self.bed,
            "tip": self.tip.to_array(), "tip_body": self.tip_body, "skip": self.skip.iter().collect::<Vec<_>>(),
            "groups": self.groups.iter().map(|g| serde_json::json!({"name": g.name, "vel": g.vel.map(|v| v.to_array()), "instances": g.instances, "parts": g.parts.iter().map(|p| p.tris.len()).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        })
        .to_string();
        let mut out: Vec<u8> = vec![];
        out.extend((head.len() as u64).to_le_bytes());
        out.extend(head.as_bytes());
        for g in &self.groups {
            for (t, _) in g.tris() {
                for p in t {
                    for c in p.to_array() {
                        out.extend((c as f32).to_le_bytes());
                    }
                }
            }
            for (_, b) in g.tris() {
                out.extend(b.to_le_bytes());
            }
        }
        std::fs::write(path, out).map_err(|e| format!("{path}: {e}"))
    }

    #[allow(dead_code)] // for replaying a dump_model file outside the app
    pub fn load(path: &str) -> Result<Model, String> {
        let d = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let n = u64::from_le_bytes(d[0..8].try_into().unwrap()) as usize;
        let h: serde_json::Value = serde_json::from_slice(&d[8..8 + n]).map_err(|e| e.to_string())?;
        let mut at = 8 + n;
        let f = |at: &mut usize| {
            let v = f32::from_le_bytes(d[*at..*at + 4].try_into().unwrap());
            *at += 4;
            v as f64
        };
        let mut groups = vec![];
        for g in h["groups"].as_array().ok_or("bad header")? {
            let nt: usize = g["parts"].as_array().map(|a| a.iter().filter_map(|x| x.as_u64()).sum::<u64>() as usize).or(g["tris"].as_u64().map(|n| n as usize)).unwrap_or(0);
            let mut tris = Vec::with_capacity(nt);
            for _ in 0..nt {
                let mut t = [DVec3::ZERO; 3];
                for p in &mut t {
                    *p = DVec3::new(f(&mut at), f(&mut at), f(&mut at));
                }
                tris.push(t);
            }
            let mut body = Vec::with_capacity(nt);
            for _ in 0..nt {
                body.push(u32::from_le_bytes(d[at..at + 4].try_into().unwrap()));
                at += 4;
            }
            let vel: [[f64; 3]; 3] = serde_json::from_value(g["vel"].clone()).map_err(|e| e.to_string())?;
            groups.push(Group::new(g["name"].as_str().unwrap_or("").into(), tris, body, vel.map(DVec3::from), serde_json::from_value(g["instances"].clone()).unwrap_or_default()));
        }
        let skip: Vec<(u32, u32)> = serde_json::from_value(h["skip"].clone()).map_err(|e| e.to_string())?;
        Ok(Model {
            groups,
            bodies: serde_json::from_value(h["bodies"].clone()).map_err(|e| e.to_string())?,
            axes: serde_json::from_value(h["axes"].clone()).map_err(|e| e.to_string())?,
            q_ref: serde_json::from_value(h["q_ref"].clone()).map_err(|e| e.to_string())?,
            tool: h["tool"].as_u64().unwrap_or(0) as usize,
            bed: h["bed"].as_u64().unwrap_or(0) as usize,
            tip: DVec3::from(serde_json::from_value::<[f64; 3]>(h["tip"].clone()).map_err(|e| e.to_string())?),
            tip_body: h["tip_body"].as_u64().unwrap_or(0) as u32,
            skip: skip.into_iter().collect(),
        })
    }
    pub fn q(&self, k: [f64; 3]) -> [f64; 3] {
        [self.axes[0].q(k[0]), self.axes[1].q(k[1]), self.axes[2].q(k[2])]
    }
    /// How far group g has moved from the reference pose at mate values q.
    pub fn shift(&self, g: usize, q: [f64; 3]) -> DVec3 {
        let v = &self.groups[g].vel;
        v[0] * (q[0] - self.q_ref[0]) + v[1] * (q[1] - self.q_ref[1]) + v[2] * (q[2] - self.q_ref[2])
    }
    /// The nozzle tip in the bed's reference frame at toolhead position k.
    pub fn tip_in_bed(&self, k: [f64; 3]) -> DVec3 {
        let q = self.q(k);
        self.tip + self.shift(self.tool, q) - self.shift(self.bed, q)
    }
    fn skip(&self, a: u32, b: u32) -> bool {
        self.skip.contains(&(a.min(b), a.max(b)))
    }

    /// Body pairs (of groups that move relative to each other) closer than `gap` at mate values q.
    pub fn touching(&self, q: [f64; 3], gap: f64) -> HashSet<(u32, u32)> {
        let mut out = HashSet::new();
        for i in 0..self.groups.len() {
            for j in i + 1..self.groups.len() {
                if !self.relative(i, j) {
                    continue;
                }
                let r = self.shift(i, q) - self.shift(j, q);
                let mut near = vec![];
                pair_sweep(&self.groups[i], &self.groups[j], r, r, gap, &|_, _| false, &mut near);
                for n in near {
                    out.insert((n.body_a.min(n.body_b), n.body_a.max(n.body_b)));
                }
            }
        }
        out
    }

    fn relative(&self, i: usize, j: usize) -> bool {
        (0..3).any(|k| (self.groups[i].vel[k] - self.groups[j].vel[k]).length() > 1e-9)
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Closer than this (mm) is a collision.
    pub crash: f64,
    /// Closer than this (mm) is reported as a near miss (0: none).
    pub near: f64,
    /// Check the toolhead and gantry against the beads already printed.
    pub print: bool,
    /// The bead's top sits this far under the nozzle (mm), so the nozzle riding on what it just laid isn't a hit.
    pub print_allow: f64,
    pub nozzle: f64,
    pub filament: f64,
    pub max_events: usize,
    /// Split moves so no group sweeps further than this in one check (mm): keeps the swept boxes tight.
    pub step: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options { crash: 0.05, near: 0.0, print: true, print_allow: 0.1, nozzle: 0.4, filament: 1.75, max_events: 200, step: 25.0 }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    /// collision, near_miss, print_collision, nozzle_on_bed, travel_limit
    pub kind: &'static str,
    pub first_move: usize,
    pub last_move: usize,
    pub moves: usize,
    pub line: usize,
    pub last_line: usize,
    /// The first few lines where it happens.
    pub lines: Vec<usize>,
    pub via: String,
    pub text: String,
    /// Closest approach (mm, 0 = touching or overlapping), or how far past the limit for travel.
    pub dist: f64,
    pub a: String,
    pub b: String,
    /// Where (world mm at the reference pose of the still side) and the toolhead then.
    pub at: [f64; 3],
    pub toolhead: [f64; 3],
    pub time_s: f64,
    #[serde(skip)]
    /// what the event merges on: a body of each instance (travel limits: the axis)
    pub key: (u32, u32),
    /// the two bodies at the closest point
    pub parts: (u32, u32),
    /// The worst move's index and how far along it (for posing a picture).
    pub worst_move: usize,
    pub worst_t: f64,
}

#[derive(Default, Serialize)]
pub struct Extents {
    pub toolhead_min: [f64; 3],
    pub toolhead_max: [f64; 3],
    pub mate_min: [f64; 3],
    pub mate_max: [f64; 3],
}

pub struct Run {
    pub events: Vec<Event>,
    pub dropped: usize,
    pub extents: Extents,
    pub print: Print,
    pub checked_moves: usize,
}

fn lerp3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

struct Log<'a> {
    events: Vec<Event>,
    dropped: usize,
    max: usize,
    srcs: &'a [Src],
    /// key → index of its open event
    open: HashMap<(&'static str, u32, u32), usize>,
}

impl Log<'_> {
    #[allow(clippy::too_many_arguments)]
    fn add(&mut self, kind: &'static str, key: (u32, u32), parts: (u32, u32), mi: usize, mv: &Move, t: f64, dist: f64, a: String, b: String, at: DVec3) {
        let k = (kind, key.0, key.1);
        if let Some(&ei) = self.open.get(&k) {
            // every time the same two parts meet is one event: how often, where first, where worst
            let e = &mut self.events[ei];
            if mi > e.last_move {
                e.moves += 1;
                e.last_move = mi;
                let line = self.srcs.get(mv.src as usize).map(|s| s.line).unwrap_or(0);
                e.last_line = line;
                if e.lines.len() < 12 && !e.lines.contains(&line) {
                    e.lines.push(line);
                }
            }
            if dist < e.dist - 1e-9 {
                e.dist = dist;
                e.at = at.to_array();
                e.worst_move = mi;
                e.worst_t = t;
                e.toolhead = lerp3(mv.from, mv.to, t);
                e.a = a;
                e.b = b;
                e.parts = parts;
            }
            return;
        }
        if self.events.len() >= self.max {
            self.dropped += 1;
            return;
        }
        let s = self.srcs.get(mv.src as usize);
        self.open.insert(k, self.events.len());
        self.events.push(Event {
            kind,
            first_move: mi,
            last_move: mi,
            moves: 1,
            line: s.map(|s| s.line).unwrap_or(0),
            last_line: s.map(|s| s.line).unwrap_or(0),
            lines: s.map(|s| vec![s.line]).unwrap_or_default(),
            via: s.map(|s| s.via.to_string()).unwrap_or_default(),
            text: s.map(|s| s.text.to_string()).unwrap_or_default(),
            dist,
            a,
            b,
            at: at.to_array(),
            toolhead: lerp3(mv.from, mv.to, t),
            time_s: mv.t0 + (mv.t1 - mv.t0) * t,
            key,
            parts,
            worst_move: mi,
            worst_t: t,
        });
    }
}

/// Bead size from the slicer's comments, or from the filament pushed.
fn bead_size(mv: &Move, o: &Options) -> (f64, f64) {
    let len = mv.len().max(1e-6);
    let area = mv.e * std::f64::consts::PI * o.filament * o.filament / 4.0 / len;
    let w = if mv.width > 0.0 {
        mv.width as f64
    } else if mv.height > 0.0 {
        area / mv.height as f64
    } else {
        o.nozzle * 1.125
    };
    let h = if mv.height > 0.0 { mv.height as f64 } else { area / w.max(1e-6) };
    (w.clamp(0.05, 5.0 * o.nozzle.max(0.1)), h.clamp(0.02, 2.0 * o.nozzle.max(0.1)))
}

pub fn run(m: &Model, moves: &[Move], srcs: &[Src], o: &Options, progress: &mut dyn FnMut(usize, usize)) -> Run {
    let mut log = Log { events: vec![], dropped: 0, max: o.max_events, srcs, open: HashMap::new() };
    let mut print = Print::new(4.0);
    let mut ext = Extents { toolhead_min: [f64::INFINITY; 3], toolhead_max: [f64::NEG_INFINITY; 3], mate_min: [f64::INFINITY; 3], mate_max: [f64::NEG_INFINITY; 3] };
    let reach = o.crash.max(o.near);
    let print_reach = o.crash.min(o.print_allow * 0.5);
    let ng = m.groups.len();
    let pairs: Vec<(usize, usize)> = (0..ng).flat_map(|i| (i + 1..ng).map(move |j| (i, j))).filter(|(i, j)| m.relative(*i, *j)).collect();
    let name = |b: u32| -> String {
        let bd = &m.bodies[b as usize];
        if bd.name.is_empty() {
            bd.instance_name.clone()
        } else {
            format!("{} ({})", bd.name, bd.instance_name)
        }
    };
    // events merge per pair of instances: one rail meeting one carriage is one finding, whichever faces
    let mut rep: HashMap<u64, u32> = HashMap::new();
    let rep: Vec<u32> = m.bodies.iter().enumerate().map(|(i, b)| *rep.entry(b.instance).or_insert(i as u32)).collect();
    let up = DVec3::Z;
    let mut checked = 0;
    for (mi, mv) in moves.iter().enumerate() {
        if mi % 2000 == 0 {
            progress(mi, moves.len());
        }
        if mv.kind == Kind::Dwell || mv.kind == Kind::Extrude {
            continue;
        }
        let (q0, q1) = (m.q(mv.from), m.q(mv.to));
        for k in 0..3 {
            for (p, q) in [(mv.from, q0), (mv.to, q1)] {
                ext.toolhead_min[k] = ext.toolhead_min[k].min(p[k]);
                ext.toolhead_max[k] = ext.toolhead_max[k].max(p[k]);
                ext.mate_min[k] = ext.mate_min[k].min(q[k]);
                ext.mate_max[k] = ext.mate_max[k].max(q[k]);
            }
            if let Some([lo, hi]) = m.axes[k].limits {
                let over = (lo - q0[k].min(q1[k])).max(q0[k].max(q1[k]) - hi);
                if over > 1e-3 {
                    let at_end = if q1[k] > hi || q1[k] < lo { 1.0 } else { 0.0 };
                    let msg = format!("{} at {:.2}, limits {lo} to {hi}", m.axes[k].name, if over == q0[k].max(q1[k]) - hi { q0[k].max(q1[k]) } else { q0[k].min(q1[k]) });
                    log.add("travel_limit", (k as u32, 0), (u32::MAX, u32::MAX), mi, mv, at_end, over, ["X", "Y", "Z"][k].to_string(), msg, DVec3::ZERO);
                }
            }
        }
        checked += 1;
        // split long moves so the swept boxes stay tight
        let mut longest: f64 = 0.0;
        for g in 0..ng {
            longest = longest.max((m.shift(g, q1) - m.shift(g, q0)).length());
        }
        let pieces = ((longest / o.step.max(1.0)).ceil() as usize).max(1);
        for p in 0..pieces {
            let (ta, tb) = (p as f64 / pieces as f64, (p + 1) as f64 / pieces as f64);
            let qa = lerp3(q0, q1, ta);
            let qb = lerp3(q0, q1, tb);
            for &(i, j) in &pairs {
                let (r0, r1) = (m.shift(i, qa) - m.shift(j, qa), m.shift(i, qb) - m.shift(j, qb));
                let mut near: Vec<Near> = vec![];
                pair_sweep(&m.groups[i], &m.groups[j], r0, r1, reach, &|a, b| m.skip(a, b), &mut near);
                for n in near {
                    let t = ta + (tb - ta) * n.t;
                    let th = lerp3(mv.from, mv.to, n.t * (tb - ta) + ta);
                    let tip_bed = (n.body_a == m.tip_body && j == m.bed) || (n.body_b == m.tip_body && i == m.bed);
                    let kind = if n.dist > o.crash {
                        "near_miss"
                    } else if tip_bed && th[2] > -0.05 {
                        "nozzle_on_bed"
                    } else {
                        "collision"
                    };
                    let (ra, rb) = (rep[n.body_a as usize], rep[n.body_b as usize]);
                    let key = (ra.min(rb), ra.max(rb));
                    // `at` is on group j, which sat still in the sweep: put it back where j is then
                    let at = n.at + m.shift(j, lerp3(q0, q1, t));
                    log.add(kind, key, (n.body_a, n.body_b), mi, mv, t, n.dist, name(n.body_a), name(n.body_b), at);
                }
            }
            if o.print && !print.is_empty() {
                for g in 0..ng {
                    if g == m.bed || !m.relative(g, m.bed) {
                        continue;
                    }
                    let (r0, r1) = (m.shift(g, qa) - m.shift(m.bed, qa), m.shift(g, qb) - m.shift(m.bed, qb));
                    let mut near: Vec<PrintNear> = vec![];
                    for part in &m.groups[g].parts {
                        print.sweep(part, r0, r1, print_reach, &mut near);
                    }
                    for n in near {
                        let laid = &moves[print.beads[n.bead as usize].mv as usize];
                        let line = srcs.get(laid.src as usize).map(|s| s.line).unwrap_or(0);
                        let at = n.at + m.shift(m.bed, lerp3(q0, q1, (ta + tb) / 2.0));
                        log.add(
                            "print_collision",
                            (rep[n.body as usize], u32::MAX),
                            (n.body, u32::MAX),
                            mi,
                            mv,
                            (ta + tb) / 2.0,
                            n.dist,
                            name(n.body),
                            format!("the print (bead from line {line})"),
                            at,
                        );
                    }
                }
            }
        }
        if mv.kind == Kind::Print && mv.e > 0.0 {
            let (w, h) = bead_size(mv, o);
            print.add(m.tip_in_bed(mv.from), m.tip_in_bed(mv.to), w, h, o.print_allow, up, mi as u32);
        }
    }
    progress(moves.len(), moves.len());
    Run { events: log.events, dropped: log.dropped, extents: ext, print, checked_moves: checked }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gsim::cfg::{Config, Machine};
    use crate::gsim::interp::Interp;

    fn boxy(lo: DVec3, hi: DVec3) -> Vec<[DVec3; 3]> {
        let p = |i: usize| DVec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
        let f = [[0, 1, 3, 2], [4, 6, 7, 5], [0, 4, 5, 1], [2, 3, 7, 6], [0, 2, 6, 4], [1, 5, 7, 3]];
        f.iter().flat_map(|q| [[p(q[0]), p(q[1]), p(q[2])], [p(q[0]), p(q[2]), p(q[3])]]).collect()
    }

    /// A cartesian printer made of boxes: a post on the frame, a toolhead whose nozzle
    /// tip is at the origin (Z 0 on the bed top), a bed moving in Z.
    fn model() -> Model {
        let mut tool = boxy(DVec3::new(-20.0, -20.0, 5.0), DVec3::new(20.0, 20.0, 45.0));
        tool.extend(boxy(DVec3::new(-1.0, -1.0, 0.0), DVec3::new(1.0, 1.0, 5.0)));
        let post = boxy(DVec3::new(150.0, 150.0, -50.0), DVec3::new(170.0, 170.0, 200.0));
        let bed = boxy(DVec3::new(-100.0, -100.0, -6.0), DVec3::new(140.0, 140.0, 0.0));
        let groups = vec![
            Group::new("frame".into(), post, vec![0; 12], [DVec3::ZERO; 3], vec![]),
            Group::new("toolhead".into(), tool, vec![1; 24], [DVec3::X, DVec3::Y, DVec3::ZERO], vec![]),
            Group::new("bed".into(), bed, vec![2; 12], [DVec3::ZERO, DVec3::ZERO, DVec3::NEG_Z], vec![]),
        ];
        let bodies =
            ["post", "toolhead", "bed"].iter().enumerate().map(|(g, n)| Body { name: n.to_string(), instance: g as u64, instance_name: n.to_string(), group: g }).collect();
        let ax = |n: &str, lim: [f64; 2]| AxisMap { mate: 0, name: n.into(), scale: 1.0, offset: 0.0, limits: Some(lim) };
        Model {
            groups,
            bodies,
            axes: [ax("x", [-100.0, 200.0]), ax("y", [-100.0, 200.0]), ax("z", [0.0, 150.0])],
            q_ref: [0.0; 3],
            tool: 1,
            bed: 2,
            tip: DVec3::ZERO,
            tip_body: 1,
            skip: HashSet::new(),
        }
    }

    fn sim(g: &str) -> Run {
        let m = Machine::from_config(
            &Config::parse(
                "[stepper_x]\nposition_min: -500\nposition_max: 500\n[stepper_y]\nposition_min: -500\nposition_max: 500\n[stepper_z]\nposition_min: -10\nposition_max: 500\n",
                None,
            )
            .unwrap(),
        );
        let mut it = Interp::new(&m);
        it.assume_homed([0.0, 0.0, 10.0]);
        it.run_file(g);
        super::super::plan::plan(&m, &mut it.moves);
        run(&model(), &it.moves, &it.srcs, &Options::default(), &mut |_, _| {})
    }

    #[test]
    fn crash_into_the_post_and_past_the_limits() {
        let r = sim("G1 X100 Y100 F6000\nG1 X200 Y160\nG1 X0 Y0\nG1 X250\n");
        let kinds: Vec<(&str, usize)> = r.events.iter().map(|e| (e.kind, e.line)).collect();
        assert!(kinds.contains(&("collision", 2)), "{kinds:?}");
        assert!(kinds.contains(&("travel_limit", 4)), "{kinds:?}");
        assert!(!kinds.iter().any(|k| k.1 == 1 || k.1 == 3), "{kinds:?}");
    }

    #[test]
    fn non_planar_hits_the_print() {
        // ten planar layers of a square: fine; then a travel through the wall at layer 3's height
        let mut g = String::from("M83\n");
        for l in 0..10 {
            let z = 0.2 * (l + 1) as f64;
            g += &format!("G1 Z{z:.2} F600\nG1 X-30 Y-30 F6000\n;WIDTH:0.45\n;HEIGHT:0.2\nG1 X30 Y-30 E1\nG1 X30 Y30 E1\nG1 X-30 Y30 E1\nG1 X-30 Y-30 E1\n");
        }
        let clean = sim(&g);
        assert!(clean.events.is_empty(), "{:?}", clean.events);
        assert_eq!(clean.print.beads.len(), 40);
        g += "G1 X0 Y0 Z0.6 F6000\nG1 X0 Y-60\n";
        let r = sim(&g);
        let hit: Vec<_> = r.events.iter().filter(|e| e.kind == "print_collision").collect();
        assert_eq!(hit.len(), 1, "{:?}", r.events);
        assert!(hit[0].b.contains("print"));
    }
}
