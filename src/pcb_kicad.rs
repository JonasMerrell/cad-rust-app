//! KiCad library import: footprints (`.kicad_mod`, `.pretty` folders) and schematic symbols
//! (`.kicad_sym`), from files, folders or ZIP downloads (SnapEDA, Ultra Librarian and
//! component search engines all offer KiCad). Imported parts go into the PCB's own library
//! (`Pcb::footprints`, `Pcb::symbols`), so the document carries them.

use std::path::Path;

use crate::pcb::Pcb;
use crate::pcb_lib::{self, Footprint, LibPin, LibSymbol, Pad, PadShape, Solid, G, P};

// ---------------------------------------------------------------- s-expressions

#[derive(Clone, Debug, PartialEq)]
pub enum Sx {
    List(Vec<Sx>),
    Atom(String),
}

impl Sx {
    fn items(&self) -> &[Sx] {
        match self {
            Sx::List(v) => v,
            Sx::Atom(_) => &[],
        }
    }
    fn atom(&self) -> Option<&str> {
        match self {
            Sx::Atom(s) => Some(s),
            Sx::List(_) => None,
        }
    }
    /// The list's first atom (`footprint`, `pad`, …).
    pub fn head(&self) -> Option<&str> {
        self.items().first().and_then(|x| x.atom())
    }
    /// Child lists named `name`.
    fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Sx> + 'a {
        self.items().iter().filter(move |x| x.head() == Some(name))
    }
    fn get<'a>(&'a self, name: &'a str) -> Option<&'a Sx> {
        self.all(name).next()
    }
    fn str(&self, i: usize) -> Option<&str> {
        self.items().get(i).and_then(|x| x.atom())
    }
    fn num(&self, i: usize) -> Option<f64> {
        self.str(i).and_then(|s| s.parse().ok())
    }
    /// `(name x y)` as a point.
    fn xy(&self, name: &str) -> Option<P> {
        let l = self.get(name)?;
        Some([l.num(1)?, l.num(2)?])
    }
    /// Whether a bare atom (`hide`) or `(name yes)` is present.
    fn flag(&self, name: &str) -> bool {
        self.items().iter().any(|x| x.atom() == Some(name)) || self.get(name).is_some_and(|l| l.str(1).is_none_or(|v| v == "yes"))
    }
}

pub fn parse(text: &str) -> Result<Sx, String> {
    let b = text.as_bytes();
    let mut i = 0;
    let mut stack: Vec<Vec<Sx>> = vec![];
    let mut top: Option<Sx> = None;
    while i < b.len() {
        let c = b[i];
        match c {
            b'(' => {
                stack.push(vec![]);
                i += 1;
            }
            b')' => {
                let l = stack.pop().ok_or("unbalanced )")?;
                match stack.last_mut() {
                    Some(parent) => parent.push(Sx::List(l)),
                    None => {
                        top = Some(Sx::List(l));
                        break;
                    }
                }
                i += 1;
            }
            b'"' => {
                let mut s = Vec::new();
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                        s.push(match b[i] {
                            b'n' => b'\n',
                            b't' => b'\t',
                            x => x,
                        });
                    } else {
                        s.push(b[i]);
                    }
                    i += 1;
                }
                i += 1;
                stack.last_mut().ok_or("text outside a list")?.push(Sx::Atom(String::from_utf8_lossy(&s).into_owned()));
            }
            c if c.is_ascii_whitespace() => i += 1,
            _ => {
                let st = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'(' && b[i] != b')' {
                    i += 1;
                }
                let s = String::from_utf8_lossy(&b[st..i]).into_owned();
                stack.last_mut().ok_or("text outside a list")?.push(Sx::Atom(s));
            }
        }
    }
    top.ok_or_else(|| "not an s-expression (unbalanced parentheses)".into())
}

// ---------------------------------------------------------------- geometry helpers

/// Arc through three points as a polyline (the start, a point on it, the end).
fn arc3(a: P, m: P, b: P) -> Vec<P> {
    let d = 2.0 * (a[0] * (m[1] - b[1]) + m[0] * (b[1] - a[1]) + b[0] * (a[1] - m[1]));
    if d.abs() < 1e-12 {
        return vec![a, b];
    }
    let sq = |p: P| p[0] * p[0] + p[1] * p[1];
    let c = [
        (sq(a) * (m[1] - b[1]) + sq(m) * (b[1] - a[1]) + sq(b) * (a[1] - m[1])) / d,
        (sq(a) * (b[0] - m[0]) + sq(m) * (a[0] - b[0]) + sq(b) * (m[0] - a[0])) / d,
    ];
    let r = ((a[0] - c[0]).powi(2) + (a[1] - c[1]).powi(2)).sqrt();
    let ang = |p: P| (p[1] - c[1]).atan2(p[0] - c[0]);
    let (a0, am, mut a1) = (ang(a), ang(m), ang(b));
    // sweep a0 → a1 through am
    let tau = std::f64::consts::TAU;
    let norm = |x: f64| x.rem_euclid(tau);
    let ccw = norm(am - a0) < norm(a1 - a0);
    if ccw {
        a1 = a0 + norm(a1 - a0);
    } else {
        a1 = a0 - norm(a0 - a1);
    }
    let n = ((a1 - a0).abs() / 0.2).ceil().max(4.0) as usize;
    (0..=n).map(|i| {
        let t = a0 + (a1 - a0) * i as f64 / n as f64;
        [c[0] + r * t.cos(), c[1] + r * t.sin()]
    }).collect()
}

