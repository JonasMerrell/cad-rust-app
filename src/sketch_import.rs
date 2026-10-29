//! 2D drawings into sketches: DXF (lines, arcs, circles, polylines with bulges, splines, ellipses,
//! block inserts) and SVG (every path: circles, ellipses and circular arcs stay exact, other curves
//! become fixed outlines).

use std::collections::HashMap;

use glam::DVec2;

use crate::sketch::{Geom, Id, Sketch};

/// A curve read from a file, in mm.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Line(DVec2, DVec2),
    /// Counter-clockwise from angle `a0` to `a1` (radians).
    Arc {
        c: DVec2,
        r: f64,
        a0: f64,
        a1: f64,
    },
    Circle {
        c: DVec2,
        r: f64,
    },
    /// `major`: centre to the end of the major axis; `ratio`: minor / major; parameters `t0`..`t1` (radians).
    Ellipse {
        c: DVec2,
        major: DVec2,
        ratio: f64,
        t0: f64,
        t1: f64,
    },
    /// Smooth curve through the points.
    Spline {
        pts: Vec<DVec2>,
        closed: bool,
    },
    /// Fixed outline.
    Poly {
        pts: Vec<DVec2>,
        closed: bool,
    },
}

const TAU: f64 = std::f64::consts::TAU;

impl Item {
    /// Points along the curve, about `tol` from it.
    fn sample(&self, tol: f64) -> (Vec<DVec2>, bool) {
        let arc_n = |r: f64, sweep: f64| ((sweep.abs() / (2.0 * (1.0 - (tol / r.max(tol)).min(1.0)).acos().max(1e-3))).ceil() as usize).clamp(4, 2000);
        match self {
            Item::Line(a, b) => (vec![*a, *b], false),
            Item::Arc { c, r, a0, a1 } => {
                let mut sweep = a1 - a0;
                while sweep <= 0.0 {
                    sweep += TAU;
                }
                let n = arc_n(*r, sweep);
                ((0..=n).map(|i| *c + DVec2::from_angle(a0 + sweep * i as f64 / n as f64) * *r).collect(), false)
            }
            Item::Circle { c, r } => {
                let n = arc_n(*r, TAU);
                ((0..n).map(|i| *c + DVec2::from_angle(TAU * i as f64 / n as f64) * *r).collect(), true)
            }
            Item::Ellipse { c, major, ratio, t0, t1 } => {
                let full = (t1 - t0).abs() >= TAU - 1e-9;
                let mut sweep = t1 - t0;
                while sweep <= 0.0 {
                    sweep += TAU;
                }
                let minor = major.perp() * *ratio;
                let n = arc_n(major.length(), sweep);
                let at = |t: f64| *c + *major * t.cos() + minor * t.sin();
                if full {
                    ((0..n).map(|i| at(t0 + TAU * i as f64 / n as f64)).collect(), true)
                } else {
                    ((0..=n).map(|i| at(t0 + sweep * i as f64 / n as f64)).collect(), false)
                }
            }
            Item::Spline { pts, closed } | Item::Poly { pts, closed } => (pts.clone(), *closed),
        }
    }

    /// The curve moved by `f`. A similarity keeps it exact; anything else (or a mirror for ellipses)
    /// turns curves into fixed outlines.
    fn mapped(&self, f: &Xf) -> Item {
        let m = |p: DVec2| f.apply(p);
        let s = f.scale();
        match self {
            Item::Line(a, b) => Item::Line(m(*a), m(*b)),
            Item::Spline { pts, closed } => Item::Spline { pts: pts.iter().map(|p| m(*p)).collect(), closed: *closed },
            Item::Poly { pts, closed } => Item::Poly { pts: pts.iter().map(|p| m(*p)).collect(), closed: *closed },
            Item::Circle { c, r } if s.is_some() => Item::Circle { c: m(*c), r: r * s.unwrap() },
            Item::Arc { c, r, a0, a1 } if s.is_some() => {
                let (p0, p1) = (m(*c + DVec2::from_angle(*a0) * *r), m(*c + DVec2::from_angle(*a1) * *r));
                let cc = m(*c);
                // a mirror turns it clockwise: swap the ends
                let (p0, p1) = if f.mirrors() { (p1, p0) } else { (p0, p1) };
                Item::Arc { c: cc, r: r * s.unwrap(), a0: (p0 - cc).to_angle(), a1: (p1 - cc).to_angle() }
            }
            Item::Ellipse { c, major, ratio, t0, t1 } if s.is_some() && !f.mirrors() => {
                let cc = m(*c);
                Item::Ellipse { c: cc, major: m(*c + *major) - cc, ratio: *ratio, t0: *t0, t1: *t1 }
            }
            _ => {
                let (pts, closed) = self.sample(0.01);
                Item::Poly { pts: pts.into_iter().map(m).collect(), closed }
            }
        }
    }
}

/// An affine map: p ↦ o + [a b; c d] p.
#[derive(Clone, Copy, Debug)]
struct Xf {
    m: [f64; 4],
    o: DVec2,
}

impl Xf {
    const ID: Xf = Xf { m: [1.0, 0.0, 0.0, 1.0], o: DVec2::ZERO };

