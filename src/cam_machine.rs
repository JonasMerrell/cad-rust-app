//! The milling machine itself in the CAM simulation: its parts (from a STEP assembly), which axis carries
//! each one, where the work sits on the table and where the tool comes out of the spindle. While the
//! simulation runs, the head, the column, the table and whatever is bolted to it are checked against each
//! other, against the stock, the fixtures and the part, and the axes against their travel.
//!
//! Frames: the model's own coordinates are those of the STEP file, with every axis where it was modelled
//! (`q = 0`). The simulation works in setup coordinates, where the work stands still; the model is moved
//! there by `shift`, and each carrier then by its displacement relative to the table.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use glam::{DVec3, Vec3};
use serde::{Deserialize, Serialize};

/// What carries a machine part: the frame or one of the linear axes.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum Carrier {
    #[default]
    Base,
    X,
    Y,
    Z,
}

pub const CARRIERS: [Carrier; 4] = [Carrier::Base, Carrier::X, Carrier::Y, Carrier::Z];

impl Carrier {
    pub fn label(self) -> &'static str {
        match self {
            Carrier::Base => "Base (doesn't move)",
            Carrier::X => "X axis",
            Carrier::Y => "Y axis",
            Carrier::Z => "Z axis",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Carrier::Base => "base",
            Carrier::X => "X axis",
            Carrier::Y => "Y axis",
            Carrier::Z => "Z axis",
        }
    }

    /// Index of the axis (0 = X), none for the base.
    pub fn axis(self) -> Option<usize> {
        match self {
            Carrier::Base => None,
            Carrier::X => Some(0),
            Carrier::Y => Some(1),
            Carrier::Z => Some(2),
        }
    }

    fn of_axis(a: usize) -> Carrier {
        [Carrier::X, Carrier::Y, Carrier::Z][a]
    }
}

/// One solid of the machine.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MachinePart {
    /// Path in the STEP assembly ("DMC2 Simple Z axis v4:1/hgh15ca_1:1").
    pub name: String,
    pub carrier: Carrier,
    pub color: [u8; 3],
    /// Triangles (model coordinates, 9 little-endian f32 each), compressed (see `import::pack`).
    pub mesh: String,
}

impl Default for MachinePart {
    fn default() -> Self {
        MachinePart { name: String::new(), carrier: Carrier::Base, color: [0xa8, 0xae, 0xb8], mesh: String::new() }
    }
}

/// A machine model: its parts and kinematics. Kept in ~/.config/cad-rust-app/machines/<name>.json.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MachineModel {
    pub name: String,
    pub parts: Vec<MachinePart>,
    /// What the X, Y and Z axes ride on: the base or another axis (a Z head on the X carriage).
    pub rides: [Carrier; 3],
    /// How far each axis may go from where it was modelled, mm: [min, max]. min == max: not checked.
    pub travel: [[f64; 2]; 3],
    /// Where the tool comes out of the spindle, as modelled: the collet nut's face, or the spindle face.
    pub spindle: [f64; 3],
    /// What carries the spindle.
    pub spindle_on: Carrier,
    /// The model ends at the spindle face and each tool's holder hangs below it (true), or the model already
    /// has the collet nut and tools stick out of it by their stickout (false).
    pub holders: bool,
    /// What carries the work: the table's axis (the base on a gantry router).
    pub table_on: Carrier,
    /// The point on the table that setups are placed from, as modelled (the middle of its top).
    pub table: [f64; 3],
}

impl Default for MachineModel {
    fn default() -> Self {
        MachineModel {
            name: String::new(),
            parts: vec![],
            rides: [Carrier::Base; 3],
            travel: [[0.0; 2]; 3],
            spindle: [0.0; 3],
            spindle_on: Carrier::Z,
            holders: false,
            table_on: Carrier::Y,
            table: [0.0; 3],
        }
    }
}

pub fn pack_tris(tris: &[[Vec3; 3]]) -> String {
    let mut b = Vec::with_capacity(tris.len() * 36);
    for t in tris {
        for p in t {
            for v in p.to_array() {
                b.extend(v.to_le_bytes());
            }
        }
    }
    crate::import::pack(&b)
}

pub fn unpack_tris(s: &str) -> Vec<[Vec3; 3]> {
    let b = crate::import::unpack(s).unwrap_or_default();
    let f = |o: usize| f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    (0..b.len() / 36).map(|i| [0, 1, 2].map(|k| Vec3::new(f(i * 36 + k * 12), f(i * 36 + k * 12 + 4), f(i * 36 + k * 12 + 8)))).collect()
}

fn bounds(tris: &[[Vec3; 3]]) -> (DVec3, DVec3) {
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for t in tris {
        for p in t {
            lo = lo.min(p.as_dvec3());
            hi = hi.max(p.as_dvec3());
        }
    }
    (lo, hi)
}

fn touching(a: &(DVec3, DVec3), b: &(DVec3, DVec3), gap: f64) -> bool {
    (0..3).all(|k| a.0[k] <= b.1[k] + gap && b.0[k] <= a.1[k] + gap)
}

/// The carrier a part's name suggests ("… X axis …", "spindle", "base"), from the top of its path down.
fn carrier_from_name(path: &str) -> Option<Carrier> {
    for seg in path.split('/') {
        let s = seg.to_ascii_lowercase().replace(['-', '_'], " ");
        let words: Vec<&str> = s.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
        let has = |w: &str| words.contains(&w);
        for (k, c) in [("x", Carrier::X), ("y", Carrier::Y), ("z", Carrier::Z)] {
            let axis_word = format!("{k}axis");
            if words.windows(2).any(|w| w[0] == k && (w[1] == "axis" || w[1] == "carriage" || w[1] == "slide" || w[1] == "table")) || has(&axis_word) {
                return Some(c);
            }
        }
        if has("spindle") || has("head") || has("headstock") {
            return Some(Carrier::Z);
        }
        if has("base") || has("frame") || has("column") || has("bed") || has("enclosure") {
            return Some(Carrier::Base);
        }
    }
    None
}

