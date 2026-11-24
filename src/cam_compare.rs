//! Compare two G-code programs operation by operation (`compare_gcode`): e.g. a re-post against the program
//! that was cut, or a program made one way against the same job made another way.
//!
//! Each program is read into operations (by their `(name - Kind)` comments and the work offset they run in)
//! and the control lines between the moves (tool changes, speeds, coolant, dwells, stops, messages, rcnc
//! requests such as `(RCNC FIND_STOCK …)`). Operations are paired by station and name (then, for the ones
//! left, by station, tool and order) and compared by their move counts, feed and rapid lengths, feeds, and
//! how far each one's cutting path strays from the other's (both ways, so a pass one has and the other lacks
//! shows). The control lines are compared as sequences, so a missing pause or a changed probing request shows
//! with the operation it sits in.

use std::collections::HashMap;

use glam::DVec3;
use serde_json::{json, Value};

use crate::cam::OpKind;

/// A G0 machine-coordinate retract (`G53 G0 Z…`) goes this high, in the program's coordinates.
const TOP: f64 = 1.0e3;

/// One operation's stretch of a program (all its stretches when it comes back, as in a job's cycle).
#[derive(Clone, Debug, Default)]
pub struct Op {
    pub station: String,
    pub name: String,
    pub kind: String,
    pub tool: Option<u32>,
    pub rpm: Vec<f64>,
    pub moves: usize,
    pub rapid_mm: f64,
    pub retracts: usize,
    /// Feed length by feed rate (mm/min, mm).
    pub feeds: Vec<(f64, f64)>,
    /// Cutting (G1/G2/G3) segments, arcs as short lines.
    pub segs: Vec<(DVec3, DVec3)>,
}

impl Op {
    pub fn feed_mm(&self) -> f64 {
        self.feeds.iter().map(|f| f.1).sum()
    }

    /// Minutes at the programmed feeds (no acceleration).
    pub fn feed_min(&self) -> f64 {
        self.feeds.iter().filter(|f| f.0 > 0.0).map(|f| f.1 / f.0).sum()
    }

    fn add_feed(&mut self, f: f64, mm: f64) {
        match self.feeds.iter_mut().find(|x| (x.0 - f).abs() < 1e-9) {
            Some(x) => x.1 += mm,
            None => self.feeds.push((f, mm)),
        }
    }
}

/// A control line: what it is, in which station and operation.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub line: usize,
    pub station: String,
    pub op: String,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    /// Comment lines before the first move or control line, without the title and the SIM OK stamp.
    pub header: Vec<String>,
    pub ops: Vec<Op>,
    pub events: Vec<Event>,
}

/// The operation an operation comment names: "(Rough the slots - Pocket)" -> ("Rough the slots", "Pocket").
fn op_comment(c: &str) -> Option<(String, String)> {
    let (name, kind) = c.rsplit_once(" - ")?;
    let kind = kind.trim();
    OpKind::ALL.iter().find(|k| k.label() == kind).map(|_| (name.trim().to_string(), kind.to_string()))
}

/// The code of a line without its comments, upper case.
fn code_of(line: &str) -> String {
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
    code.trim().to_string()
}