    fn apply(&self, p: DVec2) -> DVec2 {
        self.o + DVec2::new(self.m[0] * p.x + self.m[1] * p.y, self.m[2] * p.x + self.m[3] * p.y)
    }

    fn then(&self, g: &Xf) -> Xf {
        // g ∘ self
        let [a, b, c, d] = self.m;
        let [e, f, h, k] = g.m;
        Xf { m: [e * a + f * c, e * b + f * d, h * a + k * c, h * b + k * d], o: g.apply(self.o) }
    }

    /// The uniform scale, if it is a similarity.
    fn scale(&self) -> Option<f64> {
        let [a, b, c, d] = self.m;
        let (sx, sy) = (DVec2::new(a, c).length(), DVec2::new(b, d).length());
        ((sx - sy).abs() < 1e-9 * sx.max(1.0) && (a * b + c * d).abs() < 1e-9 * sx * sy.max(1e-12)).then_some(sx)
    }

    fn mirrors(&self) -> bool {
        self.m[0] * self.m[3] - self.m[1] * self.m[2] < 0.0
    }
}

/// What came out of a file.
#[derive(Debug, Default)]
pub struct Drawing {
    pub items: Vec<Item>,
    /// Entities that were left out, by type.
    pub skipped: Vec<(String, usize)>,
}

impl Drawing {
    fn skip(&mut self, what: &str) {
        match self.skipped.iter_mut().find(|s| s.0 == what) {
            Some(s) => s.1 += 1,
            None => self.skipped.push((what.to_string(), 1)),
        }
    }
}

/// Whether a file is a 2D drawing this can read.
pub fn is_drawing(path: &std::path::Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(), Some("dxf" | "svg"))
}

pub fn read(path: &std::path::Path) -> Result<Drawing, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("dxf") => dxf(&String::from_utf8_lossy(&bytes)),
        Some("svg") => svg(&bytes),
        _ => Err("import .dxf or .svg drawings".into()),
    }
}

// ---------------------------------------------------------------- DXF

type Groups = Vec<(i32, String)>;

fn num(g: &Groups, code: i32) -> Option<f64> {
    g.iter().find(|(c, _)| *c == code).and_then(|(_, v)| v.trim().parse().ok())
}

fn nums(g: &Groups, code: i32) -> Vec<f64> {
    g.iter().filter(|(c, _)| *c == code).filter_map(|(_, v)| v.trim().parse().ok()).collect()
}

fn text(g: &Groups, code: i32) -> Option<&str> {
    g.iter().find(|(c, _)| *c == code).map(|(_, v)| v.trim())
}

fn pt(g: &Groups, x: i32) -> DVec2 {
    DVec2::new(num(g, x).unwrap_or(0.0), num(g, x + 10).unwrap_or(0.0))
}

/// Read an ASCII DXF.
pub fn dxf(src: &str) -> Result<Drawing, String> {
    let lines: Vec<&str> = src.lines().collect();
    if lines.len() < 2 || !lines.iter().take(4).any(|l| l.trim() == "0" || l.trim() == "999") {
        return Err("not an ASCII DXF file (binary DXF isn't supported)".into());
    }
    let mut pairs: Vec<(i32, String)> = vec![];
    for k in (0..lines.len() - 1).step_by(2) {
        let Ok(code) = lines[k].trim().parse::<i32>() else { return Err(format!("unreadable DXF at line {}", k + 1)) };
        pairs.push((code, lines[k + 1].trim_end_matches('\r').to_string()));
    }
    // split into sections of entities: (type, groups)
    let mut scale = 1.0;
    let mut section = String::new();
    let mut ents: Vec<(String, Groups)> = vec![];
    let mut blocks: HashMap<String, (DVec2, Vec<(String, Groups)>)> = HashMap::new();
    let mut block: Option<(String, DVec2, Vec<(String, Groups)>)> = None;
    let mut i = 0;
    while i < pairs.len() {
        let (code, v) = (pairs[i].0, pairs[i].1.trim().to_string());
        if code != 0 {
            i += 1;
            continue;
        }
        // an entity or a section marker: gather its groups up to the next 0
        let mut g: Groups = vec![];
        let mut j = i + 1;
        while j < pairs.len() && pairs[j].0 != 0 {
            g.push(pairs[j].clone());
            j += 1;
        }
        // the header's variables come as groups after "0 SECTION / 2 HEADER"
        if v == "SECTION" && text(&g, 2) == Some("HEADER") {
            for w in g.windows(2) {
                if w[0].0 == 9 && w[0].1.trim() == "$INSUNITS" && w[1].0 == 70 {
                    scale = unit_mm(w[1].1.trim().parse().unwrap_or(0));
                }
            }
        }
        match v.as_str() {
            "SECTION" => section = text(&g, 2).unwrap_or("").to_string(),
            "ENDSEC" => section.clear(),
            "BLOCK" if section == "BLOCKS" => block = Some((text(&g, 2).unwrap_or("").to_string(), pt(&g, 10), vec![])),
            "ENDBLK" => {
                if let Some((name, base, list)) = block.take() {
                    blocks.insert(name, (base, list));
                }
            }
            "EOF" => break,
            _ if section == "ENTITIES" => ents.push((v.clone(), g)),
            _ if section == "BLOCKS" => {
                if let Some(b) = &mut block {
                    b.2.push((v.clone(), g));
                }
            }
            _ => {}
        }
        i = j;
    }
    let mut d = Drawing::default();
    let units = Xf { m: [scale, 0.0, 0.0, scale], o: DVec2::ZERO };
    entities(&ents, &blocks, &units, &mut d, 0);
    if d.items.is_empty() {
        return Err(if d.skipped.is_empty() { "the drawing has no geometry".into() } else { format!("nothing to import ({})", skipped_text(&d.skipped)) });
    }
    Ok(d)
}