/// A first guess at a machine from its parts' names and shapes (the user checks and corrects it).
pub fn guess(name: &str, parts: Vec<(String, [u8; 3], Vec<[Vec3; 3]>)>) -> MachineModel {
    let boxes: Vec<(DVec3, DVec3)> = parts.iter().map(|p| bounds(&p.2)).collect();
    let mut carriers: Vec<Option<Carrier>> = parts.iter().map(|p| carrier_from_name(&p.0)).collect();
    let of = |carriers: &[Option<Carrier>], c: Carrier| -> Vec<usize> { (0..parts.len()).filter(|&i| carriers[i] == Some(c)).collect() };
    let union = |ids: &[usize]| ids.iter().fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)), |a, &i| (a.0.min(boxes[i].0), a.1.max(boxes[i].1)));
    // the spindle: the middle of the lowest end of whatever carries it
    let mut spindle_on = Carrier::Z;
    if of(&carriers, Carrier::Z).is_empty() {
        spindle_on = [Carrier::X, Carrier::Y].into_iter().find(|c| !of(&carriers, *c).is_empty()).unwrap_or(Carrier::Z);
    }
    let head = of(&carriers, spindle_on);
    let mut spindle = DVec3::ZERO;
    if !head.is_empty() {
        let zmin = head.iter().map(|&i| boxes[i].0.z).fold(f64::INFINITY, f64::min);
        let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
        for &i in &head {
            for t in &parts[i].2 {
                for p in t {
                    if (p.z as f64) < zmin + 0.5 {
                        lo = lo.min(p.as_dvec3());
                        hi = hi.max(p.as_dvec3());
                    }
                }
            }
        }
        spindle = DVec3::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0, zmin);
    }
    // the table: the axis (other than the spindle's) under the spindle, else the base
    let under = |b: &(DVec3, DVec3)| b.0.x <= spindle.x && spindle.x <= b.1.x && b.0.y <= spindle.y && spindle.y <= b.1.y && b.1.z < spindle.z;
    let table_on = [Carrier::Y, Carrier::X]
        .into_iter()
        .filter(|c| *c != spindle_on)
        .find(|c| {
            let ids = of(&carriers, *c);
            !ids.is_empty() && under(&union(&ids))
        })
        .unwrap_or(Carrier::Base);
    // the table's top: the biggest part of that carrier under the spindle
    let table_part = (0..parts.len())
        .filter(|&i| carriers[i] == Some(table_on) && under(&boxes[i]))
        .max_by(|&a, &b| {
            let area = |i: usize| (boxes[i].1.x - boxes[i].0.x) * (boxes[i].1.y - boxes[i].0.y);
            area(a).total_cmp(&area(b))
        });
    let table = match table_part {
        Some(i) => DVec3::new((boxes[i].0.x + boxes[i].1.x) / 2.0, (boxes[i].0.y + boxes[i].1.y) / 2.0, boxes[i].1.z),
        None => DVec3::new(spindle.x, spindle.y, spindle.z - 100.0),
    };
    // anything unnamed standing on the table rides with it (a rotary table, a vise); the rest is frame
    let tbox = table_part.map(|i| boxes[i]);
    for i in 0..parts.len() {
        if carriers[i].is_none() {
            let b = boxes[i];
            let c = (b.0 + b.1) / 2.0;
            let on_table = tbox.map(|t| b.0.z >= t.1.z - 1.0 && c.x >= t.0.x && c.x <= t.1.x && c.y >= t.0.y && c.y <= t.1.y).unwrap_or(false);
            carriers[i] = Some(if on_table { table_on } else { Carrier::Base });
        }
    }
    let carriers: Vec<Carrier> = carriers.into_iter().map(|c| c.unwrap_or(Carrier::Base)).collect();
    let group = |c: Carrier| -> Vec<usize> { (0..parts.len()).filter(|&i| carriers[i] == c).collect() };
    let touches = |a: Carrier, b: Carrier| {
        let (ga, gb) = (group(a), group(b));
        ga.iter().any(|&i| gb.iter().any(|&j| touching(&boxes[i], &boxes[j], 0.5)))
    };
    // what rides on what: Z on X (or Y), X on Y (a gantry or a saddle), else on the base
    let mut rides = [Carrier::Base; 3];
    if touches(Carrier::X, Carrier::Y) && table_on != Carrier::X {
        rides[0] = Carrier::Y;
    } else if touches(Carrier::X, Carrier::Y) {
        rides[1] = Carrier::X;
    }
    rides[2] = if touches(Carrier::Z, Carrier::X) && rides[0] != Carrier::Z {
        Carrier::X
    } else if touches(Carrier::Z, Carrier::Y) {
        Carrier::Y
    } else {
        Carrier::Base
    };
    let mut m = MachineModel {
        name: name.to_string(),
        parts: parts.iter().zip(&carriers).map(|(p, c)| MachinePart { name: p.0.clone(), carrier: *c, color: p.1, mesh: pack_tris(&p.2) }).collect(),
        rides,
        travel: [[0.0; 2]; 3],
        spindle: spindle.to_array(),
        spindle_on,
        holders: false,
        table_on,
        table: table.to_array(),
    };
    // travel: as far as each axis's parts can slide along the parts they touch on what carries them (its
    // guide rails); a Z axis also stops with the spindle at the table
    for a in 0..3 {
        let c = Carrier::of_axis(a);
        let parent = m.rides[a];
        let (gc, gp) = (group(c), group(parent));
        let mut range = [f64::NEG_INFINITY, f64::INFINITY];
        for &i in &gc {
            for &j in &gp {
                if !touching(&boxes[i], &boxes[j], 0.5) {
                    continue;
                }
                let (mlo, mhi, flo, fhi) = (boxes[i].0[a], boxes[i].1[a], boxes[j].0[a], boxes[j].1[a]);
                // the moving part slides inside the longer one, or the shorter fixed one along it
                let r = if mhi - mlo < fhi - flo { [flo - mlo, fhi - mhi] } else { [fhi - mhi, flo - mlo] };
                if r[0] <= 1e-6 && r[1] >= -1e-6 && r[1] - r[0] > 10.0 {
                    range = [range[0].max(r[0]), range[1].min(r[1])];
                }
            }
        }
        // the part moves by +q on the head side, by -q on the table side
        let sign = if m.on_table_chain(a) { -1.0 } else { 1.0 };
        let mut q = if sign > 0.0 { range } else { [-range[1], -range[0]] };
        if a == 2 && m.spindle_on == Carrier::Z && !m.on_table_chain(2) {
            q[0] = q[0].max(table.z - spindle.z);
        }
        if q[0].is_finite() && q[1].is_finite() && q[1] > q[0] {
            m.travel[a] = [(q[0] * 10.0).round() / 10.0, (q[1] * 10.0).round() / 10.0];
        }
    }
    m
}

impl MachineModel {
    /// Remove the parts of a top-level assembly ("4-5 Axis Assembly v22:1"), or one part by its full name.
    /// Returns how many went.
    pub fn remove(&mut self, name: &str) -> usize {
        let n = self.parts.len();
        self.parts.retain(|p| !(p.name == name || p.name.starts_with(&format!("{name}/")) || clean(p.name.split('/').next().unwrap_or("")) == name));
        n - self.parts.len()
    }

