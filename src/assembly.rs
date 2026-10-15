//! Assemblies: instances of Part Studio parts placed by mates.
//!
//! Each instance has a rigid pose. Mates relate two mate connectors (a point
//! with Z and X axes, stored in the instance's own coordinates); the mate
//! solver moves free instances so every mate's conditions hold, moving the
//! instance being dragged as little as possible.

use glam::{DMat3, DVec3};
use serde::{Deserialize, Serialize};

use crate::doc::BodyId;
use crate::kernel::BodyMesh;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Pose {
    /// Rotation, column-major.
    pub r: [f64; 9],
    pub t: [f64; 3],
}

impl Default for Pose {
    fn default() -> Self {
        Pose { r: DMat3::IDENTITY.to_cols_array(), t: [0.0; 3] }
    }
}

impl Pose {
    pub fn m(&self) -> DMat3 {
        DMat3::from_cols_array(&self.r)
    }
    pub fn apply(&self, p: DVec3) -> DVec3 {
        self.m() * p + DVec3::from(self.t)
    }
    pub fn dir(&self, d: DVec3) -> DVec3 {
        self.m() * d
    }
    pub fn inverse_apply(&self, p: DVec3) -> DVec3 {
        self.m().transpose() * (p - DVec3::from(self.t))
    }
    pub fn inverse_dir(&self, d: DVec3) -> DVec3 {
        self.m().transpose() * d
    }
    /// `self` after `inner`: maps `inner`'s coordinates through both poses.
    pub fn compose(&self, inner: &Pose) -> Pose {
        Pose { r: (self.m() * inner.m()).to_cols_array(), t: self.apply(DVec3::from(inner.t)).into() }
    }
    /// Pose after a small rotation `w` (axis-angle, about the world origin of
    /// the part's centre `c`) and translation `v`.
    pub fn perturbed(&self, w: DVec3, v: DVec3, c: DVec3) -> Pose {
        let rot = if w.length() < 1e-15 { DMat3::IDENTITY } else { DMat3::from_axis_angle(w.normalize(), w.length()) };
        let m = rot * self.m();
        // rotate about the world-space centre c
        let t = rot * (DVec3::from(self.t) - c) + c + v;
        Pose { r: orthonormalize(m).to_cols_array(), t: t.into() }
    }
}

