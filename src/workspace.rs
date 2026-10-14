//! A workspace holds every tab of a project: Part Studios, Assemblies and
//! Drawings, like an Onshape document.

use serde::{Deserialize, Serialize};

use crate::assembly::Assembly;
use crate::doc::Document;
use crate::drawing::Drawing;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Studio {
    pub name: String,
    pub doc: Document,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Tab {
    Studio(usize),
    Assembly(usize),
    Drawing(usize),
}

impl Default for Tab {
    fn default() -> Self {
        Tab::Studio(0)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub format: String,
    pub studios: Vec<Studio>,
    #[serde(default)]
    pub assemblies: Vec<Assembly>,
    #[serde(default)]
    pub drawings: Vec<Drawing>,
    #[serde(default)]
    pub active: Tab,
}

impl Default for Workspace {
    fn default() -> Self {
        Workspace {
            format: "cad-rust-app/workspace-1".into(),
            studios: vec![Studio { name: "Part Studio 1".into(), doc: Document::default() }],
            assemblies: vec![],
            drawings: vec![],
            active: Tab::Studio(0),
        }
    }
}

impl Workspace {
    /// Load a workspace, or a single Part Studio document saved by older versions.
    pub fn from_json(s: &str) -> Result<Workspace, String> {
        if let Ok(w) = serde_json::from_str::<Workspace>(s) {
            if w.format.starts_with("cad-rust-app/workspace") {
                return Ok(w);
            }
        }
        let doc = Document::from_json(s)?;
        Ok(Workspace { studios: vec![Studio { name: "Part Studio 1".into(), doc }], ..Default::default() })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}
