//! Drawings: projected views of parts on a sheet, with dimensions.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DrawView {
    pub id: u64,
    /// Part Studio to project.
    pub studio: usize,
    /// "front", "top", "right", "back", "bottom", "left" or "iso"
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

/// A dimension between two points on the sheet (mm), measured in model units through the view scale.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Note {
    pub id: u64,
    #[serde(default)]
    pub sheet: usize,
    pub pos: [f64; 2],
    pub text: String,
}
