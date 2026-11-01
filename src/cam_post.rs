//! G-code posts: GRBL, LinuxCNC, Fanuc/Haas and Mach3/Mach4.

use std::fmt::Write;

use glam::{DVec2, DVec3};

use crate::cam::{Cam, Coolant, Tool, ToolKind};
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
        // rcnc runs Mach3-dialect G-code
        "mach3" | "mach4" | "rcnc" => Dialect::Mach3,
        _ => Dialect::Grbl,
    }
}

fn num(v: f64) -> String {
    let s = format!("{:.3}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s == "-0" {
        "0".into()
    } else {
        s
    }
}

fn comment(text: &str) -> String {
    format!("({})", ascii(text))
}

/// Comment text in plain ASCII (some controllers and senders mangle anything else): Ø → D, ° → deg, × → x,
/// accented letters without their accents, other characters dropped. Parentheses can't nest in G-code comments.
fn ascii(text: &str) -> String {
    let mut t = String::new();
    for c in text.chars() {
        match c {
            '(' => t.push('['),
            ')' => t.push(']'),
            'Ø' | 'ø' | '⌀' => t.push('D'),
            '°' => t.push_str("deg"),
            '×' => t.push('x'),
            '–' | '—' | '−' => t.push('-'),
            '·' => t.push('-'),
            '…' => t.push_str("..."),
            '‘' | '’' => t.push('\''),
            '“' | '”' => t.push('"'),
            'µ' => t.push('u'),
            c if c.is_ascii() => t.push(c),
            c => {
                // à, é, ü, ñ, … : the base letter
                let base = match c {
                    'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => "a",
                    'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => "A",
                    'è' | 'é' | 'ê' | 'ë' => "e",
                    'È' | 'É' | 'Ê' | 'Ë' => "E",
                    'ì' | 'í' | 'î' | 'ï' => "i",
                    'Ì' | 'Í' | 'Î' | 'Ï' => "I",
                    'ò' | 'ó' | 'ô' | 'õ' | 'ö' => "o",
                    'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' => "O",
                    'ù' | 'ú' | 'û' | 'ü' => "u",
                    'Ù' | 'Ú' | 'Û' | 'Ü' => "U",
                    'ç' => "c",
                    'Ç' => "C",
                    'ñ' => "n",
                    'Ñ' => "N",
                    'ß' => "ss",
                    'æ' => "ae",
                    'Æ' => "AE",
                    _ => "",
                };
                t.push_str(base);
            }
        }
    }
    t
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

    /// A clockwise (2) or counter-clockwise (3) arc to `p` around the centre `c` (I/J relative to the start).
    fn arc(&mut self, g: u8, from: DVec3, p: DVec3, c: DVec2, f: f64) {
        let r3 = |v: f64| (v * 1000.0).round() / 1000.0;
        let mut l = format!("G{g} X{} Y{}", num(r3(p.x)), num(r3(p.y)));
        if self.last[2] != Some(r3(p.z)) {
            let _ = write!(l, " Z{}", num(r3(p.z)));
        }
        let _ = write!(l, " I{} J{}", num(r3(c.x - from.x)), num(r3(c.y - from.y)));
        let f = f.round();
        if self.feed != Some(f) {
            let _ = write!(l, " F{}", num(f));
            self.feed = Some(f);
        }
        self.line(&l);
        self.motion = Some(g);
        self.last = [Some(r3(p.x)), Some(r3(p.y)), Some(r3(p.z))];
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

    /// Lift the spindle all the way up in machine coordinates: work Z heights aren't safe while the tool
    /// length offset is off or about to change. GRBL (often not homed) goes up to the work clearance height.
    fn top(&mut self, d: Dialect, safe_w: f64) {
        match d {
            Dialect::Grbl => self.z_up(safe_w),
            Dialect::Fanuc => {
                self.line("G91 G28 Z0");
                self.line("G90");
                self.forget();
            }
            Dialect::LinuxCnc | Dialect::Mach3 => {
                self.line("G53 G0 Z0");
                self.forget();
            }
        }
    }
}

/// A message for the operator: shown on screen by LinuxCNC and Mach3, a plain comment elsewhere.
fn message(d: Dialect, text: &str) -> String {
    match d {
        Dialect::LinuxCnc => comment(&format!("MSG, {text}")),
        Dialect::Mach3 => comment(&format!("MSG,{text}")),
        _ => comment(text),
    }
}

/// G-code for the toolpaths, in work coordinates (`origin` is the work zero in setup coordinates).
#[cfg(test)]
pub fn gcode(cam: &Cam, paths: &[Toolpath], origin: DVec3, safe: f64) -> String {
    gcode_noted(cam, paths, origin, safe, None)
}

/// G-code for the toolpaths, in work coordinates (`origin` is the work zero in setup coordinates), with `work_zero` telling the operator where the work zero is (None = the setup's origin choice).
pub fn gcode_noted(cam: &Cam, paths: &[Toolpath], origin: DVec3, safe: f64, work_zero: Option<String>) -> String {
    let d = dialect(&cam.post);
    let mut o = Out { s: String::new(), last: [None; 3], motion: None, feed: None };
    let safe_w = safe - origin.z;
    if d == Dialect::Fanuc {
        o.line("%");
        o.line(&format!("O{:04} {}", cam.program % 10000, comment(&cam.name)));
    }
    o.line(&comment(&format!("{} - made by rCAD", cam.name)));
    o.line(&comment(&format!("work zero: {}", work_zero.unwrap_or_else(|| cam.origin.label().to_lowercase()))));
    for n in cam.setup_notes() {
        o.line(&comment(&n));
    }
    match d {
        Dialect::Mach3 if cam.post == "rcnc" => o.line(&comment("retracts use G53 G0 Z0 - home the machine first")),
        Dialect::Mach3 => {
            o.line(&comment("Mach3: dwells (G4 P) are in seconds - untick 'G4 dwell param in milliseconds' in General Config"));
            o.line(&comment("Mach3: retracts use G53 G0 Z0 - reference (home) the machine first"));
        }
        Dialect::LinuxCnc => o.line(&comment("retracts use G53 G0 Z0 - home the machine first")),
        _ => {}
    }
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
    if d != Dialect::LinuxCnc && used.iter().filter_map(|id| cam.tool(*id)).any(|t| t.kind == ToolKind::Tap && !t.tapping_head) {
        o.line(&comment("no rigid tapping: the spindle reverses at the bottom of each tap - hold taps in a floating tap holder"));
    }
    let total = crate::cam_paths::cycle_time(cam, paths);
    o.line(&comment(&format!("estimated time {}:{:02}", (total / 60.0).floor(), (total % 60.0).round() as i64)));
    match d {
        Dialect::Grbl => o.line("G21 G17 G90 G94"),
        // G91.1: arc centres (I, J) relative to the start, whatever the controller was left in
        Dialect::LinuxCnc => o.line("G21 G17 G40 G49 G80 G90 G91.1 G94 G64 P0.01"),
        Dialect::Mach3 => o.line("G21 G17 G40 G49 G80 G90 G91.1 G94"),
        Dialect::Fanuc => o.line("G21 G17 G40 G49 G80 G90 G94"),
    }
    o.line("G54");
    for l in fill(&cam.post_text.header, cam, None) {
        o.line(&l);
    }
    let mut cur_tool: Option<u64> = None;
    let mut spindle_rpm: Option<f64> = None;
    let mut coolant = Coolant::Off;
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
                o.top(d, safe_w);
                if spindle_rpm.is_some() {
                    o.line("M5");
                    spindle_rpm = None;
                }
                if coolant.is_on() {
                    o.line("M9");
                    coolant = Coolant::Off;
                }
            }
            let probing = cam.probe.mode != "off";
            if !cam.post_text.tool_change.trim().is_empty() {
                for l in fill(&cam.post_text.tool_change, cam, Some(tool)) {
                    o.line(&l);
                }
            } else {
                match d {
                    Dialect::Grbl => {
                        if cur_tool.is_some() {
                            o.line(&comment(&format!("change to T{}: {}", tool.number, tool_text(tool))));
                            o.line("M0");
                        } else {
                            o.line(&comment(&format!("load T{}: {}", tool.number, tool_text(tool))));
                        }
                    }
                    // rcnc's M6 shows the new tool, waits for it, measures it on the setter
                    // and sets work Z, so no pause and no length offset
                    Dialect::Mach3 if cam.post == "rcnc" => o.line(&format!("T{} M6", tool.number)),
                    _ => {
                        if !cam.machine.tool_changer {
                            // manual tool change: stop for the operator, then pick up the new length offset
                            let verb = if cur_tool.is_some() { "change to" } else { "load" };
                            o.line(&message(d, &format!("{verb} T{}: {}, then press cycle start", tool.number, tool_text(tool))));
                            o.line("M0");
                        }
                        o.line(&format!("T{} M6", tool.number));
                        if !probing {
                            o.line(&format!("G43 H{}", tool.number));
                        }
                    }
                }
            }
            if probing {
                probe(&mut o, d, cam, tool, cur_tool.is_none(), safe_w);
            }
            o.forget();
            cur_tool = Some(tool.id);
        }
        // never above the spindle's top speed (the toolpath's feeds are scaled to match)
        let rpm = cam.spindle_rpm(tool).round();
        if spindle_rpm != Some(rpm) {
            if rpm < tool.rpm.round() {
                o.line(&comment(&format!("T{} is set to {} rpm: capped at the spindle's {}", tool.number, num(tool.rpm.round()), num(rpm))));
            }
            o.line(&format!("S{} M3", num(rpm)));
            spindle_rpm = Some(rpm);
            // let the spindle come up to speed
            if cam.machine.spin_up > 0.0 {
                o.line(&dwell(d, cam.machine.spin_up));
            }
        }
        let want = cam.coolant_for(tool, op);
        if want != coolant {
            if coolant.is_on() && want.is_on() {
                o.line("M9");
            }
            match want {
                Coolant::Flood => o.line("M8"),
                Coolant::Mist => o.line("M7"),
                _ => o.line("M9"),
            }
            coolant = want;
        }
        let mut pos = DVec3::new(0.0, 0.0, safe);
        let mut first = true;
        let mut k = 0;
        while k < tp.moves.len() {
            let m = &tp.moves[k];
            k += 1;
            // a run of feeds at one rate: replace what lies on circles by arcs
            if let (Move::Feed(_, f), true, false) = (m, cam.arcs, first) {
                let mut pts = vec![pos];
                let mut e = k - 1;
                while let Some(Move::Feed(p, g)) = tp.moves.get(e) {
                    if g != f {
                        break;
                    }
                    pts.push(DVec3::from(*p));
                    e += 1;
                }
                if pts.len() >= 4 {
                    for piece in fit_arcs(&pts) {
                        match piece {
                            Piece::Line(p) => o.mv(1, p - origin, Some(*f)),
                            Piece::Arc { from, to, c, ccw } => o.arc(if ccw { 3 } else { 2 }, from - origin, to - origin, c - origin.truncate(), *f),
                        }
                    }
                    pos = *pts.last().unwrap();
                    k = e;
                    continue;
                }
            }
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
                Move::Dwell(s) => o.line(&dwell(d, s)),
                Move::Tap { z, r, pitch } => {
                    let w = pos - origin;
                    let (zw, rw) = (z - origin.z, r - origin.z);
                    let f = rpm * pitch;
                    if tool.tapping_head {
                        // the head reverses the tap when the spindle pulls back: feed in, then out faster by its ratio
                        o.mv(0, DVec3::new(w.x, w.y, rw), None);
                        o.mv(1, DVec3::new(w.x, w.y, zw), Some(f));
                        o.mv(1, DVec3::new(w.x, w.y, rw), Some(f * tool.reverse_ratio.max(0.1)));
                        pos = DVec3::new(pos.x, pos.y, r);
                        first = false;
                        continue;
                    }
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
                            o.line(&format!("M4 S{}", num(rpm)));
                            o.mv(1, DVec3::new(w.x, w.y, rw), Some(f));
                            o.line(&format!("M3 S{}", num(rpm)));
                        }
                    }
                    pos = DVec3::new(pos.x, pos.y, r);
                }
            }
            first = false;
        }
    }
    o.line("");
    o.top(d, safe_w);
    o.line("M5");
    if coolant.is_on() {
        o.line("M9");
    }
    for l in fill(&cam.post_text.footer, cam, None) {
        o.line(&l);
    }
    o.line("M30");
    if d == Dialect::Fanuc {
        o.line("%");
    }
    o.s
}

