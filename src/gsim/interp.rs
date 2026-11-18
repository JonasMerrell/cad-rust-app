//! The G-code interpreter: Klipper's G-code state (absolute/relative, offsets, saved states,
//! homing) and its macros, turned into a list of straight toolhead moves in Klipper's
//! toolhead coordinates (what the kinematics see). Homing, Z tilt, the bed mesh scan and
//! Trad Rack toolchanges move the toolhead too, so they are expanded into the moves they make.

use std::collections::BTreeMap;
use std::rc::Rc;

use super::cfg::Machine;
use super::jinja::{self, Ctx, V};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Travel,
    Print,
    /// Extruder only (retract, prime, a Trad Rack load or unload).
    Extrude,
    /// A homing move towards an endstop (or a probe triggering).
    Home,
    /// Probing or scanning the bed (Z tilt, bed mesh, Z homing on a probe).
    Probe,
    /// Standing still (G4).
    Dwell,
}

/// Where a move came from: the line in the file, and the macros it went through.
#[derive(Clone, Debug)]
pub struct Src {
    pub line: usize,
    pub text: Rc<str>,
    /// "START_PRINT > G28" for moves a macro made, "" for the file's own.
    pub via: Rc<str>,
}

#[derive(Clone, Debug)]
pub struct Move {
    /// Toolhead position (Klipper's coordinates: the G-code position plus offsets), mm.
    pub from: [f64; 3],
    pub to: [f64; 3],
    /// Filament pushed (mm, after the extrude factor); negative retracts.
    pub e: f64,
    /// Requested speed, mm/s (dwell: its duration, s).
    pub speed: f64,
    pub kind: Kind,
    pub src: u32,
    /// Extrusion line width and layer height the slicer gave (0 when it didn't).
    pub width: f32,
    pub height: f32,
    /// The Trad Rack lane (or tool) feeding it.
    #[allow(dead_code)]
    pub tool: u16,
    /// Filled by the planner: start and end time, s.
    pub t0: f64,
    pub t1: f64,
}