fn grow(bb: &mut Option<(P, P)>, p: P) {
    match bb {
        Some((lo, hi)) => {
            lo[0] = lo[0].min(p[0]);
            lo[1] = lo[1].min(p[1]);
            hi[0] = hi[0].max(p[0]);
            hi[1] = hi[1].max(p[1]);
        }
        None => *bb = Some((p, p)),
    }
}

fn g_points(g: &G) -> Vec<P> {
    match g {
        G::Line(a, b) | G::Rect(a, b) => vec![*a, *b],
        G::Circle(c, r) => vec![[c[0] - r, c[1] - r], [c[0] + r, c[1] + r]],
        G::Poly(v, _) => v.clone(),
    }
}

/// Graphic items of a KiCad footprint or symbol: lines, rectangles, circles, arcs, polygons.
/// `fy` is -1 for footprints (KiCad's y points down there) and 1 for symbols.
fn graphic(x: &Sx, fy: f64) -> Option<G> {
    let f = |p: P| [p[0], p[1] * fy];
    let filled = x.get("fill").and_then(|l| l.get("type").and_then(|t| t.str(1)).or_else(|| l.str(1))).is_some_and(|t| t == "outline" || t == "solid" || t == "yes");
    let head = x.head()?;
    let kind = head.trim_start_matches("fp_").trim_start_matches("gr_");
    Some(match kind {
        "line" => G::Line(f(x.xy("start")?), f(x.xy("end")?)),
        "rect" | "rectangle" => {
            let (a, b) = (f(x.xy("start")?), f(x.xy("end")?));
            if filled {
                G::Poly(vec![a, [b[0], a[1]], b, [a[0], b[1]]], true)
            } else {
                G::Rect([a[0].min(b[0]), a[1].min(b[1])], [a[0].max(b[0]), a[1].max(b[1])])
            }
        }
        "circle" => {
            let c = f(x.xy("center")?);
            let r = match x.get("radius").and_then(|r| r.num(1)) {
                Some(r) => r,
                None => {
                    let e = f(x.xy("end")?);
                    ((e[0] - c[0]).powi(2) + (e[1] - c[1]).powi(2)).sqrt()
                }
            };
            if filled {
                G::Poly((0..=24).map(|i| {
                    let a = std::f64::consts::TAU * i as f64 / 24.0;
                    [c[0] + r * a.cos(), c[1] + r * a.sin()]
                }).collect(), true)
            } else {
                G::Circle(c, r)
            }
        }
        "arc" => {
            if let Some(m) = x.xy("mid") {
                G::Poly(arc3(f(x.xy("start")?), f(m), f(x.xy("end")?)), false)
            } else {
                // KiCad 5: start is the centre, end the arc's start, angle the sweep
                let c = f(x.xy("start")?);
                let s = f(x.xy("end")?);
                let sweep = x.get("angle")?.num(1)?.to_radians() * fy;
                let r = ((s[0] - c[0]).powi(2) + (s[1] - c[1]).powi(2)).sqrt();
                let a0 = (s[1] - c[1]).atan2(s[0] - c[0]);
                let n = ((sweep.abs() / 0.2).ceil() as usize).max(4);
                G::Poly((0..=n).map(|i| {
                    let a = a0 + sweep * i as f64 / n as f64;
                    [c[0] + r * a.cos(), c[1] + r * a.sin()]
                }).collect(), false)
            }
        }
        "poly" | "polyline" | "bezier" => {
            let pts: Vec<P> = x.get("pts")?.all("xy").filter_map(|q| Some(f([q.num(1)?, q.num(2)?]))).collect();
            if pts.len() < 2 {
                return None;
            }
            G::Poly(pts, filled || (head == "fp_poly" && x.get("fill").is_none()))
        }
        _ => return None,
    })
}

fn layer_of(x: &Sx) -> String {
    x.get("layer").and_then(|l| l.str(1)).unwrap_or("").to_string()
}

// ---------------------------------------------------------------- footprints

