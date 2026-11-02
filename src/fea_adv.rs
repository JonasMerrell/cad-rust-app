//! Advanced mode of the Simulation tab: a static study with crackh3d's boundary conditions and solver
//! (`crackh3d_fe`), plus periodic BCs. The mesh is the Simple mode's; its 10-node tetrahedra and boundary
//! triangles go to crackh3d as one `tet10` block and `Tri6` facets.

use std::sync::Arc;

use crackh3d_fe::fe::bcs::{BcKind, BcSet, FeProblem, PeriodicSet, SurfFacet};
use crackh3d_fe::fe::solve::LinearSolver;
use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::fea::{FacePick, Input, Outcome, Solution, Study};
use crate::fea_mesh::Mesh;

/// What an advanced boundary condition does (crackh3d's `BcKind`). Vectors are in part coordinates; mm,
/// N, MPa.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BcType {
    /// u = 0.
    Fixed,
    /// Prescribed displacement components (None: free).
    Displacement { u: [Option<f64>; 3] },
    /// No motion along the faces' normals (also inclined and curved faces).
    Symmetry,
    /// A pressure (MPa) pushing into the faces.
    Pressure { p: f64 },
    /// A uniform traction (MPa).
    Traction { t: [f64; 3] },
    /// A total force (N) spread evenly over the faces' area.
    Force { f: [f64; 3] },
    /// An elastic foundation: stiffness per unit area along x, y, z (N/mm³).
    Spring { k: [f64; 3] },
    /// The faces move as a rigid body with a reference point: each of its translations and rotations is
    /// prescribed, or free under the force and moment.
    RemoteRigid {
        point: [f64; 3],
        #[serde(default)]
        force: [f64; 3],
        #[serde(default)]
        moment: [f64; 3],
        #[serde(default)]
        u: [Option<f64>; 3],
        #[serde(default)]
        rot: [Option<f64>; 3],
    },
    /// A force and a moment about a reference point, spread over the faces' nodes (the faces stay free
    /// to deform).
    RemoteDistributing {
        point: [f64; 3],
        #[serde(default)]
        force: [f64; 3],
        #[serde(default)]
        moment: [f64; 3],
    },
    /// A force (N) at the mesh node closest to `at` (a singular stress there).
    PointForce { at: [f64; 3], f: [f64; 3] },
}

impl BcType {
    pub const KINDS: [&'static str; 10] =
        ["fixed", "displacement", "symmetry", "pressure", "traction", "force", "spring", "remote_rigid", "remote_distributing", "point_force"];

    /// A new BC of a kind (`KINDS`), with values to start from.
    pub fn new(kind: &str) -> Option<BcType> {
        Some(match kind {
            "fixed" => BcType::Fixed,
            "displacement" => BcType::Displacement { u: [Some(0.0), None, None] },
            "symmetry" => BcType::Symmetry,
            "pressure" => BcType::Pressure { p: 1.0 },
            "traction" => BcType::Traction { t: [0.0, 0.0, -1.0] },
            "force" => BcType::Force { f: [0.0, 0.0, -100.0] },
            "spring" => BcType::Spring { k: [10.0; 3] },
            "remote_rigid" => BcType::RemoteRigid { point: [0.0; 3], force: [0.0, 0.0, -100.0], moment: [0.0; 3], u: [None; 3], rot: [None; 3] },
            "remote_distributing" => BcType::RemoteDistributing { point: [0.0; 3], force: [0.0, 0.0, -100.0], moment: [0.0; 3] },
            "point_force" => BcType::PointForce { at: [0.0; 3], f: [0.0, 0.0, -100.0] },
            _ => return None,
        })
    }

    pub fn key(&self) -> &'static str {
        match self {
            BcType::Fixed => "fixed",
            BcType::Displacement { .. } => "displacement",
            BcType::Symmetry => "symmetry",
            BcType::Pressure { .. } => "pressure",
            BcType::Traction { .. } => "traction",
            BcType::Force { .. } => "force",
            BcType::Spring { .. } => "spring",
            BcType::RemoteRigid { .. } => "remote_rigid",
            BcType::RemoteDistributing { .. } => "remote_distributing",
            BcType::PointForce { .. } => "point_force",
        }
    }