impl Move {
    pub fn len(&self) -> f64 {
        let d = [self.to[0] - self.from[0], self.to[1] - self.from[1], self.to[2] - self.from[2]];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Issue {
    pub kind: &'static str,
    pub line: usize,
    pub via: String,
    pub text: String,
    pub message: String,
}

#[derive(Clone, Debug)]
struct Saved {
    abs: bool,
    abs_e: bool,
    base: [f64; 4],
    last: [f64; 4],
    origin: [f64; 3],
    speed: f64,
    speed_factor: f64,
    extrude_factor: f64,
}

pub struct Interp<'m> {
    pub m: &'m Machine,
    /// Toolhead position, None until homed (or set).
    pos: [Option<f64>; 3],
    e: f64,
    /// G-code position = toolhead - base.
    base: [f64; 4],
    origin: [f64; 3],
    abs: bool,
    abs_e: bool,
    speed: f64,
    speed_factor: f64,
    extrude_factor: f64,
    saved: BTreeMap<String, Saved>,
    pub vars: BTreeMap<String, BTreeMap<String, V>>,
    temps: BTreeMap<&'static str, f64>,
    width: f32,
    height: f32,
    tool: u16,
    tr_lane: Option<u16>,
    pub in_file: bool,
    depth: usize,
    pub moves: Vec<Move>,
    pub srcs: Vec<Src>,
    cur_src: u32,
    pub issues: Vec<Issue>,
    /// Commands and macros seen, with how they were handled.
    pub commands: BTreeMap<String, (usize, &'static str)>,
    pub layer_z: Vec<(usize, f64)>,
    /// Stops the run (action_raise_error, too many errors).
    pub aborted: Option<String>,
    renamed: BTreeMap<String, String>,
    homed_z_by_probe: bool,
    mesh_applied: bool,
    pub max_issues: usize,
}

fn word_cmd(line: &str) -> (String, &str) {
    let t = line.trim_start();
    let end = t.find(|c: char| c.is_whitespace()).unwrap_or(t.len());
    let first = &t[..end];
    // traditional G-code: a letter and a number ("G1", "M104", "T0"), maybe glued to its args ("G1X10")
    let b = first.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && (b[1].is_ascii_digit() || b[1] == b'.') {
        let mut k = 1;
        while k < b.len() && (b[k].is_ascii_digit() || b[k] == b'.') {
            k += 1;
        }
        return (first[..k].to_uppercase(), &t[k..]);
    }
    (first.to_uppercase(), &t[end..])
}

/// Letter parameters of traditional G-code ("X10 Y-3.5 F600").
fn letter_params(s: &str) -> BTreeMap<char, f64> {
    let mut out = BTreeMap::new();
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < c.len() {
        if c[i].is_ascii_alphabetic() {
            let k = c[i].to_ascii_uppercase();
            i += 1;
            let st = i;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.' || c[i] == '-' || c[i] == '+' || c[i] == 'e') {
                if c[i] == 'e' && !(i + 1 < c.len() && (c[i + 1].is_ascii_digit() || c[i + 1] == '-')) {
                    break;
                }
                i += 1;
            }
            let n: String = c[st..i].iter().collect();
            out.insert(k, n.parse().unwrap_or(0.0));
        } else {
            i += 1;
        }
    }
    out
}

/// KEY=VALUE parameters of extended commands (values may be quoted).
fn ext_params(s: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < c.len() {
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        let st = i;
        while i < c.len() && c[i] != '=' && !c[i].is_whitespace() {
            i += 1;
        }
        let key: String = c[st..i].iter().collect();
        if i < c.len() && c[i] == '=' {
            i += 1;
            let val: String = if i < c.len() && (c[i] == '"' || c[i] == '\'') {
                let q = c[i];
                i += 1;
                let st = i;
                while i < c.len() && c[i] != q {
                    i += 1;
                }
                let v = c[st..i].iter().collect();
                i += 1;
                v
            } else {
                let st = i;
                while i < c.len() && !c[i].is_whitespace() {
                    i += 1;
                }
                c[st..i].iter().collect()
            };
            out.insert(key.to_uppercase(), val);
        } else if !key.is_empty() {
            // traditional-style params on an extended command ("G28 X Y", "T0 MIN_TEMP=..")
            out.insert(key.to_uppercase(), String::new());
        }
    }
    out
}

impl<'m> Interp<'m> {
    pub fn new(m: &'m Machine) -> Interp<'m> {
        let mut vars = BTreeMap::new();
        let mut renamed = BTreeMap::new();
        for (k, mc) in &m.macros {
            vars.insert(k.clone(), mc.vars.clone());
            if let Some(r) = &mc.rename_existing {
                renamed.insert(r.clone(), k.clone());
            }
        }
        Interp {
            m,
            pos: [None; 3],
            e: 0.0,
            base: [0.0; 4],
            origin: [0.0; 3],
            abs: true,
            abs_e: true,
            speed: 25.0,
            speed_factor: 1.0,
            extrude_factor: 1.0,
            saved: BTreeMap::new(),
            vars,
            temps: BTreeMap::new(),
            width: 0.0,
            height: 0.0,
            tool: 0,
            tr_lane: None,
            in_file: false,
            depth: 0,
            moves: vec![],
            srcs: vec![],
            cur_src: 0,
            issues: vec![],
            commands: BTreeMap::new(),
            layer_z: vec![],
            aborted: None,
            renamed,
            homed_z_by_probe: false,
            mesh_applied: false,
            max_issues: 2000,
        }
    }

    /// Start with the toolhead homed at this position (to simulate a file that doesn't home).
    pub fn assume_homed(&mut self, at: [f64; 3]) {
        self.pos = [Some(at[0]), Some(at[1]), Some(at[2])];
    }

    fn issue(&mut self, kind: &'static str, msg: String) {
        if self.issues.len() >= self.max_issues {
            return;
        }
        let s = self.srcs.get(self.cur_src as usize).cloned();
        self.issues.push(Issue {
            kind,
            line: s.as_ref().map(|s| s.line).unwrap_or(0),
            via: s.as_ref().map(|s| s.via.to_string()).unwrap_or_default(),
            text: s.as_ref().map(|s| s.text.to_string()).unwrap_or_default(),
            message: msg,
        });
    }

    fn note_cmd(&mut self, c: &str, how: &'static str) {
        let e = self.commands.entry(c.to_string()).or_insert((0, how));
        e.0 += 1;
        e.1 = how;
    }

    fn homed(&self) -> String {
        ["x", "y", "z"].iter().zip(self.pos).filter(|(_, p)| p.is_some()).map(|(a, _)| *a).collect()
    }

    fn th(&self) -> [f64; 3] {
        [self.pos[0].unwrap_or(0.0), self.pos[1].unwrap_or(0.0), self.pos[2].unwrap_or(0.0)]
    }

    fn gpos(&self) -> [f64; 4] {
        let t = self.th();
        [t[0] - self.base[0], t[1] - self.base[1], t[2] - self.base[2], self.e - self.base[3]]
    }

    // ------------------------------------------------------------ moves

    fn push(&mut self, to: [f64; 3], e: f64, speed: f64, kind: Kind) {
        let from = self.th();
        let moved = (0..3).any(|i| (to[i] - from[i]).abs() > 1e-9);
        if !moved && e == 0.0 && kind != Kind::Dwell {
            return;
        }
        self.moves.push(Move { from, to, e, speed: speed.max(1e-3), kind, src: self.cur_src, width: self.width, height: self.height, tool: self.tool, t0: 0.0, t1: 0.0 });
        for i in 0..3 {
            if self.pos[i].is_some() || (to[i] - from[i]).abs() > 1e-9 {
                self.pos[i] = Some(to[i]);
            }
        }
    }

    /// A move to a toolhead position (None: that axis stays), as G1 makes it.
    fn move_to(&mut self, tgt: [Option<f64>; 3], de: f64, kind_hint: Option<Kind>) {
        let mut to = self.th();
        let mut moved = false;
        for i in 0..3 {
            if let Some(v) = tgt[i] {
                if self.pos[i].is_none() {
                    if self.issues.iter().filter(|x| x.kind == "not_homed").count() < 20 {
                        self.issue("not_homed", format!("Must home axis first: {} moved before homing (Klipper refuses this move)", ["X", "Y", "Z"][i]));
                    }
                    return;
                }
                if (v - to[i]).abs() > 1e-9 {
                    moved = true;
                }
                to[i] = v;
            }
        }
        if moved {
            let lim = &self.m.axes;
            for i in 0..3 {
                if to[i] < lim[i].min - 1e-9 || to[i] > lim[i].max + 1e-9 {
                    self.issue(
                        "out_of_range",
                        format!(
                            "Move out of range: {:.3} {:.3} {:.3} ({} limits {} to {}; Klipper stops the print here)",
                            to[0],
                            to[1],
                            to[2],
                            ["X", "Y", "Z"][i],
                            lim[i].min,
                            lim[i].max
                        ),
                    );
                    break;
                }
            }
        }
        let kind = kind_hint.unwrap_or(if !moved {
            Kind::Extrude
        } else if de > 0.0 {
            Kind::Print
        } else {
            Kind::Travel
        });
        let speed = if !moved && de != 0.0 { (self.speed * self.speed_factor).min(self.m.max_extrude_only_velocity.max(1.0)) } else { self.speed * self.speed_factor };
        self.e += de;
        self.push(to, de, speed, kind);
    }

    fn travel(&mut self, to: [f64; 3], speed: f64, kind: Kind) {
        if self.pos.iter().any(|p| p.is_none()) {
            return;
        }
        self.push(to, 0.0, speed, kind);
    }

    // ------------------------------------------------------------ entry points

    /// Run a whole G-code file.
    pub fn run_file(&mut self, text: &str) {
        self.in_file = true;
        for (k, line) in text.lines().enumerate() {
            if self.aborted.is_some() {
                break;
            }
            self.srcs.push(Src { line: k + 1, text: Rc::from(line.trim()), via: Rc::from("") });
            self.cur_src = (self.srcs.len() - 1) as u32;
            self.comment(line);
            self.exec(line);
        }
        self.in_file = false;
    }

    /// Slicer comments that say the line width, layer height and layer.
    fn comment(&mut self, line: &str) {
        let Some(p) = line.find(';') else { return };
        let c = line[p + 1..].trim();
        let num = |s: &str| s.trim().parse::<f32>().ok();
        let up = c.to_uppercase();
        for (key, w) in [("WIDTH:", true), ("LINE_WIDTH:", true), ("HEIGHT:", false), ("LAYER_HEIGHT:", false)] {
            if let Some(v) = up.strip_prefix(key).and_then(|_| num(&c[key.len()..])) {
                if w {
                    self.width = v;
                } else {
                    self.height = v;
                }
            }
        }
        if up == "LAYER_CHANGE" || up.starts_with("LAYER:") || up.starts_with("CHANGE_LAYER") {
            let z = self.th()[2];
            self.layer_z.push((self.moves.len(), z));
        }
    }

    /// Execute one line (or a macro's rendered script, line by line).
    pub fn exec(&mut self, raw: &str) {
        let line = match raw.find(';') {
            Some(p) => &raw[..p],
            None => raw,
        };
        let line = line.trim();
        if line.is_empty() || self.aborted.is_some() {
            return;
        }
        // N line numbers and checksums
        let line = if line.starts_with('N') && line[1..].chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            line.split_once(char::is_whitespace).map(|(_, r)| r).unwrap_or("")
        } else {
            line
        };
        let line = line.split('*').next().unwrap_or("").trim();
        if line.is_empty() {
            return;
        }
        let (cmd, rest) = word_cmd(line);
        // a macro named like the command wins over the built-in
        if let Some(mc) = self.m.macros.get(&cmd) {
            if mc.rename_existing.is_none() || self.depth == 0 || true {
                self.run_macro(&cmd, rest);
                return;
            }
        }
        // the renamed built-in of a macro with rename_existing
        let cmd = match self.renamed.get(&cmd) {
            Some(orig) => orig.clone(),
            None => cmd,
        };
        self.builtin(&cmd, rest, line);
    }