fn words(code: &str) -> Vec<(char, f64)> {
    let b = code.as_bytes();
    let mut w = vec![];
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_alphabetic() {
            let s = i + 1;
            let mut j = s;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.' || b[j] == b'-' || b[j] == b'+') {
                j += 1;
            }
            if let Ok(v) = code[s..j].parse::<f64>() {
                w.push((c, v));
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    w
}

/// Read a program. Incremental moves (G91) are refused.
pub fn read(text: &str) -> Result<Program, String> {
    let mut p = Program::default();
    let mut station = "G54".to_string();
    let mut cur: Option<usize> = None;
    let (mut motion, mut feed, mut tool, mut rpm) = (0u8, 0.0f64, None::<u32>, None::<f64>);
    let mut pos = DVec3::new(0.0, 0.0, TOP);
    let mut started = false;
    let mut name = String::new();
    let mut kind = String::new();
    let event = |p: &mut Program, n: usize, station: &str, name: &str, text: String| {
        p.events.push(Event { line: n + 1, station: station.to_string(), op: name.to_string(), text });
    };
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('%') {
            continue;
        }
        // whole-line comments
        if let Some(c) = line.strip_prefix('(').and_then(|c| c.strip_suffix(')')) {
            let c = c.trim();
            let upper = c.to_ascii_uppercase();
            if let Some((nm, kd)) = op_comment(c) {
                name = nm;
                kind = kd;
                cur = None;
                started = true;
            } else if upper.starts_with("RCNC ") || upper.starts_with("MSG,") {
                event(&mut p, n, &station, &name, c.to_string());
                started = true;
            } else if !started && n > 0 && !c.starts_with("SIM OK:") && !c.starts_with("UNSAFE:") && !c.starts_with("estimated time") {
                p.header.push(c.to_string());
            }
            continue;
        }
        let code = code_of(line);
        if code.is_empty() {
            continue;
        }
        started = true;
        let w = words(&code);
        let get = |l: char| w.iter().find(|x| x.0 == l).map(|x| x.1);
        let g: Vec<i64> = w.iter().filter(|x| x.0 == 'G').map(|x| (x.1 * 10.0).round() as i64).collect();
        let m: Vec<i64> = w.iter().filter(|x| x.0 == 'M').map(|x| x.1.round() as i64).collect();
        if g.contains(&910) {
            return Err(format!("line {}: incremental moves (G91) aren't read", n + 1));
        }
        if let Some(wcs) = g.iter().find(|v| (540..=590).contains(*v) && *v % 10 == 0) {
            let s = format!("G{}", wcs / 10);
            if s != station {
                station = s;
                cur = None;
                event(&mut p, n, &station, &name, station.clone());
            }
        }
        if m.contains(&6) {
            tool = get('T').map(|t| t.round() as u32);
            cur = None;
            event(&mut p, n, &station, &name, format!("T{} M6", tool.unwrap_or(0)));
        }
        if let Some(s) = get('S') {
            rpm = Some(s);
        }
        if let Some(f) = get('F') {
            feed = f;
        }
        for (mc, txt) in [(3, "M3"), (4, "M4"), (5, "M5"), (7, "M7"), (8, "M8"), (9, "M9"), (0, "M0"), (1, "M1"), (30, "M30"), (2, "M2")] {
            if m.contains(&mc) {
                let t = if (mc == 3 || mc == 4) && rpm.is_some() { format!("S{} {txt}", crate::cam_post::num(rpm.unwrap_or(0.0))) } else { txt.to_string() };
                event(&mut p, n, &station, &name, t);
            }
        }
        if g.contains(&40) {
            event(&mut p, n, &station, &name, format!("G4 P{}", crate::cam_post::num(get('P').unwrap_or(0.0))));
            continue;
        }
        for &v in &g {
            match v {
                0 => motion = 0,
                10 => motion = 1,
                20 => motion = 2,
                30 => motion = 3,
                _ => {}
            }
        }
        let machine = g.contains(&530);
        let to = if machine {
            // a machine-coordinate move: only its Z (up to the top) is followed
            if get('Z').is_none() {
                continue;
            }
            DVec3::new(pos.x, pos.y, TOP)
        } else {
            DVec3::new(get('X').unwrap_or(pos.x), get('Y').unwrap_or(pos.y), get('Z').unwrap_or(pos.z))
        };
        let arc = motion >= 2 && !machine && (get('I').is_some() || get('J').is_some());
        if to == pos && !arc {
            continue;
        }
        let k = match cur {
            Some(k) => k,
            None => {
                let k = match p.ops.iter().position(|o| o.station == station && o.name == name && o.kind == kind) {
                    Some(k) => k,
                    None => {
                        p.ops.push(Op { station: station.clone(), name: name.clone(), kind: kind.clone(), tool, ..Default::default() });
                        p.ops.len() - 1
                    }
                };
                cur = Some(k);
                k
            }
        };
        let op = &mut p.ops[k];
        if op.tool.is_none() {
            op.tool = tool;
        }
        if let Some(r) = rpm.filter(|r| !op.rpm.iter().any(|x| (x - r).abs() < 1e-9)) {
            op.rpm.push(r);
        }
        op.moves += 1;
        if machine || motion == 0 {
            if machine {
                op.retracts += 1;
            } else {
                op.rapid_mm += (to - pos).length();
            }
        } else if motion == 1 || !arc {
            op.add_feed(feed, (to - pos).length());
            op.segs.push((pos, to));
        } else {
            let c = glam::DVec2::new(pos.x + get('I').unwrap_or(0.0), pos.y + get('J').unwrap_or(0.0));
            let r = (pos.truncate() - c).length();
            let a0 = (pos.y - c.y).atan2(pos.x - c.x);
            let mut a1 = (to.y - c.y).atan2(to.x - c.x);
            if motion == 2 {
                while a1 >= a0 - 1e-12 {
                    a1 -= std::f64::consts::TAU;
                }
            } else {
                while a1 <= a0 + 1e-12 {
                    a1 += std::f64::consts::TAU;
                }
            }
            let sweep = a1 - a0;
            let steps = ((sweep.abs() * r) / 0.05).ceil().max(sweep.abs() / 0.1).max(1.0) as usize;
            let mut last = pos;
            for s in 1..=steps {
                let u = s as f64 / steps as f64;
                let a = a0 + sweep * u;
                let q = if s == steps { to } else { DVec3::new(c.x + r * a.cos(), c.y + r * a.sin(), pos.z + (to.z - pos.z) * u) };
                op.add_feed(feed, (q - last).length());
                op.segs.push((last, q));
                last = q;
            }
        }
        pos = to;
    }
    Ok(p)
}

