//! The post's safety stamp and its check: a program goes out only when rCAD's stock simulation of the very
//! toolpaths in it finds nothing wrong (`export_gcode`), and says so in its header with a hash of those
//! toolpaths; `verify_gcode` reads a program back (as posted, or after it was edited: pauses, dwells added),
//! simulates its moves against the setup and checks the stamp.

use crate::cam::Cam;
use crate::cam_paths::{Move, Toolpath};
use glam::DVec3;

/// What the stamp line starts with when the simulation was clean.
pub const OK: &str = "(SIM OK:";
/// And when the program was written anyway (`allow_problems`).
pub const UNSAFE: &str = "(UNSAFE:";

/// A hash of the toolpaths' moves (FNV-1a over their exact values), tying a program to the toolpaths that
/// were simulated.
pub fn toolpath_hash(paths: &[Toolpath]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: &[u8]| {
        for x in b {
            h ^= *x as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for tp in paths {
        eat(&tp.op.to_le_bytes());
        eat(&tp.tool.to_le_bytes());
        for m in &tp.moves {
            match *m {
                Move::Rapid(p) => {
                    eat(&[0]);
                    p.iter().for_each(|v| eat(&v.to_bits().to_le_bytes()));
                }
                Move::Feed(p, f) => {
                    eat(&[1]);
                    p.iter().for_each(|v| eat(&v.to_bits().to_le_bytes()));
                    eat(&f.to_bits().to_le_bytes());
                }
                Move::Tap { z, r, pitch } => {
                    eat(&[2]);
                    [z, r, pitch].iter().for_each(|v| eat(&v.to_bits().to_le_bytes()));
                }
                Move::Dwell(s) => {
                    eat(&[3]);
                    eat(&s.to_bits().to_le_bytes());
                }
            }
        }
    }
    h
}

/// The stamp line for a program whose simulation found `problems` problems.
pub fn stamp(problems: usize, hash: u64) -> String {
    if problems == 0 {
        format!("{OK} 0 problems in rCAD's stock simulation of these toolpaths, hash {hash:016x})")
    } else {
        format!("{UNSAFE} {problems} SIM PROBLEMS in rCAD's stock simulation of these toolpaths [rapids into stock, gouges, crashes or overloads] - DO NOT RUN, hash {hash:016x})")
    }
}

/// Whether a program may be written: with no problems, at `path` with the SIM OK stamp; with problems, not at
/// all (the error says what the simulation found), or with `allow` at `<name>.UNSAFE.<ext>` with the UNSAFE
/// stamp. Gives the path to write and the stamp line.
pub fn gate(problems: usize, first: &[String], allow: bool, path: &str, hash: u64) -> Result<(String, String), String> {
    if problems == 0 {
        return Ok((path.to_string(), stamp(0, hash)));
    }
    if !allow {
        return Err(format!(
            "not exported: rCAD's simulation of these toolpaths finds {problems} problem(s): {}. Fix them; or, only to look at it, export with allow_problems: true, which writes an UNSAFE copy",
            first.join("; ")
        ));
    }
    Ok((unsafe_path(path), stamp(problems, hash)))
}

/// The program with its stamp as the second line (after the title).
pub fn stamped(text: &str, line: &str) -> String {
    match text.find('\n') {
        Some(i) => format!("{}\n{line}\n{}", &text[..i], &text[i + 1..]),
        None => format!("{text}\n{line}\n"),
    }
}

/// `prog.nc` -> `prog.UNSAFE.nc`.
pub fn unsafe_path(path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.file_name().map(|n| n.to_string_lossy().contains(".UNSAFE")).unwrap_or(false) {
        return path.to_string();
    }
    match (p.file_stem(), p.extension()) {
        (Some(s), Some(e)) => p.with_file_name(format!("{}.UNSAFE.{}", s.to_string_lossy(), e.to_string_lossy())).display().to_string(),
        _ => format!("{path}.UNSAFE"),
    }
}

/// The stamp in a program: clean or not, and the hash.
pub fn read_stamp(text: &str) -> Option<(bool, Option<u64>)> {
    for l in text.lines().take(40) {
        let l = l.trim();
        let ok = l.starts_with(OK);
        if ok || l.starts_with(UNSAFE) {
            let hash = l.rsplit("hash ").next().and_then(|h| u64::from_str_radix(h.trim_end_matches(')').trim(), 16).ok());
            return Some((ok, hash));
        }
    }
    None
}

/// A program read back as toolpaths in setup coordinates (work coordinates + `origin`), one per operation
/// (its `(name - Kind)` comment) and tool (`T<n> M6`). `G53` moves (machine Z) go to `safe` (setup Z).
/// Arcs (G2/G3, centre from I/J relative to the start, XY plane) become short lines.
pub fn parse(text: &str, cam: &Cam, origin: DVec3, safe: f64) -> Result<Vec<Toolpath>, String> {
    let mut paths: Vec<Toolpath> = vec![];
    let mut pos = DVec3::new(0.0, 0.0, safe - origin.z);
    let (mut motion, mut feed, mut tool, mut op) = (0u8, 1000.0f64, 0u64, 0u64);
    let mut cur = |paths: &mut Vec<Toolpath>, op: u64, tool: u64| -> usize {
        if paths.last().map(|p| p.op != op || p.tool != tool).unwrap_or(true) {
            paths.push(Toolpath { op, tool, ..Default::default() });
        }
        paths.len() - 1
    };
    let ops: Vec<(String, u64)> = cam.ops.iter().map(|o| (o.name.replace('(', "[").replace(')', "]"), o.id)).collect();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        // a whole-line comment naming an operation: "(Rough the slots - Pocket)"
        if let Some(c) = line.strip_prefix('(').and_then(|c| c.strip_suffix(')')) {
            // a manual tool change (posts without T words, e.g. GRBL): "(load T1: D6 flat end mill ...)"
            if let Some(t) = c.trim_start().strip_prefix("load T") {
                let num: String = t.chars().take_while(|ch| ch.is_ascii_digit()).collect();
                if let Ok(t) = num.parse::<u32>() {
                    tool = cam.tools.iter().find(|x| x.number == t).map(|x| x.id).unwrap_or(0);
                }
                continue;
            }
            if let Some((name, _kind)) = c.rsplit_once(" - ") {
                if let Some((_, id)) = ops.iter().find(|(o, _)| o == name.trim()) {
                    op = *id;
                }
            }
            continue;
        }
        // the code, without comments
        let mut code = String::new();
        let mut depth = 0;
        for ch in line.chars() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                ';' if depth == 0 => break,
                _ if depth == 0 => code.push(ch.to_ascii_uppercase()),
                _ => {}
            }
        }
        if code.is_empty() || code.starts_with('%') {
            continue;
        }
        let mut words: Vec<(char, f64)> = vec![];
        let b = code.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let c = b[i] as char;
            if c.is_ascii_alphabetic() {
                let s = i + 1;
                let mut j = s;
                while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.' || b[j] == b'-' || b[j] == b'+') {
                    j += 1;
                }
                let v: f64 = code[s..j].parse().map_err(|_| format!("line {}: can't read '{}'", n + 1, &code[i..j]))?;
                words.push((c, v));
                i = j;
            } else {
                i += 1;
            }
        }
        let get = |l: char| words.iter().find(|w| w.0 == l).map(|w| w.1);
        let g: Vec<f64> = words.iter().filter(|w| w.0 == 'G').map(|w| w.1).collect();
        if g.iter().any(|&v| v == 91.0) {
            return Err(format!("line {}: incremental moves (G91) aren't read back", n + 1));
        }
        if let (Some(t), true) = (get('T'), words.iter().any(|w| w.0 == 'M' && w.1 == 6.0)) {
            tool = cam.tools.iter().find(|x| x.number as f64 == t).map(|x| x.id).unwrap_or(0);
        }
        if let Some(f) = get('F') {
            feed = f;
        }
        if g.iter().any(|&v| v == 53.0) {
            // a machine-coordinate move: up to the safe height where it gives Z
            if get('Z').is_some() {
                pos.z = safe - origin.z;
                // before the first tool there is nothing to simulate
                if tool != 0 {
                    let k = cur(&mut paths, op, tool);
                    paths[k].moves.push(Move::Rapid((pos + origin).to_array()));
                }
            }
            continue;
        }
        if g.iter().any(|&v| v == 4.0) {
            continue;
        }
        for &v in &g {
            match v as i64 {
                0 => motion = 0,
                1 => motion = 1,
                2 => motion = 2,
                3 => motion = 3,
                _ => {}
            }
        }
        let to = DVec3::new(get('X').unwrap_or(pos.x), get('Y').unwrap_or(pos.y), get('Z').unwrap_or(pos.z));
        if to == pos && !(motion >= 2 && (get('I').is_some() || get('J').is_some())) {
            continue;
        }
        let k = cur(&mut paths, op, tool);
        match motion {
            0 => paths[k].moves.push(Move::Rapid((to + origin).to_array())),
            1 => paths[k].moves.push(Move::Feed((to + origin).to_array(), feed)),
            _ => {
                let c = glam::DVec2::new(pos.x + get('I').unwrap_or(0.0), pos.y + get('J').unwrap_or(0.0));
                let r = (pos.truncate() - c).length();
                let a0 = (pos.y - c.y).atan2(pos.x - c.x);
                let mut a1 = (to.y - c.y).atan2(to.x - c.x);
                let cw = motion == 2;
                if cw {
                    while a1 >= a0 - 1e-12 {
                        a1 -= std::f64::consts::TAU;
                    }
                } else {
                    while a1 <= a0 + 1e-12 {
                        a1 += std::f64::consts::TAU;
                    }
                }
                // a full circle when it ends where it starts
                let sweep = a1 - a0;
                let steps = ((sweep.abs() * r) / 0.1).ceil().max(((sweep.abs()) / 0.1).ceil()).max(1.0) as usize;
                for s in 1..=steps {
                    let u = s as f64 / steps as f64;
                    let a = a0 + sweep * u;
                    let p = if s == steps { to } else { DVec3::new(c.x + r * a.cos(), c.y + r * a.sin(), pos.z + (to.z - pos.z) * u) };
                    paths[k].moves.push(Move::Feed((p + origin).to_array(), feed));
                }
            }
        }
        pos = to;
    }
    // a move read with no tool would simulate nothing: refuse rather than call the program clean
    if let Some(p) = paths.iter().find(|p| p.tool == 0 && p.moves.iter().any(|m| matches!(m, Move::Feed(..)))) {
        return Err(format!("can't tell which tool cuts in '{}': no T<n> M6 or (load T<n>) before its moves", cam.op(p.op).map(|o| o.name.as_str()).unwrap_or("the program")));
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stamp_reads_back_and_unsafe_programs_get_their_own_name() {
        let h = 0x1234_5678_9abc_def0;
        let t = stamped("(Op 1 - made by rCAD)\nG21\n", &stamp(0, h));
        assert_eq!(t.lines().nth(1).unwrap(), "(SIM OK: 0 problems in rCAD's stock simulation of these toolpaths, hash 123456789abcdef0)");
        assert_eq!(read_stamp(&t), Some((true, Some(h))));
        assert_eq!(read_stamp(&stamped("(x)\n", &stamp(3, h))), Some((false, Some(h))));
        assert_eq!(read_stamp("(x)\nG0 X0\n"), None);
        // compared as paths: on Windows the file name is joined with a backslash
        assert_eq!(std::path::Path::new(&unsafe_path("/jobs/op2-bottom.nc")), std::path::Path::new("/jobs/op2-bottom.UNSAFE.nc"));
        assert_eq!(unsafe_path("/jobs/op2-bottom.UNSAFE.nc"), "/jobs/op2-bottom.UNSAFE.nc");
    }

    #[test]
    fn a_program_with_problems_is_refused_unless_asked_for_and_then_marked() {
        let first = vec!["rapid move into stock in 'Rough' at [1, 2, 3] (0.7 mm)".to_string()];
        let e = gate(6, &first, false, "op2.nc", 7).unwrap_err();
        assert!(e.starts_with("not exported") && e.contains("6 problem(s)") && e.contains("rapid move into stock"), "{e}");
        let (p, l) = gate(6, &first, true, "op2.nc", 7).unwrap();
        assert_eq!(p, "op2.UNSAFE.nc");
        assert!(l.starts_with("(UNSAFE: 6 SIM PROBLEMS") && l.contains("DO NOT RUN"), "{l}");
        let (p, l) = gate(0, &[], false, "op2.nc", 7).unwrap();
        assert_eq!((p.as_str(), read_stamp(&format!("(t)\n{l}\n"))), ("op2.nc", Some((true, Some(7)))));
    }

    #[test]
    fn the_hash_follows_the_moves() {
        let a = Toolpath { op: 1, tool: 2, moves: vec![Move::Rapid([0.0, 0.0, 5.0]), Move::Feed([1.0, 0.0, -1.0], 300.0)], ..Default::default() };
        let mut b = a.clone();
        assert_eq!(toolpath_hash(&[a.clone()]), toolpath_hash(&[b.clone()]));
        b.moves[1] = Move::Feed([1.0, 0.0, -1.001], 300.0);
        assert_ne!(toolpath_hash(&[a]), toolpath_hash(&[b]));
    }

    #[test]
    fn a_program_reads_back_as_its_moves() {
        let mut c = Cam::default();
        c.tools.push(crate::cam::Tool { id: 4, number: 24, diameter: 6.0, ..Default::default() });
        let op = c.add_op(crate::cam::OpKind::Trace, 4);
        c.ops.iter_mut().find(|o| o.id == op).unwrap().name = "Line (both ways)".into();
        let o = DVec3::new(10.0, 20.0, 5.0);
        let text = "(Line [both ways] - Trace)\nG53 G0 Z0\nT24 M6\nS9000 M3\nG4 P3\nG0 X1 Y2\nZ1\nG1 Z-1 F300\nX5 F1000\nG2 X5 Y6 I0 J2\nG0 Z5\n";
        let p = parse(text, &c, o, 40.0).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].op, p[0].tool), (op, 4));
        let m = &p[0].moves;
        assert_eq!(m[0], Move::Rapid([11.0, 22.0, 40.0]));
        assert_eq!(m[1], Move::Rapid([11.0, 22.0, 6.0]));
        assert_eq!(m[2], Move::Feed([11.0, 22.0, 4.0], 300.0));
        assert_eq!(m[3], Move::Feed([15.0, 22.0, 4.0], 1000.0));
        // the half circle round (5, 4), clockwise from (5, 2) to (5, 6): by way of (3, 4)
        let arc: Vec<DVec3> = m[4..m.len() - 1].iter().map(|x| x.end(DVec3::ZERO) - o).collect();
        assert!((arc.last().unwrap() - DVec3::new(5.0, 6.0, -1.0)).length() < 1e-9);
        assert!(arc.iter().all(|q| ((q.x - 5.0).hypot(q.y - 4.0) - 2.0).abs() < 1e-9));
        assert!(arc.iter().any(|q| (q.x - 3.0).abs() < 0.05));
        assert_eq!(*m.last().unwrap(), Move::Rapid([15.0, 26.0, 10.0]));
    }
}
