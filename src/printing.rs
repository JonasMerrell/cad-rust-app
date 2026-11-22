//! 3D-printing helpers: slicing the parts with the OrcaSlicer command line (`orca-slice`), and
//! the printability view (overhangs and thin walls).

/// User settings for printing, kept in ~/.config/cad-rust-app/printing.json.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(default)]
pub struct PrintSettings {
    /// The slicer command: `orca-slice` (Jonas's OrcaSlicer fork with his presets) or a path to it.
    pub slicer: String,
    /// Process preset; empty for the slicer's default.
    pub process: String,
    /// Filament preset; empty for the slicer's default.
    pub filament: String,
    /// STL detail: largest distance between the mesh and the true surface, mm.
    pub stl_tolerance: f64,
    /// Overhang angle from vertical beyond which faces need support, degrees.
    pub overhang: f64,
    /// Walls thinner than this are flagged, mm.
    pub min_wall: f64,
}

impl Default for PrintSettings {
    fn default() -> Self {
        PrintSettings { slicer: "orca-slice".into(), process: String::new(), filament: String::new(), stl_tolerance: 0.01, overhang: 45.0, min_wall: 0.8 }
    }
}

fn settings_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))).or_else(|| std::env::var_os("APPDATA").map(std::path::PathBuf::from))?;
    Some(base.join("cad-rust-app").join("printing.json"))
}

pub fn load() -> PrintSettings {
    settings_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save(s: &PrintSettings) {
    if let Some(p) = settings_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, serde_json::to_string_pretty(s).unwrap_or_default());
    }
}

/// What to slice with; empty fields leave the slicer's defaults.
#[derive(Clone, Debug, Default)]
pub struct SliceOptions {
    pub printer: Option<String>,
    pub process: Option<String>,
    pub filaments: Vec<String>,
    /// Trad Rack lane 1-9 for a single-colour print.
    pub lane: Option<u32>,
    /// "off", "normal" or "tree"; None keeps the process preset's choice.
    pub support: Option<String>,
    /// Extra `key=value` overrides.
    pub set: Vec<String>,
}

impl SliceOptions {
    /// The settings' process and filament, when they are set.
    pub fn from_settings(s: &PrintSettings) -> Self {
        let some = |v: &str| (!v.trim().is_empty()).then(|| v.trim().to_string());
        SliceOptions { process: some(&s.process), filaments: some(&s.filament).into_iter().collect(), ..Default::default() }
    }

    fn args(&self) -> Vec<String> {
        let mut a = vec![];
        let mut push = |k: &str, v: &str| {
            a.push(k.to_string());
            a.push(v.to_string());
        };
        if let Some(p) = &self.printer {
            push("-p", p);
        }
        if let Some(p) = &self.process {
            push("-P", p);
        }
        for f in &self.filaments {
            push("-f", f);
        }
        if let Some(l) = self.lane {
            push("--lane", &l.to_string());
        }
        if let Some(s) = &self.support {
            push("--support", s);
        }
        for s in &self.set {
            push("--set", s);
        }
        a
    }
}

/// Where sliced files go when no path is given: <temp>/rcad-slices.
pub fn slice_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("rcad-slices")
}

/// A file name made of the job name's safe characters.
pub fn job_file_name(name: &str) -> String {
    let n: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    if n.is_empty() { "part".into() } else { n }
}

/// Slice an STL with the slicer command. `output` is a .gcode file or a folder. Returns the
/// slicer's summary: {gcode, layers, height_mm, filament_g, time, size_bytes, …}. Never uploads
/// to the printer or starts a print.
pub fn slice(slicer: &str, model: &std::path::Path, output: &std::path::Path, opts: &SliceOptions) -> Result<serde_json::Value, String> {
    let slicer = if slicer.trim().is_empty() { "orca-slice" } else { slicer.trim() };
    let out = std::process::Command::new(slicer)
        .arg(model)
        .args(opts.args())
        .arg("-o")
        .arg(output)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("the slicer command `{slicer}` was not found; install orca-slice or set its path in the 3D printing settings")
            } else {
                format!("could not run `{slicer}`: {e}")
            }
        })?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        // the first line says what failed; the last few lines of the slicer's log follow
        let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
        let mut msg = lines.first().map(|l| l.trim_start_matches("orca-slice: ").to_string()).unwrap_or_else(|| format!("the slicer exited with {}", out.status));
        if lines.len() > 1 {
            let tail = &lines[lines.len().saturating_sub(6).max(1)..];
            msg.push_str("\n");
            msg.push_str(&tail.join("\n"));
        }
        return Err(msg);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.trim()).map_err(|_| format!("the slicer's output was not understood: {}", stdout.trim().chars().take(300).collect::<String>()))
}