/// Segments in a grid of cubes, to find the ones near a point.
struct Grid<'a> {
    cell: f64,
    segs: &'a [(DVec3, DVec3)],
    cells: HashMap<(i64, i64, i64), Vec<u32>>,
}

impl<'a> Grid<'a> {
    fn new(segs: &'a [(DVec3, DVec3)], cell: f64) -> Grid<'a> {
        let mut cells: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();
        for (i, (a, b)) in segs.iter().enumerate() {
            let n = (((*b - *a).length() / (cell * 0.5)).ceil() as usize).max(1);
            for s in 0..=n {
                let q = a.lerp(*b, s as f64 / n as f64);
                let key = ((q.x / cell).floor() as i64, (q.y / cell).floor() as i64, (q.z / cell).floor() as i64);
                let v = cells.entry(key).or_default();
                if v.last() != Some(&(i as u32)) {
                    v.push(i as u32);
                }
            }
        }
        Grid { cell, segs, cells }
    }

    /// How far `dist` looks: 3/4 of a cell. A segment that close to `p` has a sample (every half cell) within a
    /// cell of `p`, so it is listed in one of the 27 cells round `p`'s.
    fn reach(&self) -> f64 {
        self.cell * 0.75
    }

    /// The distance from `p` to the nearest segment, if one is within `reach`.
    fn dist(&self, p: DVec3) -> Option<f64> {
        let k = ((p.x / self.cell).floor() as i64, (p.y / self.cell).floor() as i64, (p.z / self.cell).floor() as i64);
        let mut best: Option<f64> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let Some(v) = self.cells.get(&(k.0 + dx, k.1 + dy, k.2 + dz)) else { continue };
                    for &i in v {
                        let (a, b) = self.segs[i as usize];
                        let ab = b - a;
                        let t = if ab.length_squared() > 0.0 { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
                        let d = (a + ab * t - p).length();
                        if best.is_none_or(|b| d < b) {
                            best = Some(d);
                        }
                    }
                }
            }
        }
        best.filter(|d| *d <= self.reach())
    }
}

/// How far `a`'s cutting path strays from `b`'s: the largest distance (capped at `cap`, which means "at least"),
/// where, and the share of `a`'s length within `tol` of `b`.
fn stray(a: &[(DVec3, DVec3)], b: &Grid, tol: f64, cap: f64) -> (f64, Option<DVec3>, f64) {
    let step = tol.clamp(0.01, 0.05);
    let (mut worst, mut at, mut total, mut near) = (0.0f64, None, 0.0f64, 0.0f64);
    for (p, q) in a {
        let len = (*q - *p).length();
        let n = ((len / step).ceil() as usize).max(1);
        for s in 0..=n {
            let x = p.lerp(*q, s as f64 / n as f64);
            let d = b.dist(x).unwrap_or(cap);
            if d > worst {
                worst = d;
                at = Some(x);
            }
        }
        // the segment's share within tol, judged at its middle and ends
        let mid = b.dist(p.lerp(*q, 0.5)).unwrap_or(cap).max(b.dist(*p).unwrap_or(cap)).max(b.dist(*q).unwrap_or(cap));
        total += len;
        if mid <= tol {
            near += len;
        }
    }
    (worst.min(cap), at, if total > 0.0 { near / total } else { 1.0 })
}