/// A KiCad footprint as an rCAD footprint. Notes collect what could not be carried over.
pub fn footprint(root: &Sx, notes: &mut Vec<String>) -> Result<Footprint, String> {
    if !matches!(root.head(), Some("footprint" | "module")) {
        return Err("not a KiCad footprint".into());
    }
    let name = root.str(1).ok_or("footprint without a name")?.to_string();
    let mut f = Footprint { name: name.clone(), pads: vec![], silk: vec![], court: ([0.0; 2], [0.0; 2]), body: vec![], color: [40, 40, 44] };
    let mut court: Option<(P, P)> = None;
    let mut fab: Option<(P, P)> = None;
    let mut skipped_holes = 0;
    let mut odd_angles = 0;
    let mut custom = 0;
    for x in root.items() {
        let Some(h) = x.head() else { continue };
        if h.starts_with("fp_") {
            let layer = layer_of(x);
            let Some(g) = graphic(x, -1.0) else { continue };
            match layer.as_str() {
                "F.SilkS" | "F.Silkscreen" => f.silk.push(g),
                "F.CrtYd" | "F.Courtyard" => g_points(&g).into_iter().for_each(|p| grow(&mut court, p)),
                "F.Fab" => g_points(&g).into_iter().for_each(|p| grow(&mut fab, p)),
                _ => {}
            }
            continue;
        }
        if h != "pad" {
            continue;
        }
        let num = x.str(1).unwrap_or("").to_string();
        let kind = x.str(2).unwrap_or("smd");
        let shape = x.str(3).unwrap_or("rect");
        let at = x.get("at").ok_or_else(|| format!("{name}: pad {num} has no position"))?;
        let pos = [at.num(1).unwrap_or(0.0), -at.num(2).unwrap_or(0.0)];
        let rot = at.num(3).unwrap_or(0.0).rem_euclid(180.0);
        let sz = x.get("size").ok_or_else(|| format!("{name}: pad {num} has no size"))?;
        let mut size = [sz.num(1).unwrap_or(1.0), sz.num(2).unwrap_or(1.0)];
        if (rot - 90.0).abs() < 0.5 {
            size = [size[1], size[0]];
        } else if rot.abs() > 0.5 && shape != "circle" {
            odd_angles += 1;
        }
        let drill = x.get("drill").map(|d| {
            // (drill d) or (drill oval w h): an oval slot keeps its smaller width
            let v: Vec<f64> = d.items().iter().filter_map(|i| i.atom()?.parse().ok()).collect();
            v.iter().cloned().fold(f64::INFINITY, f64::min)
        }).filter(|d| d.is_finite()).unwrap_or(0.0);
        if kind == "np_thru_hole" {
            skipped_holes += 1;
            continue;
        }
        let layers: Vec<&str> = x.get("layers").map(|l| l.items().iter().skip(1).filter_map(|i| i.atom()).collect()).unwrap_or_default();
        if kind != "thru_hole" && !layers.iter().any(|l| *l == "F.Cu" || *l == "*.Cu") {
            // paste-only or bottom-only aperture pads have nothing to place on the top copper
            continue;
        }
        let shape = match shape {
            "circle" => PadShape::Round,
            "oval" => PadShape::Oval,
            "roundrect" => PadShape::RoundRect,
            "rect" | "trapezoid" => PadShape::Rect,
            _ => {
                custom += 1;
                PadShape::Rect
            }
        };
        f.pads.push(Pad { num, at: pos, size, shape, drill: if kind == "thru_hole" { drill } else { 0.0 } });
    }
    if f.pads.is_empty() {
        return Err(format!("{name} has no copper pads"));
    }
    if skipped_holes > 0 {
        notes.push(format!("{name}: {skipped_holes} non-plated hole(s) left out (add them as board holes)"));
    }
    if odd_angles > 0 {
        notes.push(format!("{name}: {odd_angles} pad(s) at an angle other than 90° kept square to the footprint"));
    }
    if custom > 0 {
        notes.push(format!("{name}: {custom} custom-shaped pad(s) imported as rectangles"));
    }
    let mut pads_bb = None;
    for p in &f.pads {
        grow(&mut pads_bb, [p.at[0] - p.size[0] / 2.0, p.at[1] - p.size[1] / 2.0]);
        grow(&mut pads_bb, [p.at[0] + p.size[0] / 2.0, p.at[1] + p.size[1] / 2.0]);
    }
    let pads_bb = pads_bb.unwrap();
    f.court = court.unwrap_or(([pads_bb.0[0] - 0.25, pads_bb.0[1] - 0.25], [pads_bb.1[0] + 0.25, pads_bb.1[1] + 0.25]));
    // a body box over the fab outline (or inside the courtyard), as high as the name says
    let (lo, hi) = fab.unwrap_or(([f.court.0[0] + 0.25, f.court.0[1] + 0.25], [f.court.1[0] - 0.25, f.court.1[1] - 0.25]));
    let tht = f.pads.iter().any(|p| p.drill > 0.0);
    let h = height_from_name(&name).unwrap_or(if tht { 2.5 } else { 1.0 });
    f.body = vec![Solid::Box { lo: [lo[0], lo[1], 0.0], hi: [hi[0], hi[1], h] }];
    Ok(f)
}