    pub fn label_of(key: &str) -> &'static str {
        match key {
            "fixed" => "Fixed",
            "displacement" => "Displacement",
            "symmetry" => "Symmetry",
            "pressure" => "Pressure",
            "traction" => "Traction",
            "force" => "Force",
            "spring" => "Spring (elastic support)",
            "remote_rigid" => "Remote (rigid)",
            "remote_distributing" => "Remote (distributing)",
            "point_force" => "Point force",
            _ => "?",
        }
    }

    pub fn label(&self) -> &'static str {
        Self::label_of(self.key())
    }

    /// Whether the BC holds the part (and so has a reaction).
    pub fn holds(&self) -> bool {
        matches!(self, BcType::Fixed | BcType::Displacement { .. } | BcType::Symmetry | BcType::Spring { .. } | BcType::RemoteRigid { .. })
    }

    /// Whether the BC acts on faces (a point force acts on a node).
    pub fn on_faces(&self) -> bool {
        !matches!(self, BcType::PointForce { .. })
    }
}

/// A boundary condition of an advanced study.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AdvBc {
    pub id: u64,
    pub name: String,
    pub bc: BcType,
    #[serde(default)]
    pub faces: Vec<FacePick>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suppressed: bool,
}

/// A periodic pair: side B is side A moved by `offset`, and it moves as A does plus `jump`
/// (u_B(x) = u_A(x - offset) + jump).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Periodic {
    pub id: u64,
    pub name: String,
    /// Side A (leads).
    #[serde(default)]
    pub a: Vec<FacePick>,
    /// Side B (follows).
    #[serde(default)]
    pub b: Vec<FacePick>,
    /// From A to B; None: from the centre of A's faces to the centre of B's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<[f64; 3]>,
    /// B's displacement relative to A's (mm), e.g. a mean strain times the offset.
    #[serde(default)]
    pub jump: [f64; 3],
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suppressed: bool,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SolverKind {
    /// CG with AMG, falling back to the direct solve when it doesn't converge (and the other way round).
    #[default]
    Iterative,
    /// Sparse Cholesky (LU when that fails).
    Direct,
}

/// The advanced-mode part of a study.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Advanced {
    #[serde(default)]
    pub bcs: Vec<AdvBc>,
    #[serde(default)]
    pub periodic: Vec<Periodic>,
    /// An acceleration of the whole part (g, along a direction), such as its own weight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gravity: Option<[f64; 3]>,
    #[serde(default)]
    pub solver: SolverKind,
}

impl Advanced {
    /// Ids of every BC and periodic pair.
    pub fn ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.bcs.iter().map(|b| b.id).chain(self.periodic.iter().map(|p| p.id))
    }

    pub fn bc_mut(&mut self, id: u64) -> Option<&mut AdvBc> {
        self.bcs.iter_mut().find(|b| b.id == id)
    }

    pub fn periodic_mut(&mut self, id: u64) -> Option<&mut Periodic> {
        self.periodic.iter_mut().find(|b| b.id == id)
    }

    /// Name of a BC or periodic pair.
    pub fn name_of(&self, id: u64) -> Option<&str> {
        self.bcs.iter().find(|b| b.id == id).map(|b| b.name.as_str()).or_else(|| self.periodic.iter().find(|b| b.id == id).map(|b| b.name.as_str()))
    }
}

/// Area-weighted centre of some faces of the parts.
fn centre(input: &Input, picks: &[FacePick]) -> Option<DVec3> {
    let ids: Vec<u32> = picks.iter().filter_map(|p| input.resolve(p)).collect();
    let (mut c, mut a) = (DVec3::ZERO, 0.0);
    for (t, f) in input.tris.iter().zip(&input.face) {
        if ids.contains(f) {
            let w = (t[1] - t[0]).cross(t[2] - t[0]).length() / 2.0;
            c += (t[0] + t[1] + t[2]) / 3.0 * w;
            a += w;
        }
    }
    (a > 0.0).then(|| c / a)
}

