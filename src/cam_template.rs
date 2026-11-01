//! Operation templates: a setup's operations and their tools saved under a name, to add to other setups.
//! Kept in ~/.config/cad-rust-app/templates/, one JSON file each.

use serde::{Deserialize, Serialize};

use crate::cam::{Cam, OpKind, Operation, Tool};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Template {
    pub name: String,
    pub tools: Vec<Tool>,
    pub ops: Vec<Operation>,
}

fn dir() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        return Some(std::env::temp_dir().join(format!("cam-templates-{}", std::process::id())));
    }
    crate::cam::library_path().and_then(|p| p.parent().map(|d| d.join("templates")))
}

fn file_name(name: &str) -> String {
    let s: String = name.trim().chars().map(|c| if c.is_alphanumeric() || "-_ ".contains(c) { c } else { '_' }).collect();
    format!("{}.json", if s.is_empty() { "template".into() } else { s })
}

/// Saved templates, by name.
pub fn list() -> Vec<Template> {
    let Some(d) = dir() else { return vec![] };
    let mut v: Vec<Template> = std::fs::read_dir(d)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<Template>(&t).ok())
        .filter(|t| !t.name.is_empty())
        .collect();
    v.sort_by_key(|t| t.name.to_lowercase());
    v
}

pub fn find(name: &str) -> Result<Template, String> {
    let all = list();
    let n = name.trim().to_lowercase();
    all.iter().find(|t| t.name.to_lowercase() == n).cloned().ok_or_else(|| {
        let names: Vec<&str> = all.iter().map(|t| t.name.as_str()).collect();
        format!("no template named '{name}' (saved: {})", if names.is_empty() { "none".into() } else { names.join(", ") })
    })
}

pub fn save(t: &Template) -> Result<std::path::PathBuf, String> {
    let d = dir().ok_or("no config folder")?;
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let p = d.join(file_name(&t.name));
    std::fs::write(&p, serde_json::to_string_pretty(t).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(p)
}

pub fn delete(name: &str) -> Result<(), String> {
    let t = find(name)?;
    let d = dir().ok_or("no config folder")?;
    std::fs::remove_file(d.join(file_name(&t.name))).map_err(|e| e.to_string())
}

/// A template of the operations `ids` (none = every operation that isn't suppressed) and their tools.
/// What belongs to this part is left out: heights, picked holes, points, edges and sketches.
pub fn from_cam(cam: &Cam, name: &str, ids: &[u64]) -> Template {
    let ops: Vec<Operation> = cam
        .ops
        .iter()
        .filter(|o| if ids.is_empty() { !o.suppressed } else { ids.contains(&o.id) })
        .map(|o| Operation { top: None, bottom: None, at: None, points: vec![], edges: vec![], edge_picks: vec![], sketches: vec![], ..o.clone() })
        .collect();
    let tools = cam.tools.iter().filter(|t| ops.iter().any(|o| o.tool == t.id)).cloned().collect();
    Template { name: name.trim().to_string(), tools, ops }
}

fn same_tool(a: &Tool, b: &Tool) -> bool {
    let eq = |x: f64, y: f64| (x - y).abs() < 1e-6;
    a.kind == b.kind && eq(a.diameter, b.diameter) && eq(a.corner_radius, b.corner_radius) && eq(a.angle, b.angle) && eq(a.pitch, b.pitch) && eq(a.tip_diameter, b.tip_diameter)
}

/// Add the template's operations to a setup, reusing its tools where one is the same cutter and adding
/// the others. Trace and V-carve operations follow `sketch`. Returns the new operations' ids.
pub fn apply(cam: &mut Cam, t: &Template, sketch: Option<u64>) -> Vec<u64> {
    let mut map = std::collections::HashMap::new();
    for tool in &t.tools {
        let id = match cam.tools.iter().find(|x| same_tool(x, tool)) {
            Some(x) => x.id,
            None => cam.add_tool(tool.clone()),
        };
        map.insert(tool.id, id);
    }
    let mut ids = vec![];
    for op in &t.ops {
        let mut o = op.clone();
        o.id = cam.new_id();
        o.tool = map.get(&op.tool).copied().unwrap_or(0);
        if matches!(o.kind, OpKind::Trace | OpKind::VCarve) {
            o.sketches = sketch.into_iter().collect();
        }
        // keep names unique
        let base = o.name.clone();
        let mut k = 2;
        while cam.ops.iter().any(|x| x.name == o.name) {
            o.name = format!("{base} ({k})");
            k += 1;
        }
        ids.push(o.id);
        cam.ops.push(o);
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::ToolKind;

    #[test]
    fn templates_round_trip_and_reuse_tools() {
        let d = dir().unwrap();
        let mut c = Cam::default();
        let flat = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && t.diameter == 6.0).unwrap();
        let drill = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Drill).unwrap();
        let (f, dr) = (c.add_tool(flat.clone()), c.add_tool(drill));
        let a = c.add_op(OpKind::Face, f);
        c.op_mut(a).unwrap().bottom = Some(-3.0);
        c.op_mut(a).unwrap().stepover = Some(0.6);
        c.add_op(OpKind::Drill, dr);
        let t = from_cam(&c, "Plate job", &[]);
        assert_eq!((t.ops.len(), t.tools.len()), (2, 2));
        assert_eq!((t.ops[0].bottom, t.ops[0].stepover), (None, Some(0.6)));
        save(&t).unwrap();
        assert_eq!(find("plate JOB").unwrap(), t);
        // a setup that already has the flat end mill gets the drill added and both operations
        let mut other = Cam::default();
        let mine = other.add_tool(flat);
        let ids = apply(&mut other, &find("Plate job").unwrap(), None);
        assert_eq!((ids.len(), other.tools.len()), (2, 2));
        assert_eq!(other.op(ids[0]).unwrap().tool, mine);
        assert_eq!(other.op(ids[1]).unwrap().kind, OpKind::Drill);
        // again: same tools, new names
        let again = apply(&mut other, &t, None);
        assert_eq!(other.tools.len(), 2);
        assert_eq!(other.op(again[0]).unwrap().name, "Face 1 (2)");
        delete("Plate job").unwrap();
        assert!(find("Plate job").is_err());
        let _ = std::fs::remove_dir_all(d);
    }
}
