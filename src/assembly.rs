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
    #[serde(default)]
    pub suppressed: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Assembly {
    pub name: String,
    pub instances: Vec<Instance>,
    pub mates: Vec<Mate>,
    pub next_id: u64,
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

/// Residuals of one mate for the given instance poses.
fn mate_residuals(m: &Mate, a: (DVec3, DVec3, DVec3), b: (DVec3, DVec3, DVec3), out: &mut Vec<f64>) {
    let (oa, za, xa) = a;
    let (ob, zb, xb) = b;
    let s = if m.flip { 1.0 } else { -1.0 };
    let target_o = oa + za * m.offset;
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
    let residuals = |x: &[f64]| -> Vec<f64> {
        let poses = poses_for(x);
        let mut out = vec![];
        for m in &mates {
            if let (Some(a), Some(b)) = (world(&poses, m.a.as_ref().unwrap()), world(&poses, m.b.as_ref().unwrap())) {
                mate_residuals(m, a, b, &mut out);
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
        Instance { id, name: format!("i{id}"), studio: 0, body: (1, 0), pose: Pose { t, ..Default::default() }, fixed, hidden: false }
    }

    #[test]
    fn fastened_brings_faces_together() {
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [30.0, 5.0, 40.0]));
        // top face centre of a 10 mm cube on the first, bottom face of the second
        let ca = Connector { inst: 1, origin: [5.0, 5.0, 10.0], z: [0.0, 0.0, 1.0], x: [1.0, 0.0, 0.0] };
        let cb = Connector { inst: 2, origin: [5.0, 5.0, 0.0], z: [0.0, 0.0, -1.0], x: [1.0, 0.0, 0.0] };
        a.mates.push(Mate { id: 3, name: "m".into(), kind: MateKind::Fastened, a: Some(ca), b: Some(cb), flip: false, offset: 0.0, suppressed: false });
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
        a.mates.push(Mate { id: 3, name: "m".into(), kind: MateKind::Revolute, a: Some(ca), b: Some(cb), flip: false, offset: 0.0, suppressed: false });
        let r = solve(&mut a, &[], &|_| DVec3::ZERO);
        assert!(r.ok);
        let (ob, zb, _) = a.world(&cb).unwrap();
        assert!((ob - DVec3::new(0.0, 0.0, 5.0)).length() < 1e-5);
        assert!((zb + DVec3::Z).length() < 1e-5);
    }
}