    fn run_script(&mut self, script: &str, via: &str) {
        if self.depth > 40 {
            self.issue("macro_error", format!("{via}: macros nest more than 40 deep (recursion?)"));
            return;
        }
        let parent = self.srcs[self.cur_src as usize].clone();
        let via: Rc<str> = Rc::from(if parent.via.is_empty() { via.to_string() } else { format!("{} > {via}", parent.via) });
        self.depth += 1;
        for l in script.lines() {
            if self.aborted.is_some() {
                break;
            }
            let t = l.trim();
            if t.is_empty() {
                continue;
            }
            self.srcs.push(Src { line: parent.line, text: Rc::from(t), via: via.clone() });
            self.cur_src = (self.srcs.len() - 1) as u32;
            self.exec(t);
        }
        self.depth -= 1;
        // later lines of the caller report against the caller's own source again
        self.srcs.push(parent);
        self.cur_src = (self.srcs.len() - 1) as u32;
    }

    fn render(&mut self, t: &jinja::Template, own: BTreeMap<String, V>, params: BTreeMap<String, String>, raw: &str, who: &str) -> Option<String> {
        let mut scope = own;
        scope.insert("params".into(), V::dict(params.into_iter().map(|(k, v)| (k, V::s(&v))).collect()));
        scope.insert("rawparams".into(), V::s(raw.trim()));
        scope.insert("printer".into(), self.printer_obj());
        let mut ctx = Ctx::with(scope);
        match t.render(&mut ctx) {
            Ok(s) => {
                for i in ctx.info.drain(..) {
                    let _ = i;
                }
                if let Some(e) = ctx.error.take() {
                    self.issue("macro_error", format!("{who}: action_raise_error: {e}"));
                    self.aborted = Some(format!("{who} raised an error: {e}"));
                    return None;
                }
                let mut und: Vec<String> = ctx.undefined.drain(..).filter(|u| !u.starts_with("params") && !u.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)).collect();
                und.sort();
                und.dedup();
                if !und.is_empty() {
                    self.issue("macro_undefined", format!("{who}: looked up {} which the simulator doesn't know; treated as empty/0", und.join(", ")));
                }
                Some(s)
            }
            Err(e) => {
                self.issue("macro_error", format!("{who}: template error: {e}; skipped"));
                None
            }
        }
    }

    fn run_macro(&mut self, name: &str, rest: &str) {
        self.note_cmd(name, "macro");
        let mc = &self.m.macros[name];
        let t = match &mc.template {
            Ok(t) => t.clone(),
            Err(e) => {
                let e = e.clone();
                self.issue("macro_error", format!("{name}: can't parse its template: {e}; skipped"));
                return;
            }
        };
        let own = self.vars.get(name).cloned().unwrap_or_default();
        let params = ext_params(rest);
        if let Some(s) = self.render(&t, own, params, rest, name) {
            self.run_script(&s, name);
        }
    }

