//! Klipper's printer.cfg: sections with `[include]`s and the SAVE_CONFIG block, and the parts
//! of it the motion simulation needs (kinematics, axis limits, homing, probe, levelling,
//! bed mesh, macros, Trad Rack's toolchange G-code).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::jinja::{self, V};

#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Sections in file order, later ones with the same name merged into the first.
    pub sections: Vec<(String, Vec<(String, String)>)>,
    /// Files read (for the report).
    pub files: Vec<String>,
}

fn strip_inline(v: &str) -> &str {
    // Klipper strips `;` and `#` comments that follow whitespace
    let b = v.as_bytes();
    for i in 1..b.len() {
        if (b[i] == b'#' || b[i] == b';') && b[i - 1].is_ascii_whitespace() {
            return v[..i].trim_end();
        }
    }
    v.trim_end()
}

impl Config {
    pub fn get(&self, sec: &str, key: &str) -> Option<&str> {
        let key = key.to_lowercase();
        self.sections.iter().find(|(n, _)| n == sec).and_then(|(_, kv)| kv.iter().rev().find(|(k, _)| *k == key).map(|(_, v)| v.as_str()))
    }
    pub fn has(&self, sec: &str) -> bool {
        self.sections.iter().any(|(n, _)| n == sec)
    }
    pub fn f(&self, sec: &str, key: &str) -> Option<f64> {
        self.get(sec, key).and_then(|v| v.trim().parse().ok())
    }
    pub fn b(&self, sec: &str, key: &str) -> Option<bool> {
        self.get(sec, key).map(|v| matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes" | "on"))
    }
    pub fn pairs(&self, sec: &str, key: &str) -> Vec<[f64; 2]> {
        self.get(sec, key)
            .map(|v| {
                v.split('\n')
                    .flat_map(|l| {
                        let n: Vec<f64> = l.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                        // "x, y" per line, or a flat "x1, y1, x2, y2" list on one line
                        n.chunks(2).filter(|c| c.len() == 2).map(|c| [c[0], c[1]]).collect::<Vec<_>>()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn pair(&self, sec: &str, key: &str) -> Option<[f64; 2]> {
        self.pairs(sec, key).first().copied()
    }

    fn add(&mut self, sec: &str, key: String, val: String) {
        if let Some((_, kv)) = self.sections.iter_mut().find(|(n, _)| n == sec) {
            kv.push((key, val));
        } else {
            self.sections.push((sec.to_string(), vec![(key, val)]));
        }
    }

    fn touch(&mut self, sec: &str) {
        if !self.has(sec) {
            self.sections.push((sec.to_string(), vec![]));
        }
    }

    /// Read a printer.cfg and everything it includes.
    pub fn load(path: &Path) -> Result<Config, String> {
        let mut c = Config::default();
        c.read_file(path, 0)?;
        Ok(c)
    }

    /// Parse config text (includes resolved against `dir`, when given).
    pub fn parse(text: &str, dir: Option<&Path>) -> Result<Config, String> {
        let mut c = Config::default();
        c.read_text(text, dir, 0)?;
        Ok(c)
    }

    fn read_file(&mut self, path: &Path, depth: usize) -> Result<(), String> {
        if depth > 20 {
            return Err("config includes nest too deeply".into());
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.files.push(path.display().to_string());
        self.read_text(&text, path.parent(), depth)
    }

    fn read_text(&mut self, text: &str, dir: Option<&Path>, depth: usize) -> Result<(), String> {
        // the SAVE_CONFIG block is commented out with `#*# `
        let (main, auto) = match text.find("#*# <") {
            Some(p) => (&text[..p], Some(&text[p..])),
            None => (text, None),
        };
        self.read_body(main, dir, depth)?;
        if let Some(a) = auto {
            let body: String =
                a.lines().filter_map(|l| l.strip_prefix("#*#")).map(|l| l.strip_prefix(' ').unwrap_or(l)).filter(|l| !l.starts_with('<')).collect::<Vec<_>>().join("\n");
            self.read_body(&body, dir, depth)?;
        }
        Ok(())
    }

    fn read_body(&mut self, text: &str, dir: Option<&Path>, depth: usize) -> Result<(), String> {
        let mut sec: Option<String> = None;
        let mut cur: Option<(String, String)> = None;
        let flush = |c: &mut Config, sec: &Option<String>, cur: &mut Option<(String, String)>| {
            if let (Some(s), Some((k, v))) = (sec, cur.take()) {
                c.add(s, k, v.trim_end_matches('\n').to_string());
            }
        };
        for raw in text.lines() {
            let t = raw.trim();
            if t.starts_with('#') || t.starts_with(';') {
                continue;
            }
            if t.is_empty() {
                // blank lines inside a multi-line value are kept (templates), trailing ones trimmed later
                if let Some((_, v)) = cur.as_mut() {
                    v.push('\n');
                }
                continue;
            }
            let indented = raw.starts_with(' ') || raw.starts_with('\t');
            if indented && cur.is_some() {
                let (_, v) = cur.as_mut().unwrap();
                if !v.is_empty() && !v.ends_with('\n') {
                    v.push('\n');
                }
                v.push_str(strip_inline(raw.trim()));
                v.push('\n');
                continue;
            }
            if t.starts_with('[') {
                flush(self, &sec, &mut cur);
                let name = t.trim_start_matches('[').split(']').next().unwrap_or("").trim().to_string();
                if let Some(inc) = name.strip_prefix("include ") {
                    let base = dir.map(PathBuf::from).unwrap_or_default();
                    let pat = base.join(inc.trim());
                    for p in glob(&pat) {
                        self.read_file(&p, depth + 1)?;
                    }
                    sec = None;
                    continue;
                }
                self.touch(&name);
                sec = Some(name);
                continue;
            }
            flush(self, &sec, &mut cur);
            let Some(sep) = t.find([':', '=']) else {
                // not a setting (the SAVE_CONFIG banner, stray text): Klipper would complain, the simulation goes on
                continue;
            };
            let key = t[..sep].trim().to_lowercase();
            let val = strip_inline(t[sep + 1..].trim()).to_string();
            cur = Some((key, val));
        }
        flush(self, &sec, &mut cur);
        Ok(())
    }
}

/// `[include *.cfg]`: `*` and `?` in the file name.
fn glob(pat: &Path) -> Vec<PathBuf> {
    let name = pat.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if !name.contains('*') && !name.contains('?') {
        return vec![pat.to_path_buf()];
    }
    let dir = pat.parent().unwrap_or(Path::new("."));
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.file_name().map(|f| wild(&name, &f.to_string_lossy())).unwrap_or(false)).collect())
        .unwrap_or_default();
    out.sort();
    out
}

fn wild(p: &str, s: &str) -> bool {
    let (p, s): (Vec<char>, Vec<char>) = (p.chars().collect(), s.chars().collect());
    fn m(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => m(&p[1..], s) || (!s.is_empty() && m(p, &s[1..])),
            (Some('?'), Some(_)) => m(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => m(&p[1..], &s[1..]),
            _ => false,
        }
    }
    m(&p, &s)
}

// ---------------------------------------------------------------- the machine

#[derive(Clone, Debug)]
pub struct Axis {
    pub min: f64,
    pub max: f64,
    pub endstop: Option<f64>,
    pub homing_speed: f64,
    pub retract: f64,
    /// Homes towards max.
    pub positive: bool,
}

impl Default for Axis {
    fn default() -> Self {
        Axis { min: 0.0, max: 300.0, endstop: Some(0.0), homing_speed: 5.0, retract: 5.0, positive: false }
    }
}

#[derive(Clone, Debug)]
pub struct Macro {
    pub name: String,
    pub template: Result<jinja::Template, String>,
    pub vars: BTreeMap<String, V>,
    pub rename_existing: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Probe {
    pub section: String,
    pub offset: [f64; 3],
    /// Toolhead position for Z homing (Beacon/Cartographer `home_xy_position`, or `safe_z_home`).
    pub home_xy: Option<[f64; 2]>,
    pub z_hop: f64,
    pub speed: f64,
    /// Z where a proximity probe stops (Beacon triggers about 2 mm above the bed).
    pub trigger_z: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Leveling {
    pub kind: String,
    /// Nozzle positions probed.
    pub points: Vec<[f64; 2]>,
    pub z: f64,
    pub speed: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub min: [f64; 2],
    pub max: [f64; 2],
    pub count: [usize; 2],
    pub z: f64,
    pub speed: f64,
    /// Beacon/Cartographer scan the mesh in a continuous serpentine; this many passes.
    pub runs: usize,
    pub main_y: bool,
}

#[derive(Clone, Debug)]
pub struct Machine {
    pub kinematics: String,
    pub max_velocity: f64,
    pub max_accel: f64,
    pub max_z_velocity: f64,
    pub max_z_accel: f64,
    pub scv: f64,
    pub min_cruise_ratio: f64,
    pub axes: [Axis; 3],
    pub nozzle_diameter: f64,
    pub filament_diameter: f64,
    pub max_extrude_only_velocity: f64,
    pub probe: Option<Probe>,
    pub safe_z_home: Option<([f64; 2], f64, f64)>,
    pub homing_override: Option<(jinja::Template, String)>,
    pub leveling: Option<Leveling>,
    pub mesh: Option<Mesh>,
    pub macros: BTreeMap<String, Macro>,
    pub arc_resolution: f64,
    pub tr_pre_unload: Option<jinja::Template>,
    pub tr_post_load: Option<jinja::Template>,
    pub tr_lanes: usize,
    pub notes: Vec<String>,
}

impl Default for Machine {
    fn default() -> Self {
        Machine {
            kinematics: "cartesian".into(),
            max_velocity: 300.0,
            max_accel: 3000.0,
            max_z_velocity: 15.0,
            max_z_accel: 100.0,
            scv: 5.0,
            min_cruise_ratio: 0.5,
            axes: [Axis::default(), Axis::default(), Axis { endstop: None, ..Axis::default() }],
            nozzle_diameter: 0.4,
            filament_diameter: 1.75,
            max_extrude_only_velocity: 50.0,
            probe: None,
            safe_z_home: None,
            homing_override: None,
            leveling: None,
            mesh: None,
            macros: BTreeMap::new(),
            arc_resolution: 1.0,
            tr_pre_unload: None,
            tr_post_load: None,
            tr_lanes: 0,
            notes: vec![],
        }
    }
}

impl Machine {
    pub fn from_config(c: &Config) -> Machine {
        let mut m = Machine::default();
        let p = "printer";
        m.kinematics = c.get(p, "kinematics").unwrap_or("cartesian").trim().to_string();
        m.max_velocity = c.f(p, "max_velocity").unwrap_or(m.max_velocity);
        m.max_accel = c.f(p, "max_accel").unwrap_or(m.max_accel);
        m.max_z_velocity = c.f(p, "max_z_velocity").unwrap_or(m.max_velocity);
        m.max_z_accel = c.f(p, "max_z_accel").unwrap_or(m.max_accel);
        m.scv = c.f(p, "square_corner_velocity").unwrap_or(5.0);
        m.min_cruise_ratio = c.f(p, "minimum_cruise_ratio").unwrap_or_else(|| c.f(p, "max_accel_to_decel").map(|d| 1.0 - d / m.max_accel).unwrap_or(0.5));
        for (i, a) in ["x", "y", "z"].iter().enumerate() {
            let s = format!("stepper_{a}");
            let ax = &mut m.axes[i];
            ax.min = c.f(&s, "position_min").unwrap_or(0.0);
            ax.max = c.f(&s, "position_max").unwrap_or(ax.max);
            ax.endstop = c.f(&s, "position_endstop");
            ax.homing_speed = c.f(&s, "homing_speed").unwrap_or(5.0);
            ax.retract = c.f(&s, "homing_retract_dist").unwrap_or(5.0);
            ax.positive = c.b(&s, "homing_positive_dir").unwrap_or_else(|| ax.endstop.map(|e| e >= ax.max && ax.max > ax.min).unwrap_or(false));
        }
        m.nozzle_diameter = c.f("extruder", "nozzle_diameter").unwrap_or(0.4);
        m.filament_diameter = c.f("extruder", "filament_diameter").unwrap_or(1.75);
        m.max_extrude_only_velocity = c.f("extruder", "max_extrude_only_velocity").unwrap_or(m.max_velocity.min(50.0));
        for sec in ["beacon", "cartographer", "scanner", "probe", "bltouch", "probe_eddy_current", "smart_effector"] {
            let name = c.sections.iter().map(|(n, _)| n.as_str()).find(|n| *n == sec || n.starts_with(&format!("{sec} ")));
            if let Some(n) = name {
                m.probe = Some(Probe {
                    section: n.to_string(),
                    offset: [c.f(n, "x_offset").unwrap_or(0.0), c.f(n, "y_offset").unwrap_or(0.0), c.f(n, "z_offset").unwrap_or(0.0)],
                    home_xy: c.pair(n, "home_xy_position"),
                    z_hop: c.f(n, "home_z_hop").unwrap_or(0.0),
                    speed: c.f(n, "home_xy_move_speed").or_else(|| c.f(n, "speed")).unwrap_or(50.0),
                    trigger_z: if matches!(sec, "beacon" | "cartographer" | "scanner") { c.f(n, "trigger_distance").unwrap_or(2.0) } else { c.f(n, "z_offset").unwrap_or(0.0) },
                });
                break;
            }
        }
        if c.has("safe_z_home") {
            m.safe_z_home = c.pair("safe_z_home", "home_xy_position").map(|xy| (xy, c.f("safe_z_home", "z_hop").unwrap_or(0.0), c.f("safe_z_home", "speed").unwrap_or(50.0)));
        }
        if let Some(g) = c.get("homing_override", "gcode") {
            match jinja::Template::parse(g) {
                Ok(t) => m.homing_override = Some((t, c.get("homing_override", "axes").unwrap_or("xyz").to_lowercase())),
                Err(e) => m.notes.push(format!("[homing_override] gcode: {e}")),
            }
        }
        for sec in ["z_tilt_ng", "z_tilt", "quad_gantry_level"] {
            if c.has(sec) {
                m.leveling = Some(Leveling {
                    kind: sec.to_string(),
                    points: c.pairs(sec, "points"),
                    z: c.f(sec, "horizontal_move_z").unwrap_or(5.0),
                    speed: c.f(sec, "speed").unwrap_or(50.0),
                });
                if c.b(sec, "use_probe_xy_offsets").unwrap_or(false) {
                    if let (Some(l), Some(p)) = (m.leveling.as_mut(), m.probe.as_ref()) {
                        for q in &mut l.points {
                            q[0] -= p.offset[0];
                            q[1] -= p.offset[1];
                        }
                    }
                }
                break;
            }
        }
        if c.has("bed_mesh") {
            let s = "bed_mesh";
            let count = c.get(s, "probe_count").map(|v| v.split(',').filter_map(|x| x.trim().parse::<usize>().ok()).collect::<Vec<_>>()).unwrap_or_else(|| vec![3, 3]);
            let scanning = m.probe.as_ref().map(|p| ["beacon", "cartographer", "scanner"].iter().any(|k| p.section.starts_with(k))).unwrap_or(false);
            m.mesh = Some(Mesh {
                min: c.pair(s, "mesh_min").unwrap_or([0.0, 0.0]),
                max: c.pair(s, "mesh_max").unwrap_or([m.axes[0].max, m.axes[1].max]),
                count: [count.first().copied().unwrap_or(3).max(2), count.get(1).or(count.first()).copied().unwrap_or(3).max(2)],
                z: c.f(s, "horizontal_move_z").unwrap_or(5.0),
                speed: c.f(s, "speed").unwrap_or(50.0),
                runs: if scanning { m.probe.as_ref().and_then(|p| c.f(&p.section, "mesh_runs")).unwrap_or(1.0).max(1.0) as usize } else { 0 },
                main_y: m.probe.as_ref().and_then(|p| c.get(&p.section, "mesh_main_direction")).map(|d| d.trim() == "y").unwrap_or(false),
            });
        }
        m.arc_resolution = c.f("gcode_arcs", "resolution").unwrap_or(1.0);
        if c.has("trad_rack") {
            let parse = |k: &str, m: &mut Machine| -> Option<jinja::Template> {
                c.get("trad_rack", k).and_then(|g| match jinja::Template::parse(g) {
                    Ok(t) => Some(t),
                    Err(e) => {
                        m.notes.push(format!("[trad_rack] {k}: {e}"));
                        None
                    }
                })
            };
            m.tr_pre_unload = parse("pre_unload_gcode", &mut m);
            m.tr_post_load = parse("post_load_gcode", &mut m);
            m.tr_lanes = c.f("trad_rack", "lane_count").unwrap_or(0.0) as usize;
        }
        for (name, kv) in &c.sections {
            let Some(mname) = name.strip_prefix("gcode_macro ") else { continue };
            let get = |k: &str| kv.iter().rev().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
            let mut vars = BTreeMap::new();
            for (k, v) in kv {
                if let Some(var) = k.strip_prefix("variable_") {
                    vars.insert(var.to_string(), jinja::literal(v));
                }
            }
            let up = mname.trim().to_uppercase();
            m.macros.insert(
                up.clone(),
                Macro {
                    name: up,
                    template: jinja::Template::parse(&get("gcode").unwrap_or_default()),
                    vars,
                    rename_existing: get("rename_existing").map(|s| s.trim().to_uppercase()),
                },
            );
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sections() {
        let text = "[printer]\nkinematics: corexy  # a comment\nmax_velocity: 600\n\n[stepper_x]\nposition_max: 327\nposition_min: -5\nposition_endstop: 0\n\n[gcode_macro FOO]\nvariable_n: 3\ngcode:\n    G1 X{n} ; go\n    # a comment line\n    G1 Y1\n\n#*# <---------------------- SAVE_CONFIG ---------------------->\n#*# [stepper_x]\n#*# position_max = 330\n";
        let c = Config::parse(text, None).unwrap();
        assert_eq!(c.get("printer", "kinematics"), Some("corexy"));
        let m = Machine::from_config(&c);
        assert_eq!(m.axes[0].max, 330.0);
        assert_eq!(m.axes[0].min, -5.0);
        let f = &m.macros["FOO"];
        assert_eq!(f.vars["n"], V::Int(3));
        let mut ctx = jinja::Ctx::with(f.vars.clone());
        assert_eq!(f.template.as_ref().unwrap().render(&mut ctx).unwrap().trim(), "G1 X3\nG1 Y1");
    }
}
