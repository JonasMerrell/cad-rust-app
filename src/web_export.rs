//! Interactive web viewer: a page (three.js, loaded from a CDN) showing parts on a tree of nodes that slide or
//! turn within their limits. Sliders, drags in the view, the arrow keys and scripted tours move them; parts can
//! be hidden or ghosted by layer, cut by a section plane and picked for their details. Markers read out a point
//! in another node's frame (a tool tip on the table) and can trace its path.
//!
//! The page is one .html file with the meshes inside (base64), or an .html beside a .json and a .bin it loads.
//! Positions are stored as 16-bit integers relative to each mesh's own box, and identical buffers (the shared
//! triangles of a part sampled at many poses) are stored once.

use std::collections::HashMap;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const TEMPLATE: &str = include_str!("web_viewer.html");

/// Triangles and edge segments, in the owning node's frame (or the part's own, see `Part::local`).
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    /// xyz triples
    pub positions: Vec<f32>,
    pub indices: Vec<u32>,
    /// Segment end points as xyz triples, two per segment.
    pub edges: Vec<f32>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JointKind {
    Slider,
    Revolute,
}

/// A joint that follows another: value = ratio · value(of) + offset (a screw turning with its nut's travel).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Drive {
    pub of: usize,
    pub ratio: f64,
    #[serde(default)]
    pub offset: f64,
}

/// How a node moves in its parent's frame: along `axis` (mm) or about it through `origin` (degrees).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Joint {
    pub kind: JointKind,
    #[serde(default)]
    pub origin: [f64; 3],
    pub axis: [f64; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<[f64; 2]>,
    /// The value the node's parts are drawn at.
    #[serde(default)]
    pub home: f64,
    /// The value the page opens at (default home).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drive: Option<Drive>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    /// Rest placement in the parent (3 × 4 row-major), applied before the joint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<[f64; 12]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joint: Option<Joint>,
    /// Offset in the parent at full explode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explode: Option<[f64; 3]>,
}

fn one() -> f32 {
    1.0
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Part {
    pub name: String,
    pub node: usize,
    pub mesh: usize,
    /// Placement in the node (3 × 4 row-major), for meshes shared between parts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<[f64; 12]>,
    /// rgb 0–1
    pub color: [f32; 3],
    #[serde(default = "one")]
    pub opacity: f32,
    /// 0 (matte) – 1 (polished)
    #[serde(default)]
    pub shine: f32,
    #[serde(default = "yes")]
    pub edges: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub info: Vec<(String, String)>,
}

/// A part whose shape changes with joint values, sampled on a grid of them; the page blends neighboring
/// samples that share their triangles and otherwise shows the nearest.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Morph {
    pub name: String,
    pub node: usize,
    pub color: [f32; 3],
    #[serde(default = "one")]
    pub opacity: f32,
    #[serde(default)]
    pub shine: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// Nodes whose joint values the grid is over.
    pub drivers: Vec<usize>,
    pub grid: Vec<Vec<f64>>,
    /// Row-major over the grid (last driver fastest); None where the part is absent.
    pub frames: Vec<Option<usize>>,
}

/// A point on a node, read out in another node's frame relative to where it is at home.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Marker {
    pub name: String,
    pub node: usize,
    pub pos: [f64; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative: Option<usize>,
    /// Trace its path in the `relative` node's frame.
    #[serde(default)]
    pub trail: bool,
    /// Axis names for the readout (default X, Y, Z).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axes: Option<[String; 3]>,
}

/// A wire box on a node (a work envelope).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BoxNote {
    pub name: String,
    pub node: usize,
    pub lo: [f64; 3],
    pub hi: [f64; 3],
}