/// Millimetres per drawing unit ($INSUNITS); unitless drawings are taken as mm.
fn unit_mm(code: i32) -> f64 {
    match code {
        1 => 25.4,
        2 => 304.8,
        5 => 10.0,
        6 => 1000.0,
        8 => 0.0000254,
        9 => 0.0254,
        10 => 914.4,
        14 => 100.0,
        _ => 1.0,
    }
}

pub fn skipped_text(s: &[(String, usize)]) -> String {
    s.iter().map(|(t, n)| format!("{n} {}", t.to_lowercase())).collect::<Vec<_>>().join(", ")
}

fn entities(ents: &[(String, Groups)], blocks: &HashMap<String, (DVec2, Vec<(String, Groups)>)>, f: &Xf, d: &mut Drawing, depth: usize) {
    let mut k = 0;
    while k < ents.len() {
        let (ty, g) = (&ents[k].0, &ents[k].1);
        k += 1;
        // entities drawn in a mirrored object coordinate system (extrusion along -Z)
        let ocs = if num(g, 230).unwrap_or(1.0) < 0.0 { Xf { m: [-1.0, 0.0, 0.0, 1.0], o: DVec2::ZERO } } else { Xf::ID };
        let f = ocs.then(f);
        let mut push = |it: Item| d.items.push(it.mapped(&f));
        match ty.as_str() {
            "LINE" => push(Item::Line(pt(g, 10), pt(g, 11))),
            "CIRCLE" => push(Item::Circle { c: pt(g, 10), r: num(g, 40).unwrap_or(0.0) }),
            "ARC" => push(Item::Arc { c: pt(g, 10), r: num(g, 40).unwrap_or(0.0), a0: num(g, 50).unwrap_or(0.0).to_radians(), a1: num(g, 51).unwrap_or(360.0).to_radians() }),
            "ELLIPSE" => {
                let (t0, t1) = (num(g, 41).unwrap_or(0.0), num(g, 42).unwrap_or(TAU));
                push(Item::Ellipse { c: pt(g, 10), major: pt(g, 11), ratio: num(g, 40).unwrap_or(1.0), t0, t1 });
            }
            "LWPOLYLINE" => {
                let closed = num(g, 70).unwrap_or(0.0) as i64 & 1 == 1;
                // vertices in order, each with the bulge of the segment after it
                let mut verts: Vec<(DVec2, f64)> = vec![];
                let mut x = None;
                for (c, v) in g {
                    let v: f64 = v.trim().parse().unwrap_or(0.0);
                    match c {
                        10 => x = Some(v),
                        20 => {
                            if let Some(x) = x.take() {
                                verts.push((DVec2::new(x, v), 0.0));
                            }
                        }
                        42 => {
                            if let Some(l) = verts.last_mut() {
                                l.1 = v;
                            }
                        }
                        _ => {}
                    }
                }
                for it in bulge_path(&verts, closed) {
                    push(it);
                }
            }
            "POLYLINE" => {
                let flags = num(g, 70).unwrap_or(0.0) as i64;
                let mut verts = vec![];
                while k < ents.len() && ents[k].0 == "VERTEX" {
                    let vg = &ents[k].1;
                    // skip spline frame points and mesh vertices
                    if num(vg, 70).unwrap_or(0.0) as i64 & (16 | 64 | 128) == 0 {
                        verts.push((pt(vg, 10), num(vg, 42).unwrap_or(0.0)));
                    }
                    k += 1;
                }
                if k < ents.len() && ents[k].0 == "SEQEND" {
                    k += 1;
                }
                if flags & (16 | 64) != 0 {
                    d.skip("polygon mesh");
                    continue;
                }
                for it in bulge_path(&verts, flags & 1 == 1) {
                    push(it);
                }
            }
            "SPLINE" => {
                let closed = num(g, 70).unwrap_or(0.0) as i64 & 1 == 1;
                let fit: Vec<DVec2> = nums(g, 11).into_iter().zip(nums(g, 21)).map(|(x, y)| DVec2::new(x, y)).collect();
                if fit.len() >= 2 {
                    push(Item::Spline { pts: fit, closed });
                } else {
                    let ctrl: Vec<DVec2> = nums(g, 10).into_iter().zip(nums(g, 20)).map(|(x, y)| DVec2::new(x, y)).collect();
                    let pts = nurbs(num(g, 71).unwrap_or(3.0) as usize, &nums(g, 40), &ctrl, &nums(g, 41));
                    if pts.len() >= 2 {
                        push(Item::Poly { pts, closed });
                    }
                }
            }
            "INSERT" if depth < 8 => {
                let name = text(g, 2).unwrap_or("");
                let Some((base, list)) = blocks.get(name) else {
                    d.skip("missing block");
                    continue;
                };
                let (sx, sy) = (num(g, 41).unwrap_or(1.0), num(g, 42).unwrap_or(1.0));
                let a = num(g, 50).unwrap_or(0.0).to_radians();
                let (s, c) = a.sin_cos();
                // p ↦ at + R·S·(p − base), then the outer transform
                let rs = Xf { m: [c * sx, -s * sy, s * sx, c * sy], o: DVec2::ZERO };
                let inner = Xf { m: rs.m, o: pt(g, 10) - rs.apply(*base) };
                let cols = num(g, 70).unwrap_or(1.0).max(1.0) as usize;
                let rows = num(g, 71).unwrap_or(1.0).max(1.0) as usize;
                let (dx, dy) = (num(g, 44).unwrap_or(0.0), num(g, 45).unwrap_or(0.0));
                for r in 0..rows.min(100) {
                    for q in 0..cols.min(100) {
                        let off = DVec2::new(c * dx * q as f64 - s * dy * r as f64, s * dx * q as f64 + c * dy * r as f64);
                        let t = Xf { m: inner.m, o: inner.o + off }.then(&f);
                        entities(list, blocks, &t, d, depth + 1);
                    }
                }
            }
            "POINT" | "VERTEX" | "SEQEND" | "ATTRIB" | "ATTDEF" => {}
            other => d.skip(other),
        }
    }
}