/// A pause of `s` seconds: Fanuc counts G4 P in milliseconds, the others in seconds.
fn dwell(d: Dialect, s: f64) -> String {
    match d {
        Dialect::Fanuc => format!("G4 P{}", (s * 1000.0).round()),
        _ => format!("G4 P{}", num(s)),
    }
}

/// The tool for the operator: its description, and how a tap is held.
fn tool_text(t: &Tool) -> String {
    if t.kind == ToolKind::Tap && t.tapping_head {
        format!("{} in the self-reversing tapping head", t.describe())
    } else {
        t.describe()
    }
}

/// The user's G-code with its placeholders filled in, line by line.
fn fill(text: &str, cam: &Cam, tool: Option<&Tool>) -> Vec<String> {
    // names go in as plain ASCII, like the comments the post writes
    let mut t = text.replace("{program}", &cam.program.to_string()).replace("{setup}", &ascii(&cam.name));
    if let Some(tool) = tool {
        t = t
            .replace("{tool}", &tool.number.to_string())
            .replace("{name}", &ascii(&tool.name))
            .replace("{desc}", &ascii(&tool.describe()))
            .replace("{diameter}", &num(tool.diameter))
            .replace("{rpm}", &num(tool.rpm.round()));
    }
    t.lines().map(|l| l.trim_end().to_string()).filter(|l| !l.is_empty()).collect()
}