    /// The `printer` object macros read.
    fn printer_obj(&self) -> V {
        let mut p = BTreeMap::new();
        let th = self.th();
        let ax = &self.m.axes;
        let mut toolhead = BTreeMap::new();
        toolhead.insert("homed_axes".into(), V::s(&self.homed()));
        toolhead.insert("position".into(), V::coord([th[0], th[1], th[2], self.e]));
        toolhead.insert("axis_minimum".into(), V::coord([ax[0].min, ax[1].min, ax[2].min, 0.0]));
        toolhead.insert("axis_maximum".into(), V::coord([ax[0].max, ax[1].max, ax[2].max, 0.0]));
        toolhead.insert("max_velocity".into(), V::Float(self.m.max_velocity));
        toolhead.insert("max_accel".into(), V::Float(self.m.max_accel));
        toolhead.insert("square_corner_velocity".into(), V::Float(self.m.scv));
        toolhead.insert("extruder".into(), V::s("extruder"));
        toolhead.insert("print_time".into(), V::Float(0.0));
        toolhead.insert("estimated_print_time".into(), V::Float(0.0));
        toolhead.insert("stalls".into(), V::Int(0));
        p.insert("toolhead".into(), V::dict(toolhead));
        let mut gm = BTreeMap::new();
        gm.insert("gcode_position".into(), V::coord(self.gpos()));
        gm.insert("position".into(), V::coord([th[0], th[1], th[2], self.e]));
        gm.insert("homing_origin".into(), V::coord([self.origin[0], self.origin[1], self.origin[2], 0.0]));
        gm.insert("absolute_coordinates".into(), V::Bool(self.abs));
        gm.insert("absolute_extrude".into(), V::Bool(self.abs_e));
        gm.insert("speed".into(), V::Float(self.speed * 60.0));
        gm.insert("speed_factor".into(), V::Float(self.speed_factor));
        gm.insert("extrude_factor".into(), V::Float(self.extrude_factor));
        p.insert("gcode_move".into(), V::dict(gm));
        for (obj, key) in [("extruder", "extruder"), ("heater_bed", "heater_bed"), ("heater_generic chamber", "chamber")] {
            let mut h = BTreeMap::new();
            let t = self.temps.get(key).copied().unwrap_or(0.0);
            h.insert("target".into(), V::Float(t));
            h.insert("temperature".into(), V::Float(t.max(25.0)));
            h.insert("power".into(), V::Float(0.0));
            if obj == "extruder" {
                h.insert("can_extrude".into(), V::Bool(true));
                h.insert("pressure_advance".into(), V::Float(0.0));
                h.insert("smooth_time".into(), V::Float(0.04));
            }
            p.insert(obj.into(), V::dict(h));
        }
        let mut sd = BTreeMap::new();
        sd.insert("is_active".into(), V::Bool(self.in_file));
        sd.insert("progress".into(), V::Float(0.0));
        sd.insert("file_path".into(), V::s(""));
        p.insert("virtual_sdcard".into(), V::dict(sd));
        let mut ps = BTreeMap::new();
        ps.insert("state".into(), V::s(if self.in_file { "printing" } else { "standby" }));
        ps.insert("filename".into(), V::s(""));
        p.insert("print_stats".into(), V::dict(ps));
        let mut it = BTreeMap::new();
        it.insert("state".into(), V::s(if self.in_file { "Printing" } else { "Ready" }));
        p.insert("idle_timeout".into(), V::dict(it));
        let mut pr = BTreeMap::new();
        pr.insert("is_paused".into(), V::Bool(false));
        p.insert("pause_resume".into(), V::dict(pr));
        let mut bm = BTreeMap::new();
        bm.insert("profile_name".into(), V::s(if self.mesh_applied { "default" } else { "" }));
        p.insert("bed_mesh".into(), V::dict(bm));
        let mut tr = BTreeMap::new();
        tr.insert("active_lane".into(), self.tr_lane.map(|l| V::Int(l as i64)).unwrap_or(V::None));
        tr.insert("selected_lane".into(), self.tr_lane.map(|l| V::Int(l as i64)).unwrap_or(V::None));
        tr.insert("curr_lane".into(), self.tr_lane.map(|l| V::Int(l as i64)).unwrap_or(V::None));
        p.insert("trad_rack".into(), V::dict(tr));
        let mut sv = BTreeMap::new();
        sv.insert("variables".into(), V::dict(BTreeMap::new()));
        p.insert("save_variables".into(), V::dict(sv));
        for (k, vars) in &self.vars {
            let name = self.m.macros.get(k).map(|m| m.name.clone()).unwrap_or_else(|| k.clone());
            p.insert(format!("gcode_macro {name}"), V::dict(vars.clone()));
        }
        V::dict(p)
    }

    // ------------------------------------------------------------ built-in commands