/// Lines and arcs of a polyline whose segments may bulge (bulge = tan(sweep / 4), positive counter-clockwise).
fn bulge_path(verts: &[(DVec2, f64)], closed: bool) -> Vec<Item> {
    let n = verts.len();
    if n < 2 {
        return vec![];
    }
    let segs = if closed { n } else { n - 1 };
    let mut out = vec![];
    for i in 0..segs {
        let (a, b, bulge) = (verts[i].0, verts[(i + 1) % n].0, verts[i].1);
        if a.distance(b) < 1e-9 {
            continue;
        }
        if bulge.abs() < 1e-9 {
            out.push(Item::Line(a, b));
            continue;
        }
        let sweep = 4.0 * bulge.atan();
        let chord = b - a;
        let r = chord.length() / (2.0 * (sweep / 2.0).sin().abs());
        // centre: from the chord's middle, perpendicular, on the side the arc turns to
        let h = (r * r - chord.length_squared() / 4.0).max(0.0).sqrt();
        let side = if (sweep.abs() < std::f64::consts::PI) == (sweep > 0.0) { 1.0 } else { -1.0 };
        let c = (a + b) / 2.0 + chord.perp().normalize() * h * side;
        let (pa, pb) = if sweep > 0.0 { (a, b) } else { (b, a) };
        out.push(Item::Arc { c, r, a0: (pa - c).to_angle(), a1: (pb - c).to_angle() });
    }
    out
}

/// Points along a (rational) B-spline from its knots and control points.
fn nurbs(degree: usize, knots: &[f64], ctrl: &[DVec2], weights: &[f64]) -> Vec<DVec2> {
    let n = ctrl.len();
    if n < 2 || degree == 0 || knots.len() != n + degree + 1 {
        return ctrl.to_vec();
    }
    let w = |i: usize| weights.get(i).copied().filter(|w| *w > 0.0).unwrap_or(1.0);
    let (t0, t1) = (knots[degree], knots[n]);
    let samples = (n * 16).clamp(32, 4000);
    (0..=samples)
        .map(|s| {
            let t = t0 + (t1 - t0) * s as f64 / samples as f64;
            // the knot span holding t
            let mut k = degree;
            while k < n - 1 && t >= knots[k + 1] {
                k += 1;
            }
            // de Boor on homogeneous points
            let mut d: Vec<(DVec2, f64)> = (0..=degree).map(|j| (ctrl[j + k - degree] * w(j + k - degree), w(j + k - degree))).collect();
            for r in 1..=degree {
                for j in (r..=degree).rev() {
                    let i = j + k - degree;
                    let den = knots[i + degree + 1 - r] - knots[i];
                    let a = if den.abs() < 1e-12 { 0.0 } else { (t - knots[i]) / den };
                    d[j] = (d[j - 1].0 * (1.0 - a) + d[j].0 * a, d[j - 1].1 * (1.0 - a) + d[j].1 * a);
                }
            }
            d[degree].0 / d[degree].1
        })
        .collect()
}

// ---------------------------------------------------------------- SVG

/// Read an SVG: every path, in mm (96 px to the inch), Y up with the page's bottom at 0.
pub fn svg(bytes: &[u8]) -> Result<Drawing, String> {
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default()).map_err(|e| format!("could not read the SVG: {e}"))?;
    let px = 25.4 / 96.0;
    let h = tree.size().height() as f64;
    let mut d = Drawing::default();
    let to_mm = |x: f32, y: f32| DVec2::new(x as f64 * px, (h - y as f64) * px);
    walk(tree.root(), &to_mm, &mut d);
    if d.items.is_empty() {
        return Err("the SVG has no paths".into());
    }
    Ok(d)
}