/// Measure the tool just loaded: on a touch plate (sets the work Z zero) or on a tool setter (sets its
/// length offset against the first tool, which must be the one the work Z zero was set with).
fn probe(o: &mut Out, d: Dialect, cam: &Cam, tool: &Tool, first: bool, safe_w: f64) {
    let p = &cam.probe;
    let g = if matches!(d, Dialect::Fanuc | Dialect::Mach3) { "G31" } else { "G38.2" };
    let touch = |o: &mut Out| {
        o.line(&format!("G91 {g} Z-{} F{}", num(p.travel.max(1.0)), num(p.fast.max(1.0))));
        o.line("G0 Z1");
        o.line(&format!("{g} Z-2 F{}", num(p.slow.max(1.0))));
        o.line("G90");
    };
    match p.mode.as_str() {
        "plate" => {
            let [x, y, z] = p.plate_at;
            let top = z + p.plate_thickness;
            // one stop for the tool change and the plate
            if o.s.ends_with("M0\n") {
                o.s.truncate(o.s.len() - 3);
            }
            o.line(&comment(&format!("put the touch plate at X{} Y{} under T{} and attach its clip", num(x), num(y), tool.number)));
            o.line("M0");
            if d != Dialect::Grbl {
                o.line("G49");
            }
            // the new tool's length isn't known yet: travel at the top of the machine and probe down from there
            o.line("G53 G0 Z0");
            o.line(&format!("G0 X{} Y{}", num(x), num(y)));
            touch(o);
            // the plate's top is the work Z there
            match d {
                Dialect::Fanuc => o.line(&format!("G10 L2 P1 Z[#5023-{}]", num(top))),
                _ => o.line(&format!("G10 L20 P1 Z{}", num(top))),
            }
            o.forget();
            o.z_up(safe_w.max(top + 5.0));
            o.line(&comment("remove the touch plate clip"));
            o.line("M0");
        }
        "setter" => {
            if d == Dialect::Grbl {
                o.line(&comment(&format!("set the work Z zero for T{} by hand: GRBL can't keep a tool setter measurement", tool.number)));
                o.line("M0");
                return;
            }
            let [x, y, z] = p.setter;
            o.line(&comment(&format!("measure T{} on the tool setter", tool.number)));
            o.line("G49");
            o.line("G53 G0 Z0");
            o.line(&format!("G53 G0 X{} Y{}", num(x), num(y)));
            o.line(&format!("G53 G0 Z{}", num(z)));
            touch(o);
            match d {
                Dialect::LinuxCnc => {
                    if first {
                        o.line("#<_tool_ref> = #5063");
                    }
                    o.line("G43.1 Z[#5063 - #<_tool_ref>]");
                }
                Dialect::Mach3 => {
                    if first {
                        o.line("#500 = #2002");
                    }
                    o.line(&format!("G10 L1 P{} Z[#2002 - #500]", tool.number));
                    o.line(&format!("G43 H{}", tool.number));
                }
                _ => {
                    if first {
                        o.line("#500 = #5063");
                    }
                    o.line(&format!("G10 L10 P{} R[#5063 - #500]", tool.number));
                    o.line(&format!("G43 H{}", tool.number));
                }
            }
            o.line("G53 G0 Z0");
            o.forget();
        }
        _ => {}
    }
}