    /// Add more parts riding on the table (a vise, a fixture plate, a rotary table), from their own model:
    /// `at` is where the middle of their bottom goes, from the table point (model coordinates). Their names
    /// get `group` in front, so they can be moved or removed together.
    pub fn add_on_table(&mut self, group: &str, parts: Vec<(String, [u8; 3], Vec<[Vec3; 3]>)>, at: DVec3) {
        let (lo, hi) = parts.iter().map(|p| bounds(&p.2)).fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)), |a, b| (a.0.min(b.0), a.1.max(b.1)));
        if !lo.is_finite() {
            return;
        }
        let base = DVec3::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0, lo.z);
        let d = (DVec3::from(self.table) + at - base).as_vec3();
        for (n, c, tris) in parts {
            let moved: Vec<[Vec3; 3]> = tris.iter().map(|t| t.map(|p| p + d)).collect();
            self.parts.push(MachinePart { name: format!("{group}/{n}"), carrier: self.table_on, color: c, mesh: pack_tris(&moved) });
        }
    }

    /// Move a top-level assembly's parts by `d` (model coordinates).
    pub fn shift_group(&mut self, group: &str, d: DVec3) {
        let d = d.as_vec3();
        for p in self.parts.iter_mut().filter(|p| p.name.split('/').next() == Some(group)) {
            let t: Vec<[Vec3; 3]> = unpack_tris(&p.mesh).iter().map(|t| t.map(|q| q + d)).collect();
            p.mesh = pack_tris(&t);
        }
    }

    /// The carriers from `c` down to the base: [c, what c rides on, …].
    pub fn chain(&self, c: Carrier) -> Vec<Carrier> {
        let mut v = vec![];
        let mut k = c;
        while let Some(a) = k.axis() {
            if v.contains(&k) || v.len() > 3 {
                break;
            }
            v.push(k);
            k = self.rides[a];
        }
        v
    }

    /// Whether axis `a` moves the work (the table side) rather than the tool.
    pub fn on_table_chain(&self, a: usize) -> bool {
        self.chain(self.table_on).contains(&Carrier::of_axis(a))
    }

    /// What's wrong with the kinematics, if anything: each axis must move either the tool or the work.
    pub fn problem(&self) -> Option<String> {
        if self.parts.is_empty() {
            return Some("the machine has no parts".into());
        }
        let (t, s) = (self.chain(self.table_on), self.chain(self.spindle_on));
        for a in 0..3 {
            let c = Carrier::of_axis(a);
            if self.rides[a] == c || self.chain(c).len() < 1 || self.chain(c).iter().filter(|k| **k == c).count() > 1 {
                return Some(format!("{} rides on itself", c.short()));
            }
            match (t.contains(&c), s.contains(&c)) {
                (false, false) => return Some(format!("the {} moves neither the spindle nor the table", c.short())),
                (true, true) => return Some(format!("the {} carries both the spindle and the table", c.short())),
                _ => {}
            }
        }
        None
    }

    /// Displacement of each carrier's parts relative to the table (setup coordinates) for axis positions `q`
    /// (from where they were modelled): the tool always moves by +q relative to the work.
    pub fn relative(&self, q: DVec3) -> [DVec3; 4] {
        let disp = |c: Carrier| -> DVec3 {
            let mut d = DVec3::ZERO;
            for k in self.chain(c) {
                let a = k.axis().unwrap();
                let s = if self.on_table_chain(a) { -1.0 } else { 1.0 };
                d[a] += s * q[a];
            }
            d
        };
        let base = disp(self.table_on);
        CARRIERS.map(|c| disp(c) - base)
    }

    /// How far each axis is past its travel at `q` (0 = within it).
    pub fn over_travel(&self, q: DVec3) -> [f64; 3] {
        let mut v = [0.0; 3];
        for (a, o) in v.iter_mut().enumerate() {
            let [lo, hi] = self.travel[a];
            if hi > lo {
                *o = (lo - q[a]).max(q[a] - hi).max(0.0);
            }
        }
        v
    }
}

fn machines_dir() -> Option<std::path::PathBuf> {
    crate::cam::library_path().map(|p| p.with_file_name("machines"))
}

fn file_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' }).collect();
    format!("{}.json", s.trim())
}

/// Names of the machines in the library.
pub fn list() -> Vec<String> {
    let mut v: Vec<String> = machines_dir()
        .and_then(|d| std::fs::read_dir(d).ok())
        .map(|r| r.flatten().filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned())).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Where a machine is: a library name, or a path to a machine file (scripts, examples).
fn path_of(name: &str) -> Option<std::path::PathBuf> {
    if name.ends_with(".json") || name.contains('/') || name.contains('\\') {
        return Some(std::path::PathBuf::from(name));
    }
    machines_dir().map(|d| d.join(file_name(name)))
}

pub fn save(m: &MachineModel) -> Result<std::path::PathBuf, String> {
    let d = machines_dir().ok_or("no config folder (HOME isn't set)")?;
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let p = d.join(file_name(&m.name));
    std::fs::write(&p, serde_json::to_string(m).unwrap_or_default()).map_err(|e| e.to_string())?;
    if let Ok(mut c) = cache().lock() {
        c.remove(&m.name);
    }
    Ok(p)
}

/// A machine with its meshes unpacked.
pub struct Loaded {
    pub model: MachineModel,
    /// Triangles of each part (model coordinates).
    pub tris: Vec<Vec<[Vec3; 3]>>,
}

impl Loaded {
    pub fn new(model: MachineModel) -> Loaded {
        let tris = model.parts.iter().map(|p| unpack_tris(&p.mesh)).collect();
        Loaded { model, tris }
    }
}

fn cache() -> &'static Mutex<HashMap<String, Arc<Loaded>>> {
    static C: OnceLock<Mutex<HashMap<String, Arc<Loaded>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The machine of that name (or path), unpacked once and kept.
pub fn load(name: &str) -> Result<Arc<Loaded>, String> {
    if name.is_empty() {
        return Err("no machine".into());
    }
    if let Some(m) = cache().lock().ok().and_then(|c| c.get(name).cloned()) {
        return Ok(m);
    }
    let p = path_of(name).ok_or("no config folder (HOME isn't set)")?;
    let text = std::fs::read_to_string(&p).map_err(|e| format!("can't read machine {name} ({}): {e}", p.display()))?;
    let model: MachineModel = serde_json::from_str(&text).map_err(|e| format!("machine {name} is damaged: {e}"))?;
    let l = Arc::new(Loaded::new(model));
    if let Ok(mut c) = cache().lock() {
        c.insert(name.to_string(), l.clone());
    }
    Ok(l)
}

/// Replace a cached machine (after editing it).
pub fn update_cached(name: &str, l: Arc<Loaded>) {
    if let Ok(mut c) = cache().lock() {
        c.insert(name.to_string(), l);
    }
}

/// Read a STEP assembly's parts with the OpenCascade worker (a child process, so a bad file can't take the
/// app down) and make a first guess at the machine.
pub fn import_step(path: &std::path::Path, name: &str) -> Result<MachineModel, String> {
    Ok(guess(name, read_step(path)?))
}

/// A part read from a file: name path, colour, triangles.
pub type StepPart = (String, [u8; 3], Vec<[Vec3; 3]>);