fn walk(g: &usvg::Group, to_mm: &dyn Fn(f32, f32) -> DVec2, d: &mut Drawing) {
    for n in g.children() {
        match n {
            usvg::Node::Group(g) => walk(g, to_mm, d),
            usvg::Node::Path(p) => {
                let tf = p.abs_transform();
                let map = |q: usvg::tiny_skia_path::Point| {
                    let mut q = q;
                    tf.map_point(&mut q);
                    to_mm(q.x, q.y)
                };
                // one subpath at a time
                let mut segs: Vec<Seg> = vec![];
                let mut at = DVec2::ZERO;
                let mut start = DVec2::ZERO;
                for seg in p.data().segments() {
                    use usvg::tiny_skia_path::PathSegment as S;
                    match seg {
                        S::MoveTo(q) => {
                            subpath(&mut segs, false, d);
                            at = map(q);
                            start = at;
                        }
                        S::LineTo(q) => {
                            let q = map(q);
                            segs.push(Seg::Line(at, q));
                            at = q;
                        }
                        S::QuadTo(c, q) => {
                            // as the equivalent cubic
                            let (c, q) = (map(c), map(q));
                            segs.push(Seg::Cubic([at, at + (c - at) * (2.0 / 3.0), q + (c - q) * (2.0 / 3.0), q]));
                            at = q;
                        }
                        S::CubicTo(c1, c2, q) => {
                            let q = map(q);
                            segs.push(Seg::Cubic([at, map(c1), map(c2), q]));
                            at = q;
                        }
                        S::Close => {
                            if at.distance(start) > 1e-9 {
                                segs.push(Seg::Line(at, start));
                            }
                            subpath(&mut segs, true, d);
                            at = start;
                        }
                    }
                }
                subpath(&mut segs, false, d);
            }
            usvg::Node::Image(_) => d.skip("IMAGE"),
            usvg::Node::Text(_) => d.skip("TEXT"),
        }
    }
}

/// A piece of an SVG subpath (quadratic Béziers are raised to cubics).
#[derive(Clone, Copy, Debug)]
enum Seg {
    Line(DVec2, DVec2),
    Cubic([DVec2; 4]),
}

fn cubic_at(c: &[DVec2; 4], t: f64) -> DVec2 {
    let u = 1.0 - t;
    c[0] * u * u * u + c[1] * 3.0 * t * u * u + c[2] * 3.0 * t * t * u + c[3] * t * t * t
}

/// Turn a subpath into items: straight pieces become lines; a closed run of curves that is an ellipse
/// (what `<circle>` and `<ellipse>` become) a circle or ellipse; curves that are circular arcs (SVG `A`)
/// arcs; anything else one fixed outline.
fn subpath(segs: &mut Vec<Seg>, closed: bool, d: &mut Drawing) {
    let segs = std::mem::take(segs);
    let segs: Vec<Seg> = segs
        .into_iter()
        .filter(|s| match s {
            Seg::Line(a, b) => a.distance(*b) > 1e-9,
            Seg::Cubic(c) => c.iter().any(|p| p.distance(c[0]) > 1e-9),
        })
        .collect();
    if segs.is_empty() {
        return;
    }
    let curved = segs.iter().any(|s| matches!(s, Seg::Cubic(_)));
    if curved && closed && segs.iter().all(|s| matches!(s, Seg::Cubic(_))) {
        let pts: Vec<DVec2> = segs.iter().flat_map(|s| if let Seg::Cubic(c) = s { (0..8).map(|i| cubic_at(c, i as f64 / 8.0)).collect() } else { vec![] }).collect();
        if let Some(it) = fit_ellipse(&pts) {
            // the pieces' end points lie exactly on the curve (the Béziers between only approximate it)
            let ends: Vec<DVec2> = segs.iter().filter_map(|s| if let Seg::Cubic(c) = s { Some(c[0]) } else { None }).collect();
            d.items.push(match it {
                Item::Circle { c, .. } => Item::Circle { c, r: ends.iter().map(|p| p.distance(c)).sum::<f64>() / ends.len() as f64 },
                other => other,
            });
            return;
        }
    }
    let arcs: Vec<Option<(DVec2, f64, f64)>> = segs.iter().map(|s| if let Seg::Cubic(c) = s { cubic_arc(c) } else { None }).collect();
    if segs.iter().zip(&arcs).all(|(s, a)| matches!(s, Seg::Line(..)) || a.is_some()) {
        // lines, and arcs merged where consecutive pieces continue the same circle
        let mut run: Option<(DVec2, f64, DVec2, f64)> = None;
        let flush = |run: &mut Option<(DVec2, f64, DVec2, f64)>, d: &mut Drawing| {
            if let Some((c, r, start, sweep)) = run.take() {
                if sweep.abs() >= TAU - 1e-6 {
                    d.items.push(Item::Circle { c, r });
                } else {
                    let a = (start - c).to_angle();
                    // items run counter-clockwise
                    let (a0, a1) = if sweep > 0.0 { (a, a + sweep) } else { (a + sweep, a) };
                    d.items.push(Item::Arc { c, r, a0, a1 });
                }
            }
        };
        for (s, a) in segs.iter().zip(&arcs) {
            match (s, a) {
                (Seg::Line(p, q), _) => {
                    flush(&mut run, d);
                    d.items.push(Item::Line(*p, *q));
                }
                (Seg::Cubic(cb), Some((c, r, sweep))) => {
                    let tol = 1e-3 * r + 1e-6;
                    match &mut run {
                        Some((rc, rr, _, rs)) if rc.distance(*c) < tol && (*rr - r).abs() < tol && rs.signum() == sweep.signum() => *rs += sweep,
                        _ => {
                            flush(&mut run, d);
                            run = Some((*c, *r, cb[0], *sweep));
                        }
                    }
                }
                _ => {}
            }
        }
        flush(&mut run, d);
        return;
    }
    // a fixed outline through every piece
    let mut pts = vec![];
    for s in &segs {
        match s {
            Seg::Line(a, b) => {
                if pts.is_empty() {
                    pts.push(*a);
                }
                pts.push(*b);
            }
            Seg::Cubic(c) => {
                if pts.is_empty() {
                    pts.push(c[0]);
                }
                let n = bezier_steps(c);
                pts.extend((1..=n).map(|i| cubic_at(c, i as f64 / n as f64)));
            }
        }
    }
    if closed && pts.len() > 2 && pts[0].distance(*pts.last().unwrap()) < 1e-9 {
        pts.pop();
    }
    if pts.len() >= 2 {
        d.items.push(Item::Poly { pts, closed });
    }
}

