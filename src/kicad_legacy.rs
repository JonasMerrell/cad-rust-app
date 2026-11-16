//! KiCad 5's legacy symbol libraries (`.lib`, "EESchema-LIBRARY Version 2.x"), read into the
//! same s-expression tree a KiCad 6+ `.kicad_sym` library parses to, so `pcb_kicad::symbols` and
//! the library index treat both alike. Digi-Key's KiCad library is in this format.
//!
//! The format, as KiCad's legacy reader takes it: units are mils, Y is up, and each symbol is
//!
//! ```text
//! DEF name ref 0 text_offset Y/N(pin numbers) Y/N(pin names) units L/F P/N(power)
//! F0 "ref" x y size …            F1 "value" …   F2 "footprint" …   F3 "datasheet" …
//! F4 "text" x y size … "Field name"
//! $FPLIST / filters / $ENDFPLIST
//! DRAW
//! S x1 y1 x2 y2 unit convert width fill          rectangle
//! C x y r unit convert width fill                circle
//! P n unit convert width x1 y1 … xn yn fill      polyline
//! A x y r a1 a2 unit convert width fill sx sy ex ey    arc (angles in tenths of a degree)
//! T angle x y size hidden unit convert text …    text (left out)
//! X name number x y length U/D/L/R size size unit convert type [shape]
//! ENDDRAW
//! ENDDEF
//! ```
//!
//! A `.dcm` file beside the library holds descriptions and keywords (`$CMP`, `D`, `K`, `F`).

use std::collections::HashMap;

use crate::pcb_kicad::Sx;

const MIL: f64 = 0.0254;

fn atom(s: &str) -> Sx {
    Sx::Atom(s.to_string())
}

fn list(items: Vec<Sx>) -> Sx {
    Sx::List(items)
}

fn head(h: &str, rest: Vec<Sx>) -> Sx {
    let mut v = vec![atom(h)];
    v.extend(rest);
    list(v)
}

fn num(x: f64) -> Sx {
    // mils to mm, rounded so 100 mil grids stay exact
    atom(&format!("{}", ((x * MIL) * 1e6).round() / 1e6))
}

fn xy(h: &str, x: f64, y: f64) -> Sx {
    head(h, vec![num(x), num(y)])
}

fn prop(k: &str, v: &str) -> Sx {
    head("property", vec![atom(k), atom(v)])
}

fn fill(f: &str) -> Sx {
    let t = match f {
        "F" => "outline",
        "f" => "background",
        _ => "none",
    };
    head("fill", vec![head("type", vec![atom(t)])])
}

/// Split a line into words, keeping "quoted strings" whole (without the quotes).
fn words(line: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Descriptions and keywords from a `.dcm` file, by symbol name.
pub fn docs(text: &str) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    let mut cur: Option<(String, String, String)> = None;
    for line in text.lines() {
        let line = line.trim_end();
        if let Some(n) = line.strip_prefix("$CMP ") {
            cur = Some((n.trim().to_string(), String::new(), String::new()));
        } else if let Some(d) = line.strip_prefix("D ") {
            if let Some(c) = &mut cur {
                c.1 = d.trim().to_string();
            }
        } else if let Some(k) = line.strip_prefix("K ") {
            if let Some(c) = &mut cur {
                c.2 = k.trim().to_string();
            }
        } else if line == "$ENDCMP" {
            if let Some((n, d, k)) = cur.take() {
                out.insert(n, (d, k));
            }
        }
    }
    out
}

fn pin_type(t: &str) -> &'static str {
    match t {
        "I" => "input",
        "O" => "output",
        "B" => "bidirectional",
        "T" => "tri_state",
        "P" => "passive",
        "W" => "power_in",
        "w" => "power_out",
        "C" => "open_collector",
        "E" => "open_emitter",
        "N" => "no_connect",
        _ => "unspecified",
    }
}

