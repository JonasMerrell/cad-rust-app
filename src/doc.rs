//! The part document: variables, the ordered feature list and references
//! between features and model geometry.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::expr::{self, Env, Kind, Units};
use crate::sketch::{Frame, Id as SkId, Sketch};

pub type FeatId = u64;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum OriginPlane {
    Top,
    Front,
    Right,
}

impl OriginPlane {
    pub const ALL: [OriginPlane; 3] = [OriginPlane::Top, OriginPlane::Front, OriginPlane::Right];
    pub fn frame(self) -> Frame {
        match self {
            OriginPlane::Top => Frame::TOP,
            OriginPlane::Front => Frame::FRONT,
            OriginPlane::Right => Frame::RIGHT,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            OriginPlane::Top => "Top plane",
            OriginPlane::Front => "Front plane",
            OriginPlane::Right => "Right plane",
        }
    }
}

/// Something the user picked. Model faces and edges are referred to by a point on
/// them so the reference survives rebuilds that renumber topology.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Ref {
    OriginPlane { plane: OriginPlane },
    /// A Plane feature or a sketch's plane.
    FeaturePlane { feature: FeatId },
    Face { point: [f64; 3], normal: [f64; 3] },
    Edge { point: [f64; 3] },
    Vertex { point: [f64; 3] },
    Region { sketch: FeatId, point: [f64; 2] },
    SketchCurve { sketch: FeatId, curve: SkId },
    SketchPoint { sketch: FeatId, point: SkId },
    Body { body: BodyId },
    Axis { axis: u8 },
}

impl Ref {
    pub fn describe(&self) -> String {
        match self {
            Ref::OriginPlane { plane } => plane.name().to_string(),
            Ref::FeaturePlane { .. } => "Plane".into(),
            Ref::Face { .. } => "Face".into(),
            Ref::Edge { .. } => "Edge".into(),
            Ref::Vertex { .. } => "Vertex".into(),
            Ref::Region { .. } => "Sketch region".into(),
            Ref::SketchCurve { .. } => "Sketch curve".into(),
            Ref::SketchPoint { .. } => "Sketch point".into(),
            Ref::Body { body } => format!("Body {}", body.1 + 1),
            Ref::Axis { axis } => ["X axis", "Y axis", "Z axis"][*axis as usize % 3].into(),
        }
    }
    pub fn is_planar(&self) -> bool {
        matches!(self, Ref::OriginPlane { .. } | Ref::FeaturePlane { .. } | Ref::Face { .. })
    }
}

/// (feature that created it, index among that feature's new bodies)
pub type BodyId = (FeatId, u32);

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BoolOp {
    #[default]
    New,
    Add,
    Remove,
    Intersect,
}