fn r3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// One operation of each program side by side.
fn compare_op(a: Option<&Op>, b: Option<&Op>, tol: f64, renamed: bool) -> Value {
    let any = a.or(b).unwrap();
    let pair = |f: &dyn Fn(&Op) -> Value| json!([a.map(f).unwrap_or(Value::Null), b.map(f).unwrap_or(Value::Null)]);
    let mut v = json!({
        "station": any.station,
        "operation": any.name,
        "kind": any.kind,
        "tool": pair(&|o| json!(o.tool)),
        "rpm": pair(&|o| json!(o.rpm)),
        "moves": pair(&|o| json!(o.moves)),
        "feed_mm": pair(&|o| json!(o.feed_mm().round())),
        "feed_min": pair(&|o| json!((o.feed_min() * 100.0).round() / 100.0)),
        "rapid_mm": pair(&|o| json!(o.rapid_mm.round())),
        "retracts": pair(&|o| json!(o.retracts)),
    });
    if renamed {
        v["operation"] = json!([a.map(|o| o.name.clone()), b.map(|o| o.name.clone())]);
        v["renamed"] = json!(true);
    }
    let (Some(a), Some(b)) = (a, b) else {
        v["verdict"] = json!(if a.is_some() { "only in a" } else { "only in b" });
        return v;
    };
    // feeds that differ by more than 1 mm/min or 2% of the length at them
    let mut fs: Vec<f64> = a.feeds.iter().chain(&b.feeds).map(|f| f.0).collect();
    fs.sort_by(|x, y| x.total_cmp(y));
    fs.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
    let at = |o: &Op, f: f64| o.feeds.iter().find(|x| (x.0 - f).abs() < 1e-9).map(|x| x.1).unwrap_or(0.0);
    let feeds_differ: Vec<Value> = fs.iter().filter(|f| (at(a, **f) - at(b, **f)).abs() > 1.0 + 0.02 * at(a, **f).max(at(b, **f))).map(|f| json!({"feed": f, "mm": [at(a, *f).round(), at(b, *f).round()]})).collect();
    let cap = (tol * 20.0).max(0.5);
    let same_moves = a.segs.len() == b.segs.len() && a.segs.iter().zip(&b.segs).all(|(x, y)| (x.0 - y.0).length() <= tol && (x.1 - y.1).length() <= tol);
    let ((ab, ab_at, ab_near), (ba, ba_at, ba_near)) = if same_moves {
        // move for move within tol: nothing strays further
        let d = a.segs.iter().zip(&b.segs).map(|(x, y)| (x.0 - y.0).length().max((x.1 - y.1).length())).fold(0.0, f64::max);
        ((d, None, 1.0), (d, None, 1.0))
    } else {
        let (ga, gb) = (Grid::new(&b.segs, cap / 0.75), Grid::new(&a.segs, cap / 0.75));
        (stray(&a.segs, &ga, tol, cap), stray(&b.segs, &gb, tol, cap))
    };
    let worst = ab.max(ba);
    let verdict = if a.tool != b.tool {
        "different tool"
    } else if same_moves && feeds_differ.is_empty() && a.moves == b.moves {
        "identical"
    } else if worst <= tol {
        if feeds_differ.is_empty() {
            "same path, moves differ"
        } else {
            "same path, feeds differ"
        }
    } else {
        "path differs"
    };
    let o = |p: Option<DVec3>| p.map(|p| json!([r3(p.x), r3(p.y), r3(if p.z >= TOP { 999.0 } else { p.z })])).unwrap_or(Value::Null);
    v["verdict"] = json!(verdict);
    v["deviation"] = json!({
        "a_from_b": r3(ab), "a_from_b_at": o(ab_at), "a_within_tol": (ab_near * 1000.0).round() / 10.0,
        "b_from_a": r3(ba), "b_from_a_at": o(ba_at), "b_within_tol": (ba_near * 1000.0).round() / 10.0,
        "capped_at": r3(cap),
    });
    if !feeds_differ.is_empty() {
        v["feeds_differ"] = json!(feeds_differ);
    }
    v
}