    fn builtin(&mut self, cmd: &str, rest: &str, line: &str) {
        let lp = || letter_params(rest);
        match cmd {
            "G0" | "G1" => {
                self.note_cmd(cmd, "move");
                let p = lp();
                if let Some(f) = p.get(&'F') {
                    if *f > 0.0 {
                        self.speed = f / 60.0;
                    }
                }
                let mut tgt = [None; 3];
                for (i, k) in ['X', 'Y', 'Z'].iter().enumerate() {
                    if let Some(v) = p.get(k) {
                        tgt[i] = Some(if self.abs { v + self.base[i] } else { self.pos[i].unwrap_or(0.0) + v });
                    }
                }
                let de = match p.get(&'E') {
                    Some(v) => {
                        let v = v * self.extrude_factor;
                        if !self.abs || !self.abs_e {
                            v
                        } else {
                            v + self.base[3] - self.e
                        }
                    }
                    None => 0.0,
                };
                self.move_to(tgt, de, None);
            }
            "G2" | "G3" => {
                self.note_cmd(cmd, "arc");
                self.arc(cmd == "G2", &lp());
            }
            "G4" => {
                self.note_cmd(cmd, "dwell");
                let p = lp();
                let s = p.get(&'P').map(|ms| ms / 1000.0).or_else(|| p.get(&'S').copied()).unwrap_or(0.0);
                if s > 0.0 && self.pos.iter().all(|p| p.is_some()) {
                    let at = self.th();
                    self.moves.push(Move { from: at, to: at, e: 0.0, speed: s, kind: Kind::Dwell, src: self.cur_src, width: 0.0, height: 0.0, tool: self.tool, t0: 0.0, t1: 0.0 });
                }
            }
            "G28" => {
                self.note_cmd(cmd, "home");
                let p = ext_params(rest);
                let mut axes = [p.contains_key("X"), p.contains_key("Y"), p.contains_key("Z")];
                if !axes.iter().any(|a| *a) {
                    axes = [true; 3];
                }
                let contact = p.get("METHOD").map(|m| m.eq_ignore_ascii_case("contact")).unwrap_or(false);
                self.home(axes, contact);
            }
            "G90" => self.abs = true,
            "G91" => self.abs = false,
            "M82" => self.abs_e = true,
            "M83" => self.abs_e = false,
            "G92" => {
                self.note_cmd(cmd, "set position");
                let p = lp();
                let th = self.th();
                let mut any = false;
                for (i, k) in ['X', 'Y', 'Z'].iter().enumerate() {
                    if let Some(v) = p.get(k) {
                        self.base[i] = th[i] - v;
                        any = true;
                    }
                }
                if let Some(v) = p.get(&'E') {
                    self.base[3] = self.e - v;
                    any = true;
                }
                if !any {
                    for i in 0..3 {
                        self.base[i] = th[i];
                    }
                    self.base[3] = self.e;
                }
            }
            "G17" | "G21" | "M400" | "M84" | "M18" | "M17" | "M106" | "M107" | "M117" | "M118" | "M73" | "M300" | "M115" | "M114" | "M105" | "M220" | "M221" | "M204" | "M205"
            | "M201" | "M203" | "M900" | "M141" | "M191" | "M140" | "M190" | "M104" | "M109" => {
                self.note_cmd(cmd, "no motion");
                let p = lp();
                match cmd {
                    "M220" => self.speed_factor = p.get(&'S').copied().unwrap_or(100.0) / 100.0,
                    "M221" => self.extrude_factor = p.get(&'S').copied().unwrap_or(100.0) / 100.0,
                    "M104" | "M109" => {
                        self.temps.insert("extruder", p.get(&'S').copied().unwrap_or(0.0));
                    }
                    "M140" | "M190" => {
                        self.temps.insert("heater_bed", p.get(&'S').copied().unwrap_or(0.0));
                    }
                    "M141" | "M191" => {
                        self.temps.insert("chamber", p.get(&'S').copied().unwrap_or(0.0));
                    }
                    _ => {}
                }
            }
            "G20" => self.issue("unsupported", "G20 (inches) isn't supported by Klipper".into()),
            "G10" | "G11" => self.note_cmd(cmd, "firmware retract (no XYZ motion)"),
            "SET_HEATER_TEMPERATURE" => {
                self.note_cmd(cmd, "no motion");
                let p = ext_params(rest);
                let t = p.get("TARGET").and_then(|v| v.parse().ok()).unwrap_or(0.0);
                match p.get("HEATER").map(|h| h.to_lowercase()).as_deref() {
                    Some("extruder") => {
                        self.temps.insert("extruder", t);
                    }
                    Some("heater_bed") => {
                        self.temps.insert("heater_bed", t);
                    }
                    Some("chamber") => {
                        self.temps.insert("chamber", t);
                    }
                    _ => {}
                }
            }
            "SET_GCODE_OFFSET" => {
                self.note_cmd(cmd, "offset");
                let p = ext_params(rest);
                let mut delta = [0.0; 3];
                for (i, k) in ["X", "Y", "Z"].iter().enumerate() {
                    if let Some(v) = p.get(*k).and_then(|v| v.parse::<f64>().ok()) {
                        delta[i] = v - self.origin[i];
                    }
                    if let Some(v) = p.get(&format!("{k}_ADJUST")).and_then(|v| v.parse::<f64>().ok()) {
                        delta[i] += v;
                    }
                }
                for i in 0..3 {
                    self.origin[i] += delta[i];
                    self.base[i] += delta[i];
                }
                if p.get("MOVE").map(|m| m == "1").unwrap_or(false) {
                    let speed = p.get("MOVE_SPEED").and_then(|v| v.parse().ok()).unwrap_or(self.speed);
                    let th = self.th();
                    self.travel([th[0] + delta[0], th[1] + delta[1], th[2] + delta[2]], speed, Kind::Travel);
                }
            }
            "SAVE_GCODE_STATE" => {
                self.note_cmd(cmd, "state");
                let name = ext_params(rest).get("NAME").cloned().unwrap_or_else(|| "default".into());
                let th = self.th();
                self.saved.insert(
                    name,
                    Saved {
                        abs: self.abs,
                        abs_e: self.abs_e,
                        base: self.base,
                        last: [th[0], th[1], th[2], self.e],
                        origin: self.origin,
                        speed: self.speed,
                        speed_factor: self.speed_factor,
                        extrude_factor: self.extrude_factor,
                    },
                );
            }
            "RESTORE_GCODE_STATE" => {
                self.note_cmd(cmd, "state");
                let p = ext_params(rest);
                let name = p.get("NAME").cloned().unwrap_or_else(|| "default".into());
                let Some(s) = self.saved.get(&name).cloned() else {
                    self.issue("unknown_state", format!("RESTORE_GCODE_STATE: no saved state {name}"));
                    return;
                };
                self.abs = s.abs;
                self.abs_e = s.abs_e;
                self.origin = s.origin;
                self.speed = s.speed;
                self.speed_factor = s.speed_factor;
                self.extrude_factor = s.extrude_factor;
                // Klipper keeps E where it is now
                let e_off = s.base[3] - s.last[3];
                self.base = [s.base[0], s.base[1], s.base[2], self.e + e_off];
                if p.get("MOVE").map(|m| m == "1").unwrap_or(false) {
                    let speed = p.get("MOVE_SPEED").and_then(|v| v.parse().ok()).unwrap_or(self.speed);
                    self.travel([s.last[0], s.last[1], s.last[2]], speed, Kind::Travel);
                }
            }
            "SET_GCODE_VARIABLE" => {
                self.note_cmd(cmd, "variable");
                let p = ext_params(rest);
                let (Some(m), Some(v)) = (p.get("MACRO"), p.get("VARIABLE")) else {
                    self.issue("macro_error", "SET_GCODE_VARIABLE needs MACRO and VARIABLE".into());
                    return;
                };
                let m = m.to_uppercase();
                let val = jinja::literal(p.get("VALUE").map(|s| s.as_str()).unwrap_or(""));
                match self.vars.get_mut(&m) {
                    Some(vars) => {
                        vars.insert(v.to_lowercase(), val);
                    }
                    None => self.issue("macro_error", format!("SET_GCODE_VARIABLE: no macro {m}")),
                }
            }
            "SET_KINEMATIC_POSITION" => {
                self.note_cmd(cmd, "set position");
                let p = ext_params(rest);
                let th = self.th();
                for (i, k) in ["X", "Y", "Z"].iter().enumerate() {
                    self.pos[i] = Some(p.get(*k).and_then(|v| v.parse().ok()).unwrap_or(th[i]));
                }
            }
            "BED_MESH_CALIBRATE" => {
                self.note_cmd(cmd, "bed mesh scan");
                self.bed_mesh();
            }
            "Z_TILT_ADJUST" | "QUAD_GANTRY_LEVEL" => {
                self.note_cmd(cmd, "levelling");
                self.leveling();
            }
            "TR_LOAD_TOOLHEAD" => {
                let lane = ext_params(rest).get("LANE").and_then(|l| l.parse().ok()).unwrap_or(0);
                self.toolchange(cmd, lane);
            }
            "TR_UNLOAD_TOOLHEAD" => {
                self.note_cmd(cmd, "Trad Rack unload");
                if let Some(t) = self.m.tr_pre_unload.clone() {
                    if let Some(s) = self.render(&t, BTreeMap::new(), BTreeMap::new(), "", "pre_unload_gcode") {
                        self.run_script(&s, "pre_unload_gcode");
                    }
                }
                self.tr_lane = None;
            }
            "M190.1" => {}
            c if c.len() >= 2 && c.starts_with('T') && c[1..].chars().all(|d| d.is_ascii_digit()) => {
                let lane: u16 = c[1..].parse().unwrap_or(0);
                if self.m.tr_lanes > 0 || self.m.tr_pre_unload.is_some() {
                    self.toolchange(c, lane);
                } else {
                    self.note_cmd(c, "tool select (no toolchanger configured)");
                    self.tool = lane;
                }
            }
            c if c.starts_with("TR_")
                || c.starts_with("BEACON")
                || c.starts_with("CARTOGRAPHER")
                || c.starts_with("BED_MESH")
                || c.starts_with("EXCLUDE_OBJECT")
                || c.starts_with("SET_")
                || c.starts_with("TIMELAPSE")
                || matches!(
                    c,
                    "M486"
                        | "TURN_OFF_HEATERS"
                        | "TEMPERATURE_WAIT"
                        | "PAUSE"
                        | "RESUME"
                        | "CANCEL_PRINT"
                        | "CLEAR_PAUSE"
                        | "RESPOND"
                        | "UPDATE_DELAYED_GCODE"
                        | "SAVE_VARIABLE"
                        | "STATUS"
                        | "M112"
                        | "PROBE"
                        | "PROBE_ACCURACY"
                        | "PROBE_CALIBRATE"
                        | "ACCEPT"
                        | "ABORT"
                        | "SAVE_CONFIG"
                        | "DUMP_TMC"
                        | "INIT_TMC"
                        | "ENABLE_FILAMENT_SENSOR"
                        | "DISABLE_FILAMENT_SENSOR"
                        | "FORCE_MOVE"
                        | "MANUAL_STEPPER"
                        | "ACTIVATE_EXTRUDER"
                        | "SYNC_EXTRUDER_MOTION"
                        | "SHAPER_CALIBRATE"
                        | "TEST_RESONANCES"
                        | "M600"
                        | "M80"
                        | "M81"
                        | "BASE_PAUSE"
                        | "BASE_RESUME"
                        | "BASE_CANCEL_PRINT"
                        | "SDCARD_RESET_FILE"
                        | "M24"
                        | "M25"
                        | "M23"
                        | "M27"
                ) =>
            {
                self.note_cmd(c, "no toolhead motion");
                if c == "BED_MESH_CLEAR" {
                    self.mesh_applied = false;
                } else if c == "BED_MESH_PROFILE" && rest.to_uppercase().contains("LOAD") {
                    self.mesh_applied = true;
                }
            }
            c => {
                if self.commands.get(c).is_none() {
                    self.issue("unknown_command", format!("{c}: unknown command, assumed not to move the toolhead"));
                }
                self.note_cmd(c, "unknown (no motion assumed)");
                let _ = line;
            }
        }
    }

