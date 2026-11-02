//! The part document: variables, the ordered feature list and references
//! between features and model geometry.

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
/// them and, once the kernel has seen them, by a persistent name (see `TopoName`),
/// so the reference survives rebuilds that renumber or move topology.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Ref {
    OriginPlane {
        plane: OriginPlane,
    },
    /// A Plane feature or a sketch's plane.
    FeaturePlane {
        feature: FeatId,
    },
    Face {
        point: [f64; 3],
        normal: [f64; 3],
        #[serde(default, skip_serializing_if = "TopoName::is_empty")]
        name: TopoName,
    },
    Edge {
        point: [f64; 3],
        #[serde(default, skip_serializing_if = "TopoName::is_empty")]
        name: TopoName,
    },
    Vertex {
        point: [f64; 3],
    },
    Region {
        sketch: FeatId,
        point: [f64; 2],
    },
    SketchCurve {
        sketch: FeatId,
        curve: SkId,
    },
    SketchPoint {
        sketch: FeatId,
        point: SkId,
    },
    Body {
        body: BodyId,
    },
    Axis {
        axis: u8,
    },
    /// A whole feature (picked in the feature list), e.g. for feature patterns.
    Feature {
        feature: FeatId,
    },
    /// A curve produced by a feature (e.g. a helix).
    Curve {
        feature: FeatId,
    },
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
            Ref::Feature { .. } => "Feature".into(),
            Ref::Curve { .. } => "Curve".into(),
        }
    }
    pub fn is_planar(&self) -> bool {
        matches!(self, Ref::OriginPlane { .. } | Ref::FeaturePlane { .. } | Ref::Face { .. })
    }
}

/// What a model face is, independent of where it currently is: the feature that made it
/// and its role there. Roles: "start" and "end" (the caps of an extrude or a partial
/// revolve), "side" (made by sketch curve `n`), "wall" (of the hole at sketch point `n`),
/// "fillet" and "chamfer" (made from the feature's `n`-th edge).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FaceName {
    pub feature: FeatId,
    pub role: String,
    #[serde(default)]
    pub n: u64,
}

/// Persistent name of a picked face (one `FaceName`) or edge (the two faces it joins).
/// Empty for picks the kernel could not name (imported parts, faces a later feature
/// reshaped) and in older documents; those are found by their point alone.
/// Names never take part in comparisons: two refs to the same spot are the same pick.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TopoName(pub Vec<FaceName>);

impl TopoName {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl PartialEq for TopoName {
    fn eq(&self, _: &TopoName) -> bool {
        true
    }
}

/// A reference the kernel resolved during a rebuild, with its name filled in and its
/// point moved onto where the geometry is now. Applied with `Document::apply_ref_updates`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefUpdate {
    pub feature: FeatId,
    pub old: Ref,
    pub new: Ref,
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
    UpToNext,
    UpToVertex,
}

