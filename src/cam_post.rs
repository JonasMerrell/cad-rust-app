//! G-code posts: GRBL, LinuxCNC, Fanuc/Haas and Mach3/Mach4.

use std::fmt::Write;

use glam::{DVec2, DVec3};

use crate::cam::{Cam, Coolant, OpKind, ProbeCycle, Tool, ToolKind};
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

pub(crate) fn num(v: f64) -> String {
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
        let new_mode = self.motion != Some(g);
        if new_mode {
            let _ = write!(l, "G{g}");
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
            // nothing written: the controller is still in its old motion mode
            return;
        }
        if new_mode {
            self.motion = Some(g);
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
        // the controller adds I, J to the start as it was written (rounded)
        let _ = write!(l, " I{} J{}", num(r3(c.x - r3(from.x))), num(r3(c.y - r3(from.y))));
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
    gcode_noted(cam, paths, origin, safe, None, None)
}

/// G-code for the toolpaths, in work coordinates (`origin` is the work zero in setup coordinates), with `work_zero` telling the operator where the work zero is (None = the setup's origin choice). `stock` is the stock box in setup coordinates, for rcnc's Find stock.
pub fn gcode_noted(cam: &Cam, paths: &[Toolpath], origin: DVec3, safe: f64, work_zero: Option<String>, stock: Option<(DVec3, DVec3)>) -> String {
    gcode_job(&[Station { cam, paths, origin, safe, work_zero, stock }], None)
}

/// One station of a job: a setup with its toolpaths, work zero and safe height (setup coordinates).
pub struct Station<'a> {
    pub cam: &'a Cam,
    pub paths: &'a [Toolpath],
    pub origin: DVec3,
    pub safe: f64,
    pub work_zero: Option<String>,
    pub stock: Option<(DVec3, DVec3)>,
}

/// The order a job's toolpaths run in, as (station, toolpath): each station keeps its own order, and the
/// loaded tool cuts wherever a station's next toolpath uses it before the next tool goes in (the tool of the
/// earliest station that has toolpaths left). Tools are matched by number.
pub fn job_order(tools: &[Vec<Option<u32>>]) -> Vec<(usize, usize)> {
    // past the empty toolpaths
    let skip = |next: &mut Vec<usize>| {
        for (s, t) in tools.iter().enumerate() {
            while next[s] < t.len() && t[next[s]].is_none() {
                next[s] += 1;
            }
        }
    };
    // load `cur`: every station runs its toolpaths with it, as far as they go in its own order
    let advance = |next: &mut Vec<usize>, cur: Option<u32>, out: &mut Option<&mut Vec<(usize, usize)>>| {
        for s in 0..tools.len() {
            while next[s] < tools[s].len() && (tools[s][next[s]] == cur || tools[s][next[s]].is_none()) {
                if tools[s][next[s]].is_some() {
                    if let Some(o) = out.as_mut() {
                        o.push((s, next[s]));
                    }
                }
                next[s] += 1;
            }
        }
    };
    // the tools the stations could go on with: each one's next toolpath's tool, in station order
    let heads = |next: &Vec<usize>| -> Vec<Option<u32>> {
        let mut h = vec![];
        for s in 0..tools.len() {
            if next[s] < tools[s].len() && !h.contains(&tools[s][next[s]]) {
                h.push(tools[s][next[s]]);
            }
        }
        h
    };
    // the fewest tool loads that finish every station from `next` (a station's toolpaths keep their order; the
    // greedy "the first station's next tool" can load a tool twice: op 1's chamfer mill before op 2's ball and
    // again after it)
    fn fewest(next: &Vec<usize>, memo: &mut std::collections::HashMap<Vec<usize>, usize>, heads: &dyn Fn(&Vec<usize>) -> Vec<Option<u32>>,
              advance: &dyn Fn(&mut Vec<usize>, Option<u32>)) -> usize {
        if let Some(c) = memo.get(next) {
            return *c;
        }
        let h = heads(next);
        let c = if h.is_empty() {
            0
        } else {
            h.iter().map(|t| {
                let mut n = next.clone();
                advance(&mut n, *t);
                1 + fewest(&n, memo, heads, advance)
            }).min().unwrap()
        };
        memo.insert(next.clone(), c);
        c
    }
    let mut next = vec![0usize; tools.len()];
    skip(&mut next);
    let mut memo = std::collections::HashMap::new();
    let adv = |n: &mut Vec<usize>, t: Option<u32>| advance(n, t, &mut None);
    let mut out = vec![];
    loop {
        let h = heads(&next);
        if h.is_empty() {
            break;
        }
        // the first (in station order) of the tools that lead to the fewest loads
        let best = h.iter().copied().min_by_key(|t| {
            let mut n = next.clone();
            adv(&mut n, *t);
            fewest(&n, &mut memo, &heads, &adv)
        }).unwrap();
        advance(&mut next, best, &mut Some(&mut out));
    }
    out
}