    fn arc(&mut self, cw: bool, p: &BTreeMap<char, f64>) {
        if self.pos.iter().any(|p| p.is_none()) {
            self.issue("not_homed", "arc before homing".into());
            return;
        }
        if let Some(f) = p.get(&'F') {
            if *f > 0.0 {
                self.speed = f / 60.0;
            }
        }
        let st = self.th();
        let g = |k: char, i: usize| p.get(&k).map(|v| if self.abs { v + self.base[i] } else { st[i] + v }).unwrap_or(st[i]);
        let end = [g('X', 0), g('Y', 1), g('Z', 2)];
        let (cx, cy) = if let (Some(i), Some(j)) = (p.get(&'I').or(Some(&0.0)), p.get(&'J').or(Some(&0.0))) {
            if p.contains_key(&'R') && !p.contains_key(&'I') && !p.contains_key(&'J') {
                // R form: centre on the chord's bisector
                let r = p[&'R'];
                let (dx, dy) = (end[0] - st[0], end[1] - st[1]);
                let d = (dx * dx + dy * dy).sqrt().max(1e-9);
                let h = (r * r - d * d / 4.0).max(0.0).sqrt() * if (r > 0.0) == cw { -1.0 } else { 1.0 };
                (st[0] + dx / 2.0 - h * dy / d, st[1] + dy / 2.0 + h * dx / d)
            } else {
                (st[0] + i, st[1] + j)
            }
        } else {
            (st[0], st[1])
        };
        let r = ((st[0] - cx).powi(2) + (st[1] - cy).powi(2)).sqrt();
        let a0 = (st[1] - cy).atan2(st[0] - cx);
        let mut a1 = (end[1] - cy).atan2(end[0] - cx);
        if cw {
            if a1 >= a0 - 1e-12 {
                a1 -= std::f64::consts::TAU;
            }
        } else if a1 <= a0 + 1e-12 {
            a1 += std::f64::consts::TAU;
        }
        let sweep = a1 - a0;
        let n = ((sweep.abs() * r) / self.m.arc_resolution.max(0.01)).ceil().max(1.0) as usize;
        let de_total = match p.get(&'E') {
            Some(v) => {
                let v = v * self.extrude_factor;
                if !self.abs || !self.abs_e {
                    v
                } else {
                    v + self.base[3] - self.e
                }
            }
            None => 0.0,
        };
        for k in 1..=n {
            let f = k as f64 / n as f64;
            let a = a0 + sweep * f;
            let pt = if k == n { end } else { [cx + r * a.cos(), cy + r * a.sin(), st[2] + (end[2] - st[2]) * f] };
            self.move_to([Some(pt[0]), Some(pt[1]), Some(pt[2])], de_total / n as f64, None);
        }
    }