impl BoolOp {
    pub const ALL: [BoolOp; 4] = [BoolOp::New, BoolOp::Add, BoolOp::Remove, BoolOp::Intersect];
    pub fn label(self) -> &'static str {
        match self {
            BoolOp::New => "New",
            BoolOp::Add => "Add",
            BoolOp::Remove => "Remove",
            BoolOp::Intersect => "Intersect",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EndType {
    #[default]
    Blind,
    Symmetric,
    ThroughAll,
    UpToFace,
}

impl EndType {
    pub const ALL: [EndType; 4] = [EndType::Blind, EndType::Symmetric, EndType::ThroughAll, EndType::UpToFace];
    pub fn label(self) -> &'static str {
        match self {
            EndType::Blind => "Blind",
            EndType::Symmetric => "Symmetric",
            EndType::ThroughAll => "Through all",
            EndType::UpToFace => "Up to face",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HoleType {
    #[default]
    Simple,
    Counterbore,
    Countersink,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BooleanKind {
    #[default]
    Union,
    Subtract,
    Intersect,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Op {
    Sketch {
        plane: Ref,
        sketch: Sketch,
        /// Plane frame resolved at the last rebuild.
        frame: Frame,
    },
    Plane {
        base: Option<Ref>,
        offset: String,
    },
    Extrude {
        profiles: Vec<Ref>,
        end: EndType,
        depth: String,
        opposite: bool,
        second: bool,
        depth2: String,
        up_to: Option<Ref>,
        op: BoolOp,
        draft: String,
    },
    Revolve {
        profiles: Vec<Ref>,
        axis: Option<Ref>,
        /// "full", "one" or "symmetric"
        mode: String,
        angle: String,
        op: BoolOp,
    },
    Sweep {
        profiles: Vec<Ref>,
        path: Vec<Ref>,
        op: BoolOp,
    },
    Loft {
        profiles: Vec<Ref>,
        ruled: bool,
        op: BoolOp,
    },
    Fillet {
        edges: Vec<Ref>,
        radius: String,
        variable: bool,
        end_radius: String,
    },
    Chamfer {
        edges: Vec<Ref>,
        distance: String,
    },
    Shell {
        faces: Vec<Ref>,
        thickness: String,
    },
    Hole {
        points: Vec<Ref>,
        hole_type: HoleType,
        diameter: String,
        depth: String,
        through: bool,
        cb_diameter: String,
        cb_depth: String,
        cs_diameter: String,
        cs_angle: String,
        tapped: bool,
    },
    Draft {
        faces: Vec<Ref>,
        neutral: Option<Ref>,
        angle: String,
    },
    LinearPattern {
        bodies: Vec<Ref>,
        direction: Option<Ref>,
        spacing: String,
        count: String,
        second: Option<Ref>,
        spacing2: String,
        count2: String,
    },
    CircularPattern {
        bodies: Vec<Ref>,
        axis: Option<Ref>,
        count: String,
        angle: String,
    },
    Mirror {
        bodies: Vec<Ref>,
        plane: Option<Ref>,
    },
    Boolean {
        kind: BooleanKind,
        target: Option<Ref>,
        tools: Vec<Ref>,
        keep_tools: bool,
    },
    Split {
        target: Option<Ref>,
        plane: Option<Ref>,
    },
    DeleteBody {
        bodies: Vec<Ref>,
    },
    Import {
        name: String,
        /// STEP text, embedded so the document is self-contained.
        step: String,
    },
}

impl Op {
    pub fn kind(&self) -> &'static str {
        match self {
            Op::Sketch { .. } => "sketch",
            Op::Plane { .. } => "plane",
            Op::Extrude { .. } => "extrude",
            Op::Revolve { .. } => "revolve",
            Op::Sweep { .. } => "sweep",
            Op::Loft { .. } => "loft",
            Op::Fillet { .. } => "fillet",
            Op::Chamfer { .. } => "chamfer",
            Op::Shell { .. } => "shell",
            Op::Hole { .. } => "hole",
            Op::Draft { .. } => "draft",
            Op::LinearPattern { .. } => "linearPattern",
            Op::CircularPattern { .. } => "circularPattern",
            Op::Mirror { .. } => "mirror",
            Op::Boolean { .. } => "boolean",
            Op::Split { .. } => "split",
            Op::DeleteBody { .. } => "deleteBody",
            Op::Import { .. } => "import",
        }
    }

    /// Default parameters for a new feature of `kind`, seeded with the current picks.
    pub fn new_default(kind: &str) -> Option<Op> {
        let s = |v: &str| v.to_string();
        Some(match kind {
            "plane" => Op::Plane { base: None, offset: s("10") },
            "extrude" => Op::Extrude {
                profiles: vec![],
                end: EndType::Blind,
                depth: s("10"),
                opposite: false,
                second: false,
                depth2: s("10"),
                up_to: None,
                op: BoolOp::New,
                draft: s("0"),
            },
            "revolve" => Op::Revolve { profiles: vec![], axis: None, mode: s("full"), angle: s("360"), op: BoolOp::New },
            "sweep" => Op::Sweep { profiles: vec![], path: vec![], op: BoolOp::New },
            "loft" => Op::Loft { profiles: vec![], ruled: false, op: BoolOp::New },
            "fillet" => Op::Fillet { edges: vec![], radius: s("2"), variable: false, end_radius: s("4") },
            "chamfer" => Op::Chamfer { edges: vec![], distance: s("1") },
            "shell" => Op::Shell { faces: vec![], thickness: s("1") },
            "hole" => Op::Hole {
                points: vec![],
                hole_type: HoleType::Simple,
                diameter: s("5"),
                depth: s("10"),
                through: true,
                cb_diameter: s("9"),
                cb_depth: s("3"),
                cs_diameter: s("10"),
                cs_angle: s("90"),
                tapped: false,
            },
            "draft" => Op::Draft { faces: vec![], neutral: None, angle: s("3") },
            "linearPattern" => Op::LinearPattern {
                bodies: vec![],
                direction: None,
                spacing: s("20"),
                count: s("3"),
                second: None,
                spacing2: s("20"),
                count2: s("1"),
            },
            "circularPattern" => Op::CircularPattern { bodies: vec![], axis: None, count: s("4"), angle: s("360") },
            "mirror" => Op::Mirror { bodies: vec![], plane: None },
            "boolean" => Op::Boolean { kind: BooleanKind::Union, target: None, tools: vec![], keep_tools: false },
            "split" => Op::Split { target: None, plane: None },
            "deleteBody" => Op::DeleteBody { bodies: vec![] },
            _ => return None,
        })
    }
}

pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "sketch" => "Sketch",
        "extrude" => "Extrude",
        "revolve" => "Revolve",
        "sweep" => "Sweep",
        "loft" => "Loft",
        "fillet" => "Fillet",
        "chamfer" => "Chamfer",
        "shell" => "Shell",
        "hole" => "Hole",
        "linearPattern" => "Linear pattern",
        "circularPattern" => "Circular pattern",
        "mirror" => "Mirror",
        "boolean" => "Boolean",
        "draft" => "Draft",
        "split" => "Split",
        "plane" => "Plane",
        "deleteBody" => "Delete body",
        "import" => "Import",
        _ => "Feature",
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Feature {
    pub id: FeatId,
    pub name: String,
    #[serde(default)]
    pub suppressed: bool,
    #[serde(default)]
    pub hidden: bool,
    pub op: Op,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Variable {
    pub name: String,
    pub expr: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub format: String,
    pub units: Units,
    pub variables: Vec<Variable>,
    pub features: Vec<Feature>,
    /// Number of features that are active; later ones are rolled back.
    pub rollback: Option<usize>,
    pub next_id: FeatId,
}

impl Default for Document {
    fn default() -> Self {
        Document { format: "cad-rust-app/1".into(), units: Units::Mm, variables: vec![], features: vec![], rollback: None, next_id: 0 }
    }
}

impl Document {
    pub fn new_id(&mut self) -> FeatId {
        self.next_id += 1;
        self.next_id
    }

    pub fn rollback_index(&self) -> usize {
        self.rollback.unwrap_or(self.features.len()).min(self.features.len())
    }

    pub fn feature(&self, id: FeatId) -> Option<&Feature> {
        self.features.iter().find(|f| f.id == id)
    }

    pub fn feature_mut(&mut self, id: FeatId) -> Option<&mut Feature> {
        self.features.iter_mut().find(|f| f.id == id)
    }

    pub fn index_of(&self, id: FeatId) -> Option<usize> {
        self.features.iter().position(|f| f.id == id)
    }

    /// A fresh "Extrude 2"-style name.
    pub fn next_name(&self, kind: &str) -> String {
        let label = kind_label(kind);
        let n = self.features.iter().filter(|f| f.op.kind() == kind).count() + 1;
        format!("{label} {n}")
    }

    /// Insert at the rollback bar and move the bar past it.
    pub fn insert(&mut self, name: String, op: Op) -> FeatId {
        let id = self.new_id();
        let at = self.rollback_index();
        self.features.insert(at, Feature { id, name, suppressed: false, hidden: false, op });
        if self.rollback.is_some() {
            self.rollback = Some(at + 1);
        }
        id
    }

    pub fn var_values(&self) -> (HashMap<String, f64>, Vec<Option<String>>) {
        let pairs: Vec<(String, String)> = self.variables.iter().map(|v| (v.name.clone(), v.expr.clone())).collect();
        expr::resolve_vars(&pairs, self.units)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> Result<Document, String> {
        serde_json::from_str(s).map_err(|e| e.to_string())
    }
}

/// Evaluate a feature parameter.
pub fn eval(src: &str, kind: Kind, vars: &HashMap<String, f64>, units: Units) -> Result<f64, String> {
    expr::eval(src, kind, &Env { vars, units })
}