/// Largest gap between the toolpath's points and the arcs that replace it, mm.
const ARC_TOL: f64 = 0.005;
/// Largest bulge of an arc over a segment it replaces: the segments were themselves cut short of the
/// curve they stand for (offsets keep within 0.004 mm), so the arc may run a little outside them.
const CHORD_TOL: f64 = 0.02;

enum Piece {
    Line(DVec3),
    /// From `from` to `to` around the centre `c` (XY), Z changing evenly along it (a helix).
    Arc {
        from: DVec3,
        to: DVec3,
        c: DVec2,
        ccw: bool,
    },
}

/// The circle through three points (XY), if they aren't in a line.
fn circle(a: DVec2, b: DVec2, c: DVec2) -> Option<(DVec2, f64)> {
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if d.abs() < 1e-12 {
        return None;
    }
    let (a2, b2, c2) = (a.length_squared(), b.length_squared(), c.length_squared());
    let ctr = DVec2::new((a2 * (b.y - c.y) + b2 * (c.y - a.y) + c2 * (a.y - b.y)) / d, (a2 * (c.x - b.x) + b2 * (a.x - c.x) + c2 * (b.x - a.x)) / d);
    Some((ctr, ctr.distance(a)))
}

/// Whether `pts` lie on one arc (within ARC_TOL, turning one way, Z even along it); its centre and direction.
fn arc_of(pts: &[DVec3]) -> Option<(DVec2, bool)> {
    let n = pts.len();
    let (a, m, b) = (pts[0].truncate(), pts[n / 2].truncate(), pts[n - 1].truncate());
    let (c, r) = circle(a, m, b)?;
    if !(0.05..=5000.0).contains(&r) {
        return None;
    }
    let mut sweep = 0.0;
    let mut sweeps = vec![0.0];
    let mut dir = 0.0f64;
    for w in pts.windows(2) {
        let (p, q) = (w[0].truncate() - c, w[1].truncate() - c);
        if (q.length() - r).abs() > ARC_TOL {
            return None;
        }
        let da = p.angle_to(q);
        if da.abs() < 1e-9 || da.abs() > std::f64::consts::FRAC_PI_2 || (dir != 0.0 && da.signum() != dir) {
            return None;
        }
        dir = da.signum();
        // the chord's middle must stay near the arc too
        if r - (r * r - p.distance_squared(q) / 4.0).max(0.0).sqrt() > CHORD_TOL {
            return None;
        }
        sweep += da;
        sweeps.push(sweep);
    }
    if sweep.abs() > std::f64::consts::TAU - 0.05 {
        return None;
    }
    let (z0, z1) = (pts[0].z, pts[n - 1].z);
    for (p, s) in pts.iter().zip(&sweeps) {
        if (p.z - (z0 + (z1 - z0) * s / sweep)).abs() > ARC_TOL {
            return None;
        }
    }
    Some((c, dir > 0.0))
}