    fn home(&mut self, axes: [bool; 3], contact: bool) {
        if let Some((t, ov_axes)) = self.m.homing_override.clone() {
            if self.depth < 40 && !self.srcs[self.cur_src as usize].via.contains("homing_override") {
                let wants = axes.iter().zip(['x', 'y', 'z']).any(|(a, c)| *a && ov_axes.contains(c));
                if wants {
                    let mut params = BTreeMap::new();
                    for (a, k) in axes.iter().zip(["X", "Y", "Z"]) {
                        if *a {
                            params.insert(k.to_string(), "0".to_string());
                        }
                    }
                    if let Some(s) = self.render(&t, BTreeMap::new(), params, "", "homing_override") {
                        self.run_script(&s, "homing_override");
                    }
                    return;
                }
            }
        }
        let m = self.m;
        for i in 0..2 {
            if !axes[i] {
                continue;
            }
            let a = &m.axes[i];
            let end = a.endstop.unwrap_or(if a.positive { a.max } else { a.min });
            if let (Some(_), true) = (self.pos[i], self.pos.iter().all(|p| p.is_some())) {
                let mut to = self.th();
                to[i] = end;
                self.push(to, 0.0, a.homing_speed, Kind::Home);
            } else {
                self.pos[i] = Some(end);
            }
            self.pos[i] = Some(end);
        }
        if axes[2] {
            let probe = m.probe.as_ref().filter(|_| m.axes[2].endstop.is_none());
            let xy = probe.and_then(|p| p.home_xy).or(m.safe_z_home.map(|s| s.0));
            if let Some(xy) = xy {
                if self.pos[0].is_none() || self.pos[1].is_none() {
                    self.issue("not_homed", "G28 Z: X and Y must be homed first to move to the Z homing position".into());
                } else {
                    let speed = probe.map(|p| p.speed).or(m.safe_z_home.map(|s| s.2)).unwrap_or(50.0);
                    let z = self.th()[2];
                    if self.pos[2].is_some() {
                        let hop = probe.map(|p| p.z_hop).or(m.safe_z_home.map(|s| s.1)).unwrap_or(0.0);
                        if hop > 0.0 && z < hop {
                            self.travel([self.th()[0], self.th()[1], hop], m.max_z_velocity.min(20.0), Kind::Travel);
                        }
                        self.travel([xy[0], xy[1], self.th()[2]], speed, Kind::Travel);
                    } else {
                        self.pos[0] = Some(xy[0]);
                        self.pos[1] = Some(xy[1]);
                        let prev = self.moves.last().map(|mv| mv.to);
                        if let Some(p) = prev {
                            let _ = p;
                        }
                        // came from where X/Y homing left it: show the trip to the Z homing spot at Z unknown
                    }
                }
            }
            let trig = probe.map(|p| p.trigger_z).unwrap_or(0.0);
            let zend = m.axes[2].endstop.unwrap_or(trig);
            if self.pos.iter().all(|p| p.is_some()) {
                let th = self.th();
                self.push([th[0], th[1], zend], 0.0, m.axes[2].homing_speed, Kind::Home);
                if contact {
                    let th = self.th();
                    self.push([th[0], th[1], 0.0], 0.0, 3.0, Kind::Probe);
                    self.push([th[0], th[1], zend.max(2.0)], 0.0, 3.0, Kind::Probe);
                }
            }
            self.pos[2] = Some(if contact { zend.max(2.0) } else { zend });
            self.homed_z_by_probe = probe.is_some();
            if let Some(p) = probe {
                if p.z_hop > 0.0 {
                    let th = self.th();
                    self.travel([th[0], th[1], p.z_hop], m.max_z_velocity.min(20.0), Kind::Travel);
                }
            }
        }
        // homing resets the G-code offsets to the homing origin
        for i in 0..3 {
            if axes[i] {
                self.base[i] = self.origin[i];
            }
        }
    }

    fn leveling(&mut self) {
        let Some(l) = self.m.leveling.clone() else {
            self.issue("unknown_command", "Z_TILT_ADJUST/QUAD_GANTRY_LEVEL without a [z_tilt]/[quad_gantry_level] section".into());
            return;
        };
        if self.pos.iter().any(|p| p.is_none()) {
            self.issue("not_homed", format!("{}: not homed", l.kind));
            return;
        }
        let scanning = self.m.probe.as_ref().map(|p| ["beacon", "cartographer", "scanner"].iter().any(|k| p.section.starts_with(k))).unwrap_or(false);
        let trig = self.m.probe.as_ref().map(|p| p.trigger_z).unwrap_or(0.0);
        let lift = self.m.max_z_velocity.min(20.0);
        let th = self.th();
        if th[2] < l.z {
            self.travel([th[0], th[1], l.z], lift, Kind::Travel);
        }
        for pt in &l.points {
            self.travel([pt[0], pt[1], l.z], l.speed, Kind::Probe);
            if !scanning {
                self.travel([pt[0], pt[1], trig], 5.0, Kind::Probe);
                self.travel([pt[0], pt[1], l.z], lift, Kind::Probe);
            }
        }
    }