/// A legacy library as a `kicad_symbol_lib` tree. `docs` are the `.dcm` file's entries.
pub fn library(text: &str, docs: &HashMap<String, (String, String)>) -> Result<Sx, String> {
    if !text.trim_start().starts_with("EESchema-LIBRARY") {
        return Err("not a KiCad 5 symbol library".into());
    }
    let mut syms = vec![atom("kicad_symbol_lib")];
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let w = words(line);
        if w.first().map(String::as_str) != Some("DEF") || w.len() < 2 {
            continue;
        }
        // "~NAME": the value field is hidden; the name is NAME
        let name = w[1].trim_start_matches('~').to_string();
        let reference = w.get(2).cloned().unwrap_or_else(|| "U".into());
        let show_numbers = w.get(5).map(String::as_str) != Some("N");
        let show_names = w.get(6).map(String::as_str) != Some("N");
        let power = w.get(9).map(String::as_str) == Some("P");
        let mut props: Vec<(String, String)> = vec![];
        let mut filters: Vec<String> = vec![];
        // per (unit, convert): graphics and pins
        let mut units: Vec<((u32, u32), Vec<Sx>)> = vec![];
        let mut add = |u: u32, c: u32, x: Sx, units: &mut Vec<((u32, u32), Vec<Sx>)>| {
            let c = c.max(1);
            match units.iter_mut().find(|e| e.0 == (u, c)) {
                Some(e) => e.1.push(x),
                None => units.push(((u, c), vec![x])),
            }
        };
        let mut in_fp = false;
        for line in lines.by_ref() {
            let t = line.trim();
            if t == "ENDDEF" {
                break;
            }
            if t == "$FPLIST" {
                in_fp = true;
                continue;
            }
            if t == "$ENDFPLIST" {
                in_fp = false;
                continue;
            }
            if in_fp {
                filters.push(t.to_string());
                continue;
            }
            let w = words(t);
            let Some(k) = w.first().map(String::as_str) else { continue };
            let f = |i: usize| w.get(i).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
            let u = |i: usize| w.get(i).and_then(|x| x.parse::<u32>().ok()).unwrap_or(0);
            match k {
                "F0" => props.push(("Reference".into(), w.get(1).cloned().unwrap_or_default())),
                "F1" => props.push(("Value".into(), w.get(1).cloned().unwrap_or_default())),
                "F2" => props.push(("Footprint".into(), w.get(1).cloned().unwrap_or_default())),
                "F3" => props.push(("Datasheet".into(), w.get(1).cloned().unwrap_or_default())),
                _ if k.starts_with('F') && k[1..].parse::<u32>().is_ok() => {
                    // the field's name is its last word, after the 9 position/style words
                    if let (Some(v), Some(n)) = (w.get(1), w.last().filter(|_| w.len() >= 10)) {
                        props.push((n.clone(), v.clone()));
                    }
                }
                "S" if w.len() >= 9 => {
                    let x = head("rectangle", vec![xy("start", f(1), f(2)), xy("end", f(3), f(4)), fill(&w[8])]);
                    add(u(5), u(6), x, &mut units);
                }
                "C" if w.len() >= 8 => {
                    let x = head("circle", vec![xy("center", f(1), f(2)), head("radius", vec![num(f(3))]), fill(&w[7])]);
                    add(u(4), u(5), x, &mut units);
                }
                "P" if w.len() >= 5 => {
                    let n = u(1) as usize;
                    let pts: Vec<Sx> = (0..n).map(|i| xy("xy", f(5 + 2 * i), f(6 + 2 * i))).collect();
                    let fl = w.get(5 + 2 * n).map(String::as_str).unwrap_or("N");
                    let x = head("polyline", vec![head("pts", pts), fill(fl)]);
                    add(u(2), u(3), x, &mut units);
                }
                "A" if w.len() >= 10 => {
                    let (cx, cy, r) = (f(1), f(2), f(3));
                    let (a1, a2) = (f(4) / 10.0, f(5) / 10.0);
                    // KiCad 5 arcs are under 180°: the short way from a1 to a2
                    let mut d = (a2 - a1) % 360.0;
                    if d > 180.0 {
                        d -= 360.0;
                    } else if d <= -180.0 {
                        d += 360.0;
                    }
                    let at = |a: f64| (cx + r * a.to_radians().cos(), cy + r * a.to_radians().sin());
                    let (s, m, e) = (at(a1), at(a1 + d / 2.0), at(a1 + d));
                    let x = head("arc", vec![xy("start", s.0, s.1), xy("mid", m.0, m.1), xy("end", e.0, e.1), fill(&w[9])]);
                    add(u(6), u(7), x, &mut units);
                }
                "X" if w.len() >= 12 => {
                    let ang = match w[6].as_str() {
                        "U" => 90.0,
                        "L" => 180.0,
                        "D" => 270.0,
                        _ => 0.0,
                    };
                    let pname = if w[1] == "~" { "~".to_string() } else { w[1].clone() };
                    let x = head(
                        "pin",
                        vec![
                            atom(pin_type(&w[11])),
                            atom("line"),
                            head("at", vec![num(f(3)), num(f(4)), atom(&format!("{ang}"))]),
                            head("length", vec![num(f(5))]),
                            head("name", vec![atom(&pname)]),
                            head("number", vec![atom(&w[2])]),
                        ],
                    );
                    add(u(9), u(10), x, &mut units);
                }
                _ => {}
            }
        }
        let mut body = vec![atom(&name)];
        if power {
            body.push(head("power", vec![]));
        }
        if !show_numbers {
            body.push(head("pin_numbers", vec![atom("hide")]));
        }
        if !show_names {
            body.push(head("pin_names", vec![atom("hide")]));
        }
        if !props.iter().any(|p| p.0 == "Reference") {
            props.insert(0, ("Reference".into(), reference));
        }
        if let Some((d, k)) = docs.get(&name) {
            if !props.iter().any(|p| p.0 == "Description") && !d.is_empty() {
                props.push(("Description".into(), d.clone()));
            }
            if !k.is_empty() {
                props.push(("ki_keywords".into(), k.clone()));
            }
        }
        if !filters.is_empty() {
            props.push(("ki_fp_filters".into(), filters.join(" ")));
        }
        for (k, v) in &props {
            body.push(prop(k, v));
        }
        units.sort_by_key(|u| u.0);
        for ((unit, conv), items) in units {
            let mut v = vec![atom(&format!("{name}_{unit}_{conv}"))];
            v.extend(items);
            body.push(head("symbol", v));
        }
        syms.push(head("symbol", body));
    }
    Ok(list(syms))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIB: &str = r#"EESchema-LIBRARY Version 2.3