/// A STEP assembly's leaf parts, read in a child process.
pub fn read_step(path: &std::path::Path) -> Result<Vec<StepPart>, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = std::env::temp_dir().join(format!("rcad-machine-{}.bin", std::process::id()));
    let st = std::process::Command::new(exe)
        .arg("--step-parts")
        .arg(path)
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&out);
    let _ = std::fs::remove_file(&out);
    if !st.status.success() {
        let err = String::from_utf8_lossy(&st.stderr).trim().to_string();
        return Err(if err.is_empty() { "the STEP reader stopped".into() } else { err });
    }
    decode_parts(&bytes.map_err(|e| e.to_string())?)
}

/// Entry point of `--step-parts in.step out.bin`: the leaves of a STEP assembly, meshed, written as JSON
/// lines of [name, [r, g, b], packed triangles].
pub fn step_parts_main(input: &str, output: &str) -> i32 {
    opencascade_sys::extra::ex_messages_to_stderr();
    let mut names = vec![];
    let v = opencascade_sys::extra::ex_step_parts(input, 0.25, &mut names);
    if names.is_empty() {
        eprintln!("{}", opencascade_sys::extra::ex_last_error());
        return 1;
    }
    let mut k = 0usize;
    let mut out = String::new();
    for n in names {
        let col = [v[k], v[k + 1], v[k + 2]];
        let nt = v[k + 3] as usize;
        k += 4;
        let tris: Vec<[Vec3; 3]> = (0..nt).map(|i| [0, 1, 2].map(|j| Vec3::new(v[k + i * 9 + j * 3] as f32, v[k + i * 9 + j * 3 + 1] as f32, v[k + i * 9 + j * 3 + 2] as f32))).collect();
        k += nt * 9;
        let rgb = if col[0] < 0.0 { [0xa8, 0xae, 0xb8] } else { col.map(|c| (c * 255.0).round().clamp(0.0, 255.0) as u8) };
        out.push_str(&serde_json::to_string(&(n, rgb, pack_tris(&tris))).unwrap_or_default());
        out.push('\n');
    }
    match std::fs::write(output, out) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("can't write {output}: {e}");
            1
        }
    }
}

fn decode_parts(bytes: &[u8]) -> Result<Vec<(String, [u8; 3], Vec<[Vec3; 3]>)>, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut v = vec![];
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (n, c, m): (String, [u8; 3], String) = serde_json::from_str(line).map_err(|e| format!("bad part list: {e}"))?;
        v.push((n, c, unpack_tris(&m)));
    }
    if v.is_empty() {
        return Err("the STEP file has no parts".into());
    }
    Ok(v)
}

// ---------------------------------------------------------------------------------------------------------
// collision checking

/// Solid columns of a carrier's parts seen from above (setup coordinates, modelled pose): per cell, the z
/// intervals inside a part, and which part.
struct Columns {
    x0: f64,
    y0: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    /// Cell k's intervals are spans[start[k]..start[k + 1]].
    start: Vec<u32>,
    spans: Vec<(f32, f32, u16)>,
    /// Highest top and lowest bottom per TILE×TILE cells, to skip whole chunks of points.
    tile_top: Vec<f32>,
    tile_bot: Vec<f32>,
    tnx: usize,
    lo: DVec3,
    hi: DVec3,
}

const TILE: usize = 8;

impl Columns {
    fn new(parts: &[(u16, &[[Vec3; 3]])], shift: DVec3, cell: f64) -> Option<Columns> {
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for (_, tris) in parts {
            for t in *tris {
                for p in t {
                    let q = p.as_dvec3() + shift;
                    lo = lo.min(q);
                    hi = hi.max(q);
                }
            }
        }
        if !lo.is_finite() {
            return None;
        }
        let nx = ((hi.x - lo.x) / cell).ceil() as usize + 1;
        let ny = ((hi.y - lo.y) / cell).ceil() as usize + 1;
        let (x0, y0) = (lo.x, lo.y);
        let mut cols: Vec<Vec<(f32, f32, u16)>> = vec![vec![]; nx * ny];
        let mut hits: Vec<Vec<f32>> = vec![vec![]; nx * ny];
        for (id, tris) in parts {
            let mut touched: Vec<usize> = vec![];
            for t in *tris {
                let t = t.map(|p| p.as_dvec3() + shift);
                let d = (t[1].x - t[0].x) * (t[2].y - t[0].y) - (t[2].x - t[0].x) * (t[1].y - t[0].y);
                if d.abs() < 1e-12 {
                    continue;
                }
                let (mnx, mxx) = (t[0].x.min(t[1].x).min(t[2].x), t[0].x.max(t[1].x).max(t[2].x));
                let (mny, mxy) = (t[0].y.min(t[1].y).min(t[2].y), t[0].y.max(t[1].y).max(t[2].y));
                let i0 = ((mnx - x0) / cell).ceil().max(0.0) as usize;
                let i1 = (((mxx - x0) / cell).floor().max(0.0) as usize).min(nx - 1);
                let j0 = ((mny - y0) / cell).ceil().max(0.0) as usize;
                let j1 = (((mxy - y0) / cell).floor().max(0.0) as usize).min(ny - 1);
                for j in j0..=j1 {
                    // rays a hair off the grid, so they don't run exactly along shared edges
                    let py = y0 + j as f64 * cell + 1.3e-4;
                    for i in i0..=i1 {
                        let px = x0 + i as f64 * cell + 0.7e-4;
                        let a = ((t[1].x - px) * (t[2].y - py) - (t[2].x - px) * (t[1].y - py)) / d;
                        let b = ((t[2].x - px) * (t[0].y - py) - (t[0].x - px) * (t[2].y - py)) / d;
                        let c = 1.0 - a - b;
                        if a >= 0.0 && b >= 0.0 && c >= 0.0 {
                            let k = j * nx + i;
                            if hits[k].is_empty() {
                                touched.push(k);
                            }
                            hits[k].push((a * t[0].z + b * t[1].z + c * t[2].z) as f32);
                        }
                    }
                }
            }
            // inside between pairs of crossings (a part that isn't closed leaves its last crossing over)
            for k in touched {
                let h = &mut hits[k];
                h.sort_by(|a, b| a.total_cmp(b));
                h.dedup_by(|a, b| (*a - *b).abs() < 1e-5);
                for w in h.chunks_exact(2) {
                    cols[k].push((w[0], w[1], *id));
                }
                h.clear();
            }
        }
        let mut start = Vec::with_capacity(nx * ny + 1);
        let mut spans = vec![];
        for c in &cols {
            start.push(spans.len() as u32);
            spans.extend_from_slice(c);
        }
        start.push(spans.len() as u32);
        let tnx = nx.div_ceil(TILE);
        let tny = ny.div_ceil(TILE);
        let mut tile_top = vec![f32::NEG_INFINITY; tnx * tny];
        let mut tile_bot = vec![f32::INFINITY; tnx * tny];
        for j in 0..ny {
            for i in 0..nx {
                let t = (j / TILE) * tnx + i / TILE;
                for s in &cols[j * nx + i] {
                    tile_top[t] = tile_top[t].max(s.1);
                    tile_bot[t] = tile_bot[t].min(s.0);
                }
            }
        }
        Some(Columns { x0, y0, cell, nx, ny, start, spans, tile_top, tile_bot, tnx, lo, hi })
    }