/// The circle a cubic follows, if it is a circular arc: (centre, radius, signed sweep).
fn cubic_arc(c: &[DVec2; 4]) -> Option<(DVec2, f64, f64)> {
    let (a, m, b) = (c[0], cubic_at(c, 0.5), c[3]);
    let ctr = circumcenter(a, m, b)?;
    let r = (a - ctr).length();
    let tol = 1e-3 * r + 1e-6;
    if [0.1, 0.25, 0.4, 0.6, 0.75, 0.9].iter().any(|t| ((cubic_at(c, *t) - ctr).length() - r).abs() > tol) {
        return None;
    }
    let sweep = (a - ctr).angle_to(m - ctr) + (m - ctr).angle_to(b - ctr);
    Some((ctr, r, sweep))
}

fn circumcenter(a: DVec2, b: DVec2, c: DVec2) -> Option<DVec2> {
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if d.abs() < 1e-12 * (a.distance(b) * b.distance(c)).max(1e-12) {
        return None;
    }
    let (a2, b2, c2) = (a.length_squared(), b.length_squared(), c.length_squared());
    Some(DVec2::new((a2 * (b.y - c.y) + b2 * (c.y - a.y) + c2 * (a.y - b.y)) / d, (a2 * (c.x - b.x) + b2 * (a.x - c.x) + c2 * (b.x - a.x)) / d))
}

/// The full circle or ellipse through closed-curve samples `pts`, if they lie on one.
fn fit_ellipse(pts: &[DVec2]) -> Option<Item> {
    if pts.len() < 8 {
        return None;
    }
    // A x² + B xy + C y² + D x + E y = 1, about the centroid and scaled for conditioning
    let o = pts.iter().fold(DVec2::ZERO, |a, p| a + *p) / pts.len() as f64;
    let s = pts.iter().map(|p| p.distance(o)).fold(0.0, f64::max);
    if s < 1e-9 {
        return None;
    }
    let mut ata = vec![vec![0.0; 5]; 5];
    let mut atb = vec![0.0; 5];
    for p in pts {
        let q = (*p - o) / s;
        let row = [q.x * q.x, q.x * q.y, q.y * q.y, q.x, q.y];
        for i in 0..5 {
            atb[i] += row[i];
            for j in 0..5 {
                ata[i][j] += row[i] * row[j];
            }
        }
    }
    let [a, b, c, dx, ey]: [f64; 5] = crate::sketch::solve_dense(ata, atb).try_into().ok()?;
    // centre: 2 M x0 = -(D, E) with M = [[A, B/2], [B/2, C]]
    let det = a * c - b * b / 4.0;
    if det <= 1e-12 {
        return None;
    }
    let x0 = DVec2::new(-(c * dx - b / 2.0 * ey) / (2.0 * det), -(a * ey - b / 2.0 * dx) / (2.0 * det));
    let k = 1.0 + a * x0.x * x0.x + b * x0.x * x0.y + c * x0.y * x0.y;
    let mean = (a + c) / 2.0;
    let diff = (((a - c) / 2.0).powi(2) + (b / 2.0).powi(2)).sqrt();
    let (l1, l2) = (mean - diff, mean + diff);
    if l1 <= 0.0 || k <= 0.0 {
        return None;
    }
    let (ra, rb) = ((k / l1).sqrt() * s, (k / l2).sqrt() * s);
    let ctr = o + x0 * s;
    // every sample on it, to a fraction of its size
    let tol = 5e-4 * ra + 1e-6;
    for p in pts {
        let y = (*p - o) / s - x0;
        let q = a * y.x * y.x + b * y.x * y.y + c * y.y * y.y;
        let f = (q / k).sqrt();
        if f <= 0.0 || (y.length() * s * (1.0 - 1.0 / f)).abs() > tol {
            return None;
        }
    }
    if (ra - rb).abs() < tol {
        return Some(Item::Circle { c: ctr, r: (ra + rb) / 2.0 });
    }
    let dir = if b.abs() < 1e-12 { if a <= c { DVec2::X } else { DVec2::Y } } else { DVec2::new(b / 2.0, l1 - a).normalize() };
    Some(Item::Ellipse { c: ctr, major: dir * ra, ratio: rb / ra, t0: 0.0, t1: TAU })
}

