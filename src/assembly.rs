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
    leaves_with(all, inst, false)
}

/// The parts of an instance like `leaves`, with the parts hidden inside sub-assemblies too when
/// `hidden` is set: hiding is only for looking, so a bill of materials still counts them.
pub fn leaves_with(all: &[Assembly], inst: &Instance, hidden: bool) -> Vec<Leaf> {
    leaves_shown(all, inst).into_iter().filter(|(_, shown)| hidden || *shown).map(|(l, _)| l).collect()
}

/// Every part of an instance (hidden ones inside sub-assemblies included), with whether it is in
/// sight. The position in this list is the leaf number mate connectors and picks store, so it must
/// not change when something inside a sub-assembly is hidden or shown.
pub fn leaves_shown(all: &[Assembly], inst: &Instance) -> Vec<(Leaf, bool)> {
    fn rec(all: &[Assembly], inst: &Instance, acc: Pose, depth: usize, shown: bool, out: &mut Vec<(Leaf, bool)>) {
        match inst.sub {
            None => out.push((Leaf { studio: inst.studio, body: inst.body, pose: acc }, shown)),
            Some(k) if depth < 8 => {
                if let Some(a) = all.get(k) {
                    for c in &a.instances {
                        rec(all, c, acc.compose(&c.pose), depth + 1, shown && !c.hidden, out);
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = vec![];
    rec(all, inst, Pose::default(), 0, true, &mut out);
    out
}

/// The parts in sight of assembly `ai` placed in its coordinates, for a STEP export:
/// (studio, body, row-major 3x4 pose, name). Parts inside sub-assemblies are named after the
/// instance with a number.
pub fn export_items(all: &[Assembly], ai: usize) -> Vec<crate::kernel::AsmExportItem> {
    let Some(a) = all.get(ai) else { return vec![] };
    let mut out = vec![];
    for x in a.instances.iter().filter(|x| !x.hidden) {
        let ls = leaves(all, x);
        let many = ls.len() > 1 || x.sub.is_some();
        for (k, l) in ls.iter().enumerate() {
            let p = x.pose.compose(&l.pose);
            let (m, t) = (p.m(), p.t);
            let name = if many { format!("{} {}", x.name, k + 1) } else { x.name.clone() };
            out.push((l.studio, l.body, [m.x_axis.x, m.y_axis.x, m.z_axis.x, t[0], m.x_axis.y, m.y_axis.y, m.z_axis.y, t[1], m.x_axis.z, m.y_axis.z, m.z_axis.z, t[2]], name));
        }
    }
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
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Connector {
    pub inst: u64,
    pub origin: [f64; 3],
    pub z: [f64; 3],
    pub x: [f64; 3],
    /// The geometry it was placed on, to find it again after the part changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<ConnRef>,
}

/// What a mate connector sits on: a planar face, a circle's axis or a straight edge of one of
/// the instance's parts. After a Part Studio rebuild the connector is moved onto the matching
/// geometry of the new part (see `refresh_connectors`).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ConnRef {
    /// Which part of the instance (index into `leaves`).
    pub leaf: u32,
    /// 0 planar face, 1 axis of a circular edge or cylindrical face, 2 straight edge.
    pub kind: u8,
    /// Face (planar) or edge index in the part's mesh when it was picked.
    pub index: u32,
    /// On a circular edge: the connector is at the circle's centre (else on the axis where it was picked).
    #[serde(default)]
    pub centre: bool,
    /// Circle radius (axes), when picked.
    #[serde(default)]
    pub size: f64,
    /// Planar faces: key of the face's persistent name (`face_key`), 0 when it had none.
    #[serde(default)]
    pub name: u64,
}

/// A stable key for a face's persistent name (FNV-1a over feature, role and number); 0 for none.
pub fn face_key(n: &Option<crate::doc::FaceName>) -> u64 {
    let Some(n) = n else { return 0 };
    let mut h: u64 = 0xcbf29ce484222325;
    for b in n.feature.to_le_bytes().iter().chain(n.role.as_bytes()).chain(&n.n.to_le_bytes()) {
        h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
    }
    h.max(1)
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
    /// Each turning mate's angle (radians) counted on past ±180°, as the solver last left it:
    /// relations (a screw's travel, a gear of ratio ½) and limits past ±180° need whole turns.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub turns: std::collections::HashMap<u64, f64>,
    /// Sliders linked to joints (see asm_anim).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub motions: Vec<crate::asm_anim::Motion>,
    /// Timelines of those motions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub animations: Vec<crate::asm_anim::Animation>,
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

pub(crate) type Frame3 = (DVec3, DVec3, DVec3);

/// The first connector's frame after the mate's offsets and rotation.
pub(crate) fn target_frame(m: &Mate, a: Frame3) -> Frame3 {
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

/// The angle equal to `a` (up to whole turns) nearest `prev`: an angle counted on past ±180°.
fn unwrap_near(prev: f64, a: f64) -> f64 {
    prev + wrap(a - prev)
}

/// Whether a mate turns about its axis (its angle is counted in whole turns).
fn turning(m: &Mate) -> bool {
    matches!(m.kind, MateKind::Revolute | MateKind::Cylindrical)
}

impl Assembly {
    /// Mate `m`'s angle counted past ±180° from where the solver last left it (`turns`).
    pub fn mate_turn(&self, m: &Mate, a: Frame3, b: Frame3) -> f64 {
        let t = mate_angle(m, a, b);
        self.turns.get(&m.id).map(|p| unwrap_near(*p, t)).unwrap_or(t)
    }
}

/// Residual of a relation for the mates' current frames.
/// `angle` gives a mate's angle counted past ±180° (so a screw or a gear of ratio ½ doesn't
/// jump back a turn when the angle wraps round).
fn relation_value(r: &Relation, mates: &[Mate], frames: &dyn Fn(&Mate) -> Option<(Frame3, Frame3)>, angle: &dyn Fn(&Mate, Frame3, Frame3) -> f64) -> Option<f64> {
    let ma = mates.iter().find(|m| m.id == r.a)?;
    let (a1, b1) = frames(ma)?;
    let sign = if r.reverse { -1.0 } else { 1.0 };
    Some(match r.kind {
        RelationKind::Gear => {
            let mb = mates.iter().find(|m| Some(m.id) == r.b)?;
            let (a2, b2) = frames(mb)?;
            wrap(mate_angle(mb, a2, b2) + sign * r.ratio * angle(ma, a1, b1) - r.phase) * 10.0
        }
        RelationKind::Rack => {
            let mb = mates.iter().find(|m| Some(m.id) == r.b)?;
            let (a2, b2) = frames(mb)?;
            mate_slide(mb, a2, b2) - sign * r.ratio * angle(ma, a1, b1) - r.phase
        }
        RelationKind::Screw => mate_slide(ma, a1, b1) - sign * r.ratio * angle(ma, a1, b1) / std::f64::consts::TAU - r.phase,
    })
}

/// The relation's current value with zero phase (to store as its phase when created).
pub fn relation_phase(asm: &Assembly, r: &Relation) -> f64 {
    let mut r0 = r.clone();
    r0.phase = 0.0;
    let frames = |m: &Mate| -> Option<(Frame3, Frame3)> { Some((asm.world(m.a.as_ref()?)?, asm.world(m.b.as_ref()?)?)) };
    let v = relation_value(&r0, &asm.mates, &frames, &|m, a, b| asm.mate_turn(m, a, b)).unwrap_or(0.0);
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

/// The rotation (world) that turns the second connector's frame `b` to where mate `m` wants
/// it against the first's `a`: z along (or against, unflipped) the first's z, and for mates
/// that fix the turn about the axis, x on the first's (rotated) x. Mates free to turn get the
/// shortest turn of z; a half turn goes about the second's x (turning the part over).
fn mate_rotation(m: &Mate, a: Frame3, b: Frame3) -> Option<DMat3> {
    let (_, za, xt) = target_frame(m, a);
    let (_, zb, xb) = b;
    let zt = if m.flip { za } else { -za };
    match m.kind {
        MateKind::Ball => None,
        MateKind::Fastened | MateKind::Slider => {
            let ft = DMat3::from_cols(xt, zt.cross(xt), zt);
            let fb = DMat3::from_cols(xb, zb.cross(xb), zb);
            Some(orthonormalize(ft * fb.transpose()))
        }
        _ => {
            let c = zb.dot(zt).clamp(-1.0, 1.0);
            let axis = zb.cross(zt);
            Some(if axis.length() > 1e-9 {
                DMat3::from_axis_angle(axis.normalize(), c.acos())
            } else if c > 0.0 {
                DMat3::IDENTITY
            } else {
                DMat3::from_axis_angle(xb, std::f64::consts::PI)
            })
        }
    }
}

/// Angle (radians) of a rotation matrix.
fn rotation_angle(r: DMat3) -> f64 {
    ((r.x_axis.x + r.y_axis.y + r.z_axis.z - 1.0) / 2.0).clamp(-1.0, 1.0).acos()
}

/// Turn parts that are far from what their mates want (a flipped axis, a part picked on a face
/// pointing the wrong way, a 180° rotation) roughly into place before the iterative solve.
/// Starting half a turn out the solve's gradient vanishes, so it would stop there.
/// Instances are visited outward from the fixed and held ones, each moved once by the mate
/// that reaches it first, so the parts already placed stay put and the ones hanging off a
/// turned part follow it.
fn prealign(asm: &mut Assembly, mates: &[Mate], hold: &[u64]) {
    let n = asm.instances.len();
    let idx = |asm: &Assembly, id: u64| asm.instances.iter().position(|i| i.id == id);
    let mut depth: Vec<Option<usize>> = vec![None; n];
    let mut queue = std::collections::VecDeque::new();
    for (i, x) in asm.instances.iter().enumerate() {
        if x.fixed || hold.contains(&x.id) {
            depth[i] = Some(0);
            queue.push_back(i);
        }
    }
    let ends: Vec<(usize, usize, usize)> = mates
        .iter()
        .enumerate()
        .filter_map(|(k, m)| Some((k, idx(asm, m.a?.inst)?, idx(asm, m.b?.inst)?)))
        .filter(|e| e.1 != e.2)
        .collect();
    // tree edges: (mate, mover) in visiting order
    let mut order: Vec<(usize, usize)> = vec![];
    loop {
        while let Some(i) = queue.pop_front() {
            for &(k, ia, ib) in &ends {
                let other = if ia == i { ib } else if ib == i { ia } else { continue };
                if depth[other].is_none() {
                    depth[other] = Some(depth[i].unwrap() + 1);
                    order.push((k, other));
                    queue.push_back(other);
                }
            }
        }
        // a group of parts with nothing fixed: its first mate's first part stays
        let Some(&(_, ia, _)) = ends.iter().find(|e| depth[e.1].is_none() || depth[e.2].is_none()) else { break };
        let root = if depth[ia].is_none() { ia } else { ends.iter().find(|e| depth[e.2].is_none()).unwrap().2 };
        depth[root] = Some(0);
        queue.push_back(root);
    }
    for (k, mover) in order {
        let m = &mates[k];
        let (Some(ca), Some(cb)) = (m.a.as_ref(), m.b.as_ref()) else { continue };
        let (Some(fa), Some(fb)) = (asm.world(ca), asm.world(cb)) else { continue };
        let Some(r) = mate_rotation(m, fa, fb) else { continue };
        if rotation_angle(r) < 45f64.to_radians() {
            continue;
        }
        // turning the second part by r, or the first by r⁻¹, gives the same relative frames
        let b_moves = idx(asm, cb.inst) == Some(mover);
        let (r, pivot) = if b_moves { (r, fb.0) } else { (r.transpose(), fa.0) };
        let p = &mut asm.instances[mover].pose;
        let t = r * (DVec3::from(p.t) - pivot) + pivot;
        p.r = orthonormalize(r * p.m()).to_cols_array();
        p.t = t.into();
        // mates that hold a point bring it over too (the solve does the rest)
        if matches!(m.kind, MateKind::Fastened | MateKind::Revolute) {
            let (Some(fa), Some(fb)) = (asm.world(ca), asm.world(cb)) else { continue };
            let d = target_frame(m, fa).0 - fb.0;
            let p = &mut asm.instances[mover].pose;
            p.t = (DVec3::from(p.t) + if b_moves { d } else { -d }).into();
        }
    }
}

/// Put the moving side of a newly made mate where the mate wants it: turned like `prealign`
/// does and moved so the connectors meet (the solve then only has to settle the rest). Mates
/// that leave the travel along the axis free (slider, cylindrical, planar) would otherwise
/// leave the parts wherever they were, often inside each other. The side in `hold`, or the
/// fixed one, stays.
pub fn place_new_mate(asm: &mut Assembly, mate: u64, hold: &[u64]) {
    let Some(m) = asm.mates.iter().find(|m| m.id == mate).cloned() else { return };
    let (Some(ca), Some(cb)) = (m.a, m.b) else { return };
    if ca.inst == cb.inst {
        return;
    }
    let stays = |id: u64| hold.contains(&id) || asm.instance(id).map(|x| x.fixed).unwrap_or(true);
    let b_moves = if !stays(cb.inst) { true } else if !stays(ca.inst) { false } else { return };
    let mover = if b_moves { cb.inst } else { ca.inst };
    let Some(ix) = asm.instances.iter().position(|x| x.id == mover) else { return };
    let (Some(fa), Some(fb)) = (asm.world(&ca), asm.world(&cb)) else { return };
    if let Some(r) = mate_rotation(&m, fa, fb) {
        let (r, pivot) = if b_moves { (r, fb.0) } else { (r.transpose(), fa.0) };
        let p = &mut asm.instances[ix].pose;
        p.t = (r * (DVec3::from(p.t) - pivot) + pivot).into();
        p.r = orthonormalize(r * p.m()).to_cols_array();
    }
    let (Some(fa), Some(fb)) = (asm.world(&ca), asm.world(&cb)) else { return };
    let d = target_frame(&m, fa).0 - fb.0;
    let p = &mut asm.instances[ix].pose;
    p.t = (DVec3::from(p.t) + if b_moves { d } else { -d }).into();
}

pub struct SolveResult {
    pub ok: bool,
    /// Mates still not satisfied.
    pub failed: Vec<u64>,
}

/// A mate held at an angle (degrees) and/or a travel (mm), as a motor or servo turns it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drive {
    pub mate: u64,
    pub angle: Option<f64>,
    pub slide: Option<f64>,
}

/// Move free instances to satisfy the mates. `hold` instances move as little as possible;
/// `centers` gives each instance's rotation centre (part centroid, local coordinates).
pub fn solve(asm: &mut Assembly, hold: &[u64], centers: &dyn Fn(&Instance) -> DVec3) -> SolveResult {
    solve_driven(asm, hold, centers, &[])
}

/// `solve` with mates held where `drives` puts them. A relation between driven mates is left
/// to the drives (they already agree, and the relation only sees angles within a turn).
pub fn solve_driven(asm: &mut Assembly, hold: &[u64], centers: &dyn Fn(&Instance) -> DVec3, drives: &[Drive]) -> SolveResult {
    solve_suspect(asm, hold, centers, drives, &[])
}

/// Solve while instance `inst` is dragged by its point `grab` (own coordinates): that point
/// stays where the pointer put it as far as the mates allow, and the part turns about it
/// freely, so a part on a revolute swings round after the pointer instead of sliding back.
pub fn solve_drag(asm: &mut Assembly, centers: &dyn Fn(&Instance) -> DVec3, inst: u64, grab: DVec3) -> SolveResult {
    solve_grab(asm, &[inst], centers, &[], &[], Some((inst, grab)))
}

/// `solve_driven`, and when the mates can't all be met, find the ones in conflict instead of
/// letting the error spread over every part: leave out one mate at a time (those in `suspects`,
/// the ones just made or edited, first, then the newest) and keep the first solve that meets
/// all the others. The mates left out stay where they are, reported as failed.
pub fn solve_suspect(asm: &mut Assembly, hold: &[u64], centers: &dyn Fn(&Instance) -> DVec3, drives: &[Drive], suspects: &[u64]) -> SolveResult {
    solve_grab(asm, hold, centers, drives, suspects, None)
}

fn solve_grab(asm: &mut Assembly, hold: &[u64], centers: &dyn Fn(&Instance) -> DVec3, drives: &[Drive], suspects: &[u64], grab: Option<(u64, DVec3)>) -> SolveResult {
    let start = (asm.instances.iter().map(|i| i.pose).collect::<Vec<_>>(), asm.turns.clone());
    let restore = |asm: &mut Assembly, s: &(Vec<Pose>, std::collections::HashMap<u64, f64>)| {
        for (i, p) in asm.instances.iter_mut().zip(&s.0) {
            i.pose = *p;
        }
        asm.turns = s.1.clone();
    };
    solve_once(asm, hold, centers, drives, None, grab);
    let failed = failed_mates(asm);
    let active: Vec<u64> = asm.mates.iter().filter(|m| !m.suppressed && m.a.is_some() && m.b.is_some()).map(|m| m.id).collect();
    if !failed.is_empty() && active.len() > 1 {
        let full = (asm.instances.iter().map(|i| i.pose).collect::<Vec<_>>(), asm.turns.clone());
        let mut order: Vec<u64> = suspects.iter().copied().filter(|id| active.contains(id)).collect();
        let mut newest = active.clone();
        newest.sort_unstable_by(|a, b| b.cmp(a));
        for id in newest {
            if !order.contains(&id) {
                order.push(id);
            }
        }
        for &drop in order.iter().take(3) {
            restore(asm, &start);
            solve_once(asm, hold, centers, drives, Some(drop), grab);
            if failed_mates(asm).iter().all(|&f| f == drop) {
                let failed = failed_mates(asm);
                return SolveResult { ok: failed.is_empty(), failed };
            }
        }
        restore(asm, &full);
    }
    SolveResult { ok: failed.is_empty(), failed }
}

/// Scale for turning an instance in the solve: about the distance from its centre to its mate
/// connectors, so a turn costs as much as the sliding it causes where the part is mated (a
/// fixed weight let long parts swing about rather than slide).
fn turn_scale(asm: &Assembly, mates: &[Mate], i: usize, ctr: DVec3) -> f64 {
    let id = asm.instances[i].id;
    let mut r: f64 = 5.0;
    for m in mates {
        for c in [m.a, m.b].into_iter().flatten().filter(|c| c.inst == id) {
            if let Some((o, _, _)) = asm.world(&c) {
                r = r.max((o - ctr).length());
            }
        }
    }
    r.min(1e4)
}

fn solve_once(asm: &mut Assembly, hold: &[u64], centers: &dyn Fn(&Instance) -> DVec3, drives: &[Drive], skip: Option<u64>, grab: Option<(u64, DVec3)>) {
    let all_mates: Vec<Mate> = asm.mates.iter().filter(|m| !m.suppressed && m.a.is_some() && m.b.is_some() && Some(m.id) != skip).cloned().collect();
    prealign(asm, &all_mates, hold);
    let driven = |id: u64| drives.iter().any(|d| d.mate == id);
    let all_relations: Vec<Relation> = asm.relations.iter().filter(|r| !(driven(r.a) && r.b.map(driven).unwrap_or(true))).cloned().collect();
    let start: std::collections::HashMap<u64, f64> = all_mates
        .iter()
        .filter(|m| turning(m))
        .filter_map(|m| Some((m.id, asm.mate_turn(m, asm.world(m.a.as_ref()?)?, asm.world(m.b.as_ref()?)?))))
        .collect();
    // parts that no chain of mates links move independently: solving each group on its own
    // keeps the (dense, numeric) Newton steps small in a big assembly
    for (free, mates, relations) in solve_groups(asm, &all_mates, &all_relations) {
        solve_group(asm, hold, centers, drives, grab, &free, &mates, &relations);
    }
    // turning mates' angles, counted on from where they were
    let mut turns = std::collections::HashMap::new();
    for m in all_mates.iter().filter(|m| turning(m)) {
        if let (Some(a), Some(b)) = (asm.world(m.a.as_ref().unwrap()), asm.world(m.b.as_ref().unwrap())) {
            let t = mate_angle(m, a, b);
            turns.insert(m.id, start.get(&m.id).map(|p| unwrap_near(*p, t)).unwrap_or(t));
        }
    }
    asm.turns = turns;
}

/// The independent groups of a solve: the free instances (indices) each chain of mates links,
/// with the mates and relations acting on them.
fn solve_groups(asm: &Assembly, mates: &[Mate], relations: &[Relation]) -> Vec<(Vec<usize>, Vec<Mate>, Vec<Relation>)> {
    let n = asm.instances.len();
    let idx: std::collections::HashMap<u64, usize> = asm.instances.iter().enumerate().map(|(i, x)| (x.id, i)).collect();
    let free = |i: usize| !asm.instances[i].fixed;
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    let ends = |m: &Mate| -> Vec<usize> { [m.a, m.b].into_iter().flatten().filter_map(|c| idx.get(&c.inst).copied()).filter(|&i| free(i)).collect() };
    let join = |p: &mut Vec<usize>, xs: &[usize]| {
        for w in xs.windows(2) {
            let (a, b) = (root(p, w[0]), root(p, w[1]));
            p[a] = b;
        }
    };
    for m in mates {
        join(&mut parent, &ends(m));
    }
    // a relation ties its mates' parts together
    for r in relations {
        let xs: Vec<usize> = mates.iter().filter(|m| m.id == r.a || Some(m.id) == r.b).flat_map(|m| ends(m)).collect();
        join(&mut parent, &xs);
    }
    let mut groups: Vec<(usize, Vec<usize>)> = vec![];
    for i in (0..n).filter(|&i| free(i)) {
        let r = root(&mut parent, i);
        match groups.iter_mut().find(|g| g.0 == r) {
            Some(g) => g.1.push(i),
            None => groups.push((r, vec![i])),
        }
    }
    let mut out = vec![];
    for (r, members) in groups {
        let gm: Vec<Mate> = mates.iter().filter(|m| ends(m).first().is_some_and(|&i| root(&mut parent, i) == r)).cloned().collect();
        if gm.is_empty() {
            // nothing acts on these parts: they stay where they are
            continue;
        }
        let ids: Vec<u64> = gm.iter().map(|m| m.id).collect();
        let gr: Vec<Relation> = relations.iter().filter(|x| ids.contains(&x.a)).cloned().collect();
        out.push((members, gm, gr));
    }
    out
}

/// Solve one group of `solve_groups`: move instances `free` to meet `mates` and `relations`.
#[allow(clippy::too_many_arguments)]
fn solve_group(asm: &mut Assembly, hold: &[u64], centers: &dyn Fn(&Instance) -> DVec3, drives: &[Drive], grab: Option<(u64, DVec3)>, free: &[usize], mates: &[Mate], relations: &[Relation]) {
    let n = free.len() * 6;
    let base: Vec<Pose> = asm.instances.iter().map(|i| i.pose).collect();
    // a dragged part turns about the grabbed point
    let ctr: Vec<DVec3> = asm.instances.iter().map(|i| i.pose.apply(grab.filter(|g| g.0 == i.id).map(|g| g.1).unwrap_or_else(|| centers(i)))).collect();
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
    let active: std::cell::RefCell<Vec<(u64, f64)>> = std::cell::RefCell::new(vec![]);
    // turning mates' angles at the start, counted past ±180°: each evaluation is taken as the
    // nearest angle to it (the solver's steps are far smaller than half a turn)
    let start: std::collections::HashMap<u64, f64> = mates
        .iter()
        .filter(|m| turning(m))
        .filter_map(|m| Some((m.id, asm.mate_turn(m, asm.world(m.a.as_ref()?)?, asm.world(m.b.as_ref()?)?))))
        .collect();
    let turn = |m: &Mate, a: Frame3, b: Frame3| {
        let t = mate_angle(m, a, b);
        start.get(&m.id).map(|p| unwrap_near(*p, t)).unwrap_or(t)
    };
    let residuals = |x: &[f64]| -> Vec<f64> {
        let poses = poses_for(x);
        let mut out = vec![];
        for m in mates {
            if let (Some(a), Some(b)) = (world(&poses, m.a.as_ref().unwrap()), world(&poses, m.b.as_ref().unwrap())) {
                mate_residuals(m, a, b, &mut out);
                for d in drives.iter().filter(|d| d.mate == m.id) {
                    if let Some(g) = d.angle {
                        out.push(wrap(mate_angle(m, a, b) - g.to_radians()) * 10.0);
                    }
                    if let Some(s) = d.slide {
                        out.push(mate_slide(m, a, b) - s);
                    }
                }
                // a limit that was hit holds the part at it
                if let Some((_, bound)) = active.borrow().iter().find(|x| x.0 == m.id) {
                    match m.kind {
                        MateKind::Revolute => out.push((turn(m, a, b) - bound.to_radians()) * 10.0),
                        _ => out.push(mate_slide(m, a, b) - bound),
                    }
                }
            }
        }
        let frames = |m: &Mate| -> Option<(Frame3, Frame3)> { Some((world(&poses, m.a.as_ref()?)?, world(&poses, m.b.as_ref()?)?)) };
        for r in relations {
            if let Some(v) = relation_value(r, mates, &frames, &turn) {
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
        // turns cost relative to part size, well above sliding: a mate that only needs the part
        // moved shouldn't swing it about; the dragged part turns freely about the grabbed point
        let r = turn_scale(asm, mates, i, ctr[i]);
        let dragged = grab.is_some_and(|g| g.0 == asm.instances[i].id);
        for j in 0..3 {
            w[6 * k + j] = if dragged { 1.0 } else { w[6 * k + j] * 100.0 * r * r };
        }
    }
    let mut x = vec![0.0; n];
    let norm = |r: &[f64]| r.iter().map(|v| v * v).sum::<f64>();
    for _pass in 0..4 {
    // a dragged part pinned at a limit has to come back off the pointer: holding it there too
    // leaves the solve too stiff to turn it about its mate (the hinge came apart instead)
    if _pass > 0 && !active.borrow().is_empty() {
        for (k, &i) in free.iter().enumerate() {
            if grab.is_some_and(|g| g.0 == asm.instances[i].id) {
                for j in 3..6 {
                    w[6 * k + j] = 1.0;
                }
            }
        }
    }
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
    for m in mates {
        let (Some(lim), Some(a), Some(b)) = (m.limits, world(&poses, m.a.as_ref().unwrap()), world(&poses, m.b.as_ref().unwrap())) else { continue };
        if active.borrow().iter().any(|x| x.0 == m.id) {
            continue;
        }
        let v = match m.kind {
            MateKind::Revolute => turn(m, a, b).to_degrees(),
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
    for &i in free {
        asm.instances[i].pose = poses[i];
    }
}

/// Mates whose conditions do not hold at the current poses (residual above tolerance).
pub fn failed_mates(asm: &Assembly) -> Vec<u64> {
    let mut failed = vec![];
    for m in asm.mates.iter().filter(|m| !m.suppressed) {
        let (Some(ca), Some(cb)) = (m.a.as_ref(), m.b.as_ref()) else { continue };
        if let (Some(a), Some(b)) = (asm.world(ca), asm.world(cb)) {
            let mut r = vec![];
            mate_residuals(m, a, b, &mut r);
            if r.iter().any(|v| v.abs() > 1e-4) {
                failed.push(m.id);
            }
        }
    }
    failed
}

/// The X axis a connector with Z axis `z` gets: the same for `z` and `-z`, so connectors on
/// opposed faces (fastened face to face) line up without turning the part over.
pub fn connector_x(o: DVec3, z: DVec3) -> DVec3 {
    let a = z.abs();
    let big = if a.x >= a.y && a.x >= a.z { z.x } else if a.y >= a.z { z.y } else { z.z };
    let zc = if big < 0.0 { -z } else { z };
    DVec3::from(crate::kernel::frame_from(o, zc).x)
}

/// Infer a mate connector from a pick on a part (`leaf` of the instance): circular edges and
/// cylindrical faces give their axis, planar faces the picked point and their normal. `hit`
/// and the returned connector are in the part's own coordinates.
pub fn infer_connector(inst: u64, leaf: usize, mesh: &BodyMesh, face: Option<usize>, edge: Option<usize>, hit: DVec3) -> Option<Connector> {
    let mk = |o: DVec3, z: DVec3, on: Option<ConnRef>| {
        let z = z.normalize();
        Connector { inst, origin: o.into(), z: z.into(), x: connector_x(o, z).into(), on }
    };
    let leaf = leaf as u32;
    if let Some((ei, e)) = edge.and_then(|e| mesh.edges.get(e).map(|x| (e, x))) {
        if let Some((c, n, r)) = e.circle {
            return Some(mk(c.into(), n.into(), Some(ConnRef { leaf, kind: 1, index: ei as u32, centre: true, size: r, name: 0 })));
        }
        let a = DVec3::from(e.pts[0].map(|v| v as f64));
        let b = DVec3::from(e.pts[e.pts.len() - 1].map(|v| v as f64));
        let on = e.linear.then_some(ConnRef { leaf, kind: 2, index: ei as u32, ..Default::default() });
        return Some(mk(DVec3::from(e.mid), (b - a).normalize(), on));
    }
    let fi = face?;
    let f = mesh.faces.get(fi)?;
    if f.planar {
        // at the picked point, moved onto the face's plane
        let n = DVec3::from(f.normal).normalize();
        let o = hit - n * (hit - DVec3::from(f.center)).dot(n);
        return Some(mk(o, n, Some(ConnRef { leaf, kind: 0, index: fi as u32, name: face_key(&f.name), ..Default::default() })));
    }
    // curved face: the circular edge whose axis this face wraps around
    let mut best: Option<(f64, Connector)> = None;
    for (ei, e) in mesh.edges.iter().enumerate() {
        if let Some((c, n, r)) = e.circle {
            let (c, n) = (DVec3::from(c), DVec3::from(n).normalize());
            let v = hit - c;
            let radial = (v - n * v.dot(n)).length();
            let err = (radial - r).abs();
            if err < 0.05 * r.max(1.0) && best.as_ref().map(|b| err < b.0).unwrap_or(true) {
                // put the connector on the axis level with the click
                best = Some((err, mk(c + n * v.dot(n), n, Some(ConnRef { leaf, kind: 1, index: ei as u32, centre: false, size: r, name: 0 }))));
            }
        }
    }
    best.map(|b| b.1).or_else(|| Some(mk(f.center.into(), f.normal.into(), None)))
}

/// Whether `p` (on the plane of planar face `fi`) lies inside that face.
fn face_contains(mesh: &BodyMesh, fi: usize, p: DVec3, n: DVec3) -> bool {
    let v = |i: u32| DVec3::new(mesh.positions[3 * i as usize] as f64, mesh.positions[3 * i as usize + 1] as f64, mesh.positions[3 * i as usize + 2] as f64);
    let tol = 1e-4;
    mesh.indices.chunks(3).zip(&mesh.tri_face).filter(|(_, f)| **f as usize == fi).any(|(t, _)| {
        let (a, b, c) = (v(t[0]), v(t[1]), v(t[2]));
        let q = p - n * (p - a).dot(n);
        [(a, b), (b, c), (c, a)].iter().all(|(u, w)| (*w - *u).cross(q - *u).dot(n) >= -tol * (*w - *u).length())
    })
}

/// The point of planar face `fi` nearest `p` (on the face's plane).
fn nearest_on_face(mesh: &BodyMesh, fi: usize, p: DVec3) -> Option<DVec3> {
    let v = |i: u32| DVec3::new(mesh.positions[3 * i as usize] as f64, mesh.positions[3 * i as usize + 1] as f64, mesh.positions[3 * i as usize + 2] as f64);
    let seg = |a: DVec3, b: DVec3| {
        let d = b - a;
        a + d * ((p - a).dot(d) / d.length_squared().max(1e-18)).clamp(0.0, 1.0)
    };
    mesh.indices
        .chunks(3)
        .zip(&mesh.tri_face)
        .filter(|(_, f)| **f as usize == fi)
        .flat_map(|(t, _)| {
            let (a, b, c) = (v(t[0]), v(t[1]), v(t[2]));
            [seg(a, b), seg(b, c), seg(c, a)]
        })
        .min_by(|x, y| (*x - p).length().total_cmp(&(*y - p).length()))
}

/// Find a connector's geometry on its part (`mesh`, in the part's own coordinates; `o`, `z` too).
/// With no record of what it was on (older files), it is attached to what it sits on, or to the
/// nearest parallel face whose outline holds it. Returns the new origin and the reference.
fn locate(mesh: &BodyMesh, on: Option<ConnRef>, leaf: u32, o: DVec3, z: DVec3) -> Option<(DVec3, ConnRef)> {
    let tol = 1e-3 * (1.0 + o.length() * 1e-3);
    let planes = |want: Option<ConnRef>| -> Option<(DVec3, ConnRef)> {
        // a face with the picked face's persistent name, when it had one and it is still there
        let named = want.filter(|w| w.name != 0).and_then(|w| mesh.faces.iter().position(|f| face_key(&f.name) == w.name));
        // the named face, still facing the same way, holds the connector even when it moved
        // across its plane (a boss moved in its sketch): onto it, at the nearest point if need be,
        // rather than onto whatever other face lies under the old position
        if let Some(k) = named.filter(|&k| mesh.faces[k].planar && DVec3::from(mesh.faces[k].normal).normalize().dot(z) >= 0.999) {
            let n = DVec3::from(mesh.faces[k].normal).normalize();
            let q = o + n * (DVec3::from(mesh.faces[k].center) - o).dot(n);
            let q = if face_contains(mesh, k, q, n) { q } else { nearest_on_face(mesh, k, q)? };
            return Some((q, ConnRef { leaf, kind: 0, index: k as u32, name: want.map(|w| w.name).unwrap_or(0), ..Default::default() }));
        }
        let mut best: Option<(f64, usize, DVec3)> = None;
        for (fi, f) in mesh.faces.iter().enumerate().filter(|(_, f)| f.planar) {
            let n = DVec3::from(f.normal).normalize();
            if n.dot(z) < 0.999 {
                continue;
            }
            let d = (DVec3::from(f.center) - o).dot(n);
            let q = o + n * d;
            if !face_contains(mesh, fi, q, n) {
                continue;
            }
            // the face it was picked on wins (by name, else by index); else the nearest one along the normal
            let picked = match named {
                Some(k) => k == fi,
                None => want.map(|w| w.index) == Some(fi as u32),
            };
            let score = if picked { -1.0 } else { d.abs() };
            if best.map(|b| score < b.0).unwrap_or(true) {
                best = Some((score, fi, q));
            }
        }
        best.map(|(_, fi, q)| (q, ConnRef { leaf, kind: 0, index: fi as u32, name: face_key(&mesh.faces[fi].name), ..Default::default() }))
    };
    let axes = |want: Option<ConnRef>| -> Option<(DVec3, ConnRef)> {
        let mut best: Option<(f64, usize, DVec3, f64, f64)> = None;
        for (ei, e) in mesh.edges.iter().enumerate() {
            let Some((c, n, r)) = e.circle else { continue };
            let (c, n) = (DVec3::from(c), DVec3::from(n).normalize());
            if n.dot(z).abs() < 0.999 {
                continue;
            }
            let v = o - c;
            let off = (v - n * v.dot(n)).length();
            // by where it is and its size: mesh edge indices are renumbered whenever the part's
            // topology changes, so the old index only settles a tie
            let score = match want {
                Some(w) => off + (r - w.size).abs() + if w.centre { v.length() * 1e-3 } else { 0.0 } - if w.index == ei as u32 { 1e-6 } else { 0.0 },
                None => off + v.length() * 1e-3,
            };
            if best.map(|b| score < b.0).unwrap_or(true) {
                best = Some((score, ei, c, r, off));
            }
        }
        let (_, ei, c, r, off) = best?;
        let size = want.map(|w| w.size).unwrap_or(r);
        if off > 2.0 * size.max(r) + 1.0 {
            return None;
        }
        let centre = want.map(|w| w.centre).unwrap_or((o - c).length() < tol);
        let q = if centre { c } else { c + z * (o - c).dot(z) };
        Some((q, ConnRef { leaf, kind: 1, index: ei as u32, centre, size: r, name: 0 }))
    };
    let lines = |want: Option<u32>| -> Option<(DVec3, ConnRef)> {
        let mut best: Option<(f64, usize, DVec3)> = None;
        for (ei, e) in mesh.edges.iter().enumerate().filter(|(_, e)| e.linear && e.pts.len() >= 2) {
            let a = DVec3::from(e.pts[0].map(|v| v as f64));
            let b = DVec3::from(e.pts[e.pts.len() - 1].map(|v| v as f64));
            if (b - a).normalize_or_zero().dot(z).abs() < 0.999 {
                continue;
            }
            let m = DVec3::from(e.mid);
            let score = (m - o).length() - if want == Some(ei as u32) { 1e-6 } else { 0.0 };
            if best.map(|b| score < b.0).unwrap_or(true) {
                best = Some((score, ei, m));
            }
        }
        best.map(|(_, ei, m)| (m, ConnRef { leaf, kind: 2, index: ei as u32, ..Default::default() }))
    };
    match on {
        Some(r) if r.kind == 0 => planes(Some(r)),
        Some(r) if r.kind == 1 => axes(Some(r)),
        Some(r) => lines(Some(r.index)),
        None => {
            // what it sits on now, else the nearest parallel face that holds it
            let exact = |x: Option<(DVec3, ConnRef)>| x.filter(|(q, _)| (*q - o).length() < tol);
            exact(planes(None)).or_else(|| exact(axes(None))).or_else(|| exact(lines(None))).or_else(|| planes(None))
        }
    }
}

/// Move the open assembly's mate connectors onto their parts' current geometry (after a Part
/// Studio rebuild). Returns the mates whose geometry could not be found; their connectors
/// stay where they were. `mesh` gives a Part Studio body's mesh.
pub fn refresh_connectors<'m>(all: &mut [Assembly], ai: usize, mesh: &dyn Fn(usize, BodyId) -> Option<&'m BodyMesh>) -> Vec<u64> {
    let mut lost = vec![];
    let Some(asm) = all.get(ai) else { return lost };
    let mut updates: Vec<(usize, bool, Connector)> = vec![];
    for (mi, m) in asm.mates.iter().enumerate() {
        for (side, c) in [(true, m.a), (false, m.b)] {
            let Some(c) = c else { continue };
            let Some(inst) = asm.instance(c.inst) else { continue };
            // numbered over every part, hidden ones too, like the connectors' leaf numbers
            let leaves = leaves_with(all, inst, true);
            let found = (|| {
                let li = c.on.map(|r| r.leaf as usize).or_else(|| (leaves.len() == 1).then_some(0))?;
                let l = leaves.get(li)?;
                let mesh = mesh(l.studio, l.body)?;
                let (o, z) = (l.pose.inverse_apply(c.origin.into()), l.pose.inverse_dir(c.z.into()).normalize());
                let (q, r) = locate(mesh, c.on, li as u32, o, z)?;
                Some(Connector { origin: l.pose.apply(q).into(), on: Some(r), ..c })
            })();
            match found {
                Some(n) => {
                    if n != c {
                        updates.push((mi, side, n));
                    }
                }
                // older connectors on nothing in particular keep their coordinates
                None if c.on.is_some() => {
                    if !lost.contains(&m.id) {
                        lost.push(m.id);
                    }
                }
                None => {}
            }
        }
    }
    let asm = &mut all[ai];
    for (mi, side, c) in updates {
        if side {
            asm.mates[mi].a = Some(c);
        } else {
            asm.mates[mi].b = Some(c);
        }
    }
    lost
}

/// Which side of a new mate should stay put while it is solved: the instance with more mates
/// (or fixed), else the one inserted first. The other one moves to meet it.
pub fn mate_anchor(asm: &Assembly, mate: u64) -> Vec<u64> {
    let Some(m) = asm.mates.iter().find(|m| m.id == mate) else { return vec![] };
    let (Some(a), Some(b)) = (m.a, m.b) else { return vec![] };
    let weight = |id: u64| {
        let fixed = asm.instance(id).map(|x| x.fixed).unwrap_or(false);
        let mates = asm.mates.iter().filter(|o| o.id != mate && !o.suppressed && (o.a.map(|c| c.inst) == Some(id) || o.b.map(|c| c.inst) == Some(id))).count();
        (fixed, mates)
    };
    let (wa, wb) = (weight(a.inst), weight(b.inst));
    let pos = |id: u64| asm.instances.iter().position(|x| x.id == id).unwrap_or(usize::MAX);
    if wa > wb || (wa == wb && pos(a.inst) < pos(b.inst)) { vec![a.inst] } else { vec![b.inst] }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::FaceInfo;

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
    fn hidden_parts_inside_a_sub_assembly_still_count() {
        // the sub-assembly hides one of its two parts; the BOM walk still counts it, the drawing walk doesn't
        let mut hidden = inst(2, false, [10.0, 0.0, 0.0]);
        hidden.hidden = true;
        let sub = Assembly { name: "sub".into(), instances: vec![inst(1, true, [0.0; 3]), hidden], ..Default::default() };
        let mut top_i = inst(1, true, [0.0; 3]);
        top_i.sub = Some(0);
        let all = vec![sub];
        assert_eq!(leaves(&all, &top_i).len(), 1);
        assert_eq!(leaves_with(&all, &top_i, true).len(), 2);
    }

    #[test]
    fn fastened_brings_faces_together() {
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [30.0, 5.0, 40.0]));
        // top face centre of a 10 mm cube on the first, bottom face of the second
        let ca = Connector { inst: 1, origin: [5.0, 5.0, 10.0], z: [0.0, 0.0, 1.0], x: [1.0, 0.0, 0.0], on: None };
        let cb = Connector { inst: 2, origin: [5.0, 5.0, 0.0], z: [0.0, 0.0, -1.0], x: [1.0, 0.0, 0.0], on: None };
        a.mates.push(Mate { id: 3, name: "m".into(), kind: MateKind::Fastened, a: Some(ca), b: Some(cb), flip: false, offset: 0.0, offset_xy: [0.0; 2], rotation: 0.0, limits: None, suppressed: false });
        let r = solve(&mut a, &[], &|_| DVec3::splat(5.0));
        assert!(r.ok, "{:?}", r.failed);
        let p = a.instances[1].pose;
        assert!((DVec3::from(p.t) - DVec3::new(0.0, 0.0, 10.0)).length() < 1e-5, "{:?}", p.t);
    }

    fn mate(id: u64, kind: MateKind, a: Connector, b: Connector) -> Mate {
        Mate { id, name: format!("m{id}"), kind, a: Some(a), b: Some(b), flip: false, offset: 0.0, offset_xy: [0.0; 2], rotation: 0.0, limits: None, suppressed: false }
    }

    fn conn(inst: u64, o: [f64; 3], z: [f64; 3]) -> Connector {
        let zz = DVec3::from(z).normalize();
        Connector { inst, origin: o, z: zz.into(), x: connector_x(o.into(), zz).into(), on: None }
    }

    #[test]
    fn a_door_dragged_on_past_its_limit_stays_on_its_hinge() {
        // a 500 mm door on a revolute limited to 0..-90°, its free edge pulled round to -150° a
        // frame at a time: it stops at the limit, still on its hinge, and swings back shut
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [0.0; 3]));
        let mut m = mate(3, MateKind::Revolute, conn(1, [0.0; 3], [0.0, 0.0, 1.0]), conn(2, [0.0; 3], [0.0, 0.0, 1.0]));
        m.flip = true;
        m.limits = Some([-90.0, 0.0]);
        a.mates.push(m);
        let ctr = |_: &Instance| DVec3::new(0.0, -250.0, 300.0);
        assert!(solve(&mut a, &[], &ctr).ok);
        let grab = DVec3::new(0.0, -500.0, 300.0);
        let angle = |a: &Assembly| {
            let m = &a.mates[0];
            a.mate_turn(m, a.world(&m.a.unwrap()).unwrap(), a.world(&m.b.unwrap()).unwrap()).to_degrees()
        };
        let pull = |a: &mut Assembly, deg: f64| {
            let t = deg.to_radians();
            let target = DVec3::new(500.0 * t.sin(), -500.0 * t.cos(), 300.0);
            let p = a.instances[1].pose;
            a.instances[1].pose = p.perturbed(DVec3::ZERO, target - p.apply(grab), DVec3::ZERO);
            solve_drag(a, &ctr, 2, grab)
        };
        let mut open = vec![];
        for k in 1..=100 {
            assert!(pull(&mut a, -1.5 * k as f64).ok, "the hinge came apart at step {k}");
            open.push(angle(&a));
        }
        let at = open.last().unwrap().abs();
        assert!((at - 90.0).abs() < 1.0, "stopped at {at}°");
        for k in 1..=100 {
            assert!(pull(&mut a, -150.0 + 1.6 * k as f64).ok, "the hinge came apart closing, step {k}");
        }
        assert!(angle(&a).abs() < 1.0, "shut at {}°", angle(&a));
    }

    #[test]
    fn separate_groups_solve_on_their_own() {
        // three parts on revolutes to a fixed base, two of them also geared together: two groups
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        for k in 0..3u64 {
            a.instances.push(inst(10 + k, false, [k as f64 * 40.0 + 1.5, 0.7, 0.4]));
            a.mates.push(mate(20 + k, MateKind::Revolute, conn(1, [k as f64 * 40.0, 0.0, 0.0], [0.0, 0.0, 1.0]), conn(10 + k, [0.0; 3], [0.0, 0.0, 1.0])));
        }
        a.relations.push(Relation { id: 30, name: "g".into(), kind: RelationKind::Gear, a: 20, b: Some(21), ratio: 1.0, reverse: false, phase: 0.0 });
        let groups = solve_groups(&a, &a.mates, &a.relations);
        let mut sizes: Vec<usize> = groups.iter().map(|g| g.0.len()).collect();
        sizes.sort();
        assert_eq!(sizes, vec![1, 2]);
        assert!(groups.iter().any(|g| g.2.len() == 1 && g.1.len() == 2));
        let r = solve(&mut a, &[], &|_| DVec3::ZERO);
        assert!(r.ok, "{:?}", r.failed);
        for k in 0..3 {
            let (o, _, _) = a.world(&a.mates[k].b.unwrap()).unwrap();
            assert!((o - DVec3::new(k as f64 * 40.0, 0.0, 0.0)).length() < 1e-5, "{o:?}");
        }
    }

    #[test]
    fn fastened_turns_a_part_picked_facing_the_same_way() {
        // top face of both cubes picked: the second has to be turned over to sit face to face
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [30.0, 0.0, 0.0]));
        a.mates.push(mate(3, MateKind::Fastened, conn(1, [5.0, 5.0, 10.0], [0.0, 0.0, 1.0]), conn(2, [5.0, 5.0, 10.0], [0.0, 0.0, 1.0])));
        let r = solve(&mut a, &[], &|_| DVec3::splat(5.0));
        assert!(r.ok, "{:?}", r.failed);
        let (ob, zb, _) = a.world(&a.mates[0].b.unwrap()).unwrap();
        assert!((ob - DVec3::new(5.0, 5.0, 10.0)).length() < 1e-5, "{ob:?}");
        assert!((zb + DVec3::Z).length() < 1e-5, "{zb:?}");
    }

    #[test]
    fn flipping_a_mate_turns_the_part_over_and_back() {
        for kind in [MateKind::Fastened, MateKind::Revolute, MateKind::Slider, MateKind::Cylindrical, MateKind::Planar] {
            let mut a = Assembly::default();
            a.instances.push(inst(1, true, [0.0; 3]));
            a.instances.push(inst(2, false, [0.0, 0.0, 12.0]));
            // a stack of two parts: a third fastened on top of the second follows it over
            a.instances.push(inst(4, false, [0.0, 0.0, 25.0]));
            a.mates.push(mate(3, kind, conn(1, [0.0, 0.0, 10.0], [0.0, 0.0, 1.0]), conn(2, [0.0, 0.0, 0.0], [0.0, 0.0, -1.0])));
            a.mates.push(mate(5, MateKind::Fastened, conn(2, [0.0, 0.0, 10.0], [0.0, 0.0, 1.0]), conn(4, [0.0, 0.0, 0.0], [0.0, 0.0, -1.0])));
            assert!(solve(&mut a, &[], &|_| DVec3::ZERO).ok, "{kind:?}");
            for flip in [true, false, true] {
                a.mates[0].flip = flip;
                let r = solve(&mut a, &[], &|_| DVec3::ZERO);
                assert!(r.ok, "{kind:?} flip {flip}: {:?}", r.failed);
                let (_, za, _) = a.world(&a.mates[0].a.unwrap()).unwrap();
                let (_, zb, _) = a.world(&a.mates[0].b.unwrap()).unwrap();
                assert!((zb - if flip { za } else { -za }).length() < 1e-5, "{kind:?} flip {flip}: {zb:?}");
                // the third part stays on the second
                let (_, z2, _) = a.world(&a.mates[1].a.unwrap()).unwrap();
                let (_, z4, _) = a.world(&a.mates[1].b.unwrap()).unwrap();
                assert!((z2 + z4).length() < 1e-5, "{kind:?} flip {flip}");
            }
        }
    }

    #[test]
    fn fastened_rotation_of_half_a_turn() {
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [0.0, 0.0, 10.0]));
        a.mates.push(mate(3, MateKind::Fastened, conn(1, [0.0, 0.0, 10.0], [0.0, 0.0, 1.0]), conn(2, [0.0, 0.0, 0.0], [0.0, 0.0, -1.0])));
        for deg in [180.0, 90.0, -90.0, 270.0, 0.0] {
            a.mates[0].rotation = deg;
            let r = solve(&mut a, &[], &|_| DVec3::ZERO);
            assert!(r.ok, "{deg}: {:?}", r.failed);
            let m = &a.mates[0];
            let ang = mate_angle(m, a.world(&m.a.unwrap()).unwrap(), a.world(&m.b.unwrap()).unwrap());
            assert!(ang.abs() < 1e-4, "{deg}: {ang}");
        }
    }

    #[test]
    fn flipping_moves_the_free_side_when_the_second_part_is_held() {
        // the second part is fixed: the first turns over instead
        let mut a = Assembly::default();
        a.instances.push(inst(1, false, [0.0, 0.0, 0.0]));
        a.instances.push(inst(2, true, [0.0, 0.0, 10.0]));
        a.mates.push(mate(3, MateKind::Fastened, conn(1, [0.0, 0.0, 10.0], [0.0, 0.0, 1.0]), conn(2, [0.0, 0.0, 0.0], [0.0, 0.0, -1.0])));
        assert!(solve(&mut a, &[], &|_| DVec3::ZERO).ok);
        a.mates[0].flip = true;
        let r = solve(&mut a, &[], &|_| DVec3::ZERO);
        assert!(r.ok, "{:?}", r.failed);
        assert!(DMat3::from_cols_array(&a.instances[1].pose.r).abs_diff_eq(DMat3::IDENTITY, 1e-9), "the fixed part stays");
        let (_, za, _) = a.world(&a.mates[0].a.unwrap()).unwrap();
        assert!((za + DVec3::Z).length() < 1e-5, "{za:?}");
    }

    #[test]
    fn a_conflicting_mate_is_left_unmet_instead_of_pulling_everything_askew() {
        // a chain fixed → 2 (revolute) → 4 (fastened); then 4 fastened to the fixed part somewhere it can't reach
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [0.0, 0.0, 10.0]));
        a.instances.push(inst(4, false, [0.0, 0.0, 30.0]));
        a.mates.push(mate(3, MateKind::Revolute, conn(1, [0.0, 0.0, 10.0], [0.0, 0.0, 1.0]), conn(2, [0.0, 0.0, 0.0], [0.0, 0.0, -1.0])));
        a.mates.push(mate(5, MateKind::Fastened, conn(2, [0.0, 0.0, 20.0], [0.0, 0.0, 1.0]), conn(4, [0.0, 0.0, 0.0], [0.0, 0.0, -1.0])));
        assert!(solve(&mut a, &[], &|_| DVec3::ZERO).ok);
        let good: Vec<Pose> = a.instances.iter().map(|i| i.pose).collect();
        a.mates.push(mate(6, MateKind::Fastened, conn(1, [50.0, 0.0, 0.0], [1.0, 0.0, 0.0]), conn(4, [0.0, 0.0, 0.0], [0.0, 1.0, 0.0])));
        let r = solve(&mut a, &[], &|_| DVec3::ZERO);
        assert_eq!(r.failed, vec![6]);
        for (i, p) in a.instances.iter().zip(&good) {
            assert!(DVec3::from(i.pose.t).abs_diff_eq(p.t.into(), 1e-6), "{} moved", i.id);
        }
    }

    #[test]
    fn a_revolute_far_from_the_part_centre_does_not_swing_it() {
        // a long arm whose hole is 60 mm from its centre slides onto a pin rather than turning
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [20.0, 15.0, 10.0]));
        a.mates.push(mate(3, MateKind::Revolute, conn(1, [0.0, 0.0, 10.0], [0.0, 0.0, 1.0]), conn(2, [0.0, 0.0, 0.0], [0.0, 0.0, -1.0])));
        assert!(solve(&mut a, &[], &|_| DVec3::new(60.0, 0.0, 2.0)).ok);
        let x = DMat3::from_cols_array(&a.instances[1].pose.r).x_axis;
        assert!(x.dot(DVec3::X) > 0.999, "turned to {x:?}");
    }

    #[test]
    fn dragging_a_part_on_a_revolute_swings_it_round() {
        // an arm 60 long on a pin; its far end is pulled round to +Y, a frame at a time
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [0.0, 0.0, 10.0]));
        a.mates.push(mate(3, MateKind::Revolute, conn(1, [0.0, 0.0, 10.0], [0.0, 0.0, 1.0]), conn(2, [0.0, 0.0, 0.0], [0.0, 0.0, -1.0])));
        let ctr = |_: &Instance| DVec3::new(30.0, 0.0, 2.0);
        assert!(solve(&mut a, &[], &ctr).ok);
        let grab = DVec3::new(60.0, 0.0, 5.0);
        let target = DVec3::new(0.0, 60.0, 15.0);
        for _ in 0..40 {
            let p = a.instances[1].pose;
            a.instances[1].pose = p.perturbed(DVec3::ZERO, target - p.apply(grab), DVec3::ZERO);
            assert!(solve_drag(&mut a, &ctr, 2, grab).ok);
        }
        let end = a.instances[1].pose.apply(grab);
        assert!((end - target).length() < 0.5, "{end:?}");
        // and a limit holds it however far it is dragged
        a.mates[0].limits = Some([-30.0, 30.0]);
        for _ in 0..40 {
            let p = a.instances[1].pose;
            a.instances[1].pose = p.perturbed(DVec3::ZERO, DVec3::new(-60.0, 0.0, 15.0) - p.apply(grab), DVec3::ZERO);
            solve_drag(&mut a, &ctr, 2, grab);
        }
        let m = &a.mates[0];
        let ang = a.mate_turn(m, a.world(&m.a.unwrap()).unwrap(), a.world(&m.b.unwrap()).unwrap()).to_degrees();
        assert!((ang.abs() - 30.0).abs() < 1e-3, "{ang}");
    }

    #[test]
    fn a_new_slider_brings_the_faces_together() {
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [0.0, 5.0, 3.0]));
        // the first's front face (−Y at y = 0) and the second's +X side face (x = 20)
        a.mates.push(mate(3, MateKind::Slider, conn(1, [30.0, 0.0, 5.0], [0.0, -1.0, 0.0]), conn(2, [20.0, 15.0, 7.0], [1.0, 0.0, 0.0])));
        place_new_mate(&mut a, 3, &[1]);
        assert!(solve(&mut a, &[1], &|_| DVec3::ZERO).ok);
        let (ob, zb, _) = a.world(&a.mates[0].b.unwrap()).unwrap();
        assert!((ob - DVec3::new(30.0, 0.0, 5.0)).length() < 1e-5, "{ob:?}");
        assert!((zb - DVec3::Y).length() < 1e-5, "{zb:?}");
    }

    #[test]
    fn revolute_keeps_rotation_free() {
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [3.0, 1.0, 7.0]));
        let ca = Connector { inst: 1, origin: [0.0, 0.0, 5.0], z: [0.0, 0.0, 1.0], x: [1.0, 0.0, 0.0], on: None };
        let cb = Connector { inst: 2, origin: [0.0, 0.0, 0.0], z: [0.0, 0.0, -1.0], x: [1.0, 0.0, 0.0], on: None };
        a.mates.push(Mate { id: 3, name: "m".into(), kind: MateKind::Revolute, a: Some(ca), b: Some(cb), flip: false, offset: 0.0, offset_xy: [0.0; 2], rotation: 0.0, limits: None, suppressed: false });
        let r = solve(&mut a, &[], &|_| DVec3::ZERO);
        assert!(r.ok);
        let (ob, zb, _) = a.world(&cb).unwrap();
        assert!((ob - DVec3::new(0.0, 0.0, 5.0)).length() < 1e-5);
        assert!((zb + DVec3::Z).length() < 1e-5);
    }

    /// A 40 × 40 slab `h` thick: face 0 the bottom, face 1 the top.
    fn slab(h: f32) -> BodyMesh {
        let positions = vec![0.0, 0.0, 0.0, 40.0, 0.0, 0.0, 40.0, 40.0, 0.0, 0.0, 40.0, 0.0, 0.0, 0.0, h, 40.0, 0.0, h, 40.0, 40.0, h, 0.0, 40.0, h];
        let faces = vec![
            FaceInfo { center: [20.0, 20.0, 0.0], normal: [0.0, 0.0, -1.0], planar: true, area: 1600.0, name: None },
            FaceInfo { center: [20.0, 20.0, h as f64], normal: [0.0, 0.0, 1.0], planar: true, area: 1600.0, name: None },
        ];
        BodyMesh { positions, indices: vec![0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7], tri_face: vec![0, 0, 1, 1], faces, ..Default::default() }
    }

    #[test]
    fn connectors_follow_a_rebuilt_part() {
        // an arm fastened on the top of an 8 mm plate at a picked point; the plate becomes 20 mm thick
        let thin = slab(8.0);
        let ca = infer_connector(1, 0, &thin, Some(1), None, DVec3::new(30.0, 10.0, 8.0)).unwrap();
        assert!((DVec3::from(ca.origin) - DVec3::new(30.0, 10.0, 8.0)).length() < 1e-9, "picked point, not the face centre");
        let cb = infer_connector(2, 0, &thin, Some(0), None, DVec3::new(30.0, 10.0, 0.0)).unwrap();
        // opposed faces get the same x axis up to sign, so fastening does not spin the part
        assert!(DVec3::from(ca.x).dot(cb.x.into()).abs() > 0.999);
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [0.0, 0.0, 30.0]));
        a.mates.push(Mate { id: 3, name: "m".into(), kind: MateKind::Fastened, a: Some(ca), b: Some(cb), flip: false, offset: 0.0, offset_xy: [0.0; 2], rotation: 0.0, limits: None, suppressed: false });
        assert!(solve(&mut a, &[], &|_| DVec3::ZERO).ok);
        assert!((a.instances[1].pose.t[2] - 8.0).abs() < 1e-5);
        assert!(DMat3::from_cols_array(&a.instances[1].pose.r).abs_diff_eq(DMat3::IDENTITY, 1e-6), "fastened face to face keeps the orientation");
        let thick = slab(20.0);
        let mut all = vec![a];
        let lost = refresh_connectors(&mut all, 0, &|_, _| Some(&thick));
        assert!(lost.is_empty());
        let a = &mut all[0];
        assert!(solve(a, &[], &|_| DVec3::ZERO).ok);
        assert!((a.instances[1].pose.t[2] - 20.0).abs() < 1e-5, "{:?}", a.instances[1].pose.t);
        // a part that lost the face: the mate is reported, not silently kept
        let gone = BodyMesh { faces: vec![thick.faces[0].clone()], tri_face: vec![0, 0], indices: thick.indices[..6].to_vec(), positions: thick.positions.clone(), ..Default::default() };
        let mut all = vec![all.remove(0)];
        assert_eq!(refresh_connectors(&mut all, 0, &|_, _| Some(&gone)), vec![3]);
        // the moving side of a new mate is the less constrained one
        assert_eq!(mate_anchor(&all[0], 3), vec![1]);
    }

    #[test]
    fn hiding_inside_a_sub_assembly_keeps_leaf_numbers() {
        // sub: a thin slab (leaf 0) and a thick one 100 up (leaf 1); the top assembly mates on leaf 1
        let mut low = inst(1, true, [0.0; 3]);
        low.body = (1, 0);
        let mut high = inst(2, false, [0.0, 0.0, 100.0]);
        high.body = (2, 0);
        let mut sub = Assembly { name: "sub".into(), instances: vec![low, high], ..Default::default() };
        let mut top_i = inst(5, true, [0.0; 3]);
        top_i.sub = Some(0);
        let thick = slab(20.0);
        let thin = slab(8.0);
        let mut c = infer_connector(5, 1, &thick, Some(1), None, DVec3::new(30.0, 10.0, 20.0)).unwrap();
        c.origin[2] += 100.0;
        let other = Connector { inst: 6, origin: [0.0; 3], z: [0.0, 0.0, 1.0], x: [1.0, 0.0, 0.0], on: None };
        let mut top = Assembly { name: "top".into(), instances: vec![top_i.clone()], ..Default::default() };
        top.mates.push(Mate { id: 9, name: "m".into(), kind: MateKind::Fastened, a: Some(c), b: Some(other), flip: false, offset: 0.0, offset_xy: [0.0; 2], rotation: 0.0, limits: None, suppressed: false });
        // the thin slab is hidden inside the sub-assembly
        sub.instances[0].hidden = true;
        let mut all = vec![sub, top];
        let shown = leaves_shown(&all, &top_i);
        assert_eq!(shown.iter().map(|(l, s)| (l.body, *s)).collect::<Vec<_>>(), vec![((1, 0), false), ((2, 0), true)]);
        let mesh = |_: usize, b: BodyId| if b == (2, 0) { Some(&thick) } else { Some(&thin) };
        assert!(refresh_connectors(&mut all, 1, &mesh).is_empty(), "the connector still finds its part");
        let after = all[1].mates[0].a.unwrap();
        assert_eq!(after.on.unwrap().leaf, 1);
        assert!((DVec3::from(after.origin) - DVec3::new(30.0, 10.0, 120.0)).length() < 1e-6, "{:?}", after.origin);
    }

    #[test]
    fn connectors_keep_to_their_named_face() {
        // two 40 x 40 faces facing up, one over the other: the connector is on the lower one ("end" of feature 1)
        let level = |z: f32, base: u32| (vec![0.0, 0.0, z, 40.0, 0.0, z, 40.0, 40.0, z, 0.0, 40.0, z], vec![base, base + 1, base + 2, base, base + 2, base + 3]);
        let name = |n: u64| Some(crate::doc::FaceName { feature: 1, role: "end".into(), n });
        let face = |z: f64, n: u64| FaceInfo { center: [20.0, 20.0, z], normal: [0.0, 0.0, 1.0], planar: true, area: 1600.0, name: name(n) };
        let mesh = |faces: [(f32, u64); 2]| {
            let (mut positions, mut indices) = (vec![], vec![]);
            for (k, (z, _)) in faces.iter().enumerate() {
                let (p, i) = level(*z, 4 * k as u32);
                positions.extend(p);
                indices.extend(i);
            }
            BodyMesh { positions, indices, tri_face: vec![0, 0, 1, 1], faces: faces.iter().map(|(z, n)| face(*z as f64, *n)).collect(), ..Default::default() }
        };
        let before = mesh([(8.0, 0), (60.0, 1)]);
        let c = infer_connector(1, 0, &before, Some(0), None, DVec3::new(10.0, 10.0, 8.0)).unwrap();
        assert_ne!(c.on.unwrap().name, 0);
        // rebuilt: the lower face moved up past the old spot of nothing, and the faces came out in the other order
        let after = mesh([(30.0, 1), (45.0, 0)]);
        let (q, r) = locate(&after, c.on, 0, c.origin.into(), c.z.into()).unwrap();
        assert_eq!(r.index, 1, "found by name, not by index or nearness");
        assert!((q.z - 45.0).abs() < 1e-9);
        // the named face moved sideways off the old spot, and another face now lies under it:
        // the connector goes with its face (to the nearest point of it), not onto the other one
        let mut moved = mesh([(8.0, 1), (8.0, 0)]);
        for i in 4..8 {
            moved.positions[3 * i] += 100.0;
        }
        moved.faces[1].center[0] += 100.0;
        let (q, r) = locate(&moved, c.on, 0, c.origin.into(), c.z.into()).unwrap();
        assert_eq!(r.index, 1, "{q}");
        assert!((q - DVec3::new(100.0, 10.0, 8.0)).length() < 1e-6, "{q}");
    }

    #[test]
    fn axis_connectors_go_by_position_not_by_edge_number() {
        let circle = |x: f64| crate::kernel::EdgeInfo { circle: Some(([x, 0.0, 0.0], [0.0, 0.0, 1.0], 3.0)), mid: [x + 3.0, 0.0, 0.0], ..Default::default() };
        let on = ConnRef { leaf: 0, kind: 1, index: 0, centre: true, size: 3.0, name: 0 };
        // edge 0 was the hole at x = 0; after a rebuild edge 0 is the hole at x = 50
        let after = BodyMesh { edges: vec![circle(50.0), circle(0.0)], ..Default::default() };
        let (q, r) = locate(&after, Some(on), 0, DVec3::ZERO, DVec3::Z).unwrap();
        assert_eq!(r.index, 1);
        assert!(q.length() < 1e-9, "{q}");
    }

    fn revolute(id: u64, inst: u64, at: [f64; 3]) -> Mate {
        let ca = Connector { inst: 1, origin: at, z: [0.0, 0.0, 1.0], x: [1.0, 0.0, 0.0], on: None };
        let cb = Connector { inst, origin: [0.0, 0.0, 0.0], z: [0.0, 0.0, -1.0], x: [1.0, 0.0, 0.0], on: None };
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

    #[test]
    fn a_screw_keeps_going_past_half_a_turn_and_limits_reach_past_180() {
        let mut a = Assembly::default();
        a.instances.push(inst(1, true, [0.0; 3]));
        a.instances.push(inst(2, false, [0.0, 0.0, 5.0]));
        let mut m = revolute(10, 2, [0.0, 0.0, 5.0]);
        m.kind = MateKind::Cylindrical;
        a.mates.push(m);
        assert!(solve(&mut a, &[], &|_| DVec3::ZERO).ok);
        let mut rel = Relation { id: 20, name: "s".into(), kind: RelationKind::Screw, a: 10, b: None, ratio: 2.0, reverse: false, phase: 0.0 };
        rel.phase = relation_phase(&a, &rel);
        a.relations.push(rel);
        let slide = |a: &Assembly| {
            let m = &a.mates[0];
            mate_slide(m, a.world(m.a.as_ref().unwrap()).unwrap(), a.world(m.b.as_ref().unwrap()).unwrap())
        };
        let s0 = slide(&a);
        // a turn and a half, a twelfth at a time, as a drag does
        for _ in 0..18 {
            turn(&mut a, 2, 30.0);
            assert!(solve(&mut a, &[2], &|_| DVec3::ZERO).ok);
        }
        let travel = (slide(&a) - s0).abs();
        // (the held part gives back a hair of its turn to the relation; a jump would be a whole pitch)
        assert!((travel - 3.0).abs() < 0.05, "1.5 turns of a 2 mm pitch: {travel}");

        // a revolute limited to -10..270 turns on to 200° (it read as -160° before)
        let mut b = Assembly::default();
        b.instances.push(inst(1, true, [0.0; 3]));
        b.instances.push(inst(2, false, [0.0, 0.0, 5.0]));
        let mut m = revolute(10, 2, [0.0, 0.0, 5.0]);
        m.limits = Some([-10.0, 270.0]);
        b.mates.push(m);
        assert!(solve(&mut b, &[], &|_| DVec3::ZERO).ok);
        for _ in 0..10 {
            turn(&mut b, 2, 20.0);
            assert!(solve(&mut b, &[2], &|_| DVec3::ZERO).ok);
        }
        let t = b.turns[&10].to_degrees().abs();
        assert!((t - 200.0).abs() < 1e-3, "{t}");
    }
}
