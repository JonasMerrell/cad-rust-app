//! Drawings: projected views of parts on a sheet, with dimensions.

use serde::{Deserialize, Serialize};

use crate::doc::BodyId;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DrawView {
    pub id: u64,
    /// Part Studio to project.
    pub studio: usize,
    /// "front", "top", "right", "back", "bottom", "left", "iso", or "section:<front|top|right|…>"
    pub dir: String,
    /// Centre on the sheet in mm.
    pub pos: [f64; 2],
    pub scale: f64,
    #[serde(default)]
    pub hidden_lines: bool,
    /// Detail view: (source view id, centre in the source view's model coordinates, radius in mm).
    #[serde(default)]
    pub detail: Option<(u64, [f64; 2], f64)>,
    /// Sheet the view is on (0-based).
    #[serde(default)]
    pub sheet: usize,
    /// Only this part of the Part Studio (every part when None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<BodyId>,
    /// An assembly (index) shown with its parts at their poses, in place of a Part Studio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assembly: Option<usize>,
    /// Section views: the cutting plane as p·(view direction), mm; None cuts through the middle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut: Option<f64>,
    /// Section and detail views: their letter (A-A, DETAIL B).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub letter: String,
    /// Section views: the view the cutting line is drawn on (found automatically when None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    /// Keep auto-dimensions of this view generated from the model.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto: bool,
    /// Auto-dimensions the user deleted (their keys), so they are not made again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_off: Vec<String>,
    /// Assembly views: item balloons linked to the BOM.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub balloons: bool,
}

/// Title block fields.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TitleBlock {
    /// Part name (the shown part's name when empty).
    #[serde(default)]
    pub part: String,
    #[serde(default)]
    pub part_no: String,
    /// Material (the part's material when empty).
    #[serde(default)]
    pub material: String,
    /// Raw stock under the material (the part's stock size when empty; "none" leaves it out).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stock: String,
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub drawn_by: String,
    /// General tolerance note, e.g. "ISO 2768-m" or "±0.1".
    #[serde(default)]
    pub tolerance: String,
    /// First-angle projection (third-angle otherwise).
    #[serde(default)]
    pub first_angle: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Drawing {
    pub name: String,
    /// "A4", "A3", "A2", "Letter", "Tabloid"
    pub sheet: String,
    pub views: Vec<DrawView>,
    #[serde(default)]
    pub dims: Vec<DrawDim>,
    #[serde(default)]
    pub notes: Vec<Note>,
    pub next_id: u64,
    /// Bill of materials table: (assembly index, top-left on the sheet).
    #[serde(default)]
    pub bom: Option<(usize, [f64; 2])>,
    #[serde(default)]
    pub bom_sheet: usize,
    /// Number of sheets (0 is read as 1).
    #[serde(default)]
    pub sheets: usize,
    /// Geometric tolerances and datum symbols.
    #[serde(default)]
    pub gdts: Vec<Gdt>,
    #[serde(default)]
    pub title: TitleBlock,
    /// Decimal places of dimension values (trailing zeros trimmed when None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decimals: Option<u8>,
}

impl Drawing {
    pub fn sheet_count(&self) -> usize {
        self.sheets.max(1)
    }
}

/// GD&T annotation: a feature control frame, or a datum feature symbol when `kind` is "datum".
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Gdt {
    pub id: u64,
    #[serde(default)]
    pub sheet: usize,
    /// Bottom-left of the frame on the sheet (mm).
    pub pos: [f64; 2],
    /// One of GDT_KINDS, or "datum".
    pub kind: String,
    /// Tolerance text, e.g. "Ø0.1 M"; for a datum, its letter.
    pub tol: String,
    /// Datum references, space separated ("A B C").
    #[serde(default)]
    pub datums: String,
    /// Point the leader arrow (or datum triangle) touches, on the sheet.
    #[serde(default)]
    pub leader: Option<[f64; 2]>,
}

pub const GDT_KINDS: &[(&str, &str)] = &[
    ("position", "Position"),
    ("flatness", "Flatness"),
    ("straightness", "Straightness"),
    ("circularity", "Circularity"),
    ("cylindricity", "Cylindricity"),
    ("perpendicularity", "Perpendicularity"),
    ("parallelism", "Parallelism"),
    ("angularity", "Angularity"),
    ("profileLine", "Profile of a line"),
    ("profileSurface", "Profile of a surface"),
    ("concentricity", "Concentricity"),
    ("symmetry", "Symmetry"),
    ("runout", "Circular runout"),
    ("totalRunout", "Total runout"),
];

/// A dimension between two points of a view, in the view's model coordinates (mm).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DrawDim {
    pub id: u64,
    pub view: u64,
    pub a: [f64; 2],
    pub b: [f64; 2],
    /// 0 aligned, 1 horizontal, 2 vertical, 3 radius, 4 diameter, 5 angle.
    /// Radius/diameter: `a` is the centre and `b` a point on the circle; `offset` is the leader length.
    /// Angle: `a`-`b` and `extra[0]`-`extra[1]` are the two lines; `offset` is the arc radius.
    pub kind: u8,
    pub offset: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<[f64; 2]>,
    /// Text before the value, e.g. "4× ".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    /// Text after the value; lines after the first go underneath (hole callouts: " THRU\n⌴Ø16 ↧5").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub suffix: String,
    /// What `a`, `b` and `extra` are attached to: model edges of the view, looked up again
    /// whenever the view is projected, so the dimension follows the model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<Option<Anchor>>,
    /// An anchor could not be found after the model changed: the value is out of date.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dangling: bool,
    /// Made by auto-dimension (a key saying which); regenerated from the model.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auto: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tol: Option<DimTol>,
}

/// A point on a model edge as seen in a view (see kernel::ProjEdge).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Anchor {
    /// The part (body id, or (item, 0) in an assembly view) and the edge's index in it.
    pub part: BodyId,
    pub edge: u32,
    /// The edge's kind when picked (0 straight, 1 circle, 2 other), to notice a different edge.
    pub kind: u8,
    /// 0 start, 1 end, 2 centre of a circle or arc, 3 at fraction `t` of its length, 4 on a circle at angle `t` (radians).
    pub at: u8,
    #[serde(default)]
    pub t: f64,
}

/// Dimension tolerance.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DimTol {
    /// ±value
    Symmetric { value: f64 },
    /// +upper / −lower deviations
    Deviation { upper: f64, lower: f64 },
    /// The two limit sizes (nominal + upper, nominal − lower)
    Limits { upper: f64, lower: f64 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Note {
    pub id: u64,
    #[serde(default)]
    pub sheet: usize,
    pub pos: [f64; 2],
    pub text: String,
}