/// One program for several stations (setups each in their own work offset, e.g. two vise stations), in
/// `order` (default: the first station's toolpaths in turn). Each tool is loaded once per run of the order;
/// moving to another station goes to the top, switches the work offset and travels over at the top.
pub fn gcode_job(st: &[Station], order: Option<Vec<(usize, usize)>>) -> String {
    let order = order.unwrap_or_else(|| (0..st[0].paths.len()).map(|i| (0, i)).collect());
    let multi = st.len() > 1;
    let first_station = order.first().map(|o| o.0).unwrap_or(0);
    let cam = st[first_station].cam;
    let work_zero = st[first_station].work_zero.clone();
    let d = dialect(&cam.post);
    let mut o = Out { s: String::new(), last: [None; 3], motion: None, feed: None };
    if d == Dialect::Fanuc {
        o.line("%");
        o.line(&format!("O{:04} {}", cam.program % 10000, comment(&cam.name)));
    }
    o.line(&comment(&format!("{} - made by rCAD", cam.name)));
    if cam.post == "rcnc" {
        rcnc_header(&mut o, cam);
    }
    o.line(&comment(&format!("work zero: {}", work_zero.unwrap_or_else(|| cam.origin.label().to_lowercase()))));
    if multi {
        o.line(&comment(&format!("{} stations, each in its own work offset", st.len())));
        for (i, s) in st.iter().enumerate() {
            o.line(&comment(&format!("station {}: G{} {} - work zero: {}", i + 1, 53 + s.cam.wcs() as u32, s.cam.name, s.work_zero.clone().unwrap_or_else(|| s.cam.origin.label().to_lowercase()))));
            for n in s.cam.setup_notes() {
                o.line(&comment(&format!("  {n}")));
            }
        }
    } else {
        for n in cam.setup_notes() {
            o.line(&comment(&n));
        }
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
    // each tool once, by number, in the order they go in
    let mut used: Vec<&Tool> = vec![];
    for &(si, pi) in &order {
        let s = &st[si];
        let Some(t) = s.paths.get(pi).filter(|p| !p.moves.is_empty()).and_then(|p| s.cam.tool(p.tool)) else { continue };
        match used.iter().find(|u| u.number == t.number) {
            Some(u) if u.describe() != t.describe() => o.line(&comment(&format!("WARNING: T{} is {} in one station and {} in another", t.number, u.describe(), t.describe()))),
            Some(_) => {}
            None => used.push(t),
        }
    }
    for t in used.iter().copied() {
        {
            let d = t.describe();
            let mut line = if t.name == d || t.name.is_empty() { format!("T{} {d}", t.number) } else { format!("T{} {} - {d}", t.number, t.name) };
            // rcnc fills its tool table from these lines, for the picture at the tool change
            if cam.post == "rcnc" {
                line += &format!(", flute length {}, stickout {}", num(t.flute_length), num(t.stickout));
            }
            o.line(&comment(&line));
        }
    }
    if d != Dialect::LinuxCnc && cam.post != "rcnc" && used.iter().any(|t| t.kind == ToolKind::Tap && !t.tapping_head) {
        o.line(&comment("no rigid tapping: the spindle reverses at the bottom of each tap - hold taps in a floating tap holder"));
    }
    let total: f64 = st.iter().map(|s| crate::cam_paths::cycle_time(s.cam, s.paths)).sum();
    // rounded once, so 239.6 s reads 4:00, not 3:60
    let secs = total.round().max(0.0) as i64;
    o.line(&comment(&format!("estimated time {}:{:02}", secs / 60, secs % 60)));
    match d {
        Dialect::Grbl => o.line("G21 G17 G90 G94"),
        // G91.1: arc centres (I, J) relative to the start, whatever the controller was left in
        Dialect::LinuxCnc => o.line("G21 G17 G40 G49 G80 G90 G91.1 G94 G64 P0.01"),
        // rcnc: constant velocity, or every one of a 3D path's short moves stops dead at its end
        Dialect::Mach3 if cam.post == "rcnc" => o.line("G21 G17 G40 G49 G80 G90 G91.1 G94 G64"),
        Dialect::Mach3 => o.line("G21 G17 G40 G49 G80 G90 G91.1 G94"),
        Dialect::Fanuc => o.line("G21 G17 G40 G49 G80 G90 G94"),
    }
    o.line(&format!("G{}", 53 + cam.wcs() as u32));
    for l in fill(&cam.post_text.header, cam, None) {
        o.line(&l);
    }
    // the loaded tool, by number (stations have their own tool lists)
    let mut cur_tool: Option<u32> = None;
    let mut found_stock = vec![false; st.len()];
    // the probing operations already posted as part of an rcnc Find bores
    let mut found_bores: Vec<Vec<u64>> = vec![vec![]; st.len()];
    // each station's Z reference goes in its own work offset, the first time the job gets there
    let mut zref = vec![false; st.len()];
    let z_reference = |o: &mut Out, c: &Cam| {
        if c.post == "rcnc" && !c.probe.z_reference.trim().is_empty() {
            o.line(&comment(&format!("RCNC Z_REFERENCE {}", c.probe.z_reference.trim())));
        }
    };
    z_reference(&mut o, cam);
    zref[first_station] = true;
    let mut station = first_station;
    // just moved to another station: travel over at the top before coming down
    let mut travel = false;
    let mut spindle_rpm: Option<f64> = None;
    let mut coolant = Coolant::Off;
    for &(si, pi) in &order {
        let Some(tp) = st[si].paths.get(pi) else { continue };
        if tp.moves.is_empty() {
            continue;
        }
        let Station { cam, paths, origin, safe, stock, .. } = st[si];
        let safe_w = safe - origin.z;
        let Some(tool) = cam.path_tool(tp.op, tp.tool) else { continue };
        let tool: &Tool = &tool;
        if si != station {
            o.line("");
            o.line(&comment(&format!("station {}: {}", si + 1, cam.name)));
            o.top(d, st[station].safe - st[station].origin.z);
            o.line(&format!("G{}", 53 + cam.wcs() as u32));
            o.forget();
            if !zref[si] {
                z_reference(&mut o, cam);
                zref[si] = true;
            }
            station = si;
            travel = true;
        }
        let op = cam.op(tp.op);
        o.line("");
        o.line(&comment(&format!("{} - {}", op.map(|o| o.name.as_str()).unwrap_or("operation"), op.map(|o| o.kind.label()).unwrap_or(""))));
        if cur_tool != Some(tool.number) {
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
            // rcnc's M6 measures the tool itself: a probe cycle here would measure it twice
            let probing = cam.probe.mode != "off" && cam.post != "rcnc";
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
            cur_tool = Some(tool.number);
        }
        if tool.kind == ToolKind::Probe {
            // a touch probe never spins and stays dry
            if spindle_rpm.take().is_some() {
                o.line("M5");
            }
            if coolant.is_on() {
                o.line("M9");
                coolant = Coolant::Off;
            }
            // rcnc finds the stock itself (Find stock: jog over it, it probes the top and all four sides and
            // checks the size), so probing the stock's top or middle becomes one request for that
            let find = op.and_then(|o| o.probing.as_ref()).filter(|p| cam.post == "rcnc" && !p.on_part && matches!(p.cycle, ProbeCycle::Z | ProbeCycle::Boss));
            if let (Some(p), Some((lo, hi))) = (find, stock) {
                if !found_stock[si] {
                    // the fixed jaw for X may be set on either of the stock probes (top or middle)
                    let probes = || cam.ops.iter().filter(|o| !o.suppressed && o.kind == OpKind::Probe).filter_map(|o| o.probing.as_ref());
                    let jaw = probes().find(|p| p.jaw_x != 0.0);
                    let size = probes().find_map(|p| p.find_size);
                    let vise = vise_request(cam, lo, hi, origin);
                    // rcnc takes the saved vise location or a touched jaw, not both: the vise wins
                    let jaw = if vise.is_some() { None } else { jaw };
                    find_stock(&mut o, p, jaw, size, lo, hi, origin, !cam.probe.z_reference.trim().is_empty(), vise);
                    found_stock[si] = true;
                }
                continue;
            }
            // two (or more) bores of the part in a row: rcnc finds them all and sets X/Y from them
            if cam.post == "rcnc" && found_bores[si].contains(&tp.op) {
                continue;
            }
            if let Some(bores) = rcnc_bores(cam, paths, tp, tool.radius(), origin) {
                found_bores[si].extend(bores.1);
                o.line(&comment("rcnc Find bores: the probe finds the locating bores and sets X/Y from them"));
                o.line(&comment(&bores.0));
                o.forget();
                continue;
            }
            match &tp.probe {
                Some(plan) => probe_cycle(&mut o, d, cam, plan, origin, safe_w),
                None => o.line(&comment("nothing to probe")),
            }
            continue;
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
            // (rcnc waits for its own spin-up time after M3, so no dwell on top)
            if cam.machine.spin_up > 0.0 && cam.post != "rcnc" {
                o.line(&dwell(d, cam.machine.spin_up));
            }
            // rcnc: a few seconds in air at the new speed, where its bog guard learns the no-load speed
            if cam.post == "rcnc" && cam.rcnc.speed_dwell() > 0.0 {
                o.line(&dwell(d, cam.rcnc.speed_dwell()));
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
                    let surface = op.is_some_and(|o| matches!(o.kind, OpKind::Parallel | OpKind::Waterline | OpKind::Scallop | OpKind::Pencil | OpKind::Deburr));
                    for piece in fit_arcs_tol(&pts, if surface { CHORD_TOL_3D } else { CHORD_TOL }) {
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
                    if first && travel {
                        o.line(&format!("G0 X{} Y{}", num(w.x), num(w.y)));
                        o.forget();
                        travel = false;
                    }
                    if first && !tp.linked {
                        // go up first, then over (a linked toolpath carries on from where the one before ended)
                        o.z_up(safe_w);
                        o.mv(0, DVec3::new(w.x, w.y, safe_w), None);
                    }
                    o.mv(0, w, None);
                    pos = p.into();
                }
                Move::Feed(p, f) => {
                    if first && travel {
                        let w = DVec3::from(p) - origin;
                        o.line(&format!("G0 X{} Y{}", num(w.x), num(w.y)));
                        o.forget();
                        travel = false;
                    }
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
                        // rcnc refuses G84 on a spindle that can't reverse (the DMC2's can't):
                        // stop over the hole and leave the tapping to the operator
                        Dialect::Mach3 if cam.post == "rcnc" => {
                            o.mv(0, DVec3::new(w.x, w.y, rw), None);
                            o.line(&message(d, &format!("tap by hand at X{} Y{} to Z{}, then press cycle start", num(w.x), num(w.y), num(zw))));
                            o.line("M0");
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
    o.top(d, st[station].safe - st[station].origin.z);
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
/// rcnc: the bore probing operations starting at `tp` (consecutive probes of the part's bores with their middle
/// and diameter given) as one `(RCNC FIND_BORES ...)` request, and their operation ids. None unless there are two.
fn rcnc_bores(cam: &Cam, paths: &[Toolpath], tp: &Toolpath, r: f64, origin: DVec3) -> Option<(String, Vec<u64>)> {
    if cam.post != "rcnc" {
        return None;
    }
    let bore = |t: &Toolpath| {
        let op = cam.op(t.op)?;
        let p = op.probing.as_ref().filter(|p| p.cycle == ProbeCycle::Bore && p.size > 0.0)?;
        let at = op.at?;
        // the ball's tip goes `depth` + its radius below the bore's top
        let tip = t.probe.as_ref()?.steps.iter().find_map(|s| match s {
            crate::cam_probe::Step::Touch { from, .. } => Some(from[2]),
            _ => None,
        })?;
        Some((op.id, at, p.clone(), tip - origin.z + p.depth + r))
    };
    let start = paths.iter().position(|p| std::ptr::eq(p, tp))?;
    let found: Vec<_> = paths[start..].iter().map_while(bore).collect();
    let [(_, a, pa, top), (_, b, _, _), ..] = found.as_slice() else { return None };
    let text = format!(
        "RCNC FIND_BORES A={},{} B={},{} D={} TOP={} DEPTH={} TOL={} ROT={}",
        num(a[0]),
        num(a[1]),
        num(b[0]),
        num(b[1]),
        num(pa.size),
        num(*top),
        num(pa.depth),
        num(pa.tolerance),
        num(pa.max_turn)
    );
    Some((text, found.iter().map(|f| f.0).collect()))
}

/// rcnc: the lines under the title: the stock it is for, the bog guard, the sound recording.
fn rcnc_header(o: &mut Out, cam: &Cam) {
    let r = &cam.rcnc;
    if !r.stock_note.trim().is_empty() {
        o.line(&comment(&format!("STOCK {}", r.stock_note.trim())));
    }
    if r.require_guard {
        o.line(&comment("RCNC REQUIRE_GUARD: the bog guard [Settings -> Listening, feed hold on] must be armed: rcnc refuses Cycle Start without it"));
    }
    if r.record_audio {
        o.line(&comment("RCNC RECORD_AUDIO: rcnc saves the run's sound beside this file as .wav"));
    }
}

/// rcnc: Find stock from the vise location rcnc saved (` VISE=1` / ` VISE=-1`, with ` JAWAT=` the work X of the
/// fixed jaw's face when that isn't the stock's side), for a setup with `rcnc.vise`. `lo`, `hi`: the stock box.
fn vise_request(cam: &Cam, lo: DVec3, hi: DVec3, origin: DVec3) -> Option<String> {
    let side = cam.rcnc.vise_side()?;
    let (lo, hi) = (lo - origin, hi - origin);
    let mid = (lo.x + hi.x) / 2.0;
    let stock_face = if side > 0 { hi.x } else { lo.x };
    // the fixed jaw: the nearest fixture on its side of the stock, beside it in Y and Z (not a shim under it)
    let beside = |f: &&crate::cam::Fixture| f.max[1] > lo.y && f.min[1] < hi.y && f.max[2] > lo.z && f.min[2] < hi.z;
    let face = cam.rcnc.jaw_at.or_else(|| {
        let near = cam.fixtures.iter().filter(beside);
        if side > 0 {
            near.filter(|f| f.min[0] >= mid).map(|f| f.min[0]).min_by(|a, b| a.total_cmp(b))
        } else {
            near.filter(|f| f.max[0] <= mid).map(|f| f.max[0]).max_by(|a, b| a.total_cmp(b))
        }
    });
    let jaw_at = face.filter(|x| (x - stock_face).abs() > 0.005).map(|x| format!(" JAWAT={}", num(x))).unwrap_or_default();
    Some(format!(" VISE={side}{jaw_at}"))
}

/// rcnc: ask for its Find stock wizard (the touch probe is loaded already): the operator jogs over the middle
/// of the stock, rcnc probes its top and four sides, checks its size and sets the work zero so the stock's
/// top centre reads its place in this setup. Every tool measured after it is zeroed on the probed top.
#[allow(clippy::too_many_arguments)]
fn find_stock(o: &mut Out, p: &crate::cam::ProbeOp, jaw: Option<&crate::cam::ProbeOp>, find_size: Option<[f64; 2]>, lo: DVec3, hi: DVec3, origin: DVec3, keep_z: bool, vise: Option<String>) {
    let mut size = hi - lo;
    if let Some([x, y]) = find_size {
        size = DVec3::new(x, y, size.z);
    }
    let top_centre = DVec3::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0, hi.z) - origin;
    o.line(&comment("rcnc Find stock: jog the probe over the middle of the stock, a few mm above it, and press Continue"));
    o.line(&comment(&format!("Find stock touches the sides {} below the top: measure sawn stock there, its ends may not be square", num(p.depth))));
    o.line(&comment(&format!(
        "RCNC FIND_STOCK SIZE={},{},{} AT={},{},{} TOL={} DEPTH={}{}{}{}",
        num(size.x),
        num(size.y),
        num(size.z),
        num(top_centre.x),
        num(top_centre.y),
        num(top_centre.z),
        num(p.tolerance),
        num(p.depth),
        if keep_z { " KEEPZ=1" } else { "" },
        jaw.map(|j| format!(
            " JAWX={} JAWZ={}{}",
            if j.jaw_x < 0.0 { -1 } else { 1 },
            num(j.jaw_z),
            j.jaw_at.map(|x| format!(" JAWAT={}", num(x))).unwrap_or_default()
        ))
        .unwrap_or_default(),
        vise.unwrap_or_default()
    )));
    o.forget();
}

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
                Dialect::Fanuc => o.line(&format!("G10 L2 P{} Z[#5023-{}]", cam.wcs(), num(top))),
                _ => o.line(&format!("G10 L20 P{} Z{}", cam.wcs(), num(top))),
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

/// A probing operation's cycle: each touch searches fast, backs off and touches again slowly; then the work
/// zero (G54) axes are set so the surfaces found are where the setup has them.
fn probe_cycle(o: &mut Out, d: Dialect, cam: &Cam, plan: &crate::cam_probe::Plan, origin: DVec3, safe_w: f64) {
    use crate::cam_probe::Step;
    let g = if matches!(d, Dialect::Fanuc | Dialect::Mach3) { "G31" } else { "G38.2" };
    // where the controller leaves the trip position (work coordinates)
    let trip = if d == Dialect::Mach3 { 2000 } else { 5061 };
    let var = |slot: usize| 110 + slot;
    let axes = ['X', 'Y', 'Z'];
    let readings = d != Dialect::Grbl;
    o.line(&comment(&format!("find {} and set {}", plan.what, plan.sets_text(origin))));
    if !readings && plan.needs_readings() {
        o.line(&message(d, &format!("GRBL can't work out {} from probe readings: set {} there by hand, then press cycle start", plan.what, plan.sets_text(origin))));
        o.line("M0");
        return;
    }
    // "+ 1" / "- 1"; nothing for 0
    let signed = |v: f64| {
        let v = (v * 1000.0).round() / 1000.0;
        if v.abs() < 1e-9 {
            String::new()
        } else if v > 0.0 {
            format!(" + {}", num(v))
        } else {
            format!(" - {}", num(-v))
        }
    };
    let comp_of = |slot: usize| {
        plan.steps.iter().find_map(|s| match s {
            Step::Touch { slot: k, comp, .. } if *k == slot => Some(*comp),
            _ => None,
        })
    };
    o.z_up(safe_w);
    for step in &plan.steps {
        match step {
            Step::Move { to, feed, centre_x } => {
                let p = DVec3::from(*to) - origin;
                let gm = if feed.is_some() { 1 } else { 0 };
                match centre_x {
                    Some([a, b]) if readings => {
                        let mut l = format!("G{gm} X[[#{} + #{}] / 2] Y{} Z{}", var(*a), var(*b), num(p.y), num(p.z));
                        if let Some(f) = feed {
                            let _ = write!(l, " F{}", num(f.round()));
                        }
                        o.line(&l);
                        o.forget();
                    }
                    _ => o.mv(gm, p, *feed),
                }
            }
            Step::Touch { from, axis, dir, expect, slot, comp } => {
                let p = DVec3::from(*from) - origin;
                let ax = axes[*axis];
                let start = p[*axis];
                let target = start + dir * (expect + plan.overtravel);
                o.line(&format!("{g} {ax}{} F{}", num(target), num(plan.fast.round())));
                o.line("G91");
                o.line(&format!("G1 {ax}{} F{}", num(-dir * plan.latch), num(plan.fast.round())));
                o.line("G90");
                o.line(&format!("{g} {ax}{} F{}", num(target), num(plan.slow.round())));
                if readings {
                    o.line(&format!("#{} = #{}", var(*slot), trip + axis));
                } else if let Some(set) = plan.sets.iter().find(|s| s.touches == [*slot]) {
                    // GRBL: the probe is where it tripped, so set the axis from here
                    o.line(&format!("G10 L20 P{} {ax}{}", cam.wcs(), num(((set.target - origin[set.axis] - comp) * 1000.0).round() / 1000.0)));
                }
                o.line(&format!("G1 {ax}{} F{}", num(start), num(plan.fast.round())));
                o.forget();
            }
        }
    }
    o.z_up(safe_w);
    if readings && !plan.sets.is_empty() {
        let mut l = format!("G10 L2 P{}", cam.wcs());
        for s in &plan.sets {
            let terms: Vec<String> = s.touches.iter().map(|k| format!("#{}{}", var(*k), signed(comp_of(*k).unwrap_or(0.0)))).collect();
            let mean = if terms.len() == 1 { terms[0].clone() } else { format!("[{}] / {}", terms.join(" + "), terms.len()) };
            let _ = write!(l, " {}[#{} + {mean}{}]", axes[s.axis], 5221 + 20 * (cam.wcs() as usize - 1) + s.axis, signed(-(s.target - origin[s.axis])));
        }
        o.line(&l);
    }
    o.forget();
}

/// Largest gap between the toolpath's points and the arcs that replace it, mm.
const ARC_TOL: f64 = 0.005;
/// Largest bulge of an arc over a segment it replaces: the segments were themselves cut short of the
/// curve they stand for (offsets keep within 0.004 mm), so the arc may run a little outside them.
const CHORD_TOL: f64 = 0.02;
/// The same for 3D finishing, whose points touch the part: there a bulge can be into it (a waterline round
/// the Eclipse car's tail corners was fitted with arcs 0.02-0.05 into the wall), so arcs only where the
/// segments already lie on them.
const CHORD_TOL_3D: f64 = 0.002;

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
fn arc_of(pts: &[DVec3], chord_tol: f64) -> Option<(DVec2, bool)> {
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
        if r - (r * r - p.distance_squared(q) / 4.0).max(0.0).sqrt() > chord_tol {
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
    fit_arcs_tol(pts, CHORD_TOL)
}

fn fit_arcs_tol(pts: &[DVec3], chord_tol: f64) -> Vec<Piece> {
    let mut out = vec![];
    let mut i = 0;
    while i + 1 < pts.len() {
        let mut best = None;
        let mut j = i + 4;
        while j < pts.len() && j - i <= 400 {
            match arc_of(&pts[i..=j], chord_tol) {
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
    fn a_move_that_writes_nothing_keeps_the_motion_mode() {
        let mut o = Out { s: String::new(), last: [None; 3], motion: None, feed: None };
        o.mv(1, DVec3::new(0.0, 0.0, 1.0), Some(500.0));
        o.mv(0, DVec3::new(0.0, 0.0, 5.0), None);
        // rounds to where the tool already is: no line, and the controller stays in G0
        o.mv(1, DVec3::new(0.0, 0.0, 5.0002), Some(500.0));
        o.mv(1, DVec3::new(0.0, 0.0, 1.0), Some(500.0));
        assert_eq!(o.s.lines().last(), Some("G1 Z1"), "{}", o.s);
    }

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