impl EndType {
    pub const ALL: [EndType; 6] = [EndType::Blind, EndType::Symmetric, EndType::ThroughAll, EndType::UpToNext, EndType::UpToFace, EndType::UpToVertex];
    pub fn label(self) -> &'static str {
        match self {
            EndType::Blind => "Blind",
            EndType::Symmetric => "Symmetric",
            EndType::ThroughAll => "Through all",
            EndType::UpToFace => "Up to face",
            EndType::UpToNext => "Up to next",
            EndType::UpToVertex => "Up to vertex",
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
        /// "offset", "angle" (about a line), "threePoint", "normalToCurve", "midPlane"
        #[serde(default = "plane_mode")]
        mode: String,
        #[serde(default)]
        refs: Vec<Ref>,
        #[serde(default = "zero")]
        angle: String,
        #[serde(default)]
        flip: bool,
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
        /// "equal", "two" (two distances) or "angle" (distance and angle)
        #[serde(default = "equal")]
        mode: String,
        #[serde(default = "one")]
        distance2: String,
        #[serde(default = "deg45")]
        angle: String,
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
        /// "part" or "feature"
        #[serde(default = "part")]
        kind: String,
        bodies: Vec<Ref>,
        direction: Option<Ref>,
        spacing: String,
        count: String,
        second: Option<Ref>,
        spacing2: String,
        count2: String,
    },
    /// Copies spaced evenly along a curve, optionally turning with it.
    CurvePattern {
        #[serde(default = "part")]
        kind: String,
        bodies: Vec<Ref>,
        path: Vec<Ref>,
        count: String,
        /// Turn each copy to follow the curve's direction.
        #[serde(default)]
        align: bool,
    },
    /// A copy at each selected sketch point (moved from the first point, or the seed's centre).
    PointPattern {
        #[serde(default = "part")]
        kind: String,
        bodies: Vec<Ref>,
        points: Vec<Ref>,
    },
    CircularPattern {
        #[serde(default = "part")]
        kind: String,
        bodies: Vec<Ref>,
        axis: Option<Ref>,
        count: String,
        angle: String,
    },
    Mirror {
        #[serde(default = "part")]
        kind: String,
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
    Thicken {
        faces: Vec<Ref>,
        thickness: String,
        #[serde(default)]
        opposite: bool,
        op: BoolOp,
    },
    Helix {
        axis: Option<Ref>,
        radius: String,
        pitch: String,
        turns: String,
        #[serde(default)]
        left: bool,
    },
    Transform {
        bodies: Vec<Ref>,
        /// "translate" or "rotate"
        mode: String,
        direction: Option<Ref>,
        distance: String,
        axis: Option<Ref>,
        angle: String,
        copy: bool,
    },
    DeleteFace {
        faces: Vec<Ref>,
    },
    /// Surface swept straight from curves.
    SurfaceExtrude {
        curves: Vec<Ref>,
        depth: String,
        #[serde(default)]
        symmetric: bool,
        #[serde(default)]
        opposite: bool,
    },
    SurfaceRevolve {
        curves: Vec<Ref>,
        axis: Option<Ref>,
        angle: String,
    },
    /// Surface through profile curves (one profile per sketch or edge).
    SurfaceLoft {
        curves: Vec<Ref>,
        #[serde(default)]
        ruled: bool,
    },
    /// Surface patch filling a closed boundary.
    Fill {
        edges: Vec<Ref>,
    },
    OffsetSurface {
        faces: Vec<Ref>,
        distance: String,
    },
    /// Join surfaces along shared edges, optionally into a solid.
    Knit {
        bodies: Vec<Ref>,
        #[serde(default)]
        solid: bool,
    },
    /// Bent wall along a sheet metal edge.
    Flange {
        edges: Vec<Ref>,
        length: String,
        angle: String,
        /// Bend downwards instead of up.
        #[serde(default)]
        flip: bool,
        /// Keep the flange this far from each end of the edge, with bend reliefs.
        #[serde(default = "zero")]
        inset: String,
    },
    /// Flat tab added to a sheet metal part from a sketch region on its face.
    Tab {
        profiles: Vec<Ref>,
    },
    /// Make a sheet metal part from a solid: hollow it, removing the chosen faces.
    ConvertSheet {
        faces: Vec<Ref>,
        thickness: String,
    },
    /// Parts brought in from another Part Studio.
    Derived {
        studio: usize,
        /// Bodies to bring in ([feature, index]); empty means all.
        #[serde(default)]
        bodies: Vec<BodyId>,
    },
    /// Push or pull planar faces along their normals.
    MoveFace {
        faces: Vec<Ref>,
        distance: String,
    },
    /// Repair parts: drop slivers and tiny edges/faces, close small gaps.
    Heal {
        bodies: Vec<Ref>,
        /// Features smaller than this are removed and gaps up to it are closed.
        tolerance: String,
    },
    /// A user-defined feature: runs the document's script `name` with these values.
    Custom {
        name: String,
        /// Parameter name -> expression.
        #[serde(default)]
        values: std::collections::BTreeMap<String, String>,
        #[serde(default)]
        refs: Vec<Ref>,
        /// What `refs` holds: "points" or "faces" (copied from the definition).
        #[serde(default)]
        select: String,
        #[serde(default)]
        op: BoolOp,
    },
    /// Rib from sketch lines: thickness about the sketch plane, filled towards the part.
    Rib {
        profile: Vec<Ref>,
        thickness: String,
        flip: bool,
    },
    /// Sheet metal part from a chain of sketch lines (bent) or a region (flat plate).
    SheetMetal {
        profile: Vec<Ref>,
        thickness: String,
        radius: String,
        k_factor: String,
        depth: String,
        /// Put the material on the other side of the sketch lines.
        flip: bool,
        op: BoolOp,
    },
    Import {
        name: String,
        /// STEP text from older documents (newer ones use `data`).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        step: String,
        /// "step", "iges", "stl" or "obj"; empty means STEP/IGES in `step`.
        #[serde(default)]
        format: String,
        /// The file, compressed and base64-encoded, so the document is self-contained.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        data: String,
        /// Scale factor applied on import, e.g. 25.4 for a model drawn in inches.
        #[serde(default = "one")]
        scale: String,
        /// Meshes: merge coplanar triangles into flat faces (easier to sketch on and fillet).
        #[serde(default = "yes")]
        merge: bool,
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
            Op::CurvePattern { .. } => "curvePattern",
            Op::PointPattern { .. } => "pointPattern",
            Op::Mirror { .. } => "mirror",
            Op::Boolean { .. } => "boolean",
            Op::Split { .. } => "split",
            Op::DeleteBody { .. } => "deleteBody",
            Op::Import { .. } => "import",
            Op::Thicken { .. } => "thicken",
            Op::Helix { .. } => "helix",
            Op::Transform { .. } => "transform",
            Op::DeleteFace { .. } => "deleteFace",
            Op::SheetMetal { .. } => "sheetMetal",
            Op::MoveFace { .. } => "moveFace",
            Op::Derived { .. } => "derived",
            Op::Flange { .. } => "flange",
            Op::Tab { .. } => "tab",
            Op::ConvertSheet { .. } => "convertSheet",
            Op::SurfaceExtrude { .. } => "surfaceExtrude",
            Op::SurfaceRevolve { .. } => "surfaceRevolve",
            Op::SurfaceLoft { .. } => "surfaceLoft",
            Op::Fill { .. } => "fill",
            Op::OffsetSurface { .. } => "offsetSurface",
            Op::Knit { .. } => "knit",
            Op::Rib { .. } => "rib",
            Op::Custom { .. } => "custom",
            Op::Heal { .. } => "heal",
        }
    }

