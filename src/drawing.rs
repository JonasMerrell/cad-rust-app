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
}

/// A dimension between two points on the sheet (mm), measured in model units through the view scale.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DrawDim {
    pub id: u64,
    pub view: u64,
    pub a: [f64; 2],
    pub b: [f64; 2],
    /// 0 aligned, 1 horizontal, 2 vertical
    pub kind: u8,
    pub offset: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Note {
    pub id: u64,
    pub pos: [f64; 2],
    pub text: String,
}