/// `…_H3.5mm…` or IPC names (`SOIC127P600X175-8N` → 1.75 mm).
fn height_from_name(n: &str) -> Option<f64> {
    if let Some(i) = n.find("_H") {
        let s: String = n[i + 2..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        if let Ok(v) = s.parse::<f64>() {
            return Some(v);
        }
    }
    None
}

// ---------------------------------------------------------------- symbols

fn symbol_props(s: &Sx) -> Vec<(String, String, Option<P>)> {
    s.all("property").filter_map(|p| Some((p.str(1)?.to_string(), p.str(2).unwrap_or("").to_string(), p.get("at").and_then(|a| Some([a.num(1)?, a.num(2)?]))))).collect()
}

/// The symbols of a `.kicad_sym` library. `lib` names the library (the file stem) for the keys.
pub fn symbols(root: &Sx, lib: &str, notes: &mut Vec<String>) -> Result<Vec<LibSymbol>, String> {
    if root.head() != Some("kicad_symbol_lib") {
        return Err("not a KiCad symbol library".into());
    }
    let all: Vec<&Sx> = root.all("symbol").collect();
    let by_name = |n: &str| all.iter().find(|s| s.str(1) == Some(n)).copied();
    let mut out = vec![];
    for s in &all {
        let name = s.str(1).unwrap_or("").to_string();
        // an alias takes its drawing from the symbol it extends
        let base = match s.get("extends").and_then(|e| e.str(1)) {
            Some(b) => match by_name(b) {
                Some(x) => x,
                None => {
                    notes.push(format!("{name} extends {b}, which is not in the library"));
                    continue;
                }
            },
            None => s,
        };
        let base_name = base.str(1).unwrap_or("").to_string();
        let mut props = symbol_props(base);
        for (k, v, at) in symbol_props(s) {
            match props.iter_mut().find(|p| p.0 == k) {
                Some(p) => *p = (k, v, at.or(p.2)),
                None => props.push((k, v, at)),
            }
        }
        let prop = |k: &str| props.iter().find(|p| p.0 == k).map(|p| p.1.clone()).unwrap_or_default();
        let mut sym = LibSymbol {
            key: format!("{lib}:{name}"),
            description: { let d = prop("Description"); if d.is_empty() { prop("ki_description") } else { d } },
            prefix: prop("Reference").trim_end_matches('?').to_string(),
            value: { let v = prop("Value"); if v.is_empty() { name.clone() } else { v } },
            footprint: prop("Footprint"),
            fp_filters: prop("ki_fp_filters").split_whitespace().filter(|f| !f.contains(':')).map(String::from).collect(),
            pin_numbers: !base.get("pin_numbers").is_some_and(|l| l.flag("hide")),
            pin_names: !base.get("pin_names").is_some_and(|l| l.flag("hide")),
            ..Default::default()
        };
        if sym.prefix.is_empty() {
            sym.prefix = "U".into();
        }
        // units side by side; De Morgan alternates (style 2) left out
        let mut units: Vec<(u32, Vec<&Sx>)> = vec![];
        for u in base.all("symbol") {
            let n = u.str(1).unwrap_or("");
            let rest = n.strip_prefix(&base_name).and_then(|r| r.strip_prefix('_')).unwrap_or("");
            let mut it = rest.split('_');
            let unit: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            let style: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
            if style > 1 {
                continue;
            }
            match units.iter_mut().find(|x| x.0 == unit) {
                Some(x) => x.1.push(u),
                None => units.push((unit, vec![u])),
            }
        }
        units.sort_by_key(|u| u.0);
        let mut x_off = 0.0;
        let mut first = true;
        for (unit, parts) in &units {
            let mut g = vec![];
            let mut pins = vec![];
            for u in parts {
                for item in u.items() {
                    if item.head() == Some("pin") {
                        let Some(at) = item.get("at") else { continue };
                        let ang = at.num(3).unwrap_or(0.0).to_radians();
                        let pname = item.get("name").and_then(|n| n.str(1)).unwrap_or("");
                        pins.push(LibPin {
                            num: item.get("number").and_then(|n| n.str(1)).unwrap_or("").to_string(),
                            name: if pname == "~" { String::new() } else { pname.to_string() },
                            at: [at.num(1).unwrap_or(0.0), at.num(2).unwrap_or(0.0)],
                            dir: [round6(ang.cos()), round6(ang.sin())],
                            len: item.get("length").and_then(|l| l.num(1)).unwrap_or(2.54),
                            kind: item.str(1).unwrap_or("passive").to_string(),
                        });
                    } else if let Some(gr) = graphic(item, 1.0) {
                        g.push(gr);
                    }
                }
            }
            // unit 0 is common to every unit: drawn with the first
            let mut bb = None;
            for gr in &g {
                g_points(gr).into_iter().for_each(|p| grow(&mut bb, p));
            }
            for p in &pins {
                grow(&mut bb, p.at);
            }
            let Some((lo, hi)) = bb else { continue };
            let dx = if *unit == 0 || first { 0.0 } else { x_off - lo[0] };
            if *unit != 0 {
                first = false;
            }
            let sh = |p: P| [p[0] + dx, p[1]];
            for gr in g {
                sym.g.push(match gr {
                    G::Line(a, b) => G::Line(sh(a), sh(b)),
                    G::Rect(a, b) => G::Rect(sh(a), sh(b)),
                    G::Circle(c, r) => G::Circle(sh(c), r),
                    G::Poly(v, f) => G::Poly(v.into_iter().map(sh).collect(), f),
                });
            }
            for mut p in pins {
                p.at = sh(p.at);
                sym.pins.push(p);
            }
            x_off = x_off.max(hi[0] + dx + 7.62);
        }
        if sym.pins.is_empty() {
            notes.push(format!("{name} has no pins and was left out"));
            continue;
        }
        if units.iter().filter(|u| u.0 > 0).count() > 1 {
            notes.push(format!("{name}: {} units drawn side by side as one symbol", units.iter().filter(|u| u.0 > 0).count()));
        }
        let mut bb = None;
        for gr in &sym.g {
            g_points(gr).into_iter().for_each(|p| grow(&mut bb, p));
        }
        for p in &sym.pins {
            grow(&mut bb, p.at);
        }
        let (lo, hi) = bb.unwrap();
        sym.ref_at = [hi[0] + 1.0, (hi[1] + lo[1]) / 2.0 + 0.6];
        sym.value_at = [hi[0] + 1.0, (hi[1] + lo[1]) / 2.0 - 1.8];
        if sym.pins.len() > 4 {
            // on ICs the reference goes above the body and the value below, like KiCad
            sym.ref_at = [lo[0], hi[1] + 1.0];
            sym.value_at = [lo[0], lo[1] - 2.5];
        }
        out.push(sym);
    }
    Ok(out)
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

// ---------------------------------------------------------------- import

#[derive(Default, Debug)]
pub struct Imported {
    pub footprints: Vec<Footprint>,
    pub symbols: Vec<LibSymbol>,
    pub notes: Vec<String>,
}

#[cfg(test)]
fn import_text(name: &str, text: &str, im: &mut Imported) -> Result<(), String> {
    import_files(&[(name.to_string(), text.to_string())], im);
    match im.notes.iter().position(|n| n.starts_with(name)) {
        Some(i) if im.footprints.is_empty() && im.symbols.is_empty() => Err(im.notes.remove(i)),
        _ => Ok(()),
    }
}

fn wanted(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.ends_with(".kicad_mod") || l.ends_with(".kicad_sym")
}

/// The library a symbol file belongs to: KiCad 9+ keeps one symbol per file in a
/// `Name.kicad_symdir` folder, older libraries are one `Name.kicad_sym` file.
fn sym_lib(name: &str) -> (String, bool) {
    let p = Path::new(name);
    let stem = |q: &Path| q.file_stem().and_then(|s| s.to_str()).unwrap_or("kicad").to_string();
    match p.parent().filter(|d| d.extension().is_some_and(|e| e == "kicad_symdir")) {
        Some(d) => (stem(d), true),
        None => (stem(p), false),
    }
}

/// Footprints and symbols from (name, text) pairs. Symbol files of one `.kicad_symdir` form one
/// library, so `extends` finds its base in a sibling file.
fn import_files(files: &[(String, String)], im: &mut Imported) {
    let mut libs: Vec<(String, Vec<Sx>)> = vec![];
    for (name, text) in files {
        let root = match parse(text) {
            Ok(r) => r,
            Err(e) => {
                im.notes.push(format!("{name}: {e}"));
                continue;
            }
        };
        if root.head() == Some("kicad_symbol_lib") {
            let (lib, _) = sym_lib(name);
            let syms: Vec<Sx> = root.all("symbol").cloned().collect();
            match libs.iter_mut().find(|l| l.0 == lib) {
                Some(l) => l.1.extend(syms),
                None => libs.push((lib, syms)),
            }
        } else {
            match footprint(&root, &mut im.notes) {
                Ok(f) => im.footprints.push(f),
                Err(e) => im.notes.push(format!("{name}: {e}")),
            }
        }
    }
    for (lib, syms) in libs {
        let mut items = vec![Sx::Atom("kicad_symbol_lib".into())];
        items.extend(syms);
        match symbols(&Sx::List(items), &lib, &mut im.notes) {
            Ok(v) => im.symbols.extend(v),
            Err(e) => im.notes.push(format!("{lib}: {e}")),
        }
    }
}

/// Read KiCad footprints and symbols from a file, a folder (with its `.pretty` and
/// `.kicad_symdir` subfolders), or a ZIP.
pub fn import_path(path: &Path) -> Result<Imported, String> {
    let mut im = Imported::default();
    let mut files: Vec<(String, String)> = vec![];
    let read = |q: &Path| std::fs::read(q).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|e| format!("{}: {e}", q.display()));
    if path.is_dir() {
        let mut stack = vec![(path.to_path_buf(), 0)];
        while let Some((dir, depth)) = stack.pop() {
            let mut entries: Vec<_> = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            entries.sort();
            for q in entries {
                if q.is_dir() && depth < 3 {
                    stack.push((q, depth + 1));
                } else if q.to_str().is_some_and(wanted) {
                    files.push((q.to_string_lossy().into_owned(), read(&q)?));
                }
            }
        }
    } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
        let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        for (n, data) in crate::grabcad::unzip(&b)? {
            if wanted(&n) {
                files.push((n, String::from_utf8_lossy(&data).into_owned()));
            }
        }
    } else {
        files.push((path.to_string_lossy().into_owned(), read(path)?));
        // one symbol out of a .kicad_symdir: bring in the symbols it extends
        if sym_lib(&path.to_string_lossy()).1 {
            let dir = path.parent().unwrap();
            let mut k = 0;
            while k < files.len() {
                let base = parse(&files[k].1).ok().and_then(|r| r.all("symbol").find_map(|s| s.get("extends").and_then(|e| e.str(1)).map(String::from)));
                if let Some(b) = base {
                    let q = dir.join(format!("{b}.kicad_sym"));
                    if q.exists() && !files.iter().any(|f| Path::new(&f.0) == q) {
                        files.push((q.to_string_lossy().into_owned(), read(&q)?));
                    }
                }
                k += 1;
            }
        }
    }
    import_files(&files, &mut im);
    if im.footprints.is_empty() && im.symbols.is_empty() {
        return Err(if im.notes.is_empty() { format!("no KiCad footprints (.kicad_mod) or symbols (.kicad_sym) in {}", path.display()) } else { im.notes.join("; ") });
    }
    Ok(im)
}