/// Replace runs of points that lie on circles (at least four segments long) by arcs; `pts[0]` is where the tool is.
fn fit_arcs(pts: &[DVec3]) -> Vec<Piece> {
    let mut out = vec![];
    let mut i = 0;
    while i + 1 < pts.len() {
        let mut best = None;
        let mut j = i + 4;
        while j < pts.len() && j - i <= 400 {
            match arc_of(&pts[i..=j]) {
                Some(a) => {
                    best = Some((j, a));
                    j += 1;
                }
                None => break,
            }
        }
        match best {
            Some((j, (c, ccw))) => {
                out.push(Piece::Arc { from: pts[i], to: pts[j], c, ccw });
                i = j;
            }
            None => {
                out.push(Piece::Line(pts[i + 1]));
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_are_ascii() {
        assert_eq!(comment("T1 Ø6 90° chamfer (M5 × 0.8) – Müller"), "(T1 D6 90deg chamfer [M5 x 0.8] - Muller)");
        assert!(comment("日本 ok").is_ascii());
    }

    #[test]
    fn fits_arcs_and_helices() {
        // a line in, a half circle (r 5 around (10, 0)) counter-clockwise, then a helix quarter going down
        let mut pts = vec![DVec3::new(0.0, -5.0, 0.0), DVec3::new(5.0, -5.0, 0.0)];
        // half circle from (10,-5) round to (10,5)
        pts.push(DVec3::new(10.0, -5.0, 0.0));
        for k in 1..=24 {
            let a = -std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * k as f64 / 24.0;
            pts.push(DVec3::new(10.0 + 5.0 * a.cos(), 5.0 * a.sin(), 0.0));
        }
        for k in 1..=12 {
            let a = std::f64::consts::FRAC_PI_2 + std::f64::consts::FRAC_PI_2 * k as f64 / 12.0;
            pts.push(DVec3::new(10.0 + 5.0 * a.cos(), 5.0 * a.sin(), -0.1 * k as f64));
        }
        let pieces = fit_arcs(&pts);
        let arcs: Vec<_> = pieces.iter().filter_map(|p| if let Piece::Arc { from, to, c, ccw } = p { Some((*from, *to, *c, *ccw)) } else { None }).collect();
        assert!(!arcs.is_empty() && arcs.len() <= 2, "{}", arcs.len());
        for (_, _, c, ccw) in &arcs {
            assert!(c.distance(DVec2::new(10.0, 0.0)) < 1e-6 && *ccw);
        }
        assert!((arcs.last().unwrap().1 - *pts.last().unwrap()).length() < 1e-9);
        // a wiggly line stays lines
        let zig: Vec<DVec3> = (0..20).map(|k| DVec3::new(k as f64, if k % 2 == 0 { 0.0 } else { 0.3 }, 0.0)).collect();
        assert!(fit_arcs(&zig).iter().all(|p| matches!(p, Piece::Line(_))));
    }
}
