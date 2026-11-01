//! Simulation tab model: linear elastic studies of a Part Studio's parts (static, modal and transient),
//! with supports and loads on faces. Units: mm, N, MPa, seconds.

use std::sync::Arc;

use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::doc::{BodyId, Document};
use crate::fea_mesh::{Mesh, Sizing, Surface};
use crate::fea_solve::{Elastic, Fix, Mode, Problem, Profile, TransientResult, TransientSettings};
use crate::kernel::BodyMesh;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Analysis {
    /// Deflection and stress under constant loads.
    #[default]
    Static,
    /// Natural frequencies and mode shapes.
    Modal,
    /// Response over time to loads that change (shock, impact, vibration).
    Transient,
}

impl Analysis {
    pub const ALL: [Analysis; 3] = [Analysis::Static, Analysis::Modal, Analysis::Transient];
    pub fn label(self) -> &'static str {
        match self {
            Analysis::Static => "Static",
            Analysis::Modal => "Modal (vibration)",
            Analysis::Transient => "Transient (dynamic)",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Analysis::Static => "static",
            Analysis::Modal => "modal",
            Analysis::Transient => "transient",
        }
    }
    pub fn from_key(k: &str) -> Option<Analysis> {
        Analysis::ALL.into_iter().find(|a| a.key() == k)
    }
}

/// A picked face of a part: a point on it and its normal there (part coordinates), like `Ref::Face`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct FacePick {
    pub point: [f64; 3],
    pub normal: [f64; 3],
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SupportKind {
    /// The faces cannot move at all.
    #[default]
    Fixed,
    /// The faces slide but cannot move along their normal (also used for symmetry planes).
    Roller,
}

impl SupportKind {
    pub fn label(self) -> &'static str {
        match self {
            SupportKind::Fixed => "Fixed",
            SupportKind::Roller => "Roller",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            SupportKind::Fixed => "feaFixed",
            SupportKind::Roller => "feaRoller",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Support {
    pub id: u64,
    pub name: String,
    pub kind: SupportKind,
    #[serde(default)]
    pub faces: Vec<FacePick>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suppressed: bool,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoadKind {
    /// A total force (N) spread evenly over the faces.
    #[default]
    Force,
    /// A pressure (MPa) pushing on the faces.
    Pressure,
    /// An acceleration of the whole part (in g), such as its own weight.
    Gravity,
}

impl LoadKind {
    pub fn label(self) -> &'static str {
        match self {
            LoadKind::Force => "Force",
            LoadKind::Pressure => "Pressure",
            LoadKind::Gravity => "Gravity",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            LoadKind::Force => "feaForce",
            LoadKind::Pressure => "feaPressure",
            LoadKind::Gravity => "feaGravity",
        }
    }
    pub fn unit(self) -> &'static str {
        match self {
            LoadKind::Force => " N",
            LoadKind::Pressure => " MPa",
            LoadKind::Gravity => " g",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Load {
    pub id: u64,
    pub name: String,
    pub kind: LoadKind,
    #[serde(default)]
    pub faces: Vec<FacePick>,
    /// N, MPa or g.
    pub value: f64,
    /// Direction of a force or acceleration (part coordinates); ignored when `normal` is set.
    #[serde(default = "down")]
    pub direction: [f64; 3],
    /// A force pushes into the faces along their normals rather than along `direction`.
    #[serde(default)]
    pub normal: bool,
    /// Only the part of the faces within `radius` of this point (part coordinates), e.g. an impact at the
    /// centre of a face.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suppressed: bool,
}

fn down() -> [f64; 3] {
    [0.0, 0.0, -1.0]
}

impl Default for Load {
    fn default() -> Self {
        Load { id: 0, name: String::new(), kind: LoadKind::Force, faces: vec![], value: 100.0, direction: down(), normal: false, at: None, radius: None, suppressed: false }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    /// Applied suddenly and held.
    #[default]
    Step,
    /// Rises steadily over `time`, then held.
    Ramp,
    /// A half-sine pulse lasting `time` (an impact).
    Pulse,
    /// Shakes at `frequency`.
    Sine,
}

impl ProfileKind {
    pub const ALL: [ProfileKind; 4] = [ProfileKind::Step, ProfileKind::Ramp, ProfileKind::Pulse, ProfileKind::Sine];
    pub fn label(self) -> &'static str {
        match self {
            ProfileKind::Step => "Step (sudden)",
            ProfileKind::Ramp => "Ramp",
            ProfileKind::Pulse => "Pulse (impact)",
            ProfileKind::Sine => "Sine (shaking)",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            ProfileKind::Step => "step",
            ProfileKind::Ramp => "ramp",
            ProfileKind::Pulse => "pulse",
            ProfileKind::Sine => "sine",
        }
    }
}

/// A smaller element size on some faces, such as a fillet where the peak stress is.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MeshControl {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub faces: Vec<FacePick>,
    /// Element size on the faces (mm); it grows back to the study's size away from them.
    pub size: f64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suppressed: bool,
}

/// Settings of a transient analysis.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Transient {
    /// How the loads change over time.
    #[serde(default)]
    pub profile: ProfileKind,
    /// Ramp or pulse time (s).
    #[serde(default = "pulse_time")]
    pub time: f64,
    /// Shaking frequency (Hz).
    #[serde(default = "shake_hz")]
    pub frequency: f64,
    /// Simulated time (s); None: ten periods of the lowest natural frequency.
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default = "steps")]
    pub steps: usize,
    /// Fraction of critical damping.
    #[serde(default = "damping")]
    pub damping: f64,
}

fn pulse_time() -> f64 {
    0.001
}
fn shake_hz() -> f64 {
    100.0
}
fn steps() -> usize {
    400
}
fn damping() -> f64 {
    0.02
}

impl Default for Transient {
    fn default() -> Self {
        Transient { profile: ProfileKind::Step, time: pulse_time(), frequency: shake_hz(), duration: None, steps: steps(), damping: damping() }
    }
}