    /// Whether anything is in the box at all.
    fn may_hit(&self, lo: DVec3, hi: DVec3) -> bool {
        if hi.x < self.lo.x || lo.x > self.hi.x || hi.y < self.lo.y || lo.y > self.hi.y || hi.z < self.lo.z || lo.z > self.hi.z {
            return false;
        }
        let ti = |x: f64, x0: f64, n: usize| ((((x - x0) / self.cell).round().max(0.0) as usize).min(n - 1)) / TILE;
        let (i0, i1) = (ti(lo.x, self.x0, self.nx), ti(hi.x, self.x0, self.nx));
        let (j0, j1) = (ti(lo.y, self.y0, self.ny), ti(hi.y, self.y0, self.ny));
        for j in j0..=j1 {
            for i in i0..=i1 {
                let t = j * self.tnx + i;
                if self.tile_top[t] as f64 > lo.z && (self.tile_bot[t] as f64) < hi.z {
                    return true;
                }
            }
        }
        false
    }

    /// The part `p` is inside (by more than `tol` up and down) and how deep.
    fn inside(&self, p: DVec3, tol: f32) -> Option<(u16, f32)> {
        let i = ((p.x - self.x0) / self.cell).round();
        let j = ((p.y - self.y0) / self.cell).round();
        if i < 0.0 || j < 0.0 || i >= self.nx as f64 || j >= self.ny as f64 {
            return None;
        }
        let k = j as usize * self.nx + i as usize;
        let z = p.z as f32;
        for s in &self.spans[self.start[k] as usize..self.start[k + 1] as usize] {
            if z > s.0 + tol && z < s.1 - tol {
                return Some((s.2, (z - s.0).min(s.1 - z)));
            }
        }
        None
    }
}

/// Points on a carrier's surfaces in chunks, for checking against other carriers' columns.
struct Points {
    chunks: Vec<(DVec3, DVec3, std::ops::Range<usize>)>,
    pts: Vec<(Vec3, u16)>,
}

fn sample_surface(parts: &[(u16, &[[Vec3; 3]])], shift: DVec3, step: f32) -> Points {
    let mut pts: Vec<(Vec3, u16)> = vec![];
    let s = shift.as_vec3();
    for (id, tris) in parts {
        for t in *tris {
            // split the longest edge until every piece is small (long thin triangles stay evenly sampled)
            let mut todo = vec![*t];
            while let Some(t) = todo.pop() {
                let e = [(t[1] - t[0]).length(), (t[2] - t[1]).length(), (t[0] - t[2]).length()];
                let k = (0..3).max_by(|a, b| e[*a].total_cmp(&e[*b])).unwrap();
                if e[k] <= step || pts.len() > 4_000_000 {
                    pts.push((t[0] + s, *id));
                    pts.push(((t[0] + t[1] + t[2]) / 3.0 + s, *id));
                    continue;
                }
                let (a, b, c) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
                let m = (a + b) / 2.0;
                todo.push([a, m, c]);
                todo.push([m, b, c]);
            }
            pts.extend(t.iter().map(|p| (*p + s, *id)));
        }
    }
    // chunks of 24 mm cubes
    let key = |p: Vec3| ((p.x / 24.0).floor() as i32, (p.y / 24.0).floor() as i32, (p.z / 24.0).floor() as i32);
    pts.sort_by_key(|p| key(p.0));
    pts.dedup_by(|a, b| (a.0 - b.0).length_squared() < (step * 0.3) * (step * 0.3));
    let mut chunks = vec![];
    let mut i = 0;
    while i < pts.len() {
        let k = key(pts[i].0);
        let mut j = i;
        let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
        while j < pts.len() && key(pts[j].0) == k {
            lo = lo.min(pts[j].0.as_dvec3());
            hi = hi.max(pts[j].0.as_dvec3());
            j += 1;
        }
        chunks.push((lo, hi, i..j));
        i = j;
    }
    Points { chunks, pts }
}

/// Something a machine part ran into.
#[derive(Clone, Debug, PartialEq)]
pub enum Obstacle {
    /// Another machine part (index into the model's parts).
    Part(usize),
    Stock,
    /// The finished part.
    Workpiece,
    /// A fixture, by name.
    Fixture(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// The machine part that hit (None: the tool or its holder).
    pub mover: Option<usize>,
    pub what: Obstacle,
    /// Where (setup coordinates) and how deep, mm.
    pub at: DVec3,
    pub depth: f64,
}

/// The stock and part height maps the simulation keeps, for the machine checks (setup coordinates).
pub struct Work<'a> {
    pub x0: f64,
    pub y0: f64,
    pub cell: f64,
    pub nx: usize,
    pub ny: usize,
    pub top: &'a [f32],
    pub lo: &'a [f32],
    pub part: &'a [f32],
    pub max_h: f32,
    pub fixtures: &'a [(String, DVec3, DVec3)],
}

impl Work<'_> {
    fn inside(&self, p: DVec3, tol: f32) -> Option<(Obstacle, f32)> {
        for (n, lo, hi) in self.fixtures {
            let t = tol as f64;
            if p.x > lo.x + t && p.x < hi.x - t && p.y > lo.y + t && p.y < hi.y - t && p.z > lo.z + t && p.z < hi.z - t {
                return Some((Obstacle::Fixture(n.clone()), (hi.z - p.z) as f32));
            }
        }
        if p.z as f32 > self.max_h {
            return None;
        }
        let i = ((p.x - self.x0) / self.cell).round();
        let j = ((p.y - self.y0) / self.cell).round();
        if i < 0.0 || j < 0.0 || i >= self.nx as f64 || j >= self.ny as f64 {
            return None;
        }
        let k = j as usize * self.nx + i as usize;
        let z = p.z as f32;
        if z < self.top[k] - tol && z > self.lo[k] + tol {
            return Some((if z < self.part[k] { Obstacle::Workpiece } else { Obstacle::Stock }, self.top[k] - z));
        }
        None
    }

    fn bounds(&self) -> (DVec3, DVec3) {
        let mut lo = DVec3::new(self.x0, self.y0, self.lo.iter().copied().fold(f32::INFINITY, f32::min) as f64);
        let mut hi = DVec3::new(self.x0 + self.nx as f64 * self.cell, self.y0 + self.ny as f64 * self.cell, self.max_h as f64);
        for (_, a, b) in self.fixtures {
            lo = lo.min(*a);
            hi = hi.max(*b);
        }
        (lo, hi)
    }
}

/// A machine placed for one setup, ready to check.
pub struct Placed {
    pub machine: Arc<Loaded>,
    /// Model → setup coordinates at the modelled pose.
    pub shift: DVec3,
    /// Per carrier (CARRIERS order): its parts' columns and surface points.
    cols: [Option<Columns>; 4],
    pts: [Option<Points>; 4],
    /// Carrier pairs that are checked against each other.
    pairs: Vec<(usize, usize)>,
    /// Carrier pairs left out because their parts already overlap as modelled.
    pub overlapping: Vec<(Carrier, Carrier)>,
    /// How often to check, mm of travel.
    pub step: f64,
}

