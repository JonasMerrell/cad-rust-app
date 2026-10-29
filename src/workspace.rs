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
    /// Manufacture (CAM) tab.
    Cam(usize),
    /// Simulation (FEA) tab.
    Sim(usize),
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
    /// Manufacture tabs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cams: Vec<crate::cam::Cam>,
    /// Simulation tabs.
    #[serde(default, rename = "simulations", skip_serializing_if = "Vec::is_empty")]
    pub sims: Vec<crate::fea::Study>,
    #[serde(default)]
    pub active: Tab,
    /// Saved snapshots (like Onshape versions), each a workspace JSON without versions.
    #[serde(default)]
    pub versions: Vec<Version>,
    /// The order of the tab bar, as the user arranged it; tabs not listed follow, by type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tab_order: Vec<Tab>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Version {
    pub name: String,
    pub created: String,
    pub data: String,
}

/// Current UTC time as "YYYY-MM-DD HH:MM".
pub fn now_string() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // civil from days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, rem % 3600 / 60)
}

impl Default for Workspace {
    fn default() -> Self {
        Workspace {
            format: "cad-rust-app/workspace-1".into(),
            studios: vec![Studio { name: "Part Studio 1".into(), doc: Document::default() }],
            assemblies: vec![],
            drawings: vec![],
            cams: vec![],
            sims: vec![],
            active: Tab::Studio(0),
            versions: vec![],
            tab_order: vec![],
        }
    }
}

impl Workspace {
    /// Snapshot everything except the version list.
    pub fn make_version(&mut self, name: String) {
        let mut copy = self.clone();
        copy.versions.clear();
        self.versions.push(Version { name, created: now_string(), data: copy.to_json() });
    }

    /// Replace the content with a version, keeping the version list.
    pub fn restore(&mut self, i: usize) -> Result<(), String> {
        let v = self.versions.get(i).ok_or("no such version")?;
        let mut w: Workspace = serde_json::from_str(&v.data).map_err(|e| e.to_string())?;
        w.versions = std::mem::take(&mut self.versions);
        *self = w;
        Ok(())
    }

    /// Every tab in tab-bar order.
    pub fn tabs(&self) -> Vec<Tab> {
        let all = (0..self.studios.len())
            .map(Tab::Studio)
            .chain((0..self.assemblies.len()).map(Tab::Assembly))
            .chain((0..self.drawings.len()).map(Tab::Drawing))
            .chain((0..self.cams.len()).map(Tab::Cam))
            .chain((0..self.sims.len()).map(Tab::Sim));
        let all: Vec<Tab> = all.collect();
        let mut out: Vec<Tab> = self.tab_order.iter().copied().filter(|t| all.contains(t)).collect();
        out.dedup();
        out.extend(all.into_iter().filter(|t| !self.tab_order.contains(t)));
        out
    }

    /// Move a tab `by` places along the tab bar (negative is left).
    pub fn move_tab(&mut self, t: Tab, by: isize) {
        let mut v = self.tabs();
        let Some(i) = v.iter().position(|x| *x == t) else { return };
        let j = (i as isize + by).clamp(0, v.len() as isize - 1) as usize;
        let x = v.remove(i);
        v.insert(j, x);
        self.tab_order = v;
    }

    /// Keep the tab order right after tab `t` is removed (later tabs of its kind move down one).
    pub fn forget_tab(&mut self, t: Tab) {
        let same = |a: Tab, b: Tab| std::mem::discriminant(&a) == std::mem::discriminant(&b);
        let idx = |t: Tab| match t {
            Tab::Studio(i) | Tab::Assembly(i) | Tab::Drawing(i) | Tab::Cam(i) | Tab::Sim(i) => i,
        };
        let with = |t: Tab, i: usize| match t {
            Tab::Studio(_) => Tab::Studio(i),
            Tab::Assembly(_) => Tab::Assembly(i),
            Tab::Drawing(_) => Tab::Drawing(i),
            Tab::Cam(_) => Tab::Cam(i),
            Tab::Sim(_) => Tab::Sim(i),
        };
        self.tab_order.retain(|x| *x != t);
        for x in &mut self.tab_order {
            if same(*x, t) && idx(*x) > idx(t) {
                *x = with(*x, idx(*x) - 1);
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_old_files() {
        let mut w = Workspace::default();
        w.studios[0].doc.variables.push(crate::doc::Variable { name: "w".into(), expr: "10".into() });
        w.make_version("V1".into());
        w.studios[0].doc.variables[0].expr = "20".into();
        w.restore(0).unwrap();
        assert_eq!(w.studios[0].doc.variables[0].expr, "10");
        assert_eq!(w.versions.len(), 1);
        // a single-document file loads as one Part Studio
        let old = crate::doc::Document::default().to_json();
        let w2 = Workspace::from_json(&old).unwrap();
        assert_eq!(w2.studios.len(), 1);
        assert!(now_string().len() >= 16);
    }

    #[test]
    fn tab_order() {
        let mut w = Workspace::default();
        w.studios.push(Studio { name: "B".into(), doc: Document::default() });
        w.assemblies.push(Default::default());
        assert_eq!(w.tabs(), vec![Tab::Studio(0), Tab::Studio(1), Tab::Assembly(0)]);
        w.move_tab(Tab::Assembly(0), -2);
        assert_eq!(w.tabs(), vec![Tab::Assembly(0), Tab::Studio(0), Tab::Studio(1)]);
        // a new tab goes at the end
        w.drawings.push(Default::default());
        assert_eq!(w.tabs().last(), Some(&Tab::Drawing(0)));
        // removing a studio renumbers the later one
        w.studios.remove(0);
        w.forget_tab(Tab::Studio(0));
        assert_eq!(w.tabs(), vec![Tab::Assembly(0), Tab::Studio(0), Tab::Drawing(0)]);
        let back = Workspace::from_json(&w.to_json()).unwrap();
        assert_eq!(back.tabs(), w.tabs());
    }

    #[test]
    fn configuration_overrides_variables() {
        let mut d = crate::doc::Document::default();
        d.variables.push(crate::doc::Variable { name: "w".into(), expr: "10".into() });
        let mut c = crate::doc::Config { name: "Big".into(), values: Default::default() };
        c.values.insert("w".into(), "30".into());
        d.configs.push(c);
        assert_eq!(d.var_values().0["w"], 10.0);
        d.active_config = Some(0);
        assert_eq!(d.var_values().0["w"], 30.0);
    }
}