/// Open a URL in the default browser.
pub fn open_url(url: &str) {
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Per-triangle printability of a mesh: 0 fine, 1 overhang, 2 thin wall.
/// `floor` is the build plate height (the lowest point of all parts).
pub fn classify(m: &crate::kernel::BodyMesh, floor: f64, overhang_deg: f64, min_wall: f64) -> Vec<u8> {
    use glam::DVec3;
    let p = |i: u32| DVec3::new(m.positions[3 * i as usize] as f64, m.positions[3 * i as usize + 1] as f64, m.positions[3 * i as usize + 2] as f64);
    let tris: Vec<[DVec3; 3]> = m.indices.chunks(3).map(|t| [p(t[0]), p(t[1]), p(t[2])]).collect();
    // a face tilted θ from vertical, facing down, has normal z = −sin θ: past the limit is an overhang
    let limit = overhang_deg.to_radians().sin();
    let mut out = vec![0u8; tris.len()];
    let grid = Grid::new(&tris);
    for (i, t) in tris.iter().enumerate() {
        let n = (t[1] - t[0]).cross(t[2] - t[0]);
        let area2 = n.length();
        if area2 < 1e-12 {
            continue;
        }
        let n = n / area2;
        let c = (t[0] + t[1] + t[2]) / 3.0;
        let on_plate = t.iter().all(|v| v.z < floor + 0.05);
        if n.z < -limit && !on_plate {
            out[i] = 1;
        }
        // thickness: distance to the opposite wall along the inward normal
        if min_wall > 0.0 {
            if let Some(d) = grid.hit(c - n * 1e-4, -n, min_wall, i) {
                if d < min_wall {
                    out[i] = 2;
                }
            }
        }
    }
    out
}

/// Uniform grid over triangles for ray casting.
struct Grid<'a> {
    tris: &'a [[glam::DVec3; 3]],
    lo: glam::DVec3,
    cell: f64,
    dims: [usize; 3],
    cells: Vec<Vec<u32>>,
}

impl<'a> Grid<'a> {
    fn new(tris: &'a [[glam::DVec3; 3]]) -> Grid<'a> {
        use glam::DVec3;
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for t in tris {
            for v in t {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
        }
        if !lo.x.is_finite() {
            return Grid { tris, lo: DVec3::ZERO, cell: 1.0, dims: [1, 1, 1], cells: vec![vec![]] };
        }
        let size = (hi - lo).max(DVec3::splat(1e-6));
        // about 2 triangles per cell
        let cell = ((size.x * size.y * size.z) / (tris.len().max(1) as f64 / 2.0)).cbrt().max(size.max_element() / 200.0);
        let dims = [((size.x / cell).ceil() as usize).max(1), ((size.y / cell).ceil() as usize).max(1), ((size.z / cell).ceil() as usize).max(1)];
        let mut cells = vec![vec![]; dims[0] * dims[1] * dims[2]];
        let idx = |v: DVec3, d: usize, k: usize| (((v[k] - lo[k]) / cell).floor().max(0.0) as usize).min(d - 1);
        for (i, t) in tris.iter().enumerate() {
            let (a, b) = (t[0].min(t[1]).min(t[2]), t[0].max(t[1]).max(t[2]));
            for x in idx(a, dims[0], 0)..=idx(b, dims[0], 0) {
                for y in idx(a, dims[1], 1)..=idx(b, dims[1], 1) {
                    for z in idx(a, dims[2], 2)..=idx(b, dims[2], 2) {
                        cells[x + dims[0] * (y + dims[1] * z)].push(i as u32);
                    }
                }
            }
        }
        Grid { tris, lo, cell, dims, cells }
    }

    /// Nearest hit distance along `d` within `max`, ignoring triangle `skip`.
    fn hit(&self, o: glam::DVec3, d: glam::DVec3, max: f64, skip: usize) -> Option<f64> {
        let mut best: Option<f64> = None;
        let steps = (max / (self.cell * 0.5)).ceil() as usize + 1;
        let mut seen: Vec<u32> = vec![];
        for s in 0..=steps {
            let q = o + d * (s as f64 * self.cell * 0.5).min(max);
            let c = [0, 1, 2].map(|k| ((q[k] - self.lo[k]) / self.cell).floor());
            if c.iter().enumerate().any(|(k, v)| *v < 0.0 || *v as usize >= self.dims[k]) {
                continue;
            }
            let ci = c[0] as usize + self.dims[0] * (c[1] as usize + self.dims[1] * c[2] as usize);
            for &t in &self.cells[ci] {
                if t as usize == skip || seen.contains(&t) {
                    continue;
                }
                seen.push(t);
                let [a, b, cc] = self.tris[t as usize];
                if let Some(h) = ray_tri(o, d, a, b, cc) {
                    if h > 1e-6 && h <= max && best.map(|x| h < x).unwrap_or(true) {
                        best = Some(h);
                    }
                }
            }
            if best.map(|b| b < s as f64 * self.cell * 0.5 - self.cell).unwrap_or(false) {
                break;
            }
        }
        best
    }
}

fn ray_tri(o: glam::DVec3, d: glam::DVec3, a: glam::DVec3, b: glam::DVec3, c: glam::DVec3) -> Option<f64> {
    let (e1, e2) = (b - a, c - a);
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-14 {
        return None;
    }
    let inv = 1.0 / det;
    let t = o - a;
    let u = t.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = t.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    Some(e2.dot(q) * inv)
}