const TOL: f32 = 0.3;

impl Placed {
    /// Place the machine for a setup: the reference point of the setup (`at`, setup coordinates: the middle
    /// of the stock, at the bottom of whatever stands on the table) goes to the table point plus `offset`.
    pub fn new(machine: Arc<Loaded>, at: DVec3, offset: DVec3) -> Placed {
        let m = &machine.model;
        let shift = at - (DVec3::from(m.table) + offset);
        let group = |c: Carrier| -> Vec<(u16, &[[Vec3; 3]])> {
            m.parts.iter().enumerate().filter(|(_, p)| p.carrier == c).map(|(i, _)| (i as u16, machine.tris[i].as_slice())).collect()
        };
        let cols = CARRIERS.map(|c| Columns::new(&group(c), shift, 1.0));
        let pts = CARRIERS.map(|c| {
            let g = group(c);
            if g.is_empty() {
                None
            } else {
                Some(sample_surface(&g, shift, 2.0))
            }
        });
        // carriers that ride on each other are joined by their guides: those aren't checked
        let linked = |a: Carrier, b: Carrier| (a.axis().map(|k| m.rides[k] == b).unwrap_or(false)) || (b.axis().map(|k| m.rides[k] == a).unwrap_or(false));
        let mut pairs = vec![];
        let mut overlapping = vec![];
        for i in 0..4 {
            for j in i + 1..4 {
                let (a, b) = (CARRIERS[i], CARRIERS[j]);
                if linked(a, b) || pts[i].is_none() || pts[j].is_none() {
                    continue;
                }
                let clash = pts[i].as_ref().map(|pp| any_inside(pp, cols[j].as_ref(), DVec3::ZERO)).unwrap_or(false)
                    || pts[j].as_ref().map(|pp| any_inside(pp, cols[i].as_ref(), DVec3::ZERO)).unwrap_or(false);
                if clash {
                    overlapping.push((a, b));
                } else {
                    pairs.push((i, j));
                }
            }
        }
        Placed { machine, shift, cols, pts, pairs, overlapping, step: 2.0 }
    }

    /// The setup's reference point for placing it on the table: the middle of the stock, at the bottom of the
    /// stock or of the lowest fixture (a vise stands on the table, the stock in it).
    pub fn reference(stock: (DVec3, DVec3), fixtures: &[(String, DVec3, DVec3)]) -> DVec3 {
        let z = fixtures.iter().map(|f| f.1.z).fold(stock.0.z, f64::min);
        DVec3::new((stock.0.x + stock.1.x) / 2.0, (stock.0.y + stock.1.y) / 2.0, z)
    }

    /// Where the tool tip is with every axis where it was modelled (setup coordinates), for a tool sticking
    /// out `gauge` below the spindle point.
    pub fn tip_home(&self, gauge: f64) -> DVec3 {
        DVec3::from(self.machine.model.spindle) + self.shift - DVec3::Z * gauge
    }

    /// Axis positions (from the modelled pose) that put the tool tip at `tip`.
    pub fn axes_at(&self, tip: DVec3, gauge: f64) -> DVec3 {
        tip - self.tip_home(gauge)
    }

    /// Displacement of each carrier relative to the work for the tool tip at `tip`.
    pub fn pose(&self, tip: DVec3, gauge: f64) -> [DVec3; 4] {
        self.machine.model.relative(self.axes_at(tip, gauge))
    }

    /// Everything that collides with the tool tip at `tip`: machine parts against each other and the work,
    /// and the tool (as points `tool`, relative to its tip) against the machine.
    pub fn check(&self, tip: DVec3, gauge: f64, tool: &[Vec3], work: &Work) -> Vec<Hit> {
        let m = &self.machine.model;
        let rel = self.pose(tip, gauge);
        let mut hits: Vec<Hit> = vec![];
        let mut add = |h: Hit| {
            if let Some(e) = hits.iter_mut().find(|e| e.mover == h.mover && e.what == h.what) {
                e.depth = e.depth.max(h.depth);
            } else {
                hits.push(h);
            }
        };
        // machine against machine
        for &(i, j) in &self.pairs {
            // whichever way round it shows first, one hit per pair (named head first, then the column, …)
            for (a, b) in [(j, i), (i, j)] {
                let (Some(pa), Some(cb)) = (&self.pts[a], &self.cols[b]) else { continue };
                let d = rel[a] - rel[b];
                if let Some((who, what, at, depth)) = first_inside(pa, cb, d) {
                    let (who, what) = if a == j { (who, what) } else { (what, who) };
                    add(Hit { mover: Some(who as usize), what: Obstacle::Part(what as usize), at: at + rel[b], depth: depth as f64 });
                    break;
                }
            }
        }
        // machine against the work (everything but the table it sits on)
        let (wlo, whi) = work.bounds();
        let table = CARRIERS.iter().position(|c| *c == m.table_on).unwrap_or(0);
        for (a, pa) in self.pts.iter().enumerate() {
            let Some(pa) = pa else { continue };
            if a == table {
                continue;
            }
            let d = rel[a];
            for (lo, hi, r) in &pa.chunks {
                let (lo, hi) = (*lo + d, *hi + d);
                if hi.x < wlo.x || lo.x > whi.x || hi.y < wlo.y || lo.y > whi.y || hi.z < wlo.z || lo.z > whi.z {
                    continue;
                }
                for (p, id) in &pa.pts[r.clone()] {
                    let q = p.as_dvec3() + d;
                    if let Some((what, depth)) = work.inside(q, TOL) {
                        add(Hit { mover: Some(*id as usize), what, at: q, depth: depth as f64 });
                        break;
                    }
                }
            }
        }
        // the tool and holder against the machine (except what carries the spindle)
        let carried = m.chain(m.spindle_on);
        if !tool.is_empty() {
            let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
            for p in tool {
                lo = lo.min(p.as_dvec3());
                hi = hi.max(p.as_dvec3());
            }
            for (b, cb) in self.cols.iter().enumerate() {
                let Some(cb) = cb else { continue };
                if carried.contains(&CARRIERS[b]) || (CARRIERS[b] == Carrier::Base && m.spindle_on == Carrier::Base) {
                    continue;
                }
                // the tool's points in that carrier's frame
                let d = tip - rel[b];
                if !cb.may_hit(lo + d, hi + d) {
                    continue;
                }
                for p in tool {
                    let q = p.as_dvec3() + d;
                    if let Some((what, depth)) = cb.inside(q, TOL) {
                        add(Hit { mover: None, what: Obstacle::Part(what as usize), at: q + rel[b], depth: depth as f64 });
                        break;
                    }
                }
            }
        }
        hits
    }
}

fn any_inside(pa: &Points, cb: Option<&Columns>, d: DVec3) -> bool {
    cb.map(|cb| first_inside(pa, cb, d).is_some()).unwrap_or(false)
}