/// One move of a tour: to these joint values, at `feed` (mm/s along the slider joints, or degrees/s) or in `time` s.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TourStep {
    pub values: Vec<(usize, f64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tour {
    pub name: String,
    pub steps: Vec<TourStep>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Scene {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub parts: Vec<Part>,
    #[serde(default)]
    pub morphs: Vec<Morph>,
    #[serde(default)]
    pub markers: Vec<Marker>,
    #[serde(default)]
    pub boxes: Vec<BoxNote>,
    #[serde(default)]
    pub tours: Vec<Tour>,
    /// How layers start out: (layer, "show" | "ghost" | "hide").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<(String, String)>,
}

impl Scene {
    pub fn node(&mut self, name: impl Into<String>, parent: Option<usize>) -> usize {
        self.nodes.push(Node { name: name.into(), parent, base: None, joint: None, explode: None });
        self.nodes.len() - 1
    }

    fn check(&self, meshes: usize) -> Result<(), String> {
        let n = self.nodes.len();
        for (i, x) in self.nodes.iter().enumerate() {
            let mut p = x.parent;
            let mut depth = 0;
            while let Some(k) = p {
                if k >= n || depth > n {
                    return Err(format!("node {i} ({}): bad or looping parent", x.name));
                }
                p = self.nodes[k].parent;
                depth += 1;
            }
            if let Some(d) = x.joint.as_ref().and_then(|j| j.drive.as_ref()) {
                if d.of >= n || d.of == i {
                    return Err(format!("node {i} ({}): drive.of must be another node", x.name));
                }
            }
        }
        for p in &self.parts {
            if p.node >= n || p.mesh >= meshes {
                return Err(format!("part {}: node or mesh out of range", p.name));
            }
        }
        for m in &self.morphs {
            let want: usize = m.grid.iter().map(|g| g.len()).product();
            if m.node >= n || m.drivers.len() != m.grid.len() || m.drivers.iter().any(|&d| d >= n) || m.frames.len() != want {
                return Err(format!("morph {}: drivers, grid and frames don't match", m.name));
            }
            if m.frames.iter().flatten().any(|&f| f >= meshes) {
                return Err(format!("morph {}: mesh out of range", m.name));
            }
        }
        for mk in &self.markers {
            if mk.node >= n || mk.relative.is_some_and(|r| r >= n) {
                return Err(format!("marker {}: node out of range", mk.name));
            }
        }
        if let Some((l, st)) = self.layers.iter().find(|(_, st)| !matches!(st.as_str(), "show" | "ghost" | "hide")) {
            return Err(format!("layer {l}: state {st} must be show, ghost or hide"));
        }
        for t in &self.tours {
            if t.steps.iter().flat_map(|s| &s.values).any(|&(k, _)| k >= n || self.nodes[k].joint.is_none()) {
                return Err(format!("tour {}: a step moves a node without a joint", t.name));
            }
        }
        Ok(())
    }
}

/// The buffers, each stored once and 4-byte aligned.
#[derive(Default)]
struct Blob {
    bytes: Vec<u8>,
    seen: HashMap<u64, Vec<(usize, usize)>>,
}

impl Blob {
    fn add(&mut self, data: &[u8]) -> usize {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        data.hash(&mut h);
        let key = h.finish();
        if let Some(hits) = self.seen.get(&key) {
            for &(off, len) in hits {
                if len == data.len() && &self.bytes[off..off + len] == data {
                    return off;
                }
            }
        }
        while self.bytes.len() % 4 != 0 {
            self.bytes.push(0);
        }
        let off = self.bytes.len();
        self.bytes.extend_from_slice(data);
        self.seen.entry(key).or_default().push((off, data.len()));
        off
    }

    fn mesh(&mut self, m: &Mesh) -> Value {
        let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for p in m.positions.chunks_exact(3).chain(m.edges.chunks_exact(3)) {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        if !lo[0].is_finite() {
            (lo, hi) = ([0.0; 3], [0.0; 3]);
        }
        let o: [f64; 3] = std::array::from_fn(|k| ((lo[k] as f64 + hi[k] as f64) / 2.0 * 1e4).round() / 1e4);
        let span = (0..3).map(|k| (hi[k] - lo[k]) as f64).fold(0.0, f64::max);
        let s = (span / 65000.0).max(1e-5);
        let quant = |v: &[f32]| -> Vec<u8> {
            v.chunks_exact(3)
                .flat_map(|p| (0..3).map(move |k| ((p[k] as f64 - o[k]) / s).round().clamp(-32767.0, 32767.0) as i16))
                .flat_map(|q| q.to_le_bytes())
                .collect()
        };
        let nv = m.positions.len() / 3;
        let v = self.add(&quant(&m.positions));
        let (idx, width): (Vec<u8>, usize) = if nv < 65536 {
            (m.indices.iter().flat_map(|&i| (i as u16).to_le_bytes()).collect(), 2)
        } else {
            (m.indices.iter().flat_map(|&i| i.to_le_bytes()).collect(), 4)
        };
        let t = self.add(&idx);
        let mut out = json!({"o": o, "s": s, "v": [v, nv], "t": [t, m.indices.len(), width]});
        if m.edges.len() >= 6 {
            let e = self.add(&quant(&m.edges));
            out["e"] = json!([e, m.edges.len() / 3]);
        }
        out
    }
}

/// What was written, for a one-line report.
pub struct Written {
    pub bytes: usize,
    pub meshes: usize,
    pub triangles: usize,
}

/// Write the page to `path` (.html). `inline`: one self-contained file; otherwise `<stem>.json` and `<stem>.bin`
/// beside it, which the page fetches (so it has to be served, not opened from disk).
pub fn write(scene: &Scene, meshes: &[Mesh], path: &std::path::Path, inline: bool) -> Result<Written, String> {
    scene.check(meshes.len())?;
    let mut blob = Blob::default();
    let refs: Vec<Value> = meshes.iter().map(|m| blob.mesh(m)).collect();
    let mut spec = serde_json::to_value(scene).map_err(|e| e.to_string())?;
    spec["meshes"] = Value::Array(refs);
    spec["format"] = json!(1);
    let spec = serde_json::to_string(&spec).map_err(|e| e.to_string())?.replace("</", "<\\/");
    let title = html_escape(&scene.title);
    let data = if inline {
        let b64 = base64::engine::general_purpose::STANDARD.encode(&blob.bytes);
        format!("window.SCENE = {spec};\nwindow.BLOB64 = \"{b64}\";")
    } else {
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "scene".into());
        let dir = path.parent().unwrap_or(std::path::Path::new("."));
        std::fs::write(dir.join(format!("{stem}.json")), &spec).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(format!("{stem}.bin")), &blob.bytes).map_err(|e| e.to_string())?;
        format!("window.SCENE_URL = {};\nwindow.BLOB_URL = {};", json!(format!("{stem}.json")), json!(format!("{stem}.bin")))
    };
    let page = TEMPLATE.replacen("__TITLE__", &title, 1).replacen("/*__DATA__*/", &data, 1);
    std::fs::write(path, &page).map_err(|e| format!("{}: {e}", path.display()))?;
    let triangles = meshes.iter().map(|m| m.indices.len() / 3).sum();
    Ok(Written { bytes: page.len() + if inline { 0 } else { blob.bytes.len() + spec.len() }, meshes: meshes.len(), triangles })
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> Mesh {
        let positions = vec![0., 0., 0., 10., 0., 0., 10., 10., 0., 0., 10., 0.];
        Mesh { positions, indices: vec![0, 1, 2, 0, 2, 3], edges: vec![0., 0., 0., 10., 0., 0.] }
    }

    #[test]
    fn writes_and_shares_buffers() {
        let mut s = Scene { title: "t".into(), ..Default::default() };
        let base = s.node("base", None);
        let slide = s.node("slide", Some(base));
        s.nodes[slide].joint = Some(Joint { kind: JointKind::Slider, origin: [0.; 3], axis: [1., 0., 0.], limits: Some([-5., 5.]), home: 0., value: None, label: None, drive: None });
        for (i, n) in [base, slide].into_iter().enumerate() {
            s.parts.push(Part { name: format!("p{i}"), node: n, mesh: i, local: None, color: [0.5; 3], opacity: 1., shine: 0., edges: true, layer: None, info: vec![] });
        }
        let dir = std::env::temp_dir().join(format!("web-export-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let w = write(&s, &[cube(), cube()], &dir.join("t.html"), false).unwrap();
        assert_eq!(w.triangles, 4);
        let bin = std::fs::read(dir.join("t.bin")).unwrap();
        // one position, index and edge buffer for the two identical meshes: 24 + 12 + 12 bytes
        assert_eq!(bin.len(), 48);
        s.parts[0].node = 7;
        assert!(write(&s, &[cube(), cube()], &dir.join("t.html"), true).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