    /// Default parameters for a new feature of `kind`, seeded with the current picks.
    pub fn new_default(kind: &str) -> Option<Op> {
        let s = |v: &str| v.to_string();
        Some(match kind {
            "plane" => Op::Plane { base: None, offset: s("10"), mode: s("offset"), refs: vec![], angle: s("45"), flip: false },
            "thicken" => Op::Thicken { faces: vec![], thickness: s("2"), opposite: false, op: BoolOp::New },
            "helix" => Op::Helix { axis: None, radius: s("10"), pitch: s("5"), turns: s("4"), left: false },
            "transform" => Op::Transform { bodies: vec![], mode: s("translate"), direction: None, distance: s("10"), axis: None, angle: s("90"), copy: false },
            "deleteFace" => Op::DeleteFace { faces: vec![] },
            "derived" => Op::Derived { studio: 0, bodies: vec![] },
            "flange" => Op::Flange { edges: vec![], length: s("20"), angle: s("90"), flip: false, inset: s("0") },
            "tab" => Op::Tab { profiles: vec![] },
            "convertSheet" => Op::ConvertSheet { faces: vec![], thickness: s("1") },
            "surfaceExtrude" => Op::SurfaceExtrude { curves: vec![], depth: s("10"), symmetric: false, opposite: false },
            "surfaceRevolve" => Op::SurfaceRevolve { curves: vec![], axis: None, angle: s("360") },
            "surfaceLoft" => Op::SurfaceLoft { curves: vec![], ruled: false },
            "fill" => Op::Fill { edges: vec![] },
            "offsetSurface" => Op::OffsetSurface { faces: vec![], distance: s("2") },
            "knit" => Op::Knit { bodies: vec![], solid: true },
            "moveFace" => Op::MoveFace { faces: vec![], distance: s("2") },
            "rib" => Op::Rib { profile: vec![], thickness: s("2"), flip: false },
            "heal" => Op::Heal { bodies: vec![], tolerance: s("0.01") },
            "custom" => Op::Custom { name: String::new(), values: Default::default(), refs: vec![], select: String::new(), op: BoolOp::New },
            "sheetMetal" => Op::SheetMetal { profile: vec![], thickness: s("1"), radius: s("1"), k_factor: s("0.45"), depth: s("40"), flip: false, op: BoolOp::New },
            "extrude" => {
                Op::Extrude { profiles: vec![], end: EndType::Blind, depth: s("10"), opposite: false, second: false, depth2: s("10"), up_to: None, op: BoolOp::Add, draft: s("0") }
            }
            "revolve" => Op::Revolve { profiles: vec![], axis: None, mode: s("full"), angle: s("360"), op: BoolOp::New },
            "sweep" => Op::Sweep { profiles: vec![], path: vec![], op: BoolOp::New },
            "loft" => Op::Loft { profiles: vec![], ruled: false, op: BoolOp::New },
            "fillet" => Op::Fillet { edges: vec![], radius: s("2"), variable: false, end_radius: s("4") },
            "chamfer" => Op::Chamfer { edges: vec![], distance: s("1"), mode: s("equal"), distance2: s("1"), angle: s("45") },
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
            "linearPattern" => {
                Op::LinearPattern { kind: s("part"), bodies: vec![], direction: None, spacing: s("20"), count: s("3"), second: None, spacing2: s("20"), count2: s("1") }
            }
            "curvePattern" => Op::CurvePattern { kind: s("part"), bodies: vec![], path: vec![], count: s("5"), align: true },
            "pointPattern" => Op::PointPattern { kind: s("feature"), bodies: vec![], points: vec![] },
            "circularPattern" => Op::CircularPattern { kind: s("part"), bodies: vec![], axis: None, count: s("4"), angle: s("360") },
            "mirror" => Op::Mirror { kind: s("part"), bodies: vec![], plane: None },
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
        "curvePattern" => "Curve pattern",
        "pointPattern" => "Sketch point pattern",
        "mirror" => "Mirror",
        "boolean" => "Boolean",
        "draft" => "Draft",
        "split" => "Split",
        "plane" => "Plane",
        "deleteBody" => "Delete body",
        "import" => "Import",
        "thicken" => "Thicken",
        "helix" => "Helix",
        "transform" => "Transform",
        "deleteFace" => "Delete face",
        "sheetMetal" => "Sheet metal",
        "moveFace" => "Move face",
        "derived" => "Derived",
        "flange" => "Flange",
        "tab" => "Tab",
        "convertSheet" => "Convert to sheet metal",
        "surfaceExtrude" => "Extrude surface",
        "surfaceRevolve" => "Revolve surface",
        "surfaceLoft" => "Loft surface",
        "fill" => "Fill",
        "offsetSurface" => "Offset surface",
        "knit" => "Knit",
        "rib" => "Rib",
        "custom" => "Custom feature",
        "heal" => "Heal",
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

/// Per-part settings, keyed by body id ("feature:index").
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PartProps {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    /// Material name from `MATERIALS`.
    #[serde(default)]
    pub material: Option<String>,
    /// Hidden in the viewport.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// Surface finish: "matte", "satin", "glossy", "metal" or "glass".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<String>,
    /// How its raw stock is worked out (shape, size standard, allowances); `None` = the defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stock: Option<crate::stock_size::StockPref>,
}

/// (name, density g/cm³)
pub const MATERIALS: &[(&str, f64)] = &[
    ("PLA", 1.24),
    ("ABS", 1.04),
    ("PETG", 1.27),
    ("Nylon", 1.14),
    ("ASA", 1.07),
    ("PC", 1.20),
    ("PLA-CF", 1.29),
    ("PETG-CF", 1.25),
    ("ASA-CF", 1.10),
    ("PET-CF", 1.29),
    ("Aluminium 6061", 2.70),
    ("Aluminium 7075", 2.81),
    ("Steel", 7.85),
    ("Stainless steel", 8.00),
    ("Stainless steel 303", 8.03),
    ("Tungsten alloy", 17.6),
    ("Urethane foam", 0.24),
    ("Brass", 8.50),
    ("Copper", 8.96),
    ("Titanium", 4.43),
    ("Oak", 0.75),
    ("Pine", 0.50),
];

pub fn body_key(id: BodyId) -> String {
    format!("{}:{}", id.0, id.1)
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
    #[serde(default)]
    pub parts: std::collections::BTreeMap<String, PartProps>,
    /// Named sets of variable values; the active one overrides the variables.
    #[serde(default)]
    pub configs: Vec<Config>,
    #[serde(default)]
    pub active_config: Option<usize>,
    /// Custom feature types defined by scripts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_features: Vec<crate::script::CustomDef>,
    /// Images laid on faces (labels, logos).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decals: Vec<Decal>,
    /// Default geometry hidden in the viewport: "Origin", "Top", "Front", "Right".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_defaults: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Decal {
    pub id: u64,
    pub name: String,
    /// The image file (PNG or JPEG), compressed and base64-encoded.
    pub image: String,
    /// Centre on the face, and the face's outward normal.
    pub at: [f64; 3],
    pub normal: [f64; 3],
    /// Width in mm (height follows the image's proportions).
    pub width: f64,
    /// Turn about the normal, degrees.
    #[serde(default)]
    pub angle: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub name: String,
    /// variable name -> expression
    pub values: std::collections::BTreeMap<String, String>,
}

impl Default for Document {
    fn default() -> Self {
        Document {
            format: "cad-rust-app/1".into(),
            units: Units::Mm,
            variables: vec![],
            features: vec![],
            rollback: None,
            next_id: 0,
            parts: Default::default(),
            configs: vec![],
            active_config: None,
            custom_features: vec![],
            decals: vec![],
            hidden_defaults: vec![],
        }
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

    /// `base`, or `base 2`, `base 3`, … when a feature already has that name.
    pub fn unique_name(&self, base: &str) -> String {
        let taken = |n: &str| self.features.iter().any(|f| f.name == n);
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap()
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

    pub fn var_values(&self) -> (expr::Vars, Vec<Option<String>>) {
        let cfg = self.active_config.and_then(|i| self.configs.get(i));
        let pairs: Vec<(String, String)> =
            self.variables.iter().map(|v| (v.name.clone(), cfg.and_then(|c| c.values.get(&v.name)).cloned().unwrap_or_else(|| v.expr.clone()))).collect();
        expr::resolve_vars(&pairs, self.units)
    }

    /// Store names and fresh points the kernel found for references (not an undoable
    /// edit: the model is the same). Only refs that still read exactly as the kernel saw
    /// them change. Returns whether anything changed.
    pub fn apply_ref_updates(&mut self, updates: &[RefUpdate]) -> bool {
        fn walk(v: &mut serde_json::Value, old: &serde_json::Value, new: &serde_json::Value) -> bool {
            if v == old {
                *v = new.clone();
                return true;
            }
            match v {
                serde_json::Value::Array(a) => a.iter_mut().fold(false, |c, x| walk(x, old, new) | c),
                serde_json::Value::Object(o) => o.values_mut().fold(false, |c, x| walk(x, old, new) | c),
                _ => false,
            }
        }
        let mut changed = false;
        for u in updates {
            let Some(f) = self.feature_mut(u.feature) else { continue };
            let (Ok(mut v), Ok(old), Ok(new)) = (serde_json::to_value(&f.op), serde_json::to_value(&u.old), serde_json::to_value(&u.new)) else { continue };
            if walk(&mut v, &old, &new) {
                if let Ok(op) = serde_json::from_value(v) {
                    f.op = op;
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> Result<Document, String> {
        serde_json::from_str(s).map_err(|e| e.to_string())
    }
}

fn plane_mode() -> String {
    "offset".into()
}
fn zero() -> String {
    "0".into()
}
fn equal() -> String {
    "equal".into()
}
fn one() -> String {
    "1".into()
}
fn yes() -> bool {
    true
}
fn deg45() -> String {
    "45".into()
}
fn part() -> String {
    "part".into()
}

/// Evaluate a feature parameter.
pub fn eval(src: &str, kind: Kind, vars: &expr::Vars, units: Units) -> Result<f64, String> {
    expr::eval(src, kind, &Env { vars, units })
}