/// The offset from side A to side B: the pair's own, or from the centre of A's faces to B's (components a
/// hair from zero made zero).
pub fn offset_of(p: &Periodic, input: &Input) -> Option<DVec3> {
    if let Some(o) = p.offset {
        return Some(DVec3::from(o));
    }
    let d = centre(input, &p.b)? - centre(input, &p.a)?;
    let l = d.length();
    Some(DVec3::from_array(d.to_array().map(|x| if x.abs() < 1e-6 * l { 0.0 } else { x })))
}

/// The mesh as crackh3d's: the same nodes, one `tet10` block (gmsh numbers the mid-edge nodes of edges 13
/// and 23 the other way round).
fn to_crackh3d(mesh: &Mesh) -> crackh3d_fe::mesh_data::Mesh {
    let connectivity: Vec<Vec<usize>> =
        mesh.tets.iter().map(|t| [t[0], t[1], t[2], t[3], t[4], t[5], t[6], t[7], t[9], t[8]].iter().map(|v| *v as usize).collect()).collect();
    let block = crackh3d_fe::mesh_data::ElementBlock::new("body", "tet10", connectivity, 1).expect("tet10 block");
    crackh3d_fe::mesh_data::Mesh { points: mesh.nodes.iter().map(|p| p.to_array()).collect(), blocks: vec![block], ..Default::default() }
}