/// Enough steps to keep a Bézier within about 0.01 mm.
fn bezier_steps(c: &[DVec2]) -> usize {
    let mut dd = 0.0f64;
    for w in c.windows(3) {
        dd = dd.max((w[0] - w[1] * 2.0 + w[2]).length());
    }
    ((dd * (c.len() - 1) as f64 * (c.len() - 2) as f64 / (8.0 * 0.01)).sqrt().ceil() as usize).clamp(2, 400)
}

// ---------------------------------------------------------------- into a sketch

/// Add a drawing to a sketch, scaled by `scale` and moved by `offset`; ends that meet share one point.
/// Returns the number of curves added.
pub fn add_to_sketch(sk: &mut Sketch, d: &Drawing, scale: f64, offset: DVec2) -> usize {
    let f = Xf { m: [scale, 0.0, 0.0, scale], o: offset };
    let mut grid: HashMap<(i64, i64), Vec<Id>> = HashMap::new();
    let q = 1e-3;
    let mut point = |sk: &mut Sketch, p: DVec2| -> Id {
        let (i, j) = ((p.x / q).round() as i64, (p.y / q).round() as i64);
        for di in -1..=1 {
            for dj in -1..=1 {
                if let Some(ids) = grid.get(&(i + di, j + dj)) {
                    if let Some(id) = ids.iter().find(|id| sk.pv(**id).distance(p) < q) {
                        return *id;
                    }
                }
            }
        }
        let id = sk.add_point(p);
        grid.entry((i, j)).or_default().push(id);
        id
    };
    let mut n = 0;
    for it in &d.items {
        let geom = match it.mapped(&f) {
            Item::Line(a, b) => {
                if a.distance(b) < 1e-9 {
                    continue;
                }
                Geom::Line { a: point(sk, a), b: point(sk, b) }
            }
            Item::Circle { c, r } if r > 0.0 => Geom::Circle { c: point(sk, c), r },
            Item::Arc { c, r, a0, a1 } if r > 0.0 => {
                let (pa, pb) = (c + DVec2::from_angle(a0) * r, c + DVec2::from_angle(a1) * r);
                if pa.distance(pb) < 1e-9 {
                    Geom::Circle { c: point(sk, c), r }
                } else {
                    Geom::Arc { c: point(sk, c), a: point(sk, pa), b: point(sk, pb) }
                }
            }
            Item::Ellipse { c, major, ratio, t0, t1 } => {
                let rmin = major.length() * ratio.abs();
                let (cc, a) = (point(sk, c), point(sk, c + major));
                if (t1 - t0).abs() >= TAU - 1e-9 {
                    Geom::Ellipse { c: cc, a, rmin }
                } else {
                    Geom::EllipticalArc { c: cc, a, rmin, t0, t1 }
                }
            }
            Item::Spline { pts, closed } => Geom::Spline { pts: pts.iter().map(|p| point(sk, *p)).collect(), closed },
            Item::Poly { pts, closed } if pts.len() >= 2 => Geom::Poly { pts: pts.iter().map(|p| [p.x, p.y]).collect(), closed },
            _ => continue,
        };
        sk.add_curve(geom, false);
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dxf_of(body: &str, units: i32) -> String {
        format!("0\nSECTION\n2\nHEADER\n9\n$INSUNITS\n70\n{units}\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n{body}0\nENDSEC\n0\nEOF\n")
    }

    #[test]
    fn reads_dxf_entities() {
        let body = "0\nLINE\n8\n0\n10\n0\n20\n0\n11\n10\n21\n0\n\
                    0\nARC\n10\n10\n20\n5\n40\n5\n50\n270\n51\n90\n\
                    0\nCIRCLE\n10\n30\n20\n30\n40\n2.5\n\
                    0\nLWPOLYLINE\n90\n2\n70\n0\n10\n0\n20\n10\n42\n1\n10\n10\n20\n10\n\
                    0\nTEXT\n1\nhello\n";
        let d = dxf(&dxf_of(body, 4)).unwrap();
        assert_eq!(d.items.len(), 4);
        assert_eq!(d.items[0], Item::Line(DVec2::ZERO, DVec2::new(10.0, 0.0)));
        let Item::Arc { c, r, a0, a1 } = d.items[1] else { panic!() };
        assert!((c - DVec2::new(10.0, 5.0)).length() < 1e-9 && (r - 5.0).abs() < 1e-9);
        assert!((DVec2::from_angle(a0) - DVec2::new(0.0, -1.0)).length() < 1e-9 && (DVec2::from_angle(a1) - DVec2::new(0.0, 1.0)).length() < 1e-9);
        // a bulge of 1 is a half circle, counter-clockwise from (0,10) to (10,10): centre (5,10), radius 5
        let Item::Arc { c, r, a0, .. } = d.items[3] else { panic!("{:?}", d.items[3]) };
        assert!((c - DVec2::new(5.0, 10.0)).length() < 1e-9 && (r - 5.0).abs() < 1e-9, "{c} {r}");
        assert!((DVec2::from_angle(a0) - DVec2::new(-1.0, 0.0)).length() < 1e-9);
        assert_eq!(d.skipped, vec![("TEXT".to_string(), 1)]);
        // inches
        let d = dxf(&dxf_of("0\nCIRCLE\n10\n1\n20\n1\n40\n0.5\n", 1)).unwrap();
        assert_eq!(d.items[0], Item::Circle { c: DVec2::splat(25.4), r: 12.7 });
    }

    #[test]
    fn dxf_blocks_and_closed_outline_into_a_sketch() {
        let src = "0\nSECTION\n2\nBLOCKS\n0\nBLOCK\n2\nSQ\n10\n0\n20\n0\n\
                   0\nLWPOLYLINE\n90\n4\n70\n1\n10\n0\n20\n0\n10\n4\n20\n0\n10\n4\n20\n4\n10\n0\n20\n4\n\
                   0\nENDBLK\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n\
                   0\nINSERT\n2\nSQ\n10\n10\n20\n0\n50\n90\n\
                   0\nENDSEC\n0\nEOF\n";
        let d = dxf(src).unwrap();
        assert_eq!(d.items.len(), 4);
        let mut sk = Sketch::default();
        assert_eq!(add_to_sketch(&mut sk, &d, 1.0, DVec2::ZERO), 4);
        // the square's corners are shared: 4 points, turned 90° about its corner at (10, 0)
        assert_eq!(sk.points.len(), 4);
        let mut xs: Vec<(i64, i64)> = sk.points.iter().map(|p| (p.x.round() as i64, p.y.round() as i64)).collect();
        xs.sort();
        assert_eq!(xs, vec![(6, 0), (6, 4), (10, 0), (10, 4)]);
        let regions = crate::regions::find_regions(&sk);
        assert_eq!(regions.len(), 1);
        assert!((regions[0].area - 16.0).abs() < 1e-6);
    }

    #[test]
    fn reads_svg_paths() {
        let svg_src = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="50mm" viewBox="0 0 100 50">
            <rect x="10" y="10" width="20" height="10"/>
            <path d="M 50 40 C 60 20 80 20 90 40 Z"/>
            <g transform="translate(5 0)"><circle cx="20" cy="40" r="5"/></g>
        </svg>"#;
        let d = svg(svg_src).unwrap();
        // the rectangle as 4 lines, the curve as an outline, the circle as a circle
        let lines = d.items.iter().filter(|i| matches!(i, Item::Line(..))).count();
        let polys: Vec<&Item> = d.items.iter().filter(|i| matches!(i, Item::Poly { .. })).collect();
        assert_eq!(lines, 4);
        assert_eq!(polys.len(), 1);
        // mm, Y up from the page's bottom: the rectangle spans y 30..40
        let ys: Vec<f64> = d.items.iter().filter_map(|i| if let Item::Line(a, _) = i { Some(a.y) } else { None }).collect();
        assert!(ys.iter().all(|y| (y - 30.0).abs() < 1e-3 || (y - 40.0).abs() < 1e-3), "{ys:?}");
        let Item::Poly { closed, .. } = polys[0] else { panic!() };
        assert!(*closed);
        let circle = d.items.iter().find_map(|i| if let Item::Circle { c, r } = i { Some((*c, *r)) } else { None }).expect("the circle stays a circle");
        assert!((circle.0 - DVec2::new(25.0, 10.0)).length() < 1e-3 && (circle.1 - 5.0).abs() < 1e-6, "{circle:?}");
    }

    #[test]
    fn svg_ellipses_and_arcs_stay_exact() {
        let svg_src = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="100mm" viewBox="0 0 100 100">
            <ellipse cx="50" cy="50" rx="20" ry="10" transform="rotate(30 50 50)"/>
            <path d="M 10 90 A 10 10 0 0 1 30 90 L 30 95 L 10 95 Z"/>
        </svg>"#;
        let d = svg(svg_src).unwrap();
        let Some(Item::Ellipse { c, major, ratio, .. }) = d.items.iter().find(|i| matches!(i, Item::Ellipse { .. })) else { panic!("{:?}", d.items) };
        assert!((*c - DVec2::new(50.0, 50.0)).length() < 1e-3, "{c}");
        assert!((major.length() - 20.0).abs() < 1e-2 && (ratio - 0.5).abs() < 1e-3, "{major} {ratio}");
        // rotated 30° clockwise on the page is -30° with Y up (or the opposite end of the axis)
        let ang = major.y.atan2(major.x).to_degrees().rem_euclid(180.0);
        assert!((ang - 150.0).abs() < 0.1, "{ang}");
        // the half circle is one arc, centre (20, 10) in mm with Y up, joined by three lines
        let arcs: Vec<&Item> = d.items.iter().filter(|i| matches!(i, Item::Arc { .. })).collect();
        assert_eq!(arcs.len(), 1, "{:?}", d.items);
        let Item::Arc { c, r, a0, a1 } = arcs[0] else { panic!() };
        assert!((*c - DVec2::new(20.0, 10.0)).length() < 1e-3 && (r - 10.0).abs() < 1e-3, "{c} {r}");
        assert!(((a1 - a0).rem_euclid(TAU) - std::f64::consts::PI).abs() < 1e-3);
        assert_eq!(d.items.iter().filter(|i| matches!(i, Item::Line(..))).count(), 3);
    }
}