/// The first point of `pa` (moved by `d`) inside `cb`: (its part, the part it is in, where, how deep).
fn first_inside(pa: &Points, cb: &Columns, d: DVec3) -> Option<(u16, u16, DVec3, f32)> {
    for (lo, hi, r) in &pa.chunks {
        if !cb.may_hit(*lo + d, *hi + d) {
            continue;
        }
        for (p, id) in &pa.pts[r.clone()] {
            let q = p.as_dvec3() + d;
            if let Some((what, depth)) = cb.inside(q, TOL) {
                return Some((*id, what, q, depth));
            }
        }
    }
    None
}

/// Points on the surface of a tool and holder given as bands (radius, height of the band's bottom above
/// the tip), up to `top` above the tip, every `step` mm: its tip face and its sides.
pub fn tool_points(body: &[(f64, f64)], top: f64, step: f64) -> Vec<Vec3> {
    let mut v = vec![];
    let ring = |v: &mut Vec<Vec3>, r: f64, z: f64| {
        let n = ((std::f64::consts::TAU * r / step).ceil() as usize).max(6);
        for k in 0..n {
            let a = k as f64 / n as f64 * std::f64::consts::TAU;
            v.push(Vec3::new((a.cos() * r) as f32, (a.sin() * r) as f32, z as f32));
        }
    };
    if let Some(&(r, _)) = body.first() {
        v.push(Vec3::ZERO);
        let mut rr = step;
        while rr < r {
            ring(&mut v, rr, 0.0);
            rr += step;
        }
    }
    for (k, (r, z0)) in body.iter().enumerate() {
        let z1 = body.get(k + 1).map(|b| b.1).unwrap_or(top).min(top);
        let mut z = *z0;
        while z <= z1 + 1e-9 {
            ring(&mut v, *r, z);
            z += step;
        }
        ring(&mut v, *r, z1);
        // the step to the next band
        if let Some(&(r2, _)) = body.get(k + 1) {
            let (a, b) = (r.min(r2), r.max(r2));
            let mut rr = a + step;
            while rr < b {
                ring(&mut v, rr, z1);
                rr += step;
            }
        }
    }
    v
}

/// How long a tool's holder is from its bottom face to the spindle face: where its outline reaches the
/// spindle's diameter (the presets end with the spindle nose), else 40 mm for a plain holder.
pub fn holder_length(t: &crate::cam::Tool) -> f64 {
    if t.holder.len() >= 3 {
        let last = t.holder[t.holder.len() - 1][0];
        if let Some(p) = t.holder.iter().find(|p| (p[0] - last).abs() < 1e-6) {
            if p[1] > 0.0 {
                return p[1];
            }
        }
        return t.holder[t.holder.len() - 1][1];
    }
    40.0
}

/// A closed cylinder standing on z = 0 (for things given by their size, such as a tool setter).
pub fn cylinder(diameter: f64, height: f64) -> Vec<[Vec3; 3]> {
    let (r, h) = ((diameter / 2.0) as f32, height as f32);
    let n = 32;
    let p = |k: usize, z: f32| {
        let a = k as f32 / n as f32 * std::f32::consts::TAU;
        Vec3::new(a.cos() * r, a.sin() * r, z)
    };
    let (b, t) = (Vec3::ZERO, Vec3::new(0.0, 0.0, h));
    let mut v = vec![];
    for k in 0..n {
        let (a0, a1, c0, c1) = (p(k, 0.0), p(k + 1, 0.0), p(k, h), p(k + 1, h));
        v.extend([[a0, a1, c1], [a0, c1, c0], [b, a1, a0], [t, c0, c1]]);
    }
    v
}

/// A closed box standing on z = 0, centred on the origin.
pub fn cuboid(size: [f64; 3]) -> Vec<[Vec3; 3]> {
    let (x, y, z) = ((size[0] / 2.0) as f32, (size[1] / 2.0) as f32, size[2] as f32);
    let c = |k: usize| Vec3::new([-x, x][k & 1], [-y, y][(k >> 1) & 1], [0.0, z][(k >> 2) & 1]);
    [[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]].iter().map(|f| [c(f[0]), c(f[1]), c(f[2])]).collect()
}

/// A text for a hit: "Z axis (spindle nose) hits the stock".
pub fn describe(m: &MachineModel, h: &Hit) -> String {
    let part = |i: usize| {
        let p = &m.parts[i];
        let leaf = p.name.rsplit('/').next().unwrap_or(&p.name);
        let top = p.name.split('/').next().unwrap_or(&p.name);
        // (an assembly's own body has no name of its own: "=>[0:1:1:14]")
        if leaf == top || leaf.starts_with("=>") {
            format!("{} ({})", p.carrier.short(), clean(top))
        } else {
            format!("{} ({} / {})", p.carrier.short(), clean(top), clean(leaf))
        }
    };
    let who = match h.mover {
        Some(i) => part(i),
        None => "the tool".into(),
    };
    let what = match &h.what {
        Obstacle::Part(i) => part(*i),
        Obstacle::Stock => "the stock".into(),
        Obstacle::Workpiece => "the part".into(),
        Obstacle::Fixture(n) => n.clone(),
    };
    format!("{who} hits {what}")
}