impl Transient {
    pub fn profile(&self) -> Profile {
        match self.profile {
            ProfileKind::Step => Profile::Step,
            ProfileKind::Ramp => Profile::Ramp(self.time),
            ProfileKind::Pulse => Profile::HalfSine(self.time),
            ProfileKind::Sine => Profile::Sine(self.frequency),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SimMode {
    /// Supports and loads, solved by the app (static, modal and transient).
    #[default]
    Simple,
    /// crackh3d's boundary conditions and solver, and periodic BCs (static only).
    Advanced,
}

/// A Simulation tab.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Study {
    pub name: String,
    pub studio: usize,
    /// Parts to analyse (empty: every visible part). Parts that touch are bonded together.
    #[serde(default)]
    pub bodies: Vec<BodyId>,
    #[serde(default)]
    pub analysis: Analysis,
    #[serde(default, skip_serializing_if = "is_simple")]
    pub mode: SimMode,
    /// The boundary conditions of Advanced mode.
    #[serde(default, skip_serializing_if = "is_default")]
    pub advanced: crate::fea_adv::Advanced,
    /// Material name from `MATERIALS`; empty: the parts' own material (steel when none is set).
    #[serde(default)]
    pub material: String,
    /// Element size (mm); None: worked out from the part size.
    #[serde(default)]
    pub mesh_size: Option<f64>,
    /// Refine the mesh by itself at holes and fillets.
    #[serde(default = "yes")]
    pub refine: bool,
    /// Smaller element sizes on chosen faces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mesh_controls: Vec<MeshControl>,
    #[serde(default)]
    pub supports: Vec<Support>,
    #[serde(default)]
    pub loads: Vec<Load>,
    /// Number of modes for a modal analysis.
    #[serde(default = "modes")]
    pub modes: usize,
    #[serde(default)]
    pub transient: Transient,
    /// Loss factors (η) by material name, replacing the library's values.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub loss_factors: std::collections::BTreeMap<String, f64>,
    #[serde(default)]
    pub next_id: u64,
}

fn modes() -> usize {
    6
}

fn yes() -> bool {
    true
}

fn is_simple(m: &SimMode) -> bool {
    *m == SimMode::Simple
}

fn is_default(a: &crate::fea_adv::Advanced) -> bool {
    *a == crate::fea_adv::Advanced::default()
}

impl Default for Study {
    fn default() -> Self {
        Study {
            name: "Simulation 1".into(),
            studio: 0,
            bodies: vec![],
            analysis: Analysis::Static,
            mode: SimMode::Simple,
            advanced: Default::default(),
            material: String::new(),
            mesh_size: None,
            refine: true,
            mesh_controls: vec![],
            supports: vec![],
            loads: vec![],
            modes: modes(),
            transient: Transient::default(),
            loss_factors: Default::default(),
            next_id: 1,
        }
    }
}

impl Study {
    fn take_id(&mut self) -> u64 {
        self.next_id =
            self.next_id.max(1 + self.supports.iter().map(|s| s.id).chain(self.loads.iter().map(|l| l.id)).chain(self.mesh_controls.iter().map(|c| c.id)).chain(self.advanced.ids()).max().unwrap_or(0));
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn add_support(&mut self, kind: SupportKind) -> u64 {
        let id = self.take_id();
        let n = self.supports.iter().filter(|s| s.kind == kind).count() + 1;
        self.supports.push(Support { id, name: format!("{} {n}", kind.label()), kind, faces: vec![], suppressed: false });
        id
    }

    pub fn add_load(&mut self, kind: LoadKind) -> u64 {
        let id = self.take_id();
        let n = self.loads.iter().filter(|s| s.kind == kind).count() + 1;
        let value = match kind {
            LoadKind::Force => 100.0,
            LoadKind::Pressure => 0.1,
            LoadKind::Gravity => 1.0,
        };
        self.loads.push(Load { id, name: format!("{} {n}", kind.label()), kind, value, normal: kind == LoadKind::Pressure, ..Default::default() });
        id
    }

    /// Add an advanced boundary condition of a kind (`fea_adv::BcType::KINDS`).
    pub fn add_adv_bc(&mut self, kind: &str) -> Option<u64> {
        let bc = crate::fea_adv::BcType::new(kind)?;
        let id = self.take_id();
        let n = self.advanced.bcs.iter().filter(|b| b.bc.key() == kind).count() + 1;
        let name = format!("{} {n}", crate::fea_adv::BcType::label_of(kind).split(" (").next().unwrap_or(kind));
        self.advanced.bcs.push(crate::fea_adv::AdvBc { id, name, bc, faces: vec![], suppressed: false });
        Some(id)
    }

    /// Add a periodic pair (with no faces yet).
    pub fn add_periodic(&mut self) -> u64 {
        let id = self.take_id();
        let n = self.advanced.periodic.len() + 1;
        self.advanced.periodic.push(crate::fea_adv::Periodic { id, name: format!("Periodic {n}"), ..Default::default() });
        id
    }

    /// Add a mesh control of `size` mm (with no faces yet).
    pub fn add_mesh_control(&mut self, size: f64) -> u64 {
        let id = self.take_id();
        let n = self.mesh_controls.len() + 1;
        self.mesh_controls.push(MeshControl { id, name: format!("Mesh control {n}"), faces: vec![], size, suppressed: false });
        id
    }

    pub fn mesh_control_mut(&mut self, id: u64) -> Option<&mut MeshControl> {
        self.mesh_controls.iter_mut().find(|s| s.id == id)
    }

    /// The element size the study meshes with: its own, or one worked out from the parts.
    pub fn element_size(&self, input: &Input) -> f64 {
        self.mesh_size.unwrap_or_else(|| input.auto_size(self.refine))
    }

    pub fn support_mut(&mut self, id: u64) -> Option<&mut Support> {
        self.supports.iter_mut().find(|s| s.id == id)
    }

    pub fn load_mut(&mut self, id: u64) -> Option<&mut Load> {
        self.loads.iter_mut().find(|s| s.id == id)
    }

    /// Faces of a support or load.
    pub fn faces_mut(&mut self, id: u64) -> Option<&mut Vec<FacePick>> {
        if let Some(s) = self.supports.iter_mut().find(|s| s.id == id) {
            return Some(&mut s.faces);
        }
        if let Some(c) = self.mesh_controls.iter_mut().find(|s| s.id == id) {
            return Some(&mut c.faces);
        }
        if let Some(c) = self.advanced.bcs.iter_mut().find(|s| s.id == id) {
            return Some(&mut c.faces);
        }
        self.loads.iter_mut().find(|s| s.id == id).map(|l| &mut l.faces)
    }

    pub fn faces(&self, id: u64) -> Option<&Vec<FacePick>> {
        self.supports
            .iter()
            .find(|s| s.id == id)
            .map(|s| &s.faces)
            .or_else(|| self.loads.iter().find(|s| s.id == id).map(|l| &l.faces))
            .or_else(|| self.mesh_controls.iter().find(|s| s.id == id).map(|c| &c.faces))
            .or_else(|| self.advanced.bcs.iter().find(|s| s.id == id).map(|c| &c.faces))
    }
}

/// (name, Young's modulus MPa, Poisson's ratio, density g/cm³, yield strength MPa). Names match the
/// Part Studio's materials; woods are treated as isotropic along the grain.
pub const MATERIALS: &[(&str, f64, f64, f64, f64)] = &[
    ("Steel", 200000.0, 0.30, 7.85, 250.0),
    ("Stainless steel", 193000.0, 0.29, 8.00, 215.0),
    ("Aluminium 6061", 68900.0, 0.33, 2.70, 276.0),
    ("Aluminium 7075", 71700.0, 0.33, 2.81, 503.0),
    ("Stainless steel 303", 193000.0, 0.29, 8.03, 240.0),
    ("Tungsten alloy", 330000.0, 0.28, 17.6, 650.0),
    ("Urethane foam", 60.0, 0.30, 0.24, 2.0),
    ("Brass", 100000.0, 0.34, 8.50, 200.0),
    ("Copper", 117000.0, 0.34, 8.96, 70.0),
    ("Titanium", 113800.0, 0.34, 4.43, 880.0),
    ("PLA", 3500.0, 0.36, 1.24, 50.0),
    ("ABS", 2200.0, 0.35, 1.04, 40.0),
    ("PETG", 2100.0, 0.38, 1.27, 50.0),
    ("Nylon", 2700.0, 0.39, 1.14, 70.0),
    ("Oak", 11000.0, 0.35, 0.75, 50.0),
    ("Pine", 9000.0, 0.35, 0.50, 40.0),
];

pub fn elastic(name: &str) -> Option<Elastic> {
    MATERIALS.iter().find(|m| m.0.eq_ignore_ascii_case(name)).map(|m| Elastic { e: m.1, nu: m.2, rho: m.3 * 1e-9, yield_mpa: m.4, eta: loss_factor(m.0) })
}

/// Typical loss factor (structural damping η) of a library material at room temperature.
pub fn loss_factor(name: &str) -> f64 {
    match name {
        "Aluminium 6061" | "Aluminium 7075" => 0.0003,
        "Steel" | "Stainless steel" | "Stainless steel 303" | "Titanium" | "Tungsten alloy" => 0.0005,
        "Brass" => 0.001,
        "Copper" => 0.002,
        "PLA" | "ABS" | "PETG" => 0.02,
        "Nylon" => 0.03,
        "Oak" | "Pine" => 0.01,
        "Urethane foam" => 0.1,
        _ => 0.0,
    }
}

/// Standard gravity in mm/s².
pub const G: f64 = 9806.65;

// ---------------------------------------------------------------- running a study

/// The parts of a study as one triangulated surface, with CAD faces numbered across all parts.
#[derive(Clone, Default)]
pub struct Input {
    pub tris: Vec<[DVec3; 3]>,
    /// Global face of each triangle.
    pub face: Vec<u32>,
    pub body: Vec<u32>,
    /// Material of the first part that has one.
    pub part_material: Option<String>,
    pub bodies: Vec<BodyId>,
    /// Each part's own material (same order as `bodies`).
    pub materials: Vec<Option<String>>,
    /// Smallest radius of the parts' circular edges (holes, fillets).
    pub min_radius: Option<f64>,
    /// The plane of each global face that is planar, as its CAD normal and a point on it (None: curved).
    pub planar: Vec<Option<(DVec3, DVec3)>>,
}

/// Triangles of the parts a study analyses (all visible parts when none are chosen).
pub fn input(bodies: &[BodyMesh], doc: Option<&Document>, sel: &[BodyId]) -> Input {
    let mut out = Input::default();
    let mut face_base = 0u32;
    for b in bodies {
        let props = doc.and_then(|d| d.parts.get(&crate::doc::body_key(b.id)));
        let hidden = props.map(|p| p.hidden).unwrap_or(false);
        if if sel.is_empty() { hidden || b.surface } else { !sel.contains(&b.id) } {
            continue;
        }
        if out.part_material.is_none() {
            out.part_material = props.and_then(|p| p.material.clone());
        }
        let bi = out.bodies.len() as u32;
        out.bodies.push(b.id);
        out.materials.push(props.and_then(|p| p.material.clone()));
        let p = |i: u32| DVec3::new(b.positions[3 * i as usize] as f64, b.positions[3 * i as usize + 1] as f64, b.positions[3 * i as usize + 2] as f64);
        for (ti, t) in b.indices.chunks(3).enumerate() {
            out.tris.push([p(t[0]), p(t[1]), p(t[2])]);
            out.face.push(face_base + b.tri_face.get(ti).copied().unwrap_or(0));
            out.body.push(bi);
        }
        face_base += b.faces.len().max(1) as u32;
        out.planar.extend(b.faces.iter().map(|f| Some((DVec3::from(f.normal).normalize_or_zero(), DVec3::from(f.center))).filter(|p| f.planar && p.0 != DVec3::ZERO)));
        if b.faces.is_empty() {
            out.planar.push(None);
        }
        for e in &b.edges {
            if let Some((_, _, r)) = e.circle {
                out.min_radius = Some(out.min_radius.map_or(r, |m: f64| m.min(r)));
            }
        }
    }
    out
}

impl Input {
    pub fn bounds(&self) -> (DVec3, DVec3) {
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for t in &self.tris {
            for p in t {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        (lo, hi)
    }

    /// The face a pick lies on: the closest triangle, preferring ones facing the same way.
    pub fn resolve(&self, f: &FacePick) -> Option<u32> {
        let p = DVec3::from(f.point);
        let n = DVec3::from(f.normal);
        let (lo, hi) = self.bounds();
        let tol = (hi - lo).length() * 1e-3 + 1e-3;
        let mut best: Option<(f64, u32)> = None;
        for (t, face) in self.tris.iter().zip(&self.face) {
            let d = crate::fea_mesh::closest_on_tri(p, t[0], t[1], t[2]).distance(p);
            if d > tol {
                continue;
            }
            let tn = (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
            let score = d + if tn.dot(n) > 0.5 { 0.0 } else { tol };
            if best.map(|b| score < b.0).unwrap_or(true) {
                best = Some((score, *face));
            }
        }
        best.map(|b| b.1)
    }

    /// The element size the mesher uses for `size`: no smaller than 1/400 of the parts' largest side.
    pub fn used_size(&self, size: f64) -> f64 {
        let (lo, hi) = self.bounds();
        size.max((hi - lo).max_element() / 400.0).max(1e-6)
    }

    /// Rough node count of a mesh with elements `size` across (some 24 nodes of 10-node tetrahedra per
    /// cube of that size), before the refinement near holes and fillets.
    pub fn estimate_nodes(&self, size: f64) -> f64 {
        let vol: f64 = self.tris.iter().map(|t| t[0].dot(t[1].cross(t[2])) / 6.0).sum::<f64>().abs();
        24.0 * vol / self.used_size(size).powi(3)
    }

    /// Element size from the parts' volume, small enough for a quick solve. Unless the mesh is `refine`d
    /// at holes and fillets, it also shrinks for them.
    pub fn auto_size(&self, refine: bool) -> f64 {
        let vol: f64 = self.tris.iter().map(|t| t[0].dot(t[1].cross(t[2])) / 6.0).sum::<f64>().abs();
        let (lo, hi) = self.bounds();
        let ext = hi - lo;
        // ~1200 lattice cells inside the parts (some 15 000 elements), but at least 3 across the thinnest box side
        let h = (vol.max(1e-9) / 1200.0).cbrt();
        let h = h.min(ext.min_element().max(1e-6) / 3.0);
        // holes and fillets want a few elements around them
        let h = match self.min_radius.filter(|_| !refine) {
            Some(r) => h.min(r / 2.0).max(ext.max_element() / 60.0).min(h),
            None => h,
        };
        let h = h.max(ext.max_element() / 200.0);
        // keep the solve in memory: about 88 V / h³ unknowns, and 600 000 take some 8 GB
        let h = h.max((88.0 * vol / 600_000.0).cbrt());
        nice(h)
    }
}

/// Round a size to 1, 1.2, 1.5, 2, 2.5, 3, 4, 5, 6, 8 × 10ⁿ.
pub fn nice(h: f64) -> f64 {
    if !(h > 0.0) {
        return 1.0;
    }
    let e = 10f64.powf(h.log10().floor());
    let m = h / e;
    let s = [1.0, 1.2, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0].into_iter().min_by(|a, b| (a - m).abs().total_cmp(&(b - m).abs())).unwrap();
    s * e
}

pub enum Solution {
    Static {
        u: Vec<f64>,
        stress: Vec<[f32; 6]>,
        /// Reaction force of each support (id).
        reactions: Vec<(u64, DVec3)>,
    },
    Modal {
        modes: Vec<Mode>,
    },
    Transient(TransientResult),
}

/// A solved study.
pub struct Outcome {
    pub mesh: Arc<Mesh>,
    pub material: String,
    pub mat: Elastic,
    /// Each part's material when they differ (`mesh.body` numbering); empty: `mat` everywhere.
    pub body_mats: Vec<Elastic>,
    pub solution: Solution,
    pub warnings: Vec<String>,
    pub mass: f64,
    /// Transient: the largest von Mises stress (MPa) over the kept frames and its time (s).
    pub peak_stress: Option<(f64, f64)>,
}

impl Outcome {
    /// Largest displacement magnitude (static: the result; modal: 1 per mode; transient: over all frames).
    pub fn max_disp(&self) -> f64 {
        match &self.solution {
            Solution::Static { u, .. } => max_mag(u),
            Solution::Modal { .. } => 1.0,
            Solution::Transient(t) => t.max_disp.iter().copied().fold(0.0, f64::max),
        }
    }
}

pub fn max_mag(u: &[f64]) -> f64 {
    u.chunks(3).map(|c| (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt()).fold(0.0, f64::max)
}

pub fn material_of(study: &Study, input: &Input) -> (String, Elastic) {
    let name = if !study.material.is_empty() { study.material.clone() } else { input.part_material.clone().filter(|m| elastic(m).is_some()).unwrap_or_else(|| "Steel".into()) };
    (name.clone(), material_named(study, &name))
}

fn material_named(study: &Study, name: &str) -> Elastic {
    let mut m = elastic(name).unwrap_or_else(|| elastic("Steel").unwrap());
    if let Some(eta) = study.loss_factors.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|x| *x.1) {
        m.eta = eta.max(0.0);
    }
    m
}

/// Each part's material: the study's when it sets one, else the part's own, else the first part's
/// (steel when none is set).
pub fn part_materials(study: &Study, input: &Input) -> Vec<(String, Elastic)> {
    let (fallback, _) = material_of(study, input);
    input
        .materials
        .iter()
        .map(|m| {
            let name = if !study.material.is_empty() { study.material.clone() } else { m.clone().filter(|m| elastic(m).is_some()).unwrap_or_else(|| fallback.clone()) };
            let mat = material_named(study, &name);
            (name, mat)
        })
        .collect()
}

/// Whether the parts have different materials, so they are meshed one by one and bonded where they touch.
pub fn mixed(study: &Study, input: &Input) -> bool {
    let m = part_materials(study, input);
    m.iter().any(|x| x.0 != m[0].0)
}

/// Element size wanted on a hole or fillet of radius `r`: about six elements across a hole resolve its
/// peak stress to within a few percent.
pub fn hole_size(r: f64) -> f64 {
    r / 3.0
}

/// Where the study's mesh is finer than `size`: at holes and fillets (when it refines by itself) and on
/// the faces of its mesh controls.
pub fn sizing(study: &Study, input: &Input, size: f64) -> Sizing {
    let mut out = Sizing::default();
    if study.refine {
        let mut radii: Vec<(u32, f64)> = crate::fea_mesh::face_radii(&input.tris, &input.face).into_iter().collect();
        radii.sort_unstable_by_key(|x| x.0);
        for (f, r) in radii {
            // no finer than an eighth of the element size, so a tiny fillet doesn't swamp the mesh
            let s = hole_size(r).max(size / 8.0);
            if s < 0.8 * size {
                out.add_tris(input.tris.iter().zip(&input.face).filter(|x| *x.1 == f).map(|x| x.0), s);
            }
        }
    }
    for c in study.mesh_controls.iter().filter(|c| !c.suppressed && c.size > 0.0 && c.size < size) {
        let ids: Vec<u32> = c.faces.iter().filter_map(|p| input.resolve(p)).collect();
        out.add_tris(input.tris.iter().zip(&input.face).filter(|x| ids.contains(x.1)).map(|x| x.0), c.size);
    }
    out
}

/// Rough node count of the study's mesh with elements `size` across, refinement included.
pub fn estimate_nodes(study: &Study, input: &Input, size: f64) -> f64 {
    let h = input.used_size(size);
    let mut n = input.estimate_nodes(size);
    // each refined face adds a layer whose element size grows from s to h at GRADING per mm: about
    // ∫ A / (s + g d)³ dd = A / 2g (1/s² - 1/h²) cubes' worth. Bisected tets are less regular than the
    // lattice's and the grading spreads past the face's edges, so measured meshes come out about 70 nodes
    // per such cube rather than 24.
    for (area, s) in sizing(study, input, size).patches {
        let s = s.max(h / 16.0);
        n += 70.0 * area / (2.0 * crate::fea_mesh::GRADING) * (1.0 / (s * s) - 1.0 / (h * h));
    }
    n
}

/// Mesh the study's parts: as one when they share a material, part by part (bonded) when they don't.
pub fn mesh_input(study: &Study, input: &Input, size: f64, cancel: &dyn Fn() -> bool) -> Result<Mesh, String> {
    let surf = Surface::new(input.tris.clone(), input.face.clone(), input.body.clone());
    let sz = sizing(study, input, size);
    if mixed(study, input) { crate::fea_mesh::mesh_parts(&surf, size, &sz, cancel) } else { crate::fea_mesh::mesh(&surf, size, &sz, cancel) }
}

/// Mesh the parts (or reuse `mesh`) and solve the study.
pub fn run(study: &Study, input: &Input, mesh: Option<Arc<Mesh>>, progress: &dyn Fn(&str, f32), cancel: &dyn Fn() -> bool) -> Result<Outcome, String> {
    if input.tris.is_empty() {
        return Err("No parts to analyse".into());
    }
    let size = study.element_size(input);
    let mesh = match mesh {
        Some(m) => m,
        None => {
            progress("Meshing", 0.0);
            Arc::new(mesh_input(study, input, size, cancel)?)
        }
    };
    let mut warnings = vec![];
    if mesh.size > size * 1.001 {
        warnings.push(format!("Element size {} mm is too small for parts this big: used {} mm", crate::expr::trim(size, 4), crate::expr::trim(mesh.size, 4)));
    }
    if study.mode == SimMode::Advanced {
        if let Some(w) = hole_warning(input, &mesh) {
            warnings.push(w);
        }
        return crate::fea_adv::run(study, input, mesh, progress, &mut warnings);
    }
    let (mut material, mat) = material_of(study, input);
    let parts = part_materials(study, input);
    let body_mats: Vec<Elastic> = if mesh.body.is_empty() { vec![] } else { parts.iter().map(|p| p.1).collect() };
    if !body_mats.is_empty() {
        let mut names: Vec<String> = parts.iter().map(|p| p.0.clone()).collect();
        names.dedup();
        material = names.join(" + ");
    }
    let nn = mesh.nodes.len();
    // mesh faces of a list of picks, and the area of the CAD faces they stand for
    let faces_of = |picks: &[FacePick], what: &str, warnings: &mut Vec<String>| -> (Vec<usize>, f64) {
        let mut ids = vec![];
        for p in picks {
            match input.resolve(p) {
                Some(f) => ids.push(f),
                None => warnings.push(format!("{what}: a face is no longer on the parts")),
            }
        }
        let cad: f64 = input.tris.iter().zip(&input.face).filter(|(_, f)| ids.contains(f)).map(|(t, _)| (t[1] - t[0]).cross(t[2] - t[0]).length() / 2.0).sum();
        ((0..mesh.faces.len()).filter(|i| ids.contains(&mesh.faces[*i].1)).collect::<Vec<usize>>(), cad)
    };
    // a support or load on faces the mesh barely resolves (small holes) acts on a few nodes: the stress
    // next to it is an artefact, often orders of magnitude too high
    let coarse = |what: &str, fs: &[usize], cad: Option<f64>, warnings: &mut Vec<String>| {
        let area = crate::fea_solve::area_of(&mesh, fs);
        let share = cad.filter(|c| *c > 0.0).map(|c| area / c);
        if !fs.is_empty() && (fs.len() < 6 || share.is_some_and(|k| k < 0.5)) {
            let covered = share.map(|k| format!("{:.0}% of its faces, ", 100.0 * k.min(1.0))).unwrap_or_default();
            warnings.push(format!(
                "{what}: the mesh covers {covered}{} triangles, so it acts on a few nodes and the stress next to it is unreliable: use a smaller mesh size (now {size} mm) or bigger faces",
                fs.len()
            ));
        }
    };
    let (blo, bhi) = input.bounds();
    // how far a node may lie off a planar face and still be on it (the mesher's cut points sit a hair off)
    let on_plane = (bhi - blo).length() * 1e-4;
    let mut fixes = vec![];
    // (support id, Fixed?, the nodes it holds with the direction each is held along (Fixed: none))
    let mut support_nodes: Vec<(u64, Vec<(u32, Option<DVec3>)>)> = vec![];
    let mut unheld = vec![];
    for s in study.supports.iter().filter(|s| !s.suppressed) {
        let (fs, cad) = faces_of(&s.faces, &s.name, &mut warnings);
        if fs.is_empty() {
            warnings.push(format!("{} has no faces", s.name));
            unheld.push(s.name.clone());
            continue;
        }
        coarse(&s.name, &fs, Some(cad), &mut warnings);
        let mut nodes: Vec<u32> = fs.iter().flat_map(|f| mesh.faces[*f].0).collect();
        nodes.sort_unstable();
        nodes.dedup();
        match s.kind {
            SupportKind::Fixed => {
                fixes.push(Fix::Fixed(nodes.clone()));
                support_nodes.push((s.id, nodes.iter().map(|v| (*v, None)).collect()));
            }
            SupportKind::Roller => {
                // a node on a planar face slides in that face's plane (the CAD normal, not the facets', which
                // tilt next to holes and fillets); a node only on curved faces slides along its averaged
                // facet normal
                let mut planes: Vec<Vec<DVec3>> = vec![vec![]; nn];
                let mut curved = vec![DVec3::ZERO; nn];
                for f in &fs {
                    let (t, cad) = &mesh.faces[*f];
                    match input.planar.get(*cad as usize).copied().flatten() {
                        Some((n, at)) => {
                            for v in t {
                                let ps = &mut planes[*v as usize];
                                if (mesh.nodes[*v as usize] - at).dot(n).abs() <= on_plane && !ps.iter().any(|q| q.dot(n).abs() > 1.0 - 1e-6) {
                                    ps.push(n);
                                }
                            }
                        }
                        None => {
                            let p = |i: usize| mesh.nodes[t[i] as usize];
                            let n = (p(1) - p(0)).cross(p(2) - p(0));
                            for v in t {
                                curved[*v as usize] += n;
                            }
                        }
                    }
                }
                let snap = |d: DVec3| {
                    let d = d.normalize_or_zero();
                    match (0..3).find(|&a| d[a].abs() > 1.0 - 1e-6) {
                        Some(a) => DVec3::from_array(std::array::from_fn(|k| if k == a { d[a].signum() } else { 0.0 })),
                        None => d,
                    }
                };
                let mut held: Vec<(u32, DVec3)> = vec![];
                for v in &nodes {
                    let ps = &planes[*v as usize];
                    if !ps.is_empty() {
                        held.extend(ps.iter().map(|n| (*v, snap(*n))));
                    } else if curved[*v as usize] != DVec3::ZERO && !fs.iter().any(|f| input.planar.get(mesh.faces[*f].1 as usize).copied().flatten().is_some() && mesh.faces[*f].0.contains(v)) {
                        held.push((*v, snap(curved[*v as usize])));
                    }
                }
                support_nodes.push((s.id, held.iter().map(|(v, n)| (*v, Some(*n))).collect()));
                fixes.push(Fix::Roller(held));
            }
        }
    }
    let mut force = vec![0.0; 3 * nn];
    let mut any_load = false;
    let mut unloaded = vec![];
    for l in study.loads.iter().filter(|l| !l.suppressed) {
        let dir = DVec3::from(l.direction).normalize_or_zero();
        match l.kind {
            LoadKind::Gravity => {
                crate::fea_solve::body_loads(&mesh, dir * (l.value * G), |ei| crate::fea_solve::mat_at(&mesh, &body_mats, &mat, ei).rho, &mut force);
                any_load = true;
            }
            LoadKind::Force | LoadKind::Pressure => {
                let (mut fs, cad) = faces_of(&l.faces, &l.name, &mut warnings);
                let mut cad = Some(cad);
                if let (Some(at), Some(r)) = (l.at, l.radius) {
                    cad = None;
                    let at = DVec3::from(at);
                    fs.retain(|f| {
                        let t = &mesh.faces[*f].0;
                        ((mesh.nodes[t[0] as usize] + mesh.nodes[t[1] as usize] + mesh.nodes[t[2] as usize]) / 3.0).distance(at) <= r
                    });
                }
                if fs.is_empty() {
                    warnings.push(format!("{} has no faces", l.name));
                    unloaded.push(l.name.clone());
                } else {
                    coarse(&l.name, &fs, cad, &mut warnings);
                    let area = crate::fea_solve::area_of(&mesh, &fs);
                    let (v, normal) = (l.value, l.normal || l.kind == LoadKind::Pressure);
                    let per_area = if l.kind == LoadKind::Force { v / area.max(1e-12) } else { v };
                    crate::fea_solve::traction_loads(&mesh, &fs, |n| if normal { -n * per_area } else { dir * per_area }, &mut force);
                    any_load = true;
                }
            }
        }
    }
    // a support or load whose faces are gone says so, rather than asking for one to be added
    let no_faces = |names: &[String]| format!("{} {} no faces on the parts: pick {} faces again", names.join(", "), if names.len() == 1 { "has" } else { "have" }, if names.len() == 1 { "its" } else { "their" });
    let held = !fixes.is_empty();
    if study.analysis == Analysis::Static {
        if !held {
            return Err(if unheld.is_empty() { "Add a support (Fixed or Roller) so the part is held".into() } else { no_faces(&unheld) });
        }
    }
    if study.analysis != Analysis::Modal && !any_load {
        return Err(if unloaded.is_empty() { "Add a load (Force, Pressure or Gravity)".into() } else { no_faces(&unloaded) });
    }
    if study.analysis == Analysis::Modal && study.loads.iter().any(|l| !l.suppressed) {
        warnings.push("Loads are ignored in a modal analysis: natural frequencies depend only on the supports, the shape and the material".into());
    }
    if let Some(w) = hole_warning(input, &mesh) {
        warnings.push(w);
    }
    let problem = Problem { mesh: &mesh, mat, body_mats: body_mats.clone(), fixes, force, couplings: vec![], masses: vec![] };
    let free = crate::fea_solve::is_free(&problem);
    let solution = match study.analysis {
        Analysis::Static => {
            progress("Solving", 0.3);
            let r = crate::fea_solve::static_solve(&problem, cancel)?;
            progress("Stresses", 0.9);
            let stress = crate::fea_solve::nodal_stress(&mesh, &mat, &body_mats, &r.u);
            // a peak far above every other node is a badly shaped element (often at a small hole the
            // mesh barely resolves), not a stress concentration
            let mut vm: Vec<f32> = stress.iter().map(crate::fea_solve::von_mises).collect();
            vm.sort_unstable_by(|a, b| a.total_cmp(b));
            if let (Some(&top), Some(&near)) = (vm.last(), vm.get(vm.len() * 999 / 1000)) {
                if top > 5.0 * near && top as f64 > 0.2 * mat.yield_mpa {
                    warnings.push(format!(
                        "The largest stress ({top:.3e} MPa) is a spike at a few nodes, over 5 times the 99.9th percentile ({near:.3} MPa): most likely badly shaped elements, so don't trust the peak or the factor of safety; use a smaller mesh size (now {size} mm)"
                    ));
                }
            }
            let reactions = reactions(&mesh, &mat, &body_mats, &r.u, &problem.force, &support_nodes);
            if let Some(w) = yield_warning(&mesh, &mat, &body_mats, &stress, None) {
                warnings.push(w);
            }
            Solution::Static { u: r.u, stress, reactions }
        }
        Analysis::Modal => {
            progress("Finding modes", 0.3);
            // a free part: its six rigid-body modes come first, then the modes asked for
            let want = study.modes.clamp(1, 50);
            let modes = crate::fea_solve::modal(&problem, if free { want + 6 } else { want }, cancel)?;
            let rigid = modes.iter().filter(|m| m.rigid).count();
            if rigid > 0 && free {
                warnings.push(format!("No supports: the first {rigid} modes are the part moving as a whole (0 Hz); its vibration modes follow"));
            } else if rigid > 0 {
                warnings.push(format!("The supports don't hold the part fully: {rigid} mode{} the part (or a part) sliding or turning as a whole (0 Hz), not vibration; add supports to hold it", if rigid == 1 { " is" } else { "s are" }));
            }
            Solution::Modal { modes }
        }
        Analysis::Transient => {
            let t = &study.transient;
            let s = TransientSettings { duration: t.duration, steps: t.steps.clamp(10, 20000), damping: t.damping.clamp(0.0, 1.0), profile: t.profile(), frames: 120 };
            progress("Time stepping", 0.1);
            let r = crate::fea_solve::transient(&problem, &s, &|f| progress("Time stepping", 0.1 + 0.9 * f), cancel)?;
            if r.free {
                warnings.push("No supports: the part floats freely, so the results show its vibration with the rigid motion taken out".into());
            }
            if r.dt * r.f1 > 0.1 {
                warnings.push(format!("The time step ({}) is long for the lowest natural frequency ({:.0} Hz): use more steps", fmt_time(r.dt), r.f1));
            }
            Solution::Transient(r)
        }
    };
    // the largest stress over a time history, and when
    let peak_stress = match &solution {
        Solution::Transient(t) => {
            let mut best: Option<(f64, f64, Vec<[f32; 6]>)> = None;
            for (tt, u) in &t.frames {
                if cancel() {
                    return Err("cancelled".into());
                }
                let s = crate::fea_solve::nodal_stress(&mesh, &mat, &body_mats, u);
                let vm = s.iter().map(crate::fea_solve::von_mises).fold(0.0f32, f32::max) as f64;
                if best.as_ref().is_none_or(|b| vm > b.0) {
                    best = Some((vm, *tt, s));
                }
            }
            best.map(|(vm, tt, s)| {
                if let Some(w) = yield_warning(&mesh, &mat, &body_mats, &s, Some(tt)) {
                    warnings.push(w);
                }
                (vm, tt)
            })
        }
        _ => None,
    };
    let mass = (0..mesh.tets.len()).map(|ei| crate::fea_solve::mat_at(&mesh, &body_mats, &mat, ei).rho * crate::fea_solve::element_volume(&mesh, ei)).sum();
    Ok(Outcome { mesh, material, mat, body_mats, solution, warnings, mass, peak_stress })
}

/// Reaction force of each support. Every node's reaction is counted once: a Fixed support takes all of it
/// (the first Fixed support holding the node), a Roller only the part along the direction it holds the node,
/// leaving the rest (along another roller's normal) to the next support.
fn reactions(mesh: &Mesh, mat: &Elastic, body_mats: &[Elastic], u: &[f64], force: &[f64], supports: &[(u64, Vec<(u32, Option<DVec3>)>)]) -> Vec<(u64, DVec3)> {
    let mut nodes: Vec<u32> = supports.iter().flat_map(|s| s.1.iter().map(|x| x.0)).collect();
    nodes.sort_unstable();
    nodes.dedup();
    let r = crate::fea_solve::node_reactions(mesh, mat, body_mats, u, force, &nodes);
    let mut rem: std::collections::HashMap<u32, DVec3> = nodes.into_iter().zip(r).collect();
    let mut out: Vec<(u64, DVec3)> = supports.iter().map(|s| (s.0, DVec3::ZERO)).collect();
    for (k, (_, held)) in supports.iter().enumerate() {
        for (v, _) in held.iter().filter(|x| x.1.is_none()) {
            if let Some(x) = rem.get_mut(v) {
                out[k].1 += *x;
                *x = DVec3::ZERO;
            }
        }
    }
    for (k, (_, held)) in supports.iter().enumerate() {
        for (v, n) in held.iter().filter_map(|x| x.1.map(|n| (x.0, n))) {
            if let Some(x) = rem.get_mut(&v) {
                let c = n * x.dot(n);
                out[k].1 += c;
                *x -= c;
            }
        }
    }
    out
}

/// Yield strength at each node: the weakest material of the elements around it.
pub fn node_yield(mesh: &Mesh, mat: &Elastic, body_mats: &[Elastic]) -> Vec<f64> {
    let mut y = vec![f64::INFINITY; mesh.nodes.len()];
    for (ei, t) in mesh.tets.iter().enumerate() {
        let ym = crate::fea_solve::mat_at(mesh, body_mats, mat, ei).yield_mpa;
        for v in t {
            y[*v as usize] = y[*v as usize].min(ym);
        }
    }
    y.iter().map(|v| if v.is_finite() { *v } else { mat.yield_mpa }).collect()
}

/// A warning when the stress somewhere passes the material's yield strength (at time `at` of a time history).
fn yield_warning(mesh: &Mesh, mat: &Elastic, body_mats: &[Elastic], stress: &[[f32; 6]], at: Option<f64>) -> Option<String> {
    let y = node_yield(mesh, mat, body_mats);
    let (k, ratio) = stress.iter().zip(&y).enumerate().map(|(k, (s, y))| (k, crate::fea_solve::von_mises(s) as f64 / y.max(1e-12))).fold((0, 0.0), |a, b| if b.1 > a.1 { b } else { a });
    if ratio <= 1.0 {
        return None;
    }
    let vm = crate::fea_solve::von_mises(&stress[k]);
    let when = at.map(|t| format!(" at {}", fmt_time(t))).unwrap_or_default();
    Some(format!(
        "The stress reaches {vm:.0} MPa{when}, above the yield strength ({:.0} MPa): the part would bend permanently there, which a linear analysis does not model",
        y[k]
    ))
}

/// A warning when the smallest hole or fillet has too few elements across for its peak stress to be
/// trusted.
fn hole_warning(input: &Input, mesh: &Mesh) -> Option<String> {
    let radii = crate::fea_mesh::face_radii(&input.tris, &input.face);
    // average edge length of the mesh on each curved face
    let mut len: std::collections::HashMap<u32, (f64, usize)> = std::collections::HashMap::new();
    for (f, cad) in &mesh.faces {
        if !radii.contains_key(cad) {
            continue;
        }
        let e = len.entry(*cad).or_default();
        for (a, b) in [(0, 1), (1, 2), (2, 0)] {
            e.0 += mesh.nodes[f[a] as usize].distance(mesh.nodes[f[b] as usize]);
            e.1 += 1;
        }
    }
    let (r, h) = len.iter().filter(|x| x.1.1 > 0).map(|(f, (sum, n))| (radii[f], sum / *n as f64)).min_by(|a, b| (a.0 / a.1).total_cmp(&(b.0 / b.1)))?;
    let across = 2.0 * r / h;
    // about six elements across a hole resolve its peak stress to within a few percent
    (across < 5.0).then(|| {
        format!(
            "The smallest hole or fillet (radius {} mm) is only about {across:.0} elements across, so the peak stress there is rough: try an element size of {} mm or less",
            crate::expr::trim(r, 3),
            crate::expr::trim(nice(mesh.size.max(h) * across / 6.0), 3)
        )
    })
}

/// A time in s, ms or µs.
pub fn fmt_time(t: f64) -> String {
    if t >= 1.0 {
        format!("{:.3} s", t)
    } else if t >= 1e-3 {
        format!("{:.3} ms", t * 1e3)
    } else {
        format!("{:.1} µs", t * 1e6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 60 × 30 × 4 mm plate with a 10 mm hole in the middle, as the CAD tessellation would give it.
    /// Faces: 0 top, 1 bottom, 2 x = 0, 3 x = 60, 4 y = 0, 5 y = 30, 6 the hole.
    fn plate() -> Input {
        let (w, h, t, r, c) = (60.0, 30.0, 4.0, 5.0, DVec3::new(30.0, 15.0, 0.0));
        // rays from the hole's centre: 48 around plus the four corners, so the outline keeps its corners
        let mut ang: Vec<f64> = (0..48).map(|i| i as f64 * std::f64::consts::TAU / 48.0).collect();
        for (x, y) in [(w, h), (0.0, h), (0.0, 0.0), (w, 0.0)] {
            ang.push((y - c.y).atan2(x - c.x).rem_euclid(std::f64::consts::TAU));
        }
        ang.sort_by(f64::total_cmp);
        let hole = |a: f64, z: f64| DVec3::new(c.x + r * a.cos(), c.y + r * a.sin(), z);
        let edge = |a: f64, z: f64| {
            let d = DVec3::new(a.cos(), a.sin(), 0.0);
            let s = [if d.x > 1e-12 { (w - c.x) / d.x } else if d.x < -1e-12 { -c.x / d.x } else { f64::INFINITY }, if d.y > 1e-12 { (h - c.y) / d.y } else if d.y < -1e-12 { -c.y / d.y } else { f64::INFINITY }];
            let p = c + d * s[0].min(s[1]);
            DVec3::new(p.x.clamp(0.0, w), p.y.clamp(0.0, h), z)
        };
        let mut input = Input { planar: vec![], ..Default::default() };
        let mut add = |input: &mut Input, f: u32, a: DVec3, b: DVec3, cc: DVec3| {
            input.tris.push([a, b, cc]);
            input.face.push(f);
            input.body.push(0);
        };
        for i in 0..ang.len() {
            let (a0, a1) = (ang[i], ang[(i + 1) % ang.len()]);
            // top (outward +z) and bottom (−z) rings, hole wall (normal towards the axis)
            add(&mut input, 0, hole(a0, t), edge(a0, t), edge(a1, t));
            add(&mut input, 0, hole(a0, t), edge(a1, t), hole(a1, t));
            add(&mut input, 1, hole(a0, 0.0), edge(a1, 0.0), edge(a0, 0.0));
            add(&mut input, 1, hole(a0, 0.0), hole(a1, 0.0), edge(a1, 0.0));
            add(&mut input, 6, hole(a0, 0.0), hole(a0, t), hole(a1, t));
            add(&mut input, 6, hole(a0, 0.0), hole(a1, t), hole(a1, 0.0));
        }
        let q = |input: &mut Input, f: u32, p: [DVec3; 4]| {
            input.tris.push([p[0], p[1], p[2]]);
            input.tris.push([p[0], p[2], p[3]]);
            input.face.extend([f; 2]);
            input.body.extend([0; 2]);
        };
        let v = |x: f64, y: f64, z: f64| DVec3::new(x, y, z);
        q(&mut input, 2, [v(0.0, 0.0, 0.0), v(0.0, 0.0, t), v(0.0, h, t), v(0.0, h, 0.0)]);
        q(&mut input, 3, [v(w, 0.0, 0.0), v(w, h, 0.0), v(w, h, t), v(w, 0.0, t)]);
        q(&mut input, 4, [v(0.0, 0.0, 0.0), v(w, 0.0, 0.0), v(w, 0.0, t), v(0.0, 0.0, t)]);
        q(&mut input, 5, [v(0.0, h, 0.0), v(0.0, h, t), v(w, h, t), v(w, h, 0.0)]);
        let n = [v(0.0, 0.0, 1.0), v(0.0, 0.0, -1.0), v(-1.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, -1.0, 0.0), v(0.0, 1.0, 0.0)];
        let at = [v(30.0, 3.0, t), v(30.0, 3.0, 0.0), v(0.0, 15.0, 2.0), v(w, 15.0, 2.0), v(30.0, 0.0, 2.0), v(30.0, h, 2.0)];
        input.planar = (0..6).map(|i| Some((n[i], at[i]))).chain([None]).collect();
        input.bodies = vec![(1, 0)];
        input.materials = vec![None];
        input.min_radius = Some(r);
        input
    }

    fn pick(p: [f64; 3], n: [f64; 3]) -> FacePick {
        FacePick { point: p, normal: n }
    }

    /// The plate pulled by 3000 N, held by rollers on its far end, a side and the bottom (symmetry planes).
    fn pulled(size: f64, refine: bool) -> Study {
        let mut s = Study { material: "Steel".into(), mesh_size: Some(size), refine, ..Default::default() };
        for (p, n) in [([60.0, 15.0, 2.0], [1.0, 0.0, 0.0]), ([30.0, 0.0, 2.0], [0.0, -1.0, 0.0]), ([30.0, 3.0, 0.0], [0.0, 0.0, -1.0])] {
            let id = s.add_support(SupportKind::Roller);
            s.support_mut(id).unwrap().faces = vec![pick(p, n)];
        }
        let id = s.add_load(LoadKind::Force);
        let l = s.load_mut(id).unwrap();
        l.value = 3000.0;
        l.direction = [-1.0, 0.0, 0.0];
        l.faces = vec![pick([0.0, 15.0, 2.0], [-1.0, 0.0, 0.0])];
        s
    }

    fn peak_stress(o: &Outcome) -> f32 {
        let Solution::Static { stress, .. } = &o.solution else { panic!() };
        stress.iter().map(crate::fea_solve::von_mises).fold(0.0f32, f32::max)
    }

    #[test]
    fn roller_beside_a_hole_slides_in_its_plane() {
        let input = plate();
        let s = pulled(2.0, false);
        let o = run(&s, &input, None, &|_, _| {}, &|| false).unwrap();
        let Solution::Static { stress, reactions, .. } = &o.solution else { panic!() };
        // each roller only pushes along its normal, and the end takes the whole load
        let r: Vec<DVec3> = reactions.iter().map(|x| x.1).collect();
        assert!((r[0].x - 3000.0).abs() < 1.0 && r[0].y.abs() < 1e-6 && r[0].z.abs() < 1e-6, "{r:?}");
        assert!(r[1].x.abs() < 1e-6 && r[1].z.abs() < 1e-6 && r[2].x.abs() < 1e-6 && r[2].y.abs() < 1e-6, "{r:?}");
        let total = r.iter().fold(DVec3::ZERO, |a, b| a + *b);
        assert!((total - DVec3::new(3000.0, 0.0, 0.0)).length() < 1.0, "{total}");
        // net-section stress 37.5 MPa × Kt 2.3 (Peterson, d/W = 1/3): about 86 MPa
        let vm = stress.iter().map(crate::fea_solve::von_mises).fold(0.0f32, f32::max);
        assert!((vm - 86.0).abs() < 8.0, "{vm}");
    }

    #[test]
    fn refining_at_the_hole_finds_its_peak_stress() {
        // 5 mm elements are two across the hole: far too coarse for its stress, unless the mesh refines there
        let input = plate();
        let coarse = run(&pulled(5.0, false), &input, None, &|_, _| {}, &|| false).unwrap();
        let fine = run(&pulled(5.0, true), &input, None, &|_, _| {}, &|| false).unwrap();
        let uniform = run(&pulled(2.0, false), &input, None, &|_, _| {}, &|| false).unwrap();
        let (vc, vf) = (peak_stress(&coarse), peak_stress(&fine));
        eprintln!("coarse {vc} ({} nodes) refined {vf} ({} nodes, smallest {}, {:.1}°) uniform 2 mm {} ({} nodes)", coarse.mesh.nodes.len(), fine.mesh.nodes.len(), fine.mesh.smallest, fine.mesh.min_dihedral, peak_stress(&uniform), uniform.mesh.nodes.len());
        assert!(coarse.warnings.iter().any(|w| w.contains("smallest hole")), "{:?}", coarse.warnings);
        assert!(!fine.warnings.iter().any(|w| w.contains("smallest hole")), "{:?}", fine.warnings);
        assert!((vf - 86.0).abs() < 6.0, "{vf}");
        assert!(fine.mesh.smallest < 2.0 && fine.mesh.size == 5.0);
        // and with fewer nodes than a mesh that fine everywhere
        assert!(fine.mesh.nodes.len() < uniform.mesh.nodes.len());
        for t in &fine.mesh.tets {
            assert!(crate::fea_mesh::tet10_valid(&std::array::from_fn(|i| fine.mesh.nodes[t[i] as usize])));
        }
    }

    /// The pulled plate in Advanced mode: symmetry instead of the rollers and a total force, through
    /// crackh3d's solver.
    fn pulled_advanced(size: f64, solver: crate::fea_adv::SolverKind) -> Study {
        use crate::fea_adv::BcType;
        let mut s = Study { material: "Steel".into(), mesh_size: Some(size), refine: false, mode: SimMode::Advanced, ..Default::default() };
        s.advanced.solver = solver;
        for (p, n) in [([60.0, 15.0, 2.0], [1.0, 0.0, 0.0]), ([30.0, 0.0, 2.0], [0.0, -1.0, 0.0]), ([30.0, 3.0, 0.0], [0.0, 0.0, -1.0])] {
            let id = s.add_adv_bc("symmetry").unwrap();
            s.advanced.bc_mut(id).unwrap().faces = vec![pick(p, n)];
        }
        let id = s.add_adv_bc("force").unwrap();
        let b = s.advanced.bc_mut(id).unwrap();
        b.bc = BcType::Force { f: [-3000.0, 0.0, 0.0] };
        b.faces = vec![pick([0.0, 15.0, 2.0], [-1.0, 0.0, 0.0])];
        s
    }

    #[test]
    fn advanced_mode_matches_simple() {
        let input = plate();
        let simple = run(&pulled(2.0, false), &input, None, &|_, _| {}, &|| false).unwrap();
        for solver in [crate::fea_adv::SolverKind::Direct, crate::fea_adv::SolverKind::Iterative] {
            let adv = run(&pulled_advanced(2.0, solver), &input, None, &|_, _| {}, &|| false).unwrap();
            let (a, b) = (peak_stress(&simple), peak_stress(&adv));
            assert!((a - b).abs() < 0.01 * a, "{solver:?}: simple {a} advanced {b}");
            let Solution::Static { reactions, .. } = &adv.solution else { panic!() };
            assert!((reactions[0].1.x - 3000.0).abs() < 1.0, "{reactions:?}");
        }
    }

    /// A box from the origin to `hi`; faces 0 −x, 1 +x, 2 −y, 3 +y, 4 −z, 5 +z.
    fn block(hi: DVec3) -> Input {
        let c = |i: usize| DVec3::new([0.0, hi.x][i & 1], [0.0, hi.y][(i >> 1) & 1], [0.0, hi.z][(i >> 2) & 1]);
        let quads = [[0, 4, 6, 2], [1, 3, 7, 5], [0, 1, 5, 4], [2, 6, 7, 3], [0, 2, 3, 1], [4, 5, 7, 6]];
        let mut input = Input::default();
        for (fi, q) in quads.iter().enumerate() {
            input.tris.push([c(q[0]), c(q[1]), c(q[2])]);
            input.tris.push([c(q[0]), c(q[2]), c(q[3])]);
            input.face.extend([fi as u32; 2]);
            input.body.extend([0; 2]);
            let n = (c(q[1]) - c(q[0])).cross(c(q[2]) - c(q[0])).normalize();
            input.planar.push(Some((n, c(q[0]))));
        }
        input.bodies = vec![(1, 0)];
        input.materials = vec![None];
        input
    }

    #[test]
    fn periodic_pair_stretches_a_bar() {
        // a 40 × 10 × 10 bar whose ends are periodic, B 0.04 mm further along x than A (a mean strain of
        // 0.001), held by symmetry on a side and the bottom and in x at one end: plain tension, E ε = 200 MPa
        let input = block(DVec3::new(40.0, 10.0, 10.0));
        let mut s = Study { material: "Steel".into(), mesh_size: Some(2.5), refine: false, mode: SimMode::Advanced, ..Default::default() };
        for (p, n) in [([20.0, 0.0, 5.0], [0.0, -1.0, 0.0]), ([20.0, 5.0, 0.0], [0.0, 0.0, -1.0])] {
            let id = s.add_adv_bc("symmetry").unwrap();
            s.advanced.bc_mut(id).unwrap().faces = vec![pick(p, n)];
        }
        // the periodic pair holds the ends together, not the bar in x: hold one end's x
        let id = s.add_adv_bc("displacement").unwrap();
        let b = s.advanced.bc_mut(id).unwrap();
        b.bc = crate::fea_adv::BcType::Displacement { u: [Some(0.0), None, None] };
        b.faces = vec![pick([0.0, 5.0, 5.0], [-1.0, 0.0, 0.0])];
        let id = s.add_periodic();
        let p = s.advanced.periodic_mut(id).unwrap();
        p.a = vec![pick([0.0, 5.0, 5.0], [-1.0, 0.0, 0.0])];
        p.b = vec![pick([40.0, 5.0, 5.0], [1.0, 0.0, 0.0])];
        p.jump = [0.04, 0.0, 0.0];
        assert_eq!(crate::fea_adv::offset_of(p, &input), Some(DVec3::new(40.0, 0.0, 0.0)));
        let o = run(&s, &input, None, &|_, _| {}, &|| false).unwrap();
        let Solution::Static { u, stress, .. } = &o.solution else { panic!() };
        for (i, p) in o.mesh.nodes.iter().enumerate() {
            assert!((u[3 * i] - 0.001 * p.x).abs() < 1e-7, "u_x {} at {p}", u[3 * i]);
            assert!((stress[i][0] - 200.0).abs() < 0.1, "σx {} at {p}", stress[i][0]);
        }
    }

    #[test]
    fn missing_load_faces_say_so() {
        let input = plate();
        let mut s = Study { material: "Steel".into(), mesh_size: Some(5.0), ..Default::default() };
        let id = s.add_support(SupportKind::Fixed);
        s.support_mut(id).unwrap().faces = vec![pick([60.0, 15.0, 2.0], [1.0, 0.0, 0.0])];
        let id = s.add_load(LoadKind::Force);
        s.load_mut(id).unwrap().faces = vec![pick([300.0, 15.0, 2.0], [1.0, 0.0, 0.0])];
        let e = run(&s, &input, None, &|_, _| {}, &|| false).err().unwrap();
        assert!(e.starts_with("Force 1 has no faces"), "{e}");
    }
}