/// A footprint for a symbol whose own footprint is not in the library: one its filters accept
/// with as many pads as it has pins (the built-in symbol's default first), else a generated
/// package of that pin count.
fn fallback_footprint(s: &LibSymbol, n: usize) -> Option<String> {
    let fits = |c: &str| pcb_lib::footprint(c).is_some_and(|f| f.pin_numbers().len() == n);
    let mut names = pcb_lib::footprint_names();
    if let Some(k) = pcb_lib::symbol_kind(&s.prefix) {
        names.insert(0, k.footprint.to_string());
    }
    let mut ok: Vec<&String> = names.iter().filter(|f| s.fp_filters.iter().any(|p| pcb_lib::glob(p, f)) && fits(f)).collect();
    // among chip sizes, 0805 then 0603 (easy to place by hand) before the first listed
    ok.sort_by_key(|f| if f.contains("0805") { 0 } else if f.contains("0603") { 1 } else { 2 });
    if let Some(f) = ok.first() {
        return Some((*f).clone());
    }
    let mut cands = vec![format!("SOIC-{n}"), format!("DIP-{n}"), format!("PinHeader_1x{n:02}")];
    if n % 2 == 0 {
        cands.push(format!("PinHeader_2x{:02}", n / 2));
    }
    cands.into_iter().find(|c| fits(c))
}