#encoding utf-8
DEF COM-12999 D 0 40 Y Y 1 F N
F0 "D" -300 300 60 H V C C N N
F1 "COM-12999" 300 -300 60 H V C C N N
F2 "digikey-footprints:LED_RGB_WP154A4SUREQBFZGC" 200 200 60 H I L C N N
F3 "https://cdn.sparkfun.com/datasheets/Components/LED/COM-12877.pdf" 200 300 60 H I L C N N
F4 "1568-1214-ND" 200 400 60 H I L CNN "Digi-Key_PN"
F5 "COM-12999" 200 500 60 H I L CNN "MPN"
F10 "ADDRESS LED DISC SER RGB 5MM 5PC" 200 1000 60 H I L CNN "Description"
F11 "SparkFun Electronics" 200 1100 60 H I L CNN "Manufacturer"
DRAW
S -300 200 200 -200 0 1 0 f
A 200 50 71 450 -450 0 1 0 N 250 100 250 0
X DIN 1 -500 0 200 R 50 50 1 1 I
X VDD 2 -100 400 200 D 50 50 1 1 W
X GND 3 -100 -400 200 U 50 50 1 1 W
X DOUT 4 400 0 200 L 50 50 1 1 O
ENDDRAW
ENDDEF
"#;

    #[test]
    fn reads_a_digikey_symbol() {
        let sx = library(LIB, &HashMap::new()).unwrap();
        let mut notes = vec![];
        let syms = crate::pcb_kicad::symbols(&sx, "dk_Test", &mut notes).unwrap();
        assert_eq!(syms.len(), 1, "{notes:?}");
        let s = &syms[0];
        assert_eq!(s.key, "dk_Test:COM-12999");
        assert_eq!(s.prefix, "D");
        assert_eq!(s.footprint, "digikey-footprints:LED_RGB_WP154A4SUREQBFZGC");
        assert_eq!(s.description, "ADDRESS LED DISC SER RGB 5MM 5PC");
        assert_eq!(s.pins.len(), 4);
        // DIN sits 500 mil left of centre, pointing right into the body
        let din = s.pins.iter().find(|p| p.name == "DIN").unwrap();
        assert!((din.at[0] + 12.7).abs() < 1e-9 && din.at[1].abs() < 1e-9 && din.dir == [1.0, 0.0], "{din:?}");
        assert!((din.len - 5.08).abs() < 1e-9);
        assert_eq!(din.kind, "input");
        let vdd = s.pins.iter().find(|p| p.name == "VDD").unwrap();
        assert_eq!(vdd.dir, [0.0, -1.0]);
        assert_eq!(vdd.kind, "power_in");
        assert!(s.fields.iter().any(|(k, v)| k == "Digi-Key_PN" && v == "1568-1214-ND"));
        assert!(s.fields.iter().any(|(k, v)| k == "Manufacturer" && v == "SparkFun Electronics"));
        assert!(!s.g.is_empty());
    }

    /// Every symbol of a checkout of Digi-Key's library (RCAD_DIGIKEY_LIB_DIR) reads, with pins
    /// and a Digi-Key number, and every footprint it names is in it.
    #[test]
    #[ignore]
    fn load_digikey_library() {
        let Some(d) = std::env::var_os("RCAD_DIGIKEY_LIB_DIR").map(std::path::PathBuf::from) else { return };
        let (mut n, mut bad, mut no_pn, mut no_fp) = (0, vec![], 0, vec![]);
        let mut files: Vec<_> = std::fs::read_dir(d.join("digikey-symbols")).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "lib")).collect();
        files.sort();
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap();
            let docs = std::fs::read_to_string(f.with_extension("dcm")).map(|t| docs(&t)).unwrap_or_default();
            let sx = library(&text, &docs).unwrap();
            let mut notes = vec![];
            let syms = crate::pcb_kicad::symbols(&sx, "dk", &mut notes).unwrap();
            bad.extend(notes);
            for s in syms {
                n += 1;
                if !s.fields.iter().any(|f| f.0 == "Digi-Key_PN") {
                    no_pn += 1;
                }
                let fp = s.footprint.trim_start_matches("digikey-footprints:");
                if !d.join("digikey-footprints.pretty").join(format!("{fp}.kicad_mod")).exists() {
                    no_fp.push(s.footprint.clone());
                }
            }
        }
        let mut fps = 0;
        for e in std::fs::read_dir(d.join("digikey-footprints.pretty")).unwrap().filter_map(|e| e.ok()) {
            let text = std::fs::read_to_string(e.path()).unwrap();
            let mut notes = vec![];
            if crate::pcb_kicad::footprint(&crate::pcb_kicad::parse(&text).unwrap(), &mut notes).is_ok() {
                fps += 1;
            }
        }
        eprintln!("{n} symbols, {} notes {:?}, {no_pn} without a Digi-Key number, {} footprints missing {:?}, {fps} footprints read", bad.len(), bad.iter().take(5).collect::<Vec<_>>(), no_fp.len(), no_fp.iter().take(5).collect::<Vec<_>>());
        assert!(n > 1000 && no_pn == 0 && fps > 450);
    }

    #[test]
    fn words_keep_quoted_text() {
        assert_eq!(words(r#"F10 "A B  C" 1 2 "" "Description""#), vec!["F10", "A B  C", "1", "2", "", "Description"]);
    }
}