/// The edits that turn sequence `a` into `b` (longest common subsequence), as (index in a, index in b) with
/// None for a line only one side has.
fn diff<T: PartialEq>(a: &[T], b: &[T]) -> Vec<(Option<usize>, Option<usize>)> {
    let (n, m) = (a.len(), b.len());
    let mut l = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            l[i][j] = if a[i] == b[j] { l[i + 1][j + 1] + 1 } else { l[i + 1][j].max(l[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, vec![]);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if j < m && (i == n || l[i][j + 1] >= l[i + 1][j]) {
            out.push((None, Some(j)));
            j += 1;
        } else {
            out.push((Some(i), None));
            i += 1;
        }
    }
    out
}

/// Compare program `a` with program `b`; `tol` (mm) is how far a cutting path may stray and still count as the same.
pub fn compare(a: &Program, b: &Program, tol: f64) -> Value {
    // pair by station and name, then the rest by station, tool and order
    let mut pairs: Vec<(Option<usize>, Option<usize>, bool)> = vec![];
    let mut used_b = vec![false; b.ops.len()];
    let mut left_a = vec![];
    for (i, o) in a.ops.iter().enumerate() {
        match b.ops.iter().enumerate().position(|(j, x)| !used_b[j] && x.station == o.station && x.name == o.name) {
            Some(j) => {
                used_b[j] = true;
                pairs.push((Some(i), Some(j), false));
            }
            None => left_a.push(i),
        }
    }
    for i in left_a {
        let o = &a.ops[i];
        match b.ops.iter().enumerate().position(|(j, x)| !used_b[j] && x.station == o.station && x.tool == o.tool && x.kind == o.kind) {
            Some(j) => {
                used_b[j] = true;
                pairs.push((Some(i), Some(j), true));
            }
            None => pairs.push((Some(i), None, false)),
        }
    }
    for (j, u) in used_b.iter().enumerate() {
        if !u {
            pairs.push((None, Some(j), false));
        }
    }
    // in a's order, b's leftovers after their station
    pairs.sort_by_key(|p| (p.0.unwrap_or(usize::MAX), p.1.unwrap_or(0)));
    let ops: Vec<Value> = pairs.iter().map(|(i, j, r)| compare_op(i.map(|i| &a.ops[i]), j.map(|j| &b.ops[j]), tol, *r)).collect();
    let count = |v: &str| ops.iter().filter(|o| o["verdict"] == v).count();
    // control lines: compared as text with their station (which operation they sit in is shown, not compared:
    // a renamed operation shouldn't make every line differ)
    let key = |e: &Event| format!("{} {}", e.station, e.text);
    let (ka, kb): (Vec<String>, Vec<String>) = (a.events.iter().map(key).collect(), b.events.iter().map(key).collect());
    let ev = |e: &Event| json!({"line": e.line, "station": e.station, "operation": e.op, "text": e.text});
    let events: Vec<Value> = diff(&ka, &kb)
        .into_iter()
        .map(|(i, j)| match (i, j) {
            (Some(i), _) => json!({"only_in": "a", "event": ev(&a.events[i])}),
            (_, Some(j)) => json!({"only_in": "b", "event": ev(&b.events[j])}),
            _ => Value::Null,
        })
        .collect();
    let header: Vec<Value> = diff(&a.header, &b.header)
        .into_iter()
        .map(|(i, j)| match (i, j) {
            (Some(i), _) => json!({"only_in": "a", "line": a.header[i]}),
            (_, Some(j)) => json!({"only_in": "b", "line": b.header[j]}),
            _ => Value::Null,
        })
        .collect();
    let stations = |p: &Program| {
        let mut s: Vec<String> = p.ops.iter().map(|o| o.station.clone()).collect();
        s.dedup();
        s.sort();
        s.dedup();
        s
    };
    let minutes = |p: &Program| (p.ops.iter().map(|o| o.feed_min()).sum::<f64>() * 100.0).round() / 100.0;
    let same = ops.iter().all(|o| o["verdict"] == "identical") && events.is_empty();
    json!({
        "same": same,
        "tolerance": tol,
        "summary": {
            "operations": [a.ops.len(), b.ops.len()],
            "identical": count("identical"),
            "same_path": count("same path, moves differ") + count("same path, feeds differ"),
            "path_differs": count("path differs"),
            "different_tool": count("different tool"),
            "only_in_a": count("only in a"),
            "only_in_b": count("only in b"),
            "renamed": ops.iter().filter(|o| o["renamed"] == true).count(),
            "control_lines_differ": events.len(),
            "header_lines_differ": header.len(),
            "stations": [stations(a), stations(b)],
            "feed_min": [minutes(a), minutes(b)],
        },
        "operations": ops,
        "control_lines": events,
        "header": header,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "(Test - made by rCAD)\n(SIM OK: 0 problems, hash 1)\n(T24 flat)\nG21 G90\nG54\n(RCNC Z_REFERENCE Vise)\n\n(Rough - Adaptive)\nT24 M6\nS9000 M3\nG4 P3\nM8\nG0 X0 Y0 Z5\nG1 Z-1 F300\nX10 F3000\nG2 X10 Y10 I0 J5\nG53 G0 Z0\nG55\n(Finish - Contour)\nG0 X0 Y0 Z2\nG1 Z-2 F300\nX20 F2000\nM30\n";

    #[test]
    fn same_program_is_identical() {
        let a = read(A).unwrap();
        assert_eq!(a.ops.len(), 2);
        assert_eq!(a.ops[0].station, "G54");
        assert_eq!(a.ops[1].station, "G55");
        assert_eq!(a.ops[0].tool, Some(24));
        assert_eq!(a.ops[0].retracts, 1);
        // the 6 plunge from Z5, 10 along X and half a circle of R5
        assert!((a.ops[0].feed_mm() - (16.0 + 5.0 * std::f64::consts::PI)).abs() < 0.01, "{}", a.ops[0].feed_mm());
        assert_eq!(a.header, vec!["T24 flat".to_string()]);
        let r = compare(&a, &a, 0.01);
        assert_eq!(r["same"], true, "{r}");
        assert_eq!(r["summary"]["identical"], 2);
    }

    #[test]
    fn finds_moved_pass_missing_pause_and_feed() {
        let a = read(A).unwrap();
        // the finish 0.1 over, a dwell gone, a rough feed changed
        let t = A.replace("X20 F2000", "Y0.1\nX20 Y0.1 F2000").replace("G4 P3\n", "").replace("X10 F3000", "X10 F2500");
        let b = read(&t).unwrap();
        let r = compare(&a, &b, 0.01);
        assert_eq!(r["same"], false);
        let ops = r["operations"].as_array().unwrap();
        assert_eq!(ops[0]["verdict"], "same path, feeds differ", "{}", ops[0]);
        assert_eq!(ops[1]["verdict"], "path differs", "{}", ops[1]);
        assert!((ops[1]["deviation"]["a_from_b"].as_f64().unwrap() - 0.1).abs() < 0.01, "{}", ops[1]);
        let ev = r["control_lines"].as_array().unwrap();
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0]["only_in"], "a");
        assert_eq!(ev[0]["event"]["text"], "G4 P3");
        assert_eq!(ev[0]["event"]["operation"], "Rough");
    }

    #[test]
    fn pairs_renamed_operations_and_reports_rcnc_lines() {
        let a = read(A).unwrap();
        let t = A.replace("(Finish - Contour)", "(Finish the walls - Contour)").replace("(RCNC Z_REFERENCE Vise)", "(RCNC Z_REFERENCE Vise)\n(RCNC REQUIRE_GUARD: armed)");
        let b = read(&t).unwrap();
        let r = compare(&a, &b, 0.01);
        let ops = r["operations"].as_array().unwrap();
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[1]["renamed"], true);
        assert_eq!(ops[1]["verdict"], "identical");
        let ev = r["control_lines"].as_array().unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0]["only_in"], "b");
        assert!(ev[0]["event"]["text"].as_str().unwrap().starts_with("RCNC REQUIRE_GUARD"));
    }

    #[test]
    fn operation_only_in_one() {
        let a = read(A).unwrap();
        let t = A.replace("M30\n", "(Chamfer - Chamfer)\nT1 M6\nG0 X0 Y0 Z1\nG1 Z-0.5 F200\nX5\nM30\n");
        let b = read(&t).unwrap();
        let r = compare(&a, &b, 0.01);
        assert_eq!(r["summary"]["only_in_b"], 1, "{r}");
    }
}