/// Put imported parts into a PCB's library (replacing ones of the same name) and register them.
/// A symbol whose footprint was not imported is linked to one its filters accept, or to an
/// IPC-7351 land pattern generated from the KiCad package name, with a note.
pub fn add_to(p: &mut Pcb, mut im: Imported) -> Vec<String> {
    pcb_lib::register_footprints(&im.footprints);
    for s in &mut im.symbols {
        let want = s.footprint.rsplit(':').next().unwrap_or("").to_string();
        let n = s.pins.iter().map(|x| x.num.as_str()).collect::<std::collections::BTreeSet<_>>().len();
        if !want.is_empty() && pcb_lib::footprint(&want).is_some() {
            s.footprint = want;
            continue;
        }
        let generated = (!want.is_empty()).then(|| crate::pcb_ipc::from_kicad_name(&want)).flatten().and_then(|spec| crate::pcb_ipc::generate(&spec).ok()).filter(|f| f.pin_numbers().len() == n);
        if let Some(f) = generated {
            im.notes.push(format!("{}: footprint {want} was not imported, so it gets the IPC-7351 pattern {}", s.key, f.name));
            s.footprint = f.name.clone();
            pcb_lib::register_footprints(std::slice::from_ref(&f));
            if !im.footprints.iter().any(|g| g.name == f.name) {
                im.footprints.push(f);
            }
            continue;
        }
        match fallback_footprint(s, n) {
            Some(fb) if !want.is_empty() => im.notes.push(format!("{}: footprint {want} is not imported; using {fb} until you import it or pick another", s.key)),
            Some(_) => {}
            None => im.notes.push(format!("{}: no footprint with {n} pads; import its footprint or generate one, then pick it", s.key)),
        }
        s.footprint = fallback_footprint(s, n).unwrap_or_else(|| "PinHeader_1x02".into());
    }
    for f in &im.footprints {
        p.footprints.retain(|g| g.name != f.name);
        p.footprints.push(f.clone());
    }
    pcb_lib::register_symbols(&im.symbols);
    for s in &im.symbols {
        p.symbols.retain(|g| g.key != s.key);
        p.symbols.push(s.clone());
    }
    im.notes
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const SOIC8: &str = r#"(footprint "SOIC-8_3.9x4.9mm_P1.27mm" (version 20221018) (generator pcbnew)
  (layer "F.Cu")
  (descr "SOIC, 8 Pin")
  (attr smd)
  (fp_line (start 0 2.56) (end 1.95 2.56) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
  (fp_line (start 0 -2.56) (end -3.45 -2.56) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
  (fp_arc (start -1 -1.9) (mid -0.707 -2.607) (end 0 -2.9) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
  (fp_rect (start -3.7 -2.7) (end 3.7 2.7) (stroke (width 0.05) (type solid)) (layer "F.CrtYd"))
  (fp_poly (pts (xy -0.975 -2.45) (xy 1.95 -2.45) (xy 1.95 2.45) (xy -1.95 2.45) (xy -1.95 -1.475)) (stroke (width 0.1) (type solid)) (fill none) (layer "F.Fab"))
  (pad "1" smd roundrect (at -2.475 -1.905) (size 1.95 0.6) (layers "F.Cu" "F.Paste" "F.Mask") (roundrect_rratio 0.25))
  (pad "2" smd roundrect (at -2.475 -0.635) (size 1.95 0.6) (layers "F.Cu" "F.Paste" "F.Mask") (roundrect_rratio 0.25))
  (pad "3" smd roundrect (at -2.475 0.635) (size 1.95 0.6) (layers "F.Cu" "F.Paste" "F.Mask") (roundrect_rratio 0.25))
  (pad "4" smd roundrect (at -2.475 1.905) (size 1.95 0.6) (layers "F.Cu" "F.Paste" "F.Mask") (roundrect_rratio 0.25))
  (pad "5" smd roundrect (at 2.475 1.905) (size 1.95 0.6) (layers "F.Cu" "F.Paste" "F.Mask") (roundrect_rratio 0.25))
  (pad "6" smd roundrect (at 2.475 0.635) (size 1.95 0.6) (layers "F.Cu" "F.Paste" "F.Mask") (roundrect_rratio 0.25))
  (pad "7" smd roundrect (at 2.475 -0.635) (size 1.95 0.6) (layers "F.Cu" "F.Paste" "F.Mask") (roundrect_rratio 0.25))
  (pad "8" smd roundrect (at 2.475 -1.905) (size 1.95 0.6) (layers "F.Cu" "F.Paste" "F.Mask") (roundrect_rratio 0.25))
  (pad "" np_thru_hole circle (at 0 0) (size 1 1) (drill 1) (layers "*.Cu" "*.Mask"))
  (model "${KICAD7_3DMODEL_DIR}/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.wrl" (offset (xyz 0 0 0)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0)))
)"#;

    pub const LIB: &str = r#"(kicad_symbol_lib (version 20220914) (generator kicad_symbol_editor)
  (symbol "LM358" (pin_names (offset 0.127)) (in_bom yes) (on_board yes)
    (property "Reference" "U" (at 0 5.08 0) (effects (font (size 1.27 1.27)) (justify left)))
    (property "Value" "LM358" (at 0 -5.08 0) (effects (font (size 1.27 1.27)) (justify left)))
    (property "Footprint" "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (property "ki_description" "Low-Power, Dual Operational Amplifiers" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (symbol "LM358_1_1"
      (polyline (pts (xy -5.08 5.08) (xy 5.08 0) (xy -5.08 -5.08) (xy -5.08 5.08)) (stroke (width 0.254) (type default)) (fill (type background)))
      (pin output line (at 7.62 0 180) (length 2.54) (name "~" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
      (pin input line (at -7.62 -2.54 0) (length 2.54) (name "-" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
      (pin input line (at -7.62 2.54 0) (length 2.54) (name "+" (effects (font (size 1.27 1.27)))) (number "3" (effects (font (size 1.27 1.27)))))
    )
    (symbol "LM358_2_1"
      (polyline (pts (xy -5.08 5.08) (xy 5.08 0) (xy -5.08 -5.08) (xy -5.08 5.08)) (stroke (width 0.254) (type default)) (fill (type background)))
      (pin input line (at -7.62 2.54 0) (length 2.54) (name "+" (effects (font (size 1.27 1.27)))) (number "5" (effects (font (size 1.27 1.27)))))
      (pin input line (at -7.62 -2.54 0) (length 2.54) (name "-" (effects (font (size 1.27 1.27)))) (number "6" (effects (font (size 1.27 1.27)))))
      (pin output line (at 7.62 0 180) (length 2.54) (name "~" (effects (font (size 1.27 1.27)))) (number "7" (effects (font (size 1.27 1.27)))))
    )
    (symbol "LM358_3_1"
      (pin power_in line (at -2.54 -7.62 90) (length 3.81) (name "V-" (effects (font (size 1.27 1.27)))) (number "4" (effects (font (size 1.27 1.27)))))
      (pin power_in line (at -2.54 7.62 270) (length 3.81) (name "V+" (effects (font (size 1.27 1.27)))) (number "8" (effects (font (size 1.27 1.27)))))
    )
  )
  (symbol "TL072" (extends "LM358")
    (property "Reference" "U" (at 0 5.08 0) (effects (font (size 1.27 1.27)) (justify left)))
    (property "Value" "TL072" (at 0 -5.08 0) (effects (font (size 1.27 1.27)) (justify left)))
  )
)"#;

    #[test]
    fn reads_a_kicad_footprint() {
        let mut notes = vec![];
        let f = footprint(&parse(SOIC8).unwrap(), &mut notes).unwrap();
        assert_eq!(f.pads.len(), 8);
        // KiCad's y points down: pin 1 ends up top left, as rCAD draws it
        let p1 = &f.pads[0];
        assert!(p1.at[0] < 0.0 && p1.at[1] > 0.0 && (p1.at[1] - 1.905).abs() < 1e-9, "{:?}", p1.at);
        assert_eq!(p1.size, [1.95, 0.6]);
        assert_eq!(p1.shape, PadShape::RoundRect);
        assert_eq!(f.court, ([-3.7, -2.7], [3.7, 2.7]));
        assert_eq!(f.silk.len(), 3);
        assert!(notes.iter().any(|n| n.contains("non-plated")), "{notes:?}");
    }

    #[test]
    fn reads_symbols_and_wires_them_to_footprints() {
        let mut im = Imported::default();
        import_text("Amplifier_Operational.kicad_sym", LIB, &mut im).unwrap();
        import_text("SOIC-8_3.9x4.9mm_P1.27mm.kicad_mod", SOIC8, &mut im).unwrap();
        assert_eq!(im.symbols.len(), 2);
        let s = &im.symbols[0];
        assert_eq!(s.key, "Amplifier_Operational:LM358");
        assert_eq!(s.pins.len(), 8);
        // unit 2 sits right of unit 1
        let p5 = s.pins.iter().find(|p| p.num == "5").unwrap();
        let p3 = s.pins.iter().find(|p| p.num == "3").unwrap();
        assert!(p5.at[0] > p3.at[0] + 10.0);
        assert_eq!(s.pins.iter().find(|p| p.num == "1").unwrap().name, "");
        assert_eq!(im.symbols[1].value, "TL072");
        let mut p = crate::pcb::tests::led_board();
        let notes = add_to(&mut p, im);
        assert!(notes.iter().all(|n| !n.contains("not imported")), "{notes:?}");
        let i = p.add_part("Amplifier_Operational:TL072", None, None, [60.0, 0.0]).unwrap();
        let part = &p.parts[i];
        assert_eq!((part.reference.as_str(), part.footprint.as_str()), ("U1", "SOIC-8_3.9x4.9mm_P1.27mm"));
        assert_eq!(part.symbol().pins.len(), 8);
        // KiCad pin types drive the ERC
        assert_eq!(crate::pcb_erc::pin_type(&p, i, "1", ""), crate::pcb_erc::PinType::Output);
        assert_eq!(crate::pcb_erc::pin_type(&p, i, "8", "V+"), crate::pcb_erc::PinType::PowerIn);
        // the library travels with the document
        let text = serde_json::to_string(&p).unwrap();
        let back: Pcb = serde_json::from_str(&text).unwrap();
        assert_eq!(back.footprints.len(), 1);
        assert_eq!(back.symbols.len(), 2);
    }
}
