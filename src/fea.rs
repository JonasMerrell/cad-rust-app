//! Simulation tab model: linear elastic studies of a Part Studio's parts (static, modal and transient),
//! with supports and loads on faces. Units: mm, N, MPa, seconds.

use std::sync::Arc;

use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::doc::{BodyId, Document};
use crate::fea_mesh::{Mesh, Surface};
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
    /// Material name from `MATERIALS`; empty: the parts' own material (steel when none is set).
    #[serde(default)]
    pub material: String,
    /// Element size (mm); None: worked out from the part size.
    #[serde(default)]
    pub mesh_size: Option<f64>,
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

impl Default for Study {
    fn default() -> Self {
        Study {
            name: "Simulation 1".into(),
            studio: 0,
            bodies: vec![],
            analysis: Analysis::Static,
            material: String::new(),
            mesh_size: None,
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
        self.next_id = self.next_id.max(1 + self.supports.iter().map(|s| s.id).chain(self.loads.iter().map(|l| l.id)).max().unwrap_or(0));
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
        self.loads.iter_mut().find(|s| s.id == id).map(|l| &mut l.faces)
    }

    pub fn faces(&self, id: u64) -> Option<&Vec<FacePick>> {
        self.supports.iter().find(|s| s.id == id).map(|s| &s.faces).or_else(|| self.loads.iter().find(|s| s.id == id).map(|l| &l.faces))
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

    /// Element size from the parts' volume, small enough for a quick solve.
    pub fn auto_size(&self) -> f64 {
        let vol: f64 = self.tris.iter().map(|t| t[0].dot(t[1].cross(t[2])) / 6.0).sum::<f64>().abs();
        let (lo, hi) = self.bounds();
        let ext = hi - lo;
        // ~1200 lattice cells inside the parts (some 15 000 elements), but at least 3 across the thinnest box side
        let h = (vol.max(1e-9) / 1200.0).cbrt();
        let h = h.min(ext.min_element().max(1e-6) / 3.0);
        // holes and fillets want a few elements around them
        let h = match self.min_radius {
            Some(r) => h.min(r / 2.0).max(ext.max_element() / 60.0).min(h),
            None => h,
        };
        let h = h.max(ext.max_element() / 200.0);
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

/// Mesh the study's parts: as one when they share a material, part by part (bonded) when they don't.
pub fn mesh_input(study: &Study, input: &Input, size: f64, cancel: &dyn Fn() -> bool) -> Result<Mesh, String> {
    let surf = Surface::new(input.tris.clone(), input.face.clone(), input.body.clone());
    if mixed(study, input) { crate::fea_mesh::mesh_parts(&surf, size, cancel) } else { crate::fea_mesh::mesh(&surf, size, cancel) }
}

/// Mesh the parts (or reuse `mesh`) and solve the study.
pub fn run(study: &Study, input: &Input, mesh: Option<Arc<Mesh>>, progress: &dyn Fn(&str, f32), cancel: &dyn Fn() -> bool) -> Result<Outcome, String> {
    if input.tris.is_empty() {
        return Err("No parts to analyse".into());
    }
    let mesh = match mesh {
        Some(m) => m,
        None => {
            progress("Meshing", 0.0);
            Arc::new(mesh_input(study, input, study.mesh_size.unwrap_or_else(|| input.auto_size()), cancel)?)
        }
    };
    let (mut material, mat) = material_of(study, input);
    let parts = part_materials(study, input);
    let body_mats: Vec<Elastic> = if mesh.body.is_empty() { vec![] } else { parts.iter().map(|p| p.1).collect() };
    if !body_mats.is_empty() {
        let mut names: Vec<String> = parts.iter().map(|p| p.0.clone()).collect();
        names.dedup();
        material = names.join(" + ");
    }
    let mut warnings = vec![];
    let nn = mesh.nodes.len();
    // mesh faces of a list of picks
    let faces_of = |picks: &[FacePick], what: &str, warnings: &mut Vec<String>| -> Vec<usize> {
        let mut ids = vec![];
        for p in picks {
            match input.resolve(p) {
                Some(f) => ids.push(f),
                None => warnings.push(format!("{what}: a face is no longer on the parts")),
            }
        }
        (0..mesh.faces.len()).filter(|i| ids.contains(&mesh.faces[*i].1)).collect::<Vec<usize>>()
    };
    let mut fixes = vec![];
    let mut support_nodes = vec![];
    for s in study.supports.iter().filter(|s| !s.suppressed) {
        let fs = faces_of(&s.faces, &s.name, &mut warnings);
        if fs.is_empty() {
            warnings.push(format!("{} has no faces", s.name));
        }
        let mut nodes: Vec<u32> = fs.iter().flat_map(|f| mesh.faces[*f].0).collect();
        nodes.sort_unstable();
        nodes.dedup();
        match s.kind {
            SupportKind::Fixed => fixes.push(Fix::Fixed(nodes.clone())),
            SupportKind::Roller => {
                // each node slides along the surface: normal averaged over its faces
                let mut normal = vec![DVec3::ZERO; nn];
                for f in &fs {
                    let t = &mesh.faces[*f].0;
                    let p = |i: usize| mesh.nodes[t[i] as usize];
                    let n = (p(1) - p(0)).cross(p(2) - p(0));
                    for v in t {
                        normal[*v as usize] += n;
                    }
                }
                let snap = |d: DVec3| {
                    let d = d.normalize_or_zero();
                    match (0..3).find(|&a| d[a].abs() > 1.0 - 1e-6) {
                        Some(a) => DVec3::from_array(std::array::from_fn(|k| if k == a { d[a].signum() } else { 0.0 })),
                        None => d,
                    }
                };
                fixes.push(Fix::Roller(nodes.iter().map(|v| (*v, snap(normal[*v as usize]))).collect()));
            }
        }
        support_nodes.push((s.id, nodes));
    }
    let mut force = vec![0.0; 3 * nn];
    let mut any_load = false;
    for l in study.loads.iter().filter(|l| !l.suppressed) {
        let dir = DVec3::from(l.direction).normalize_or_zero();
        match l.kind {
            LoadKind::Gravity => {
                crate::fea_solve::body_loads(&mesh, dir * (l.value * G), |ei| crate::fea_solve::mat_at(&mesh, &body_mats, &mat, ei).rho, &mut force);
                any_load = true;
            }
            LoadKind::Force | LoadKind::Pressure => {
                let mut fs = faces_of(&l.faces, &l.name, &mut warnings);
                if let (Some(at), Some(r)) = (l.at, l.radius) {
                    let at = DVec3::from(at);
                    fs.retain(|f| {
                        let t = &mesh.faces[*f].0;
                        ((mesh.nodes[t[0] as usize] + mesh.nodes[t[1] as usize] + mesh.nodes[t[2] as usize]) / 3.0).distance(at) <= r
                    });
                }
                if fs.is_empty() {
                    warnings.push(format!("{} has no faces", l.name));
                } else {
                    let area = crate::fea_solve::area_of(&mesh, &fs);
                    let (v, normal) = (l.value, l.normal || l.kind == LoadKind::Pressure);
                    let per_area = if l.kind == LoadKind::Force { v / area.max(1e-12) } else { v };
                    crate::fea_solve::traction_loads(&mesh, &fs, |n| if normal { -n * per_area } else { dir * per_area }, &mut force);
                    any_load = true;
                }
            }
        }
    }
    let held = study.supports.iter().any(|s| !s.suppressed && !s.faces.is_empty());
    if study.analysis == Analysis::Static {
        if !held {
            return Err("Add a support (Fixed or Roller) so the part is held".into());
        }
        if !any_load {
            return Err("Add a load (Force, Pressure or Gravity)".into());
        }
    }
    if study.analysis == Analysis::Transient && !any_load {
        return Err("Add a load (Force, Pressure or Gravity)".into());
    }
    let problem = Problem { mesh: &mesh, mat, body_mats: body_mats.clone(), fixes, force };
    let solution = match study.analysis {
        Analysis::Static => {
            progress("Solving", 0.3);
            let r = crate::fea_solve::static_solve(&problem, cancel)?;
            progress("Stresses", 0.9);
            let stress = crate::fea_solve::nodal_stress(&mesh, &mat, &body_mats, &r.u);
            let reactions = support_nodes.iter().map(|(id, nodes)| (*id, crate::fea_solve::reaction(&mesh, &mat, &body_mats, &r.u, &problem.force, nodes))).collect();
            Solution::Static { u: r.u, stress, reactions }
        }
        Analysis::Modal => {
            progress("Finding modes", 0.3);
            let modes = crate::fea_solve::modal(&problem, study.modes.clamp(1, 50), cancel)?;
            if !held {
                warnings.push("No supports: the first six modes are the part moving freely (about 0 Hz)".into());
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
    let mass = mesh.tets.iter().enumerate().map(|(ei, t)| crate::fea_solve::mat_at(&mesh, &body_mats, &mat, ei).rho * tet10_volume(&mesh, t)).sum();
    Ok(Outcome { mesh, material, mat, body_mats, solution, warnings, mass })
}

fn tet10_volume(m: &Mesh, t: &[u32; 10]) -> f64 {
    let p = |i: usize| m.nodes[t[i] as usize];
    (p(1) - p(0)).cross(p(2) - p(0)).dot(p(3) - p(0)) / 6.0
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