/// A STEP instance name without its version and instance numbers ("DMC2 Simple Z axis v4:1" → "DMC2 Simple Z axis").
pub fn clean(n: &str) -> String {
    let n = n.rsplit_once(':').filter(|(_, b)| b.chars().all(|c| c.is_ascii_digit())).map(|(a, _)| a).unwrap_or(n);
    let n = n.rsplit_once(" v").filter(|(_, b)| !b.is_empty() && b.chars().all(|c| c.is_ascii_digit())).map(|(a, _)| a).unwrap_or(n);
    n.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cuboid(lo: [f32; 3], hi: [f32; 3]) -> Vec<[Vec3; 3]> {
        let c = |k: usize| Vec3::new([lo[0], hi[0]][k & 1], [lo[1], hi[1]][(k >> 1) & 1], [lo[2], hi[2]][(k >> 2) & 1]);
        [[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]].iter().map(|f| [c(f[0]), c(f[1]), c(f[2])]).collect()
    }

    /// A little bed mill: a frame with Y rails, a Y table on them, an X carriage on the column, and a Z head
    /// on the X carriage with a spindle nose at z = 150.
    pub(crate) fn test_machine() -> MachineModel {
        let parts = vec![
            ("Base v1:1".to_string(), [90, 90, 90], cuboid([-250.0, -300.0, -100.0], [250.0, 300.0, -40.0])),
            ("Base v1:1/Column".to_string(), [90, 90, 90], cuboid([-250.0, 200.0, -40.0], [250.0, 300.0, 400.0])),
            ("Base v1:1/Rail:1".to_string(), [200, 200, 200], cuboid([-150.0, -250.0, -40.0], [-130.0, 150.0, -20.0])),
            ("Y axis v1:1/Block".to_string(), [200, 200, 200], cuboid([-155.0, -100.0, -20.0], [-125.0, 0.0, -10.0])),
            ("Y axis v1:1/Table".to_string(), [60, 90, 140], cuboid([-175.0, -175.0, -10.0], [175.0, 75.0, 10.0])),
            ("X axis v1:1".to_string(), [140, 60, 60], cuboid([-80.0, 150.0, 170.0], [80.0, 200.0, 360.0])),
            ("Z axis v1:1".to_string(), [60, 140, 60], cuboid([-60.0, 20.0, 170.0], [60.0, 150.0, 500.0])),
            ("Z axis v1:1/Nose".to_string(), [40, 40, 40], cuboid([-11.0, -11.0, 150.0], [11.0, 11.0, 170.0])),
        ];
        guess("Test mill", parts)
    }

    #[test]
    fn guesses_kinematics_from_names_and_shapes() {
        let m = test_machine();
        assert_eq!(m.parts.iter().map(|p| p.carrier).collect::<Vec<_>>(), vec![Carrier::Base, Carrier::Base, Carrier::Base, Carrier::Y, Carrier::Y, Carrier::X, Carrier::Z, Carrier::Z]);
        assert_eq!(m.rides, [Carrier::Base, Carrier::Base, Carrier::X]);
        assert_eq!(m.table_on, Carrier::Y);
        assert_eq!(m.spindle, [0.0, 0.0, 150.0]);
        assert_eq!(m.table, [0.0, -50.0, 10.0]);
        assert!(m.problem().is_none(), "{:?}", m.problem());
        // the Y table slides along the rail; Z goes down until the spindle reaches the table
        assert!(m.travel[1][1] > m.travel[1][0], "{:?}", m.travel);
        assert_eq!(m.travel[2][0], -140.0);
    }

    #[test]
    fn tool_moves_by_q_relative_to_the_table() {
        let m = test_machine();
        let q = DVec3::new(10.0, 20.0, -30.0);
        let r = m.relative(q);
        assert_eq!(r[2], DVec3::ZERO, "the table stays put in setup coordinates");
        assert_eq!(r[3], q, "the head moves by q");
        assert_eq!(r[1], DVec3::new(10.0, 20.0, 0.0), "the X carriage by X and (the table moving) Y");
        assert_eq!(r[0], DVec3::new(0.0, 20.0, 0.0), "the base by Y");
    }

    #[test]
    fn finds_the_head_in_the_stock_and_nothing_at_rest() {
        let l = Arc::new(Loaded::new(test_machine()));
        // stock 100 x 80 x 40 standing on the table, its reference at the setup origin's bottom middle
        let stock = (DVec3::new(-50.0, -40.0, -40.0), DVec3::new(50.0, 40.0, 0.0));
        let at = Placed::reference(stock, &[]);
        let p = Placed::new(l, at, DVec3::ZERO);
        assert!(p.overlapping.is_empty(), "{:?}", p.overlapping);
        let (nx, ny) = (101, 81);
        let top = vec![0.0f32; nx * ny];
        let lo = vec![-40.0f32; nx * ny];
        let part = vec![f32::NEG_INFINITY; nx * ny];
        let work = Work { x0: -50.0, y0: -40.0, cell: 1.0, nx, ny, top: &top, lo: &lo, part: &part, max_h: 0.0, fixtures: &[] };
        let tool = tool_points(&[(3.0, 0.0), (3.0, 12.0)], 30.0, 1.0);
        // a 30 mm tool cutting 2 mm into the stock: the nose is 28 mm above it
        assert!(p.check(DVec3::new(0.0, 0.0, -2.0), 30.0, &tool, &work).is_empty());
        // the same tool 40 mm down: the nose is 10 mm into the stock
        let h = p.check(DVec3::new(0.0, 0.0, -40.0), 30.0, &tool, &work);
        assert!(h.iter().any(|h| h.what == Obstacle::Stock && h.mover == Some(7)), "{h:?}");
        // the tool through the stock into the table
        let h = p.check(DVec3::new(0.0, 0.0, -45.0), 30.0, &tool, &work);
        assert!(h.iter().any(|h| h.mover.is_none() && h.what == Obstacle::Part(4)), "{h:?}");
        // a fixture beside the stock and the head over it
        let fx = vec![("Vise jaw".to_string(), DVec3::new(50.0, -40.0, -40.0), DVec3::new(70.0, 40.0, 10.0))];
        let work = Work { fixtures: &fx, ..work };
        let h = p.check(DVec3::new(60.0, 0.0, -25.0), 30.0, &tool, &work);
        assert!(h.iter().any(|h| h.what == Obstacle::Fixture("Vise jaw".into())), "{h:?}");
        // the spindle nose down into the table in front of the stock
        let h = p.check(DVec3::new(0.0, -100.0, -75.0), 30.0, &[], &work);
        assert!(h.iter().any(|h| h.mover == Some(7) && h.what == Obstacle::Part(4)), "{h:?}");
    }

    #[test]
    fn removes_and_adds_parts_on_the_table() {
        let mut m = test_machine();
        assert_eq!(m.remove("Base"), 3);
        m.add_on_table("Vise", vec![("Body".into(), [1, 2, 3], cuboid([100.0, 100.0, 5.0], [140.0, 300.0, 50.0]))], DVec3::new(10.0, 0.0, 0.0));
        let p = m.parts.last().unwrap();
        assert_eq!((p.name.as_str(), p.carrier), ("Vise/Body", Carrier::Y));
        let (lo, hi) = bounds(&unpack_tris(&p.mesh));
        assert_eq!((lo, hi), (DVec3::new(-10.0, -150.0, 10.0), DVec3::new(30.0, 50.0, 55.0)));
        m.shift_group("Vise", DVec3::new(0.0, 0.0, 1.0));
        assert_eq!(bounds(&unpack_tris(&m.parts.last().unwrap().mesh)).0.z, 11.0);
    }

    #[test]
    fn travel_limits() {
        let mut m = test_machine();
        m.travel = [[-100.0, 100.0], [-50.0, 50.0], [-140.0, 0.0]];
        assert_eq!(m.over_travel(DVec3::new(120.0, 0.0, -150.0)), [20.0, 0.0, 10.0]);
    }

    #[test]
    fn names_and_packing() {
        assert_eq!(carrier_from_name("DMC2 Simple X axis v3:1/hgh15ca_1:1"), Some(Carrier::X));
        assert_eq!(carrier_from_name("4-5 Axis Assembly v22:1/4thaxis v12:1"), None);
        assert_eq!(carrier_from_name("Spindle 1.5kW"), Some(Carrier::Z));
        assert_eq!(clean("DMC2 Simple Z axis v4:1"), "DMC2 Simple Z axis");
        let t = cuboid([0.0; 3], [1.0, 2.0, 3.0]);
        assert_eq!(unpack_tris(&pack_tris(&t)), t);
    }
}
