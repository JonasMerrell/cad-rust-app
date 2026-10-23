//! G-code posts: GRBL, LinuxCNC, Fanuc/Haas and Mach3/Mach4.

use std::fmt::Write;

use glam::DVec3;

use crate::cam::Cam;
use crate::cam_paths::{Move, Toolpath};

#[derive(Clone, Copy, PartialEq)]
enum Dialect {
    Grbl,
    LinuxCnc,
    Fanuc,
    Mach3,
}

fn dialect(post: &str) -> Dialect {
    match post {
        "linuxcnc" => Dialect::LinuxCnc,
        "fanuc" | "haas" => Dialect::Fanuc,
        "mach3" | "mach4" => Dialect::Mach3,
        _ => Dialect::Grbl,
    }
}

fn num(v: f64) -> String {
    let s = format!("{:.3}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s == "-0" { "0".into() } else { s }
}

fn comment(text: &str) -> String {
    // parentheses can't nest in G-code comments
    let t: String = text
        .chars()
        .map(|c| {
            if c == '(' {
                '['
            } else if c == ')' {
                ']'
            } else {
                c
            }
        })
        .collect();
    format!("({t})")
}

struct Out {
    s: String,
    last: [Option<f64>; 3],
    motion: Option<u8>,
    feed: Option<f64>,
}

impl Out {
    fn line(&mut self, l: &str) {
        self.s.push_str(l);
        self.s.push('\n');
    }

    fn mv(&mut self, g: u8, p: DVec3, f: Option<f64>) {
        let mut l = String::new();
        if self.motion != Some(g) {
            let _ = write!(l, "G{g}");
            self.motion = Some(g);
        }
        for (i, (axis, v)) in [('X', p.x), ('Y', p.y), ('Z', p.z)].into_iter().enumerate() {
            let r = (v * 1000.0).round() / 1000.0;
            if self.last[i] != Some(r) {
                let _ = write!(l, "{}{axis}{}", if l.is_empty() { "" } else { " " }, num(r));
                self.last[i] = Some(r);
            }
        }
        if let Some(f) = f {
            let f = f.round();
            if self.feed != Some(f) {
                let _ = write!(l, " F{}", num(f));
                self.feed = Some(f);
            }
        }
        let trimmed = l.trim();
        if trimmed.is_empty() || trimmed == format!("G{g}") {
            return;
        }
        let t = trimmed.to_string();
        self.line(&t);
    }

    /// Rapid straight up to z (only Z on the line).
    fn z_up(&mut self, z: f64) {
        let r = (z * 1000.0).round() / 1000.0;
        if self.last[2] != Some(r) {
            self.line(&format!("G0 Z{}", num(r)));
            self.motion = Some(0);
            self.last[2] = Some(r);
        }
    }

    fn forget(&mut self) {
        self.last = [None; 3];
        self.motion = None;
        self.feed = None;
    }
}

/// G-code for the toolpaths, in work coordinates (`origin` is the work zero in setup coordinates).
pub fn gcode(cam: &Cam, paths: &[Toolpath], origin: DVec3, safe: f64) -> String {
    let d = dialect(&cam.post);
    let mut o = Out { s: String::new(), last: [None; 3], motion: None, feed: None };
    let safe_w = safe - origin.z;
    if d == Dialect::Fanuc {
        o.line("%");
        o.line(&format!("O{:04} {}", cam.program % 10000, comment(&cam.name)));
    }
    o.line(&comment(&format!("{} - made by cad-rust-app", cam.name)));
    o.line(&comment(&format!("work zero: {}", cam.origin.label().to_lowercase())));
    let mut used: Vec<u64> = vec![];
    for p in paths.iter().filter(|p| !p.moves.is_empty()) {
        if !used.contains(&p.tool) {
            used.push(p.tool);
        }
    }
    for id in &used {
        if let Some(t) = cam.tool(*id) {
            let d = t.describe();
            o.line(&comment(&if t.name == d || t.name.is_empty() { format!("T{} {d}", t.number) } else { format!("T{} {} - {d}", t.number, t.name) }));
        }
    }
    let total: f64 = paths.iter().map(|p| p.time).sum();
    o.line(&comment(&format!("estimated time {}:{:02}", (total / 60.0).floor(), (total % 60.0).round() as i64)));
    match d {
        Dialect::Grbl => o.line("G21 G17 G90 G94"),
        Dialect::LinuxCnc => o.line("G21 G17 G40 G49 G80 G90 G94 G64 P0.01"),
        Dialect::Fanuc | Dialect::Mach3 => o.line("G21 G17 G40 G49 G80 G90 G94"),
    }
    o.line("G54");
    let mut cur_tool: Option<u64> = None;
    let mut spindle_rpm: Option<f64> = None;
    let mut coolant = false;
    for tp in paths {
        if tp.moves.is_empty() {
            continue;
        }
        let Some(tool) = cam.tool(tp.tool) else { continue };
        let op = cam.op(tp.op);
        o.line("");
        o.line(&comment(&format!("{} - {}", op.map(|o| o.name.as_str()).unwrap_or("operation"), op.map(|o| o.kind.label()).unwrap_or(""))));
        if cur_tool != Some(tool.id) {
            if cur_tool.is_some() || d != Dialect::Grbl {
                o.z_up(safe_w);
                if spindle_rpm.is_some() {
                    o.line("M5");
                    spindle_rpm = None;
                }
                if coolant {
                    o.line("M9");
                    coolant = false;
                }
            }
            match d {
                Dialect::Grbl => {
                    if cur_tool.is_some() {
                        o.line(&comment(&format!("change to T{}: {}", tool.number, tool.describe())));
                        o.line("M0");
                    } else {
                        o.line(&comment(&format!("load T{}: {}", tool.number, tool.describe())));
                    }
                }
                _ => {
                    if !cam.machine.tool_changer {
                        // manual tool change: stop for the operator, then pick up the new length offset
                        o.line(&comment(&format!("change to T{}: {}", tool.number, tool.describe())));
                        o.line("M0");
                    }
                    o.line(&format!("T{} M6", tool.number));
                    o.line(&format!("G43 H{}", tool.number));
                }
            }
            o.forget();
            cur_tool = Some(tool.id);
        }
        if spindle_rpm != Some(tool.rpm) {
            o.line(&format!("S{} M3", num(tool.rpm.round())));
            spindle_rpm = Some(tool.rpm);
            // let the spindle come up to speed
            o.line(if d == Dialect::Fanuc { "G4 P2000" } else { "G4 P2" });
        }
        let want_coolant = tool.coolant || op.map(|o| o.coolant).unwrap_or(false);
        if want_coolant != coolant {
            o.line(if want_coolant { "M8" } else { "M9" });
            coolant = want_coolant;
        }
        let mut pos = DVec3::new(0.0, 0.0, safe);
        let mut first = true;
        for m in &tp.moves {
            match *m {
                Move::Rapid(p) => {
                    let w = DVec3::from(p) - origin;
                    if first {
                        // go up first, then over
                        o.z_up(safe_w);
                        o.mv(0, DVec3::new(w.x, w.y, safe_w), None);
                    }
                    o.mv(0, w, None);
                    pos = p.into();
                }
                Move::Feed(p, f) => {
                    if first {
                        o.z_up(safe_w);
                    }
                    o.mv(1, DVec3::from(p) - origin, Some(f));
                    pos = p.into();
                }
                Move::Dwell(s) => match d {
                    Dialect::Fanuc => o.line(&format!("G4 P{}", (s * 1000.0).round())),
                    _ => o.line(&format!("G4 P{}", num(s))),
                },
                Move::Tap { z, r, pitch } => {
                    let w = pos - origin;
                    let (zw, rw) = (z - origin.z, r - origin.z);
                    let f = tool.rpm * pitch;
                    match d {
                        Dialect::LinuxCnc => {
                            o.mv(0, DVec3::new(w.x, w.y, rw), None);
                            o.line(&format!("G33.1 Z{} K{}", num(zw), num(pitch)));
                            o.forget();
                            o.last = [Some((w.x * 1000.0).round() / 1000.0), Some((w.y * 1000.0).round() / 1000.0), Some((rw * 1000.0).round() / 1000.0)];
                        }
                        Dialect::Fanuc | Dialect::Mach3 => {
                            o.line(&format!("G98 G84 X{} Y{} Z{} R{} F{}", num(w.x), num(w.y), num(zw), num(rw), num(f)));
                            o.line("G80");
                            o.forget();
                        }
                        Dialect::Grbl => {
                            // no tapping cycle: feed in, reverse, feed out (needs a floating tap holder)
                            o.line(&comment("tapping - use a floating tap holder"));
                            o.mv(0, DVec3::new(w.x, w.y, rw), None);
                            o.mv(1, DVec3::new(w.x, w.y, zw), Some(f));
                            o.line(&format!("M4 S{}", num(tool.rpm.round())));
                            o.mv(1, DVec3::new(w.x, w.y, rw), Some(f));
                            o.line(&format!("M3 S{}", num(tool.rpm.round())));
                        }
                    }
                    pos = DVec3::new(pos.x, pos.y, r);
                }
            }
            first = false;
        }
    }
    o.line("");
    o.z_up(safe_w);
    o.line("M5");
    if coolant {
        o.line("M9");
    }
    match d {
        Dialect::Fanuc => {
            o.line("G91 G28 Z0");
            o.line("G90");
            o.line("M30");
            o.line("%");
        }
        _ => o.line("M30"),
    }
    o.s
}