/// Solve an advanced study on a mesh (static, linear elastic). `warnings` gains the run's notes.
pub fn run(study: &Study, input: &Input, mesh: Arc<Mesh>, progress: &dyn Fn(&str, f32), warnings: &mut Vec<String>) -> Result<Outcome, String> {
    let adv = &study.advanced;
    let (mut material, mat) = crate::fea::material_of(study, input);
    let parts = crate::fea::part_materials(study, input);
    if crate::fea::mixed(study, input) {
        material = parts[0].0.clone();
        warnings.push(format!("Advanced mode uses one material for every part: {material}"));
    }
    let (blo, bhi) = input.bounds();
    // how far a node may lie off a planar face and still be on it (the mesher's cut points sit a hair off)
    let on_plane = (bhi - blo).length() * 1e-4;
    // the mesh faces of some picks; `flat`: on a planar CAD face, only those lying in its plane (the mesh
    // cuts across the corner at some sharp edges, and a symmetry BC must not hold those along their
    // tilted normals)
    let facets_of = |picks: &[FacePick], what: &str, flat: bool, warnings: &mut Vec<String>| -> Vec<SurfFacet> {
        let mut ids = vec![];
        for p in picks {
            match input.resolve(p) {
                Some(f) => ids.push(f),
                None => warnings.push(format!("{what}: a face is no longer on the parts")),
            }
        }
        let in_plane = |t: &[u32; 6], cad: u32| match input.planar.get(cad as usize).copied().flatten().filter(|_| flat) {
            Some((n, at)) => t[..3].iter().all(|v| (mesh.nodes[*v as usize] - at).dot(n).abs() <= on_plane),
            None => true,
        };
        mesh.faces.iter().filter(|f| ids.contains(&f.1) && in_plane(&f.0, f.1)).map(|(t, _)| SurfFacet::Tri6(t.map(|v| v as usize))).collect()
    };
    let mut problem = FeProblem::default();
    let mut ids = vec![];
    for b in adv.bcs.iter().filter(|b| !b.suppressed) {
        let facets = match &b.bc {
            BcType::PointForce { at, .. } => {
                // the boundary node closest to the point
                let at = DVec3::from(*at);
                let n = mesh.faces.iter().flat_map(|f| f.0).min_by(|x, y| mesh.nodes[*x as usize].distance(at).total_cmp(&mesh.nodes[*y as usize].distance(at)));
                n.map(|n| vec![SurfFacet::Tri6([n as usize; 6])]).unwrap_or_default()
            }
            _ => facets_of(&b.faces, &b.name, matches!(b.bc, BcType::Symmetry), warnings),
        };
        if facets.is_empty() {
            return Err(format!("{} has no faces on the parts: pick its faces", b.name));
        }
        let kind = match &b.bc {
            BcType::Fixed => BcKind::Fixed,
            BcType::Displacement { u } => BcKind::Displacement(*u),
            BcType::Symmetry => BcKind::Symmetry,
            BcType::Pressure { p } => BcKind::Pressure(*p),
            BcType::Traction { t } => BcKind::Traction(*t),
            BcType::Force { f } => BcKind::Force(*f),
            BcType::Spring { k } => BcKind::Spring(*k),
            BcType::RemoteRigid { point, force, moment, u, rot } => BcKind::RemoteRigid { rp: *point, force: *force, moment: *moment, u: *u, rot: *rot },
            BcType::RemoteDistributing { point, force, moment } => BcKind::RemoteDistributing { rp: *point, force: *force, moment: *moment },
            BcType::PointForce { f, .. } => BcKind::PointForce(*f),
        };
        problem.bcs.push(BcSet { name: b.name.clone(), kind, facets });
        ids.push(b.id);
    }
    for p in adv.periodic.iter().filter(|p| !p.suppressed) {
        let (leader, follower) = (facets_of(&p.a, &p.name, false, warnings), facets_of(&p.b, &p.name, false, warnings));
        if leader.is_empty() || follower.is_empty() {
            return Err(format!("{} needs faces on both sides: pick side A and side B", p.name));
        }
        let offset = offset_of(p, input).ok_or_else(|| format!("{}: can't find the offset between its sides", p.name))?;
        problem.periodic.push(PeriodicSet { name: p.name.clone(), leader, follower, offset: offset.to_array(), jump: p.jump });
    }
    if let Some(g) = adv.gravity {
        let a = DVec3::from(g) * crate::fea::G;
        if a != DVec3::ZERO {
            problem.body_force = Some((a * mat.rho).to_array());
        }
    }
    if problem.bcs.iter().all(|b| !b.kind.constrains()) {
        return Err("Add a boundary condition that holds the part (Fixed, Displacement, Symmetry, Spring or a Remote (rigid) with a prescribed motion)".into());
    }
    progress("Solving (crackh3d)", 0.3);
    let cm = to_crackh3d(&mesh);
    let solver = match adv.solver {
        SolverKind::Iterative => LinearSolver::CgAmg { rtol: 1e-8 },
        SolverKind::Direct => LinearSolver::Direct,
    };
    let _ = crackh3d_fe::progress::take_solves();
    let sol = crackh3d_fe::fe::bcs::solve_fe_with(&cm, mat.e, mat.nu, &problem, solver).map_err(|e| e.to_string())?;
    for s in crackh3d_fe::progress::take_solves() {
        if let Some(f) = s.fallback {
            warnings.push(format!("Solver: {f}"));
        }
    }
    progress("Stresses", 0.9);
    let u: Vec<f64> = sol.displacement.iter().flatten().copied().collect();
    let stress = crate::fea_solve::nodal_stress(&mesh, &mat, &[], &u);
    // reactions: the holding BCs in order, then the periodic pairs (none) and crackh3d's own pins
    let holding: Vec<u64> = problem.bcs.iter().zip(&ids).filter(|(b, _)| b.kind.constrains()).map(|(_, id)| *id).collect();
    let reactions = holding.into_iter().zip(&sol.reactions).map(|(id, r)| (id, DVec3::from(r.force))).collect();
    // curved elements' own volume, not their corners' straight tetrahedron
    let mass = (0..mesh.tets.len()).map(|ei| mat.rho * crate::fea_solve::element_volume(&mesh, ei)).sum();
    Ok(Outcome { mesh, material, mat, body_mats: vec![], printed: false, safety: None, solution: Solution::Static { u, stress, reactions }, warnings: std::mem::take(warnings), mass, peak_stress: None })
}