    fn bed_mesh(&mut self) {
        let Some(mesh) = self.m.mesh.clone() else {
            self.issue("unknown_command", "BED_MESH_CALIBRATE without a [bed_mesh] section".into());
            return;
        };
        if self.pos.iter().any(|p| p.is_none()) {
            self.issue("not_homed", "BED_MESH_CALIBRATE: not homed".into());
            return;
        }
        let off = self.m.probe.as_ref().map(|p| p.offset).unwrap_or([0.0; 3]);
        let lift = self.m.max_z_velocity.min(20.0);
        let th = self.th();
        if (th[2] - mesh.z).abs() > 1e-6 {
            self.travel([th[0], th[1], mesh.z], lift, Kind::Travel);
        }
        let (nx, ny) = (mesh.count[0], mesh.count[1]);
        let pt = |i: usize, j: usize| -> [f64; 2] {
            let x = mesh.min[0] + (mesh.max[0] - mesh.min[0]) * i as f64 / (nx - 1) as f64;
            let y = mesh.min[1] + (mesh.max[1] - mesh.min[1]) * j as f64 / (ny - 1) as f64;
            // the probe goes over the point: the nozzle sits off by the probe's offset
            [x - off[0], y - off[1]]
        };
        if mesh.runs > 0 {
            // a scanning probe sweeps rows along the main direction, there and back
            let (rows, cols) = if mesh.main_y { (nx, ny) } else { (ny, nx) };
            for run in 0..mesh.runs {
                for r in 0..rows {
                    let rr = if run % 2 == 0 { r } else { rows - 1 - r };
                    let fwd = (r + run) % 2 == 0;
                    let ends = [0, cols - 1];
                    let (a, b) = if fwd { (ends[0], ends[1]) } else { (ends[1], ends[0]) };
                    let p0 = if mesh.main_y { pt(rr, a) } else { pt(a, rr) };
                    let p1 = if mesh.main_y { pt(rr, b) } else { pt(b, rr) };
                    self.travel([p0[0], p0[1], mesh.z], mesh.speed, Kind::Probe);
                    self.travel([p1[0], p1[1], mesh.z], mesh.speed, Kind::Probe);
                }
            }
        } else {
            let trig = self.m.probe.as_ref().map(|p| p.trigger_z).unwrap_or(0.0);
            for j in 0..ny {
                for k in 0..nx {
                    let i = if j % 2 == 0 { k } else { nx - 1 - k };
                    let p = pt(i, j);
                    self.travel([p[0], p[1], mesh.z], mesh.speed, Kind::Probe);
                    self.travel([p[0], p[1], trig], 5.0, Kind::Probe);
                    self.travel([p[0], p[1], mesh.z], lift, Kind::Probe);
                }
            }
        }
        self.mesh_applied = true;
    }

    fn toolchange(&mut self, cmd: &str, lane: u16) {
        self.note_cmd(cmd, "Trad Rack toolchange");
        if self.tr_lane == Some(lane) {
            return;
        }
        // an unknown lane at the start may be loaded: Trad Rack unloads it first
        if let Some(t) = self.m.tr_pre_unload.clone() {
            if let Some(s) = self.render(&t, BTreeMap::new(), BTreeMap::new(), "", "pre_unload_gcode") {
                self.run_script(&s, &format!("{cmd} pre_unload_gcode"));
            }
        }
        self.tool = lane;
        self.tr_lane = Some(lane);
        if let Some(t) = self.m.tr_post_load.clone() {
            if let Some(s) = self.render(&t, BTreeMap::new(), BTreeMap::new(), "", "post_load_gcode") {
                self.run_script(&s, &format!("{cmd} post_load_gcode"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gsim::cfg::{Config, Machine};

    fn machine() -> Machine {
        let cfg = "[printer]\nkinematics: corexy\nmax_velocity: 300\nmax_accel: 3000\n[stepper_x]\nposition_endstop: 0\nposition_max: 200\n[stepper_y]\nposition_endstop: 0\nposition_max: 200\n[stepper_z]\nposition_endstop: 0\nposition_max: 180\n[gcode_macro START]\ngcode:\n  {% set t = params.T|default(1)|int %}\n  G28\n  G1 Z{t * 5} F600\n  SAVE_GCODE_STATE NAME=s\n  G91\n  G1 X10\n  RESTORE_GCODE_STATE NAME=s\n";
        Machine::from_config(&Config::parse(cfg, None).unwrap())
    }

    #[test]
    fn moves_and_macros() {
        let m = machine();
        let mut it = Interp::new(&m);
        it.run_file("START T=2\nG1 X20 Y20 F6000\nG1 X30 E1\nG92 E0\nM83\nG1 X40 E0.5\nG1 X500\n");
        let last = it.moves.iter().filter(|m| m.kind != Kind::Home).collect::<Vec<_>>();
        // Z10, X+10 (relative), X20 Y20, X30 E1, X40 E0.5
        assert_eq!(last[0].to, [0.0, 0.0, 10.0]);
        assert_eq!(last[1].to, [10.0, 0.0, 10.0]);
        assert_eq!(last[2].to, [20.0, 20.0, 10.0]);
        assert_eq!(last[3].kind, Kind::Print);
        assert!((last[4].e - 0.5).abs() < 1e-9);
        assert!(it.issues.iter().any(|i| i.kind == "out_of_range"), "{:?}", it.issues);
        assert_eq!(it.srcs[last[0].src as usize].via.as_ref(), "START");
    }

    #[test]
    fn arcs() {
        let m = machine();
        let mut it = Interp::new(&m);
        it.assume_homed([10.0, 0.0, 1.0]);
        it.run_file("G2 X0 Y10 I-10 J0\n");
        // clockwise from (10,0) about the origin to (0,10) is three quarters of a turn
        let n = it.moves.len();
        assert!(n > 40, "{n}");
        let mid = &it.moves[n / 2];
        assert!(mid.to[1] < 0.0);
        assert!((it.moves[n - 1].to[1] - 10.0).abs() < 1e-9);
    }
}