fn orthonormalize(m: DMat3) -> DMat3 {
    let x = m.x_axis.normalize();
    let y = (m.y_axis - x * x.dot(m.y_axis)).normalize();
    let z = x.cross(y);
    DMat3::from_cols(x, y, z)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Instance {
    pub id: u64,
    pub name: String,
    /// Part Studio index and body in it.
    pub studio: usize,
    pub body: BodyId,
    pub pose: Pose,
    #[serde(default)]
    pub fixed: bool,
    #[serde(default)]
    pub hidden: bool,
    /// A whole assembly (index into the workspace's assemblies) placed rigidly,
    /// in place of a single part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<usize>,
}

/// One part inside an instance: a Part Studio body and its pose relative to the instance.
#[derive(Clone, Copy, Debug)]
pub struct Leaf {
    pub studio: usize,
    pub body: BodyId,
    pub pose: Pose,
}

/// The parts an instance stands for: itself, or every part of its sub-assembly (recursively).
pub fn leaves(all: &[Assembly], inst: &Instance) -> Vec<Leaf> {
    fn rec(all: &[Assembly], inst: &Instance, acc: Pose, depth: usize, out: &mut Vec<Leaf>) {
        match inst.sub {
            None => out.push(Leaf { studio: inst.studio, body: inst.body, pose: acc }),
            Some(k) if depth < 8 => {
                if let Some(a) = all.get(k) {
                    for c in a.instances.iter().filter(|c| !c.hidden) {
                        rec(all, c, acc.compose(&c.pose), depth + 1, out);
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = vec![];
    rec(all, inst, Pose::default(), 0, &mut out);
    out
}

/// Whether assembly `k` is `target` or contains it at any depth (inserting it would loop).
pub fn contains(all: &[Assembly], k: usize, target: usize) -> bool {
    fn rec(all: &[Assembly], k: usize, target: usize, depth: usize) -> bool {
        k == target || (depth < 8 && all.get(k).map(|a| a.instances.iter().filter_map(|x| x.sub).any(|s| rec(all, s, target, depth + 1))).unwrap_or(false))
    }
    rec(all, k, target, 0)
}

/// A step of an exploded view: instances moved along a direction.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ExplodeStep {
    pub insts: Vec<u64>,
    pub dir: [f64; 3],
    pub distance: f64,
}

/// A mate connector in the instance's own coordinates.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Connector {
    pub inst: u64,
    pub origin: [f64; 3],
    pub z: [f64; 3],
    pub x: [f64; 3],
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MateKind {
    Fastened,
    Revolute,
    Slider,
    Cylindrical,
    Planar,
    Ball,
}

impl MateKind {
    pub const ALL: [MateKind; 6] = [MateKind::Fastened, MateKind::Revolute, MateKind::Slider, MateKind::Cylindrical, MateKind::Planar, MateKind::Ball];
    pub fn label(self) -> &'static str {
        match self {
            MateKind::Fastened => "Fastened",
            MateKind::Revolute => "Revolute",
            MateKind::Slider => "Slider",
            MateKind::Cylindrical => "Cylindrical",
            MateKind::Planar => "Planar",
            MateKind::Ball => "Ball",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            MateKind::Fastened => "mateFastened",
            MateKind::Revolute => "mateRevolute",
            MateKind::Slider => "mateSlider",
            MateKind::Cylindrical => "mateCylindrical",
            MateKind::Planar => "matePlanar",
            MateKind::Ball => "mateBall",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mate {
    pub id: u64,
    pub name: String,
    pub kind: MateKind,
    pub a: Option<Connector>,
    pub b: Option<Connector>,
    /// Align the Z axes in the same direction instead of face to face.
    #[serde(default)]
    pub flip: bool,
    /// Distance along the first connector's Z axis.
    #[serde(default)]
    pub offset: f64,
    /// Offset along the first connector's X and Y axes.
    #[serde(default)]
    pub offset_xy: [f64; 2],
    /// Rotation of the second part about the mate axis, degrees.
    #[serde(default)]
    pub rotation: f64,
    /// Travel limits: degrees for revolute, mm along the axis for slider/cylindrical.
    #[serde(default)]
    pub limits: Option<[f64; 2]>,
    #[serde(default)]
    pub suppressed: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RelationKind {
    /// Two revolute mates turn together: θb = −ratio·θa
    Gear,
    /// Revolute a drives slider b: travel = ratio (pitch radius) · θa
    Rack,
    /// One cylindrical mate a: travel = ratio (pitch) · turns
    Screw,
}

impl RelationKind {
    pub const ALL: [RelationKind; 3] = [RelationKind::Gear, RelationKind::Rack, RelationKind::Screw];
    pub fn label(self) -> &'static str {
        match self {
            RelationKind::Gear => "Gear",
            RelationKind::Rack => "Rack and pinion",
            RelationKind::Screw => "Screw",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Relation {
    pub id: u64,
    pub name: String,
    pub kind: RelationKind,
    pub a: u64,
    #[serde(default)]
    pub b: Option<u64>,
    pub ratio: f64,
    #[serde(default)]
    pub reverse: bool,
    /// Value of the relation when it was made, so parts don't jump.
    #[serde(default)]
    pub phase: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Assembly {
    pub name: String,
    pub instances: Vec<Instance>,
    pub mates: Vec<Mate>,
    pub next_id: u64,
    #[serde(default)]
    pub relations: Vec<Relation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub explode_steps: Vec<ExplodeStep>,
}

impl Assembly {
    pub fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn instance(&self, id: u64) -> Option<&Instance> {
        self.instances.iter().find(|i| i.id == id)
    }

    pub fn world(&self, c: &Connector) -> Option<(DVec3, DVec3, DVec3)> {
        let i = self.instance(c.inst)?;
        Some((i.pose.apply(c.origin.into()), i.pose.dir(c.z.into()).normalize(), i.pose.dir(c.x.into()).normalize()))
    }
}

type Frame3 = (DVec3, DVec3, DVec3);

/// The first connector's frame after the mate's offsets and rotation.
fn target_frame(m: &Mate, a: Frame3) -> Frame3 {
    let (oa, za, xa) = a;
    let rot = DMat3::from_axis_angle(za, m.rotation.to_radians());
    let x = rot * xa;
    let y = za.cross(x);
    (oa + za * m.offset + x * m.offset_xy[0] + y * m.offset_xy[1], za, x)
}

/// Rotation of the second part about the mate axis (radians, −π..π).
pub fn mate_angle(m: &Mate, a: Frame3, b: Frame3) -> f64 {
    let (_, za, xa) = target_frame(m, a);
    let (_, _, xb) = b;
    // the second frame is turned over about x when faces are opposed, so x stays comparable
    xa.cross(xb).dot(za).atan2(xa.dot(xb))
}

/// Travel of the second part along the mate axis (mm).
pub fn mate_slide(m: &Mate, a: Frame3, b: Frame3) -> f64 {
    let (o, za, _) = target_frame(m, a);
    (b.0 - o).dot(za)
}

fn wrap(a: f64) -> f64 {
    a.sin().atan2(a.cos())
}

/// Residual of a relation for the mates' current frames.
fn relation_value(r: &Relation, mates: &[Mate], frames: &dyn Fn(&Mate) -> Option<(Frame3, Frame3)>) -> Option<f64> {
    let ma = mates.iter().find(|m| m.id == r.a)?;
    let (a1, b1) = frames(ma)?;
    let sign = if r.reverse { -1.0 } else { 1.0 };
    Some(match r.kind {
        RelationKind::Gear => {
            let mb = mates.iter().find(|m| Some(m.id) == r.b)?;
            let (a2, b2) = frames(mb)?;
            wrap(mate_angle(mb, a2, b2) + sign * r.ratio * mate_angle(ma, a1, b1) - r.phase) * 10.0
        }
        RelationKind::Rack => {
            let mb = mates.iter().find(|m| Some(m.id) == r.b)?;
            let (a2, b2) = frames(mb)?;
            mate_slide(mb, a2, b2) - sign * r.ratio * mate_angle(ma, a1, b1) - r.phase
        }
        RelationKind::Screw => mate_slide(ma, a1, b1) - sign * r.ratio * mate_angle(ma, a1, b1) / std::f64::consts::TAU - r.phase,
    })
}

/// The relation's current value with zero phase (to store as its phase when created).
pub fn relation_phase(asm: &Assembly, r: &Relation) -> f64 {
    let mut r0 = r.clone();
    r0.phase = 0.0;
    let frames = |m: &Mate| -> Option<(Frame3, Frame3)> { Some((asm.world(m.a.as_ref()?)?, asm.world(m.b.as_ref()?)?)) };
    let v = relation_value(&r0, &asm.mates, &frames).unwrap_or(0.0);
    match r.kind {
        RelationKind::Gear => v / 10.0,
        _ => v,
    }
}

/// Residuals of one mate for the given instance poses.
fn mate_residuals(m: &Mate, a: (DVec3, DVec3, DVec3), b: (DVec3, DVec3, DVec3), out: &mut Vec<f64>) {
    let (oa, za, xa) = target_frame(m, a);
    let (ob, zb, xb) = b;
    let s = if m.flip { 1.0 } else { -1.0 };
    let target_o = oa;
    let d = ob - target_o;
    let axis = zb - za * s;
    let push3 = |out: &mut Vec<f64>, v: DVec3| out.extend([v.x, v.y, v.z]);
    match m.kind {
        MateKind::Fastened => {
            push3(out, d);
            push3(out, axis * 10.0);
            push3(out, (xb - xa) * 10.0);
        }
        MateKind::Revolute => {
            push3(out, d);
            push3(out, axis * 10.0);
        }
        MateKind::Slider => {
            push3(out, d - za * d.dot(za));
            push3(out, axis * 10.0);
            push3(out, (xb - xa) * 10.0);
        }
        MateKind::Cylindrical => {
            push3(out, d - za * d.dot(za));
            push3(out, axis * 10.0);
        }
        MateKind::Planar => {
            out.push(d.dot(za));
            push3(out, axis * 10.0);
        }
        MateKind::Ball => push3(out, d),
    }
}

pub struct SolveResult {
    pub ok: bool,
    /// Mates still not satisfied.
    pub failed: Vec<u64>,
}

/// Move free instances to satisfy the mates. `hold` instances move as little as possible;
/// `centers` gives each instance's rotation centre (part centroid, local coordinates).
pub fn solve(asm: &mut Assembly, hold: &[u64], centers: &dyn Fn(&Instance) -> DVec3) -> SolveResult {
    let free: Vec<usize> = (0..asm.instances.len()).filter(|&i| !asm.instances[i].fixed).collect();
    let n = free.len() * 6;
    let mates: Vec<Mate> = asm.mates.iter().filter(|m| !m.suppressed && m.a.is_some() && m.b.is_some()).cloned().collect();
    let base: Vec<Pose> = asm.instances.iter().map(|i| i.pose).collect();
    let ctr: Vec<DVec3> = asm.instances.iter().map(|i| i.pose.apply(centers(i))).collect();
    let idx_of = |id: u64| asm.instances.iter().position(|i| i.id == id);

    let poses_for = |x: &[f64]| -> Vec<Pose> {
        let mut p = base.clone();
        for (k, &i) in free.iter().enumerate() {
            let w = DVec3::new(x[6 * k], x[6 * k + 1], x[6 * k + 2]);
            let v = DVec3::new(x[6 * k + 3], x[6 * k + 4], x[6 * k + 5]);
            p[i] = base[i].perturbed(w, v, ctr[i]);
        }
        p
    };
    let world = |poses: &[Pose], c: &Connector| -> Option<(DVec3, DVec3, DVec3)> {
        let i = idx_of(c.inst)?;
        let p = &poses[i];
        Some((p.apply(c.origin.into()), p.dir(c.z.into()).normalize(), p.dir(c.x.into()).normalize()))
    };
    let relations: Vec<Relation> = asm.relations.clone();
    let active: std::cell::RefCell<Vec<(u64, f64)>> = std::cell::RefCell::new(vec![]);
    let residuals = |x: &[f64]| -> Vec<f64> {
        let poses = poses_for(x);
        let mut out = vec![];
        for m in &mates {
            if let (Some(a), Some(b)) = (world(&poses, m.a.as_ref().unwrap()), world(&poses, m.b.as_ref().unwrap())) {
                mate_residuals(m, a, b, &mut out);
                // a limit that was hit holds the part at it
                if let Some((_, bound)) = active.borrow().iter().find(|x| x.0 == m.id) {
                    match m.kind {
                        MateKind::Revolute => out.push(wrap(mate_angle(m, a, b) - bound.to_radians()) * 10.0),
                        _ => out.push(mate_slide(m, a, b) - bound),
                    }
                }
            }
        }
        let frames = |m: &Mate| -> Option<(Frame3, Frame3)> { Some((world(&poses, m.a.as_ref()?)?, world(&poses, m.b.as_ref()?)?)) };
        for r in &relations {
            if let Some(v) = relation_value(r, &mates, &frames) {
                out.push(v);
            }
        }
        out
    };
    let mut w = vec![1.0; n];
    for (k, &i) in free.iter().enumerate() {
        if hold.contains(&asm.instances[i].id) {
            for j in 0..6 {
                w[6 * k + j] = 1e4;
            }
        }
        // rotations cost relative to part size
        for j in 0..3 {
            w[6 * k + j] *= 100.0;
        }
    }
    let mut x = vec![0.0; n];
    let norm = |r: &[f64]| r.iter().map(|v| v * v).sum::<f64>();
    for _pass in 0..4 {
    if n > 0 {
        for _ in 0..80 {
            let r = residuals(&x);
            let nr = norm(&r);
            if nr < 1e-14 {
                break;
            }
            // numeric jacobian
            let mut jac = vec![vec![0.0; n]; r.len()];
            for p in 0..n {
                let h = 1e-6;
                let mut xp = x.clone();
                xp[p] += h;
                let rp = residuals(&xp);
                let mut xm = x.clone();
                xm[p] -= h;
                let rm = residuals(&xm);
                for k in 0..r.len() {
                    jac[k][p] = (rp[k] - rm[k]) / (2.0 * h);
                }
            }
            let dx = crate::sketch::min_norm_step(&jac, &r, &w, n);
            let mut t = 1.0;
            let mut improved = false;
            for _ in 0..12 {
                let xn: Vec<f64> = x.iter().zip(&dx).map(|(a, d)| a + t * d).collect();
                if norm(&residuals(&xn)) < nr {
                    x = xn;
                    improved = true;
                    break;
                }
                t *= 0.5;
            }
            if !improved {
                break;
            }
        }
    }
    // enforce travel limits: pin any mate that went past one and solve again
    let poses = poses_for(&x);
    let mut newly = false;
    for m in &mates {
        let (Some(lim), Some(a), Some(b)) = (m.limits, world(&poses, m.a.as_ref().unwrap()), world(&poses, m.b.as_ref().unwrap())) else { continue };
        if active.borrow().iter().any(|x| x.0 == m.id) {
            continue;
        }
        let v = match m.kind {
            MateKind::Revolute => mate_angle(m, a, b).to_degrees(),
            MateKind::Slider | MateKind::Cylindrical => mate_slide(m, a, b),
            _ => continue,
        };
        let (lo, hi) = (lim[0].min(lim[1]), lim[0].max(lim[1]));
        if v < lo - 1e-6 || v > hi + 1e-6 {
            active.borrow_mut().push((m.id, if v < lo { lo } else { hi }));
            newly = true;
        }
    }
    if !newly {
        break;
    }
    }
    let poses = poses_for(&x);
    for (i, p) in poses.into_iter().enumerate() {
        asm.instances[i].pose = p;
    }
    // report unsatisfied mates
    let mut failed = vec![];
    for m in &mates {
        if let (Some(a), Some(b)) = (asm.world(m.a.as_ref().unwrap()), asm.world(m.b.as_ref().unwrap())) {
            let mut r = vec![];
            mate_residuals(m, a, b, &mut r);
            if r.iter().any(|v| v.abs() > 1e-4) {
                failed.push(m.id);
            }
        }
    }
    SolveResult { ok: failed.is_empty(), failed }
}

/// Infer a mate connector from a pick on a part: circular edges and cylindrical
/// faces give their axis, planar faces their centre and normal. `hit` and the
/// returned connector are in the part's own coordinates.
pub fn infer_connector(inst: u64, mesh: &BodyMesh, face: Option<usize>, edge: Option<usize>, hit: DVec3) -> Option<Connector> {
    let mk = |o: DVec3, z: DVec3| {
        let z = z.normalize();
        let x = crate::kernel::frame_from(o, z).x;
        Connector { inst, origin: o.into(), z: z.into(), x }
    };
    if let Some(e) = edge.and_then(|e| mesh.edges.get(e)) {
        if let Some((c, n, _)) = e.circle {
            return Some(mk(c.into(), n.into()));
        }
        let a = DVec3::from(e.pts[0].map(|v| v as f64));
        let b = DVec3::from(e.pts[e.pts.len() - 1].map(|v| v as f64));
        return Some(mk(DVec3::from(e.mid), (b - a).normalize()));
    }
    let f = mesh.faces.get(face?)?;
    if f.planar {
        return Some(mk(f.center.into(), f.normal.into()));
    }
    // curved face: the circular edge whose axis this face wraps around
    let mut best: Option<(f64, Connector)> = None;
    for e in &mesh.edges {
        if let Some((c, n, r)) = e.circle {
            let (c, n) = (DVec3::from(c), DVec3::from(n).normalize());
            let v = hit - c;
            let radial = (v - n * v.dot(n)).length();
            let err = (radial - r).abs();
            if err < 0.05 * r.max(1.0) && best.as_ref().map(|b| err < b.0).unwrap_or(true) {
                // put the connector on the axis level with the click
                best = Some((err, mk(c + n * v.dot(n), n)));
            }
        }
    }
    best.map(|b| b.1).or_else(|| Some(mk(f.center.into(), f.normal.into())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(id: u64, fixed: bool, t: [f64; 3]) -> Instance {
        Instance { id, name: format!("i{id}"), studio: 0, body: (1, 0), pose: Pose { t, ..Default::default() }, fixed, hidden: false, sub: None }
    }

    #[test]
    fn sub_assembly_leaves_compose_poses() {
        // sub: two parts 10 apart; top: the sub turned 90° about Z and moved to (100, 0, 0)
        let sub = Assembly { name: "sub".into(), instances: vec![inst(1, true, [0.0; 3]), inst(2, false, [10.0, 0.0, 0.0])], ..Default::default() };
        let rot = DMat3::from_rotation_z(std::f64::consts::FRAC_PI_2);
        let mut top_i = inst(1, true, [100.0, 0.0, 0.0]);
        top_i.pose.r = rot.to_cols_array();
        top_i.sub = Some(0);
        let top = Assembly { name: "top".into(), instances: vec![top_i.clone()], ..Default::default() };
        let all = vec![sub, top];
        let l = leaves(&all, &top_i);
        assert_eq!(l.len(), 2);
        let world = top_i.pose.compose(&l[1].pose).apply(DVec3::ZERO);
        assert!((world - DVec3::new(100.0, 10.0, 0.0)).length() < 1e-9, "{world:?}");
        assert!(contains(&all, 1, 0));
        assert!(!contains(&all, 0, 1));
    }

    #[test]
    fn fastened_brings_faces_together() {
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [30.0, 5.0, 40.0]));
        // top face centre of a 10 mm cube on the first, bottom face of the second
        let ca = Connector { inst: 1, origin: [5.0, 5.0, 10.0], z: [0.0, 0.0, 1.0], x: [1.0, 0.0, 0.0] };
        let cb = Connector { inst: 2, origin: [5.0, 5.0, 0.0], z: [0.0, 0.0, -1.0], x: [1.0, 0.0, 0.0] };
        a.mates.push(Mate { id: 3, name: "m".into(), kind: MateKind::Fastened, a: Some(ca), b: Some(cb), flip: false, offset: 0.0, offset_xy: [0.0; 2], rotation: 0.0, limits: None, suppressed: false });
        let r = solve(&mut a, &[], &|_| DVec3::splat(5.0));
        assert!(r.ok, "{:?}", r.failed);
        let p = a.instances[1].pose;
        assert!((DVec3::from(p.t) - DVec3::new(0.0, 0.0, 10.0)).length() < 1e-5, "{:?}", p.t);
    }

    #[test]
    fn revolute_keeps_rotation_free() {
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [3.0, 1.0, 7.0]));
        let ca = Connector { inst: 1, origin: [0.0, 0.0, 5.0], z: [0.0, 0.0, 1.0], x: [1.0, 0.0, 0.0] };
        let cb = Connector { inst: 2, origin: [0.0, 0.0, 0.0], z: [0.0, 0.0, -1.0], x: [1.0, 0.0, 0.0] };
        a.mates.push(Mate { id: 3, name: "m".into(), kind: MateKind::Revolute, a: Some(ca), b: Some(cb), flip: false, offset: 0.0, offset_xy: [0.0; 2], rotation: 0.0, limits: None, suppressed: false });
        let r = solve(&mut a, &[], &|_| DVec3::ZERO);
        assert!(r.ok);
        let (ob, zb, _) = a.world(&cb).unwrap();
        assert!((ob - DVec3::new(0.0, 0.0, 5.0)).length() < 1e-5);
        assert!((zb + DVec3::Z).length() < 1e-5);
    }

    fn revolute(id: u64, inst: u64, at: [f64; 3]) -> Mate {
        let ca = Connector { inst: 1, origin: at, z: [0.0, 0.0, 1.0], x: [1.0, 0.0, 0.0] };
        let cb = Connector { inst, origin: [0.0, 0.0, 0.0], z: [0.0, 0.0, -1.0], x: [1.0, 0.0, 0.0] };
        Mate { id, name: format!("r{id}"), kind: MateKind::Revolute, a: Some(ca), b: Some(cb), flip: false, offset: 0.0, offset_xy: [0.0; 2], rotation: 0.0, limits: None, suppressed: false }
    }

    fn turn(a: &mut Assembly, inst: u64, deg: f64) {
        let x = a.instances.iter_mut().find(|x| x.id == inst).unwrap();
        let c = DVec3::from(x.pose.t);
        x.pose = x.pose.perturbed(DVec3::Z * deg.to_radians(), DVec3::ZERO, c);
    }

    #[test]
    fn gear_relation_and_limits() {
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [0.0, 0.0, 5.0]));
        a.instances.push(inst(3, false, [30.0, 0.0, 5.0]));
        a.mates.push(revolute(10, 2, [0.0, 0.0, 5.0]));
        a.mates.push(revolute(11, 3, [30.0, 0.0, 5.0]));
        assert!(solve(&mut a, &[], &|_| DVec3::ZERO).ok);
        let mut rel = Relation { id: 20, name: "g".into(), kind: RelationKind::Gear, a: 10, b: Some(11), ratio: 2.0, reverse: false, phase: 0.0 };
        rel.phase = relation_phase(&a, &rel);
        a.relations.push(rel);
        turn(&mut a, 2, 20.0);
        let r = solve(&mut a, &[2], &|_| DVec3::ZERO);
        assert!(r.ok, "{:?}", r.failed);
        let ang = |a: &Assembly, mid: u64| {
            let m = a.mates.iter().find(|m| m.id == mid).unwrap();
            mate_angle(m, a.world(m.a.as_ref().unwrap()).unwrap(), a.world(m.b.as_ref().unwrap()).unwrap()).to_degrees()
        };
        let (t1, t2) = (ang(&a, 10), ang(&a, 11));
        assert!((t2 + 2.0 * t1).abs() < 1e-3, "{t1} {t2}");
        assert!(t1.abs() > 5.0, "the driven part should keep most of its turn: {t1}");

        // limit the first joint to ±10°
        a.relations.clear();
        a.mates[0].limits = Some([-10.0, 10.0]);
        turn(&mut a, 2, 30.0);
        solve(&mut a, &[2], &|_| DVec3::ZERO);
        let t = ang(&a, 10);
        assert!((t.abs() - 10.0).abs() < 1e-3, "clamped to the limit: {t}");
    }
}
