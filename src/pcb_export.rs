//! PCB output: the drawing lists both views are painted from (in the editor and as PNGs),
//! Gerber and Excellon files for fabrication, BOM and pick-and-place CSVs, and the 3D board
//! as a Part Studio.

use std::fmt::Write as _;

use crate::cam_paths::Contour;
use crate::pcb::{self, Fill, Label, LabelKind, Layer, Netlist, Pcb, WPad};
use crate::pcb_lib::{self, PadShape, G, P};

// ---------------------------------------------------------------- display lists

#[derive(Clone, Debug)]
pub enum Prim {
    /// A stroke `w` mm wide (0: a hairline).
    Seg { a: P, b: P, w: f64 },
    /// Filled contours, even-odd (holes cut out).
    Fill(Vec<Contour>),
    Circle { c: P, r: f64, w: f64, fill: bool },
}

#[derive(Clone, Debug)]
pub struct Draw {
    pub rgba: [u8; 4],
    pub prim: Prim,
}

fn d(rgba: [u8; 4], prim: Prim) -> Draw {
    Draw { rgba, prim }
}

pub mod colors {
    pub const TOP: [u8; 3] = [200, 52, 52];
    pub const BOTTOM: [u8; 3] = [64, 120, 205];
    pub const PAD: [u8; 3] = [198, 160, 60];
    pub const SILK_TOP: [u8; 3] = [236, 232, 170];
    pub const SILK_BOTTOM: [u8; 3] = [214, 170, 160];
    pub const EDGE: [u8; 3] = [222, 214, 80];
    pub const AIR: [u8; 3] = [150, 210, 255];
    pub const BOARD: [u8; 3] = [16, 42, 30];
    pub const WIRE: [u8; 3] = [0, 128, 0];
    pub const SYMBOL: [u8; 3] = [140, 20, 20];
    pub const PIN: [u8; 3] = [150, 40, 40];
    pub const TEXT: [u8; 3] = [0, 90, 130];
    pub const LABEL: [u8; 3] = [20, 20, 20];
    pub const POWER: [u8; 3] = [200, 0, 0];
}

fn a(c: [u8; 3], alpha: u8) -> [u8; 4] {
    [c[0], c[1], c[2], alpha]
}

/// Stroke text as segments.
pub fn text(out: &mut Vec<Draw>, s: &str, at: P, h: f64, anchor: u8, rot: f64, rgba: [u8; 4], xf: &dyn Fn(P) -> P) {
    let w = (h * 0.13).max(0.08);
    for line in pcb_lib::stroke_text(s, at, h, anchor, rot) {
        for s in line.windows(2) {
            out.push(d(rgba, Prim::Seg { a: xf(s[0]), b: xf(s[1]), w }));
        }
    }
}

fn graphic(out: &mut Vec<Draw>, g: &G, w: f64, rgba: [u8; 4], xf: &dyn Fn(P) -> P) {
    match g {
        G::Line(p, q) => out.push(d(rgba, Prim::Seg { a: xf(*p), b: xf(*q), w })),
        G::Rect(p, q) => {
            let c = [*p, [q[0], p[1]], *q, [p[0], q[1]], *p];
            for s in c.windows(2) {
                out.push(d(rgba, Prim::Seg { a: xf(s[0]), b: xf(s[1]), w }));
            }
        }
        G::Circle(c, r) => {
            // circles become polylines so mirrored and rotated placements stay right
            let n = 24;
            let pts: Vec<P> = (0..=n)
                .map(|i| {
                    let t = std::f64::consts::TAU * i as f64 / n as f64;
                    xf([c[0] + r * t.cos(), c[1] + r * t.sin()])
                })
                .collect();
            for s in pts.windows(2) {
                out.push(d(rgba, Prim::Seg { a: s[0], b: s[1], w }));
            }
        }
        G::Poly(pts, filled) => {
            let pts: Vec<P> = pts.iter().map(|p| xf(*p)).collect();
            if *filled {
                out.push(d(rgba, Prim::Fill(vec![pts])));
            } else {
                for s in pts.windows(2) {
                    out.push(d(rgba, Prim::Seg { a: s[0], b: s[1], w }));
                }
            }
        }
    }
}

/// What the board view shows.
#[derive(Clone, Copy, Debug)]
pub struct BoardView {
    /// The layer being edited is drawn on top, the other dimmed.
    pub active: Layer,
    pub top: bool,
    pub bottom: bool,
    pub silk: bool,
    pub zones: bool,
    pub air: bool,
}

impl Default for BoardView {
    fn default() -> Self {
        BoardView { active: Layer::Top, top: true, bottom: true, silk: true, zones: true, air: true }
    }
}

/// A pad's copper as filled contours (rounded rectangles get their corners).
pub fn pad_outline(p: &WPad, grow: f64) -> Contour {
    match p.shape {
        PadShape::Round => pcb::capsule(p.at, p.at, p.size[0].min(p.size[1]) / 2.0 + grow),
        PadShape::Oval => match p.cu() {
            pcb::Cu::Seg { a, b, r } => pcb::capsule(a, b, r + grow),
            pcb::Cu::Poly(c) => c,
        },
        PadShape::Rect => {
            let (w, h) = (p.size[0] / 2.0 + grow, p.size[1] / 2.0 + grow);
            [[-w, -h], [w, -h], [w, h], [-w, h]].iter().map(|c| rotate_add(*c, p.rot, p.at)).collect()
        }
        PadShape::RoundRect => {
            let (w, h) = (p.size[0] / 2.0 + grow, p.size[1] / 2.0 + grow);
            let r = (p.size[0].min(p.size[1]) * 0.25).min(0.25) + grow;
            let mut v = vec![];
            for (c, a0) in [([w - r, -h + r], -90.0f64), ([w - r, h - r], 0.0), ([-w + r, h - r], 90.0), ([-w + r, -h + r], 180.0)] {
                for i in 0..=4 {
                    let t = (a0 + 22.5 * i as f64).to_radians();
                    v.push(rotate_add([c[0] + r * t.cos(), c[1] + r * t.sin()], p.rot, p.at));
                }
            }
            v
        }
    }
}

fn rotate_add(p: P, deg: f64, at: P) -> P {
    let (s, c) = deg.to_radians().sin_cos();
    [at[0] + p[0] * c - p[1] * s, at[1] + p[0] * s + p[1] * c]
}

/// The board as a display list, back to front.
pub fn board_scene(p: &Pcb, nl: &Netlist, pads: &[WPad], fills: &[Fill], air: &[pcb::Airwire], v: &BoardView) -> Vec<Draw> {
    let mut out = vec![];
    let b = &p.board;
    if b.outline.len() >= 3 {
        out.push(d(a(colors::BOARD, 255), Prim::Fill(vec![b.outline.clone()])));
    }
    let id = |q: P| q;
    let layer_col = |l: Layer| if l == Layer::Top { colors::TOP } else { colors::BOTTOM };
    let order = [v.active.other(), v.active];
    for l in order {
        let shown = match l {
            Layer::Top => v.top,
            Layer::Bottom => v.bottom,
        };
        if !shown {
            continue;
        }
        let alpha = if l == v.active { 235 } else { 120 };
        let col = layer_col(l);
        if v.zones {
            for (zi, z) in b.zones.iter().enumerate().filter(|(_, z)| z.layer == l) {
                if let Some(f) = fills.get(zi) {
                    for piece in f {
                        out.push(d(a(col, (alpha as f32 * 0.55) as u8), Prim::Fill(piece.clone())));
                    }
                }
                for s in 0..z.outline.len() {
                    let (q, r) = (z.outline[s], z.outline[(s + 1) % z.outline.len()]);
                    out.push(d(a(col, alpha / 2), Prim::Seg { a: q, b: r, w: 0.0 }));
                }
            }
        }
        for t in b.tracks.iter().filter(|t| t.layer == l) {
            out.push(d(a(col, alpha), Prim::Seg { a: t.a, b: t.b, w: t.width }));
        }
        for pad in pads.iter().filter(|x| x.layer == Some(l)) {
            out.push(d(a(col, alpha), Prim::Fill(vec![pad_outline(pad, 0.0)])));
        }
    }
    let _ = nl;
    // plated holes and vias on top of both layers
    for pad in pads.iter().filter(|x| x.layer.is_none()) {
        out.push(d(a(colors::PAD, 255), Prim::Fill(vec![pad_outline(pad, 0.0)])));
        out.push(d([20, 20, 20, 255], Prim::Circle { c: pad.at, r: pad.drill / 2.0, w: 0.0, fill: true }));
    }
    for via in &b.vias {
        out.push(d([190, 190, 190, 255], Prim::Circle { c: via.at, r: via.d / 2.0, w: 0.0, fill: true }));
        out.push(d([30, 30, 30, 255], Prim::Circle { c: via.at, r: via.drill / 2.0, w: 0.0, fill: true }));
    }
    for h in &b.holes {
        out.push(d([8, 8, 8, 255], Prim::Circle { c: h.at, r: h.d / 2.0, w: 0.0, fill: true }));
        out.push(d(a(colors::EDGE, 255), Prim::Circle { c: h.at, r: h.d / 2.0, w: 0.1, fill: false }));
    }
    if v.silk {
        for side in order {
            for part in p.parts.iter().filter(|x| x.side() == side) {
                let col = if side == Layer::Top { colors::SILK_TOP } else { colors::SILK_BOTTOM };
                let alpha = if side == v.active { 255 } else { 150 };
                silk(&mut out, part, a(col, alpha));
            }
        }
    }
    for s in 0..b.outline.len() {
        let (q, r) = (b.outline[s], b.outline[(s + 1) % b.outline.len()]);
        out.push(d(a(colors::EDGE, 255), Prim::Seg { a: id(q), b: id(r), w: 0.15 }));
    }
    // keepouts: a dashed red outline with hatching
    for k in b.keepouts.iter().filter(|k| k.outline.len() >= 3) {
        let shown = match k.layer {
            Some(Layer::Top) => v.top,
            Some(Layer::Bottom) => v.bottom,
            None => v.top || v.bottom,
        };
        if !shown {
            continue;
        }
        let col = [220, 60, 200];
        let n = k.outline.len();
        for s in 0..n {
            let (q, r) = (k.outline[s], k.outline[(s + 1) % n]);
            let len = pcb::dist(q, r);
            let steps = (len / 0.8).ceil().max(1.0) as usize;
            for i in (0..steps).step_by(2) {
                let t0 = i as f64 / steps as f64;
                let t1 = ((i + 1) as f64 / steps as f64).min(1.0);
                let at = |t: f64| [q[0] + (r[0] - q[0]) * t, q[1] + (r[1] - q[1]) * t];
                out.push(d(a(col, 230), Prim::Seg { a: at(t0), b: at(t1), w: 0.15 }));
            }
        }
        // 45° hatching clipped to the outline (by scanline crossings)
        let (lo, hi) = pcb::bbox(&k.outline);
        let mut c = lo[0] - (hi[1] - lo[1]);
        while c < hi[0] {
            // the line y = x - c, i.e. points (c + y, y)
            let mut xs: Vec<f64> = vec![];
            for s in 0..n {
                let (q, r) = (k.outline[s], k.outline[(s + 1) % n]);
                let (fq, fr) = (q[0] - q[1] - c, r[0] - r[1] - c);
                if (fq > 0.0) != (fr > 0.0) {
                    let t = fq / (fq - fr);
                    xs.push(q[1] + (r[1] - q[1]) * t);
                }
            }
            xs.sort_by(f64::total_cmp);
            for pair in xs.chunks(2) {
                if let [y0, y1] = pair {
                    out.push(d(a(col, 90), Prim::Seg { a: [c + y0, *y0], b: [c + y1, *y1], w: 0.0 }));
                }
            }
            c += 1.0;
        }
    }
    if v.air {
        for w in air {
            out.push(d(a(colors::AIR, 220), Prim::Seg { a: w.a, b: w.b, w: 0.0 }));
        }
    }
    out
}

/// A part's silkscreen and reference, in board coordinates.
pub fn silk(out: &mut Vec<Draw>, part: &pcb::Part, rgba: [u8; 4]) {
    let fp = part.footprint();
    let xf = |q: P| part.xf(q);
    for g in &fp.silk {
        graphic(out, g, 0.15, rgba, &xf);
    }
    let (lo, hi) = fp.court;
    text(out, &part.reference, [(lo[0] + hi[0]) / 2.0, hi[1] + 0.35], 1.0, 1, 0.0, rgba, &xf);
}

/// The schematic as a display list.
pub fn schematic_scene(p: &Pcb, nl: &Netlist, highlight: Option<&str>) -> Vec<Draw> {
    let mut out = vec![];
    let pins = p.sch_pins();
    let hl_pins: Vec<(usize, String)> = highlight.and_then(|h| nl.by_name(h)).map(|n| nl.nets[n].pins.clone()).unwrap_or_default();
    for (i, part) in p.parts.iter().enumerate() {
        let sym = part.symbol();
        let xf = |q: P| part.sch_xf(q);
        for g in &sym.g {
            graphic(&mut out, g, 0.25, a(colors::SYMBOL, 255), &xf);
        }
        for sp in &sym.pins {
            let inner = [sp.at[0] + sp.dir[0] * sp.len, sp.at[1] + sp.dir[1] * sp.len];
            let hl = hl_pins.iter().any(|x| x.0 == i && x.1 == sp.num);
            out.push(d(a(if hl { [230, 120, 0] } else { colors::PIN }, 255), Prim::Seg { a: xf(sp.at), b: xf(inner), w: if hl { 0.4 } else { 0.2 } }));
            // pin numbers above the pin line, names inside the body
            let dir = part.sch_dir(sp.dir);
            let horizontal = dir[1].abs() < 0.5;
            let mid = xf([sp.at[0] + sp.dir[0] * sp.len / 2.0, sp.at[1] + sp.dir[1] * sp.len / 2.0]);
            let rot = if horizontal { 0.0 } else { 90.0 };
            if sym.pin_numbers {
                let off = if horizontal { [0.0, 0.3] } else { [-0.3, 0.0] };
                text(&mut out, &sp.num, pcb::add(mid, off), 1.0, 1, rot, a(colors::PIN, 255), &|q| q);
            }
            if sym.pin_names && !sp.name.is_empty() {
                let at = xf([inner[0] + sp.dir[0] * 0.6, inner[1] + sp.dir[1] * 0.6]);
                let (anchor, rot) = if horizontal { (if dir[0] > 0.0 { 0 } else { 2 }, 0.0) } else { (if dir[1] > 0.0 { 0 } else { 2 }, 90.0) };
                let off = if horizontal { [0.0, -0.6] } else { [0.6, 0.0] };
                text(&mut out, &sp.name, pcb::add(at, off), 1.2, anchor, rot, a(colors::TEXT, 255), &|q| q);
            }
        }
        // reference and value stay upright, beside the symbol
        let (lo, hi) = sch_bbox(part);
        let side = |y_of: f64| -> P { [hi[0] + 0.8, y_of] };
        let (r_at, v_at) = if part.sch_rot % 2 == 0 && !matches!(part.symbol.as_str(), "U" | "J" | "SW" | "Y" | "PICO" | "SERVO" | "ENC" | "STEPPER" | "STEPDRV" | "RV") {
            (side((lo[1] + hi[1]) / 2.0 + 0.4), side((lo[1] + hi[1]) / 2.0 - 2.0))
        } else {
            ([lo[0], hi[1] + 0.8], [lo[0], lo[1] - 2.2])
        };
        text(&mut out, &part.reference, r_at, 1.4, 0, 0.0, a(colors::TEXT, 255), &|q| q);
        text(&mut out, &part.value, v_at, 1.4, 0, 0.0, a(colors::TEXT, 255), &|q| q);
    }
    let hl_wire = |w: &pcb::Wire| -> bool {
        // a wire is highlighted when it touches a highlighted pin's point through the net
        highlight.is_some() && hl_pins.iter().any(|(pi, num)| pins.iter().any(|x| x.part == *pi && &x.num == num && pcb::seg_point(w.a, w.b, x.at) < 1e-3))
    };
    for w in &p.wires {
        let hl = hl_wire(w);
        out.push(d(a(if hl { [230, 120, 0] } else { colors::WIRE }, 255), Prim::Seg { a: w.a, b: w.b, w: if hl { 0.4 } else { 0.25 } }));
    }
    // junction dots where three or more wire ends or pins meet a wire
    for j in junctions(p, &pins) {
        out.push(d(a(colors::WIRE, 255), Prim::Circle { c: j, r: 0.45, w: 0.0, fill: true }));
    }
    for l in &p.labels {
        label(&mut out, l);
    }
    // no-connect marks: a blue cross
    for q in &p.no_connects {
        let r = 0.7;
        out.push(d([30, 60, 200, 255], Prim::Seg { a: [q[0] - r, q[1] - r], b: [q[0] + r, q[1] + r], w: 0.25 }));
        out.push(d([30, 60, 200, 255], Prim::Seg { a: [q[0] - r, q[1] + r], b: [q[0] + r, q[1] - r], w: 0.25 }));
    }
    // pins with nothing on them get a small open circle
    for sp in &pins {
        if nl.net_of(sp.part, &sp.num).is_none() && !p.no_connects.iter().any(|q| pcb::dist(*q, sp.at) < 1e-3) {
            out.push(d(a(colors::PIN, 160), Prim::Circle { c: sp.at, r: 0.3, w: 0.12, fill: false }));
        }
    }
    out
}

/// A symbol's extent on the sheet.
pub fn sch_bbox(part: &pcb::Part) -> (P, P) {
    let sym = part.symbol();
    let mut pts = vec![];
    for g in &sym.g {
        match g {
            G::Line(a, b) | G::Rect(a, b) => pts.extend([*a, *b]),
            G::Circle(c, r) => pts.extend([[c[0] - r, c[1] - r], [c[0] + r, c[1] + r]]),
            G::Poly(v, _) => pts.extend(v.iter().copied()),
        }
    }
    for p in &sym.pins {
        pts.push(p.at);
    }
    let pts: Vec<P> = pts.into_iter().map(|q| part.sch_xf(q)).collect();
    pcb::bbox(&pts)
}

pub fn junctions(p: &Pcb, pins: &[pcb::SchPin]) -> Vec<P> {
    let mut ends: Vec<P> = p.wires.iter().flat_map(|w| [w.a, w.b]).collect();
    ends.extend(pins.iter().map(|x| x.at));
    let mut out: Vec<P> = vec![];
    for e in &ends {
        if out.iter().any(|o| pcb::dist(*o, *e) < 1e-3) {
            continue;
        }
        let n = p.wires.iter().filter(|w| pcb::seg_point(w.a, w.b, *e) < 1e-3).count() + pins.iter().filter(|x| pcb::dist(x.at, *e) < 1e-3).count();
        // a T (a wire end on another wire's middle) or 3+ ends
        let mids = p.wires.iter().filter(|w| pcb::seg_point(w.a, w.b, *e) < 1e-3 && pcb::dist(w.a, *e) > 1e-3 && pcb::dist(w.b, *e) > 1e-3).count();
        if n >= 3 || (mids > 0 && n >= 2) {
            out.push(*e);
        }
    }
    out
}

pub fn label(out: &mut Vec<Draw>, l: &Label) {
    let c = l.at;
    match l.kind {
        LabelKind::Ground => {
            let col = a(colors::POWER, 255);
            out.push(d(col, Prim::Seg { a: c, b: [c[0], c[1] - 1.27], w: 0.25 }));
            for (k, hw) in [1.6, 1.0, 0.4].iter().enumerate() {
                let y = c[1] - 1.27 - k as f64 * 0.5;
                out.push(d(col, Prim::Seg { a: [c[0] - hw, y], b: [c[0] + hw, y], w: 0.25 }));
            }
            text(out, &l.text, [c[0], c[1] - 3.6], 1.2, 1, 0.0, col, &|q| q);
        }
        LabelKind::Power => {
            let col = a(colors::POWER, 255);
            out.push(d(col, Prim::Seg { a: c, b: [c[0], c[1] + 1.27], w: 0.25 }));
            out.push(d(col, Prim::Seg { a: [c[0] - 1.27, c[1] + 1.27], b: [c[0] + 1.27, c[1] + 1.27], w: 0.25 }));
            text(out, &l.text, [c[0], c[1] + 1.8], 1.2, 1, 0.0, col, &|q| q);
        }
        LabelKind::Net => {
            let col = a(colors::LABEL, 255);
            let (rot, anchor) = match l.rot % 4 {
                1 => (90.0, 0),
                2 => (0.0, 2),
                3 => (90.0, 2),
                _ => (0.0, 0),
            };
            let off = match l.rot % 4 {
                1 => [-0.4, 0.4],
                2 => [-0.4, 0.4],
                3 => [-0.4, -0.4],
                _ => [0.4, 0.4],
            };
            text(out, &l.text, pcb::add(c, off), 1.3, anchor, rot, col, &|q| q);
            out.push(d(a(colors::WIRE, 255), Prim::Circle { c, r: 0.25, w: 0.0, fill: true }));
        }
    }
}

/// Extent of a display list.
pub fn extent(list: &[Draw]) -> (P, P) {
    let mut pts = vec![];
    for x in list {
        match &x.prim {
            Prim::Seg { a, b, .. } => pts.extend([*a, *b]),
            Prim::Fill(cs) => pts.extend(cs.iter().flatten().copied()),
            Prim::Circle { c, r, .. } => pts.extend([[c[0] - r, c[1] - r], [c[0] + r, c[1] + r]]),
        }
    }
    pcb::bbox(&pts)
}

/// Rasterise a display list: `area` (lo, hi in mm) fills a `width` pixel wide image.
pub fn png(list: &[Draw], area: (P, P), width: u32, bg: [u8; 3]) -> Result<Vec<u8>, String> {
    use tiny_skia::{FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};
    let (lo, hi) = area;
    let size = [(hi[0] - lo[0]).max(1.0), (hi[1] - lo[1]).max(1.0)];
    let k = width as f64 / size[0];
    let height = ((size[1] * k).round() as u32).clamp(1, 8000);
    let mut pm = Pixmap::new(width, height).ok_or("bad image size")?;
    pm.fill(tiny_skia::Color::from_rgba8(bg[0], bg[1], bg[2], 255));
    let at = |q: P| (((q[0] - lo[0]) * k) as f32, ((hi[1] - q[1]) * k) as f32);
    for x in list {
        let mut paint = Paint::default();
        paint.set_color_rgba8(x.rgba[0], x.rgba[1], x.rgba[2], x.rgba[3]);
        paint.anti_alias = true;
        match &x.prim {
            Prim::Seg { a, b, w } => {
                let mut pb = PathBuilder::new();
                let (p, q) = (at(*a), at(*b));
                pb.move_to(p.0, p.1);
                pb.line_to(q.0 + if p == q { 0.01 } else { 0.0 }, q.1);
                let Some(path) = pb.finish() else { continue };
                let st = Stroke { width: ((*w * k) as f32).max(1.0), line_cap: LineCap::Round, ..Default::default() };
                pm.stroke_path(&path, &paint, &st, Transform::identity(), None);
            }
            Prim::Fill(cs) => {
                let mut pb = PathBuilder::new();
                for c in cs {
                    for (i, q) in c.iter().enumerate() {
                        let (u, v) = at(*q);
                        if i == 0 {
                            pb.move_to(u, v);
                        } else {
                            pb.line_to(u, v);
                        }
                    }
                    pb.close();
                }
                let Some(path) = pb.finish() else { continue };
                pm.fill_path(&path, &paint, FillRule::EvenOdd, Transform::identity(), None);
            }
            Prim::Circle { c, r, w, fill } => {
                let (u, v) = at(*c);
                let Some(path) = PathBuilder::from_circle(u, v, ((*r * k) as f32).max(0.5)) else { continue };
                if *fill {
                    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
                } else {
                    let st = Stroke { width: ((*w * k) as f32).max(1.0), ..Default::default() };
                    pm.stroke_path(&path, &paint, &st, Transform::identity(), None);
                }
            }
        }
    }
    pm.encode_png().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- Gerber

struct Gerber {
    body: String,
    apertures: Vec<String>,
}

impl Gerber {
    fn new() -> Gerber {
        Gerber { body: String::new(), apertures: vec![] }
    }
    fn ap(&mut self, def: String) -> usize {
        match self.apertures.iter().position(|x| *x == def) {
            Some(i) => i + 10,
            None => {
                self.apertures.push(def);
                self.apertures.len() + 9
            }
        }
    }
    fn select(&mut self, def: String) {
        let n = self.ap(def);
        let _ = writeln!(self.body, "D{n}*");
    }
    fn xy(p: P) -> String {
        format!("X{}Y{}", (p[0] * 1e6).round() as i64, (p[1] * 1e6).round() as i64)
    }
    fn line(&mut self, a: P, b: P, w: f64) {
        self.select(format!("C,{:.4}", w.max(0.01)));
        let _ = writeln!(self.body, "{}D02*", Self::xy(a));
        let _ = writeln!(self.body, "{}D01*", Self::xy(b));
    }
    fn flash_circle(&mut self, c: P, dia: f64) {
        self.select(format!("C,{:.4}", dia));
        let _ = writeln!(self.body, "{}D03*", Self::xy(c));
    }
    fn region(&mut self, c: &[P]) {
        if c.len() < 3 {
            return;
        }
        self.body += "G36*\n";
        let _ = writeln!(self.body, "{}D02*", Self::xy(c[0]));
        for q in &c[1..] {
            let _ = writeln!(self.body, "{}D01*", Self::xy(*q));
        }
        let _ = writeln!(self.body, "{}D01*", Self::xy(c[0]));
        self.body += "G37*\n";
    }
    fn polarity(&mut self, dark: bool) {
        self.body += if dark { "%LPD*%\n" } else { "%LPC*%\n" };
    }
    fn pad(&mut self, p: &WPad, grow: f64) {
        let square = (p.rot / 90.0 - (p.rot / 90.0).round()).abs() < 1e-6;
        let swap = square && ((p.rot / 90.0).round() as i64).rem_euclid(2) == 1;
        let (w, h) = if swap { (p.size[1], p.size[0]) } else { (p.size[0], p.size[1]) };
        let (w, h) = (w + 2.0 * grow, h + 2.0 * grow);
        match p.shape {
            PadShape::Round => self.flash_circle(p.at, p.size[0].min(p.size[1]) + 2.0 * grow),
            PadShape::Rect if square => {
                self.select(format!("R,{w:.4}X{h:.4}"));
                let _ = writeln!(self.body, "{}D03*", Self::xy(p.at));
            }
            PadShape::Oval if square => {
                self.select(format!("O,{w:.4}X{h:.4}"));
                let _ = writeln!(self.body, "{}D03*", Self::xy(p.at));
            }
            PadShape::Oval => {
                if let pcb::Cu::Seg { a, b, r } = p.cu() {
                    self.line(a, b, 2.0 * (r + grow));
                }
            }
            _ => self.region(&pad_outline(p, grow)),
        }
    }
    fn finish(self, function: &str, polarity: &str) -> String {
        let mut s = String::new();
        s += "G04 Made by rCAD*\n";
        s += "%TF.GenerationSoftware,rCAD,PCB*%\n";
        let _ = writeln!(s, "%TF.FileFunction,{function}*%");
        let _ = writeln!(s, "%TF.FilePolarity,{polarity}*%");
        s += "%FSLAX46Y46*%\n%MOMM*%\n";
        for (i, a) in self.apertures.iter().enumerate() {
            let _ = writeln!(s, "%ADD{}{}*%", i + 10, a);
        }
        s += "%LPD*%\nG01*\n";
        s += &self.body;
        s += "M02*\n";
        s
    }
}

fn copper_layer(p: &Pcb, pads: &[WPad], fills: &[Fill], l: Layer) -> String {
    let mut g = Gerber::new();
    // pours first (dark outer, clear holes), every piece by the size of its outer contour, biggest
    // first: a clear hole wipes whatever was drawn before it, and a pour (or island) inside a hole
    // is always smaller than the piece around it, so it is drawn after the hole is cleared
    let mut pieces: Vec<(f64, &Vec<Contour>)> = vec![];
    for (zi, _) in p.board.zones.iter().enumerate().filter(|(_, z)| z.layer == l) {
        for piece in fills.get(zi).into_iter().flatten().filter(|x| !x.is_empty()) {
            pieces.push((crate::cam_paths::area(&piece[0]).abs(), piece));
        }
    }
    pieces.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, piece) in pieces {
        g.polarity(true);
        g.region(&piece[0]);
        if piece.len() > 1 {
            g.polarity(false);
            for h in &piece[1..] {
                g.region(h);
            }
        }
    }
    g.polarity(true);
    for t in p.board.tracks.iter().filter(|t| t.layer == l) {
        g.line(t.a, t.b, t.width);
    }
    for pad in pads.iter().filter(|x| x.on(l)) {
        g.pad(pad, 0.0);
    }
    for v in &p.board.vias {
        g.flash_circle(v.at, v.d);
    }
    let f = if l == Layer::Top { "Copper,L1,Top" } else { "Copper,L2,Bot" };
    g.finish(f, "Positive")
}

fn mask_layer(pads: &[WPad], l: Layer) -> String {
    let mut g = Gerber::new();
    for pad in pads.iter().filter(|x| x.on(l)) {
        g.pad(pad, 0.05);
    }
    g.finish(if l == Layer::Top { "Soldermask,Top" } else { "Soldermask,Bot" }, "Negative")
}

fn paste_layer(pads: &[WPad], l: Layer) -> String {
    let mut g = Gerber::new();
    for pad in pads.iter().filter(|x| x.layer == Some(l)) {
        g.pad(pad, 0.0);
    }
    g.finish(if l == Layer::Top { "Paste,Top" } else { "Paste,Bot" }, "Positive")
}

fn silk_layer(p: &Pcb, l: Layer) -> String {
    let mut g = Gerber::new();
    let mut list = vec![];
    for part in p.parts.iter().filter(|x| x.side() == l) {
        silk(&mut list, part, [0; 4]);
    }
    for x in &list {
        match &x.prim {
            Prim::Seg { a, b, w } => g.line(*a, *b, w.max(0.15)),
            Prim::Fill(cs) => {
                for c in cs {
                    g.region(c);
                }
            }
            Prim::Circle { c, r, .. } => g.flash_circle(*c, 2.0 * r),
        }
    }
    g.finish(if l == Layer::Top { "Legend,Top" } else { "Legend,Bot" }, "Positive")
}

fn edge_layer(p: &Pcb) -> String {
    let mut g = Gerber::new();
    let o = &p.board.outline;
    for i in 0..o.len() {
        g.line(o[i], o[(i + 1) % o.len()], 0.1);
    }
    g.finish("Profile,NP", "Positive")
}

fn excellon(holes: &[(P, f64)], plated: bool) -> String {
    let mut sizes: Vec<f64> = vec![];
    for (_, dia) in holes {
        if !sizes.iter().any(|s| (s - dia).abs() < 1e-6) {
            sizes.push(*dia);
        }
    }
    sizes.sort_by(|a, b| a.total_cmp(b));
    let mut s = String::from("M48\n; Made by rCAD\n");
    let _ = writeln!(s, "; #@! TF.FileFunction,{},1,2,{}", if plated { "Plated" } else { "NonPlated" }, if plated { "PTH" } else { "NPTH" });
    s += "FMAT,2\nMETRIC\n";
    for (i, dia) in sizes.iter().enumerate() {
        let _ = writeln!(s, "T{}C{:.3}", i + 1, dia);
    }
    s += "%\nG05\n";
    for (i, dia) in sizes.iter().enumerate() {
        let _ = writeln!(s, "T{}", i + 1);
        for (at, _) in holes.iter().filter(|h| (h.1 - dia).abs() < 1e-6) {
            let _ = writeln!(s, "X{:.3}Y{:.3}", at[0], at[1]);
        }
    }
    s += "T0\nM30\n";
    s
}

/// Every fabrication file: (file name, contents).
pub fn gerbers(p: &Pcb) -> Vec<(String, String)> {
    let mut p = p.clone();
    let nl = p.netlist();
    p.retag_copper(&nl);
    let p = &p;
    let pads = p.pads(&nl);
    let fills = pcb::fill_zones(p, &nl, &pads);
    let base: String = p.name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let base = if base.is_empty() { "board".to_string() } else { base };
    let mut out = vec![
        (format!("{base}-F_Cu.gtl"), copper_layer(p, &pads, &fills, Layer::Top)),
        (format!("{base}-B_Cu.gbl"), copper_layer(p, &pads, &fills, Layer::Bottom)),
        (format!("{base}-F_Mask.gts"), mask_layer(&pads, Layer::Top)),
        (format!("{base}-B_Mask.gbs"), mask_layer(&pads, Layer::Bottom)),
        (format!("{base}-F_Paste.gtp"), paste_layer(&pads, Layer::Top)),
        (format!("{base}-B_Paste.gbp"), paste_layer(&pads, Layer::Bottom)),
        (format!("{base}-F_Silkscreen.gto"), silk_layer(p, Layer::Top)),
        (format!("{base}-B_Silkscreen.gbo"), silk_layer(p, Layer::Bottom)),
        (format!("{base}-Edge_Cuts.gm1"), edge_layer(p)),
    ];
    let mut pth: Vec<(P, f64)> = pads.iter().filter(|x| x.drill > 0.0).map(|x| (x.at, x.drill)).collect();
    pth.extend(p.board.vias.iter().map(|v| (v.at, v.drill)));
    let npth: Vec<(P, f64)> = p.board.holes.iter().map(|h| (h.at, h.d)).collect();
    out.push((format!("{base}-PTH.drl"), excellon(&pth, true)));
    if !npth.is_empty() {
        out.push((format!("{base}-NPTH.drl"), excellon(&npth, false)));
    }
    out
}

/// Bill of materials, grouped by value, footprint and part numbers. The first five columns are
/// JLCPCB's (LCSC is a part's "LCSC" field, or its MPN when that is an LCSC number like C25804);
/// then the manufacturer, MPN, Digi-Key number, description, datasheet and each extra field.
pub fn bom_csv(p: &Pcb) -> String {
    let lcsc = |part: &pcb::Part| {
        part.fields.iter().find(|(k, _)| k.eq_ignore_ascii_case("LCSC")).map(|f| f.1.clone()).unwrap_or_else(|| {
            let m = part.mpn.trim();
            let is_lcsc = m.len() > 1 && m.starts_with(['C', 'c']) && m[1..].chars().all(|c| c.is_ascii_digit());
            if is_lcsc || part.manufacturer.is_empty() && part.digikey.is_empty() { m.to_string() } else { String::new() }
        })
    };
    // extra field names, in first-seen order
    let mut extra: Vec<String> = vec![];
    for part in &p.parts {
        for (k, _) in &part.fields {
            if !k.trim().is_empty() && !k.eq_ignore_ascii_case("LCSC") && !extra.iter().any(|e| e.eq_ignore_ascii_case(k)) {
                extra.push(k.clone());
            }
        }
    }
    let row_of = |part: &pcb::Part| -> Vec<String> {
        let mut r = vec![part.value.clone(), part.footprint.clone(), lcsc(part), part.manufacturer.clone(), part.mpn.clone(), part.digikey.clone(), part.description.clone(), part.datasheet.clone()];
        for e in &extra {
            r.push(part.fields.iter().find(|(k, _)| k.eq_ignore_ascii_case(e)).map(|f| f.1.clone()).unwrap_or_default());
        }
        r
    };
    let mut groups: Vec<(Vec<String>, Vec<String>)> = vec![];
    for part in &p.parts {
        let r = row_of(part);
        match groups.iter_mut().find(|g| g.0 == r) {
            Some(g) => g.1.push(part.reference.clone()),
            None => groups.push((r, vec![part.reference.clone()])),
        }
    }
    for g in &mut groups {
        g.1.sort_by(|a, b| pcb::natural(a, b));
    }
    groups.sort_by(|a, b| pcb::natural(&a.1[0], &b.1[0]));
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut s = String::from("Comment,Designator,Footprint,LCSC,Quantity,Manufacturer,MPN,Digi-Key,Description,Datasheet");
    for e in &extra {
        s += &format!(",{}", q(e));
    }
    s.push('\n');
    for (r, refs) in groups {
        let _ = write!(s, "{},{},{},{},{}", q(&r[0]), q(&refs.join(",")), q(&r[1]), q(&r[2]), refs.len());
        for x in &r[3..] {
            let _ = write!(s, ",{}", q(x));
        }
        s.push('\n');
    }
    s
}

/// Pick and place: each part's centre, side and rotation.
pub fn cpl_csv(p: &Pcb) -> String {
    let mut s = String::from("Designator,Mid X,Mid Y,Layer,Rotation\n");
    let mut parts: Vec<&pcb::Part> = p.parts.iter().collect();
    parts.sort_by(|a, b| pcb::natural(&a.reference, &b.reference));
    for part in parts {
        let _ = writeln!(s, "{},{:.4}mm,{:.4}mm,{},{}", part.reference, part.at[0], part.at[1], if part.bottom { "Bottom" } else { "Top" }, (part.rot.rem_euclid(360.0) * 1000.0).round() / 1000.0);
    }
    s
}

/// A ZIP archive (deflated) of named files.
pub fn zip(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    use std::io::Write;
    let mut out: Vec<u8> = vec![];
    let mut central: Vec<u8> = vec![];
    let le16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let le32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    for (name, data) in files {
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let mut enc = flate2::write::DeflateEncoder::new(vec![], flate2::Compression::default());
        let _ = enc.write_all(data);
        let comp = enc.finish().unwrap_or_default();
        let off = out.len() as u32;
        // local header
        le32(&mut out, 0x04034b50);
        le16(&mut out, 20);
        le16(&mut out, 0);
        le16(&mut out, 8);
        le16(&mut out, 0);
        le16(&mut out, 0x21);
        le32(&mut out, crc.sum());
        le32(&mut out, comp.len() as u32);
        le32(&mut out, data.len() as u32);
        le16(&mut out, name.len() as u16);
        le16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&comp);
        // central directory entry
        le32(&mut central, 0x02014b50);
        le16(&mut central, 20);
        le16(&mut central, 20);
        le16(&mut central, 0);
        le16(&mut central, 8);
        le16(&mut central, 0);
        le16(&mut central, 0x21);
        le32(&mut central, crc.sum());
        le32(&mut central, comp.len() as u32);
        le32(&mut central, data.len() as u32);
        le16(&mut central, name.len() as u16);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le32(&mut central, 0);
        le32(&mut central, off);
        central.extend_from_slice(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    let cd_len = central.len() as u32;
    out.extend_from_slice(&central);
    le32(&mut out, 0x06054b50);
    le16(&mut out, 0);
    le16(&mut out, 0);
    le16(&mut out, files.len() as u16);
    le16(&mut out, files.len() as u16);
    le32(&mut out, cd_len);
    le32(&mut out, cd_off);
    le16(&mut out, 0);
    out
}

/// Write the fabrication outputs. A `.zip` path gets the Gerbers and drill files in one archive
/// (with the BOM and pick-and-place CSVs beside it); any other path is a folder for every file.
pub fn export(p: &Pcb, path: &std::path::Path) -> Result<Vec<String>, String> {
    let files: Vec<(String, Vec<u8>)> = gerbers(p).into_iter().map(|(n, s)| (n, s.into_bytes())).collect();
    let bom = bom_csv(p).into_bytes();
    let cpl = cpl_csv(p).into_bytes();
    let mut written = vec![];
    let write = |q: &std::path::Path, data: &[u8]| -> Result<(), String> {
        if let Some(dir) = q.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(q, data).map_err(|e| format!("{}: {e}", q.display()))
    };
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
        write(path, &zip(&files))?;
        written.push(path.display().to_string());
        let stem = path.with_extension("");
        for (suffix, data) in [("-BOM.csv", &bom), ("-CPL.csv", &cpl)] {
            let q = std::path::PathBuf::from(format!("{}{suffix}", stem.display()));
            write(&q, data)?;
            written.push(q.display().to_string());
        }
    } else {
        for (n, data) in files.iter().chain([("BOM.csv".to_string(), bom), ("CPL.csv".to_string(), cpl)].iter()) {
            let q = path.join(n);
            write(&q, data)?;
            written.push(q.display().to_string());
        }
    }
    Ok(written)
}

// ---------------------------------------------------------------- 3D

const BOARD_DEF: &str = "PCB board";
/// The value naming which PCB part (by id) a footprint feature is.
const PART_KEY: &str = "pcb_part";

fn fp_def_name(fp: &str) -> String {
    format!("PCB footprint {fp}")
}

fn num(v: f64) -> String {
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s == "-0" { "0".into() } else { s }
}

fn board_script(p: &Pcb) -> String {
    let nl = p.netlist();
    let pads = p.pads(&nl);
    let pts: Vec<String> = p.board.outline.iter().map(|q| format!("[{}, {}]", num(q[0]), num(q[1]))).collect();
    let mut holes: Vec<String> = pads.iter().filter(|x| x.drill > 0.0).map(|x| format!("[{}, {}, {}]", num(x.at[0]), num(x.at[1]), num(x.drill / 2.0))).collect();
    holes.extend(p.board.holes.iter().map(|h| format!("[{}, {}, {}]", num(h.at[0]), num(h.at[1]), num(h.d / 2.0))));
    format!(
        "// The board of PCB tab \"{}\", made by its \"3D board\" button: outline, plated and mounting holes.\n\
         let OUTLINE = [{}];\nlet HOLES = [{}];\n\
         let b = extrude(polygon(OUTLINE), vec3(0, 0, thickness));\n\
         let cuts = [];\nfor h in HOLES {{ cuts.push(cylinder(vec3(h[0], h[1], -1), vec3(0, 0, 1), h[2], thickness + 2)); }}\n\
         if cuts.len() > 0 {{ b = b.cut(cuts); }}\nb\n",
        p.name.replace('"', "'"),
        pts.join(", "),
        holes.join(", ")
    )
}

fn footprint_script(fp: &pcb_lib::Footprint) -> String {
    let mut s = format!("// {} body for PCB tabs (a simple stand-in shape).\nlet parts = [];\n", fp.name);
    for b in &fp.body {
        match b {
            pcb_lib::Solid::Box { lo, hi } => {
                let _ = writeln!(s, "parts.push(block(vec3({}, {}, {}), vec3({}, {}, {})));", num(lo[0]), num(lo[1]), num(lo[2]), num(hi[0]), num(hi[1]), num(hi[2]));
            }
            pcb_lib::Solid::Cyl { c, r, z0, h } => {
                let _ = writeln!(s, "parts.push(cylinder(vec3({}, {}, {}), vec3(0, 0, 1), {}, {}));", num(c[0]), num(c[1]), num(*z0), num(*r), num(*h));
            }
        }
    }
    s += "let s = if parts.len() == 1 { parts[0] } else { fuse(parts) };\n";
    s += "if bottom { s = s.mirror(vec3(0, 0, 0), vec3(1, 0, 0)).mirror(vec3(0, 0, 0), vec3(0, 0, 1)); }\n";
    s += "s.rotate(vec3(0, 0, 0), vec3(0, 0, 1), rot).translate(vec3(x, y, if bottom { 0.0 } else { thickness }))\n";
    s
}

/// Make or update the "<name> 3D" Part Studio: the board and a body for every part. Features
/// are kept by name, so assemblies that use the parts keep them. Returns the studio's index.
pub fn to_studio(ws: &mut crate::workspace::Workspace, pi: usize) -> Result<usize, String> {
    use crate::doc::{BoolOp, Op, PartProps};
    use crate::script::{CustomDef, ParamDef};
    let p = ws.pcbs.get(pi).ok_or("no such PCB tab")?.clone();
    let name = format!("{} 3D", p.name);
    let si = match ws.studios.iter().position(|s| s.name == name) {
        Some(i) => i,
        None => {
            let units = ws.studios.first().map(|s| s.doc.units).unwrap_or_default();
            ws.studios.push(crate::workspace::Studio { name: name.clone(), doc: crate::doc::Document { units, ..Default::default() } });
            ws.studios.len() - 1
        }
    };
    let doc = &mut ws.studios[si].doc;
    let pd = |n: &str, kind: &str, def: &str| ParamDef { name: n.into(), kind: kind.into(), default: def.into() };
    let set_def = |doc: &mut crate::doc::Document, def: CustomDef| match doc.custom_features.iter_mut().find(|d| d.name == def.name) {
        Some(d) => *d = def,
        None => doc.custom_features.push(def),
    };
    set_def(doc, CustomDef { name: BOARD_DEF.into(), params: vec![pd("thickness", "number", "1.6")], selection: String::new(), source: board_script(&p) });
    let mut kept: Vec<crate::doc::FeatId> = vec![];
    let ours = |op: &Op| matches!(op, Op::Custom { name, .. } if name == BOARD_DEF || name.starts_with("PCB footprint "));
    let part_of = |op: &Op| match op {
        Op::Custom { values, .. } => values.get(PART_KEY).cloned(),
        _ => None,
    };
    // a part's feature is found by the part's id (kept in its values; the kernel only reads the
    // definition's parameters), or in documents made before that by its title
    let mut upsert = |doc: &mut crate::doc::Document, title: &str, def: &str, values: std::collections::BTreeMap<String, String>, color: [u8; 3]| {
        let key = values.get(PART_KEY).cloned();
        let free = |f: &crate::doc::Feature| ours(&f.op) && !kept.contains(&f.id);
        let existing = key
            .as_ref()
            .and_then(|k| doc.features.iter().position(|f| free(f) && part_of(&f.op).as_ref() == Some(k)))
            .or_else(|| doc.features.iter().position(|f| free(f) && f.name == title && part_of(&f.op).is_none()));
        let op = Op::Custom { name: def.into(), values, refs: vec![], select: String::new(), op: BoolOp::New };
        let fid = match existing {
            Some(i) => {
                doc.features[i].op = op;
                doc.features[i].name = title.to_string();
                doc.features[i].id
            }
            None => doc.insert(title.to_string(), op),
        };
        kept.push(fid);
        let key = crate::doc::body_key((fid, 0));
        let props = doc.parts.entry(key).or_insert_with(PartProps::default);
        props.name = Some(title.to_string());
        props.color.get_or_insert(color);
    };
    let mut v = std::collections::BTreeMap::new();
    v.insert("thickness".to_string(), num(p.board.thickness));
    let mask = p.board.mask_rgb();
    upsert(doc, "Board", BOARD_DEF, v, mask);
    for part in &p.parts {
        let fp = part.footprint();
        let def = fp_def_name(&fp.name);
        set_def(
            doc,
            CustomDef {
                name: def.clone(),
                params: vec![pd("x", "number", "0"), pd("y", "number", "0"), pd("rot", "number", "0"), pd("bottom", "bool", "false"), pd("thickness", "number", "1.6")],
                selection: String::new(),
                source: footprint_script(&fp),
            },
        );
        let mut v = std::collections::BTreeMap::new();
        v.insert("x".to_string(), num(part.at[0]));
        v.insert("y".to_string(), num(part.at[1]));
        v.insert("rot".to_string(), num(part.rot));
        v.insert("bottom".to_string(), part.bottom.to_string());
        v.insert("thickness".to_string(), num(p.board.thickness));
        v.insert(PART_KEY.to_string(), part.id.to_string());
        let title = format!("{} {}", part.reference, part.value);
        upsert(doc, &title, &def, v, fp.color);
    }
    // parts gone from the PCB
    let gone: Vec<crate::doc::FeatId> = doc.features.iter().filter(|f| ours(&f.op) && !kept.contains(&f.id)).map(|f| f.id).collect();
    doc.features.retain(|f| !gone.contains(&f.id));
    Ok(si)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gerbers_have_the_layers() {
        let mut p = crate::pcb::tests::led_board();
        crate::pcb_route::autoroute(&mut p, None);
        p.board.zones.push(pcb::Zone { net: "GND".into(), layer: Layer::Bottom, outline: p.board.outline.clone(), ..Default::default() });
        p.board.holes.push(pcb::Hole { at: [3.0, 3.0], d: 3.2 });
        let files = gerbers(&p);
        let names: Vec<&str> = files.iter().map(|f| f.0.as_str()).collect();
        assert!(names.contains(&"LED-F_Cu.gtl") && names.contains(&"LED-NPTH.drl"), "{names:?}");
        let top = &files[0].1;
        assert!(top.contains("%FSLAX46Y46*%") && top.contains("D01*") && top.ends_with("M02*\n"));
        let bottom = &files[1].1;
        assert!(bottom.contains("G36*"), "the ground pour is a region");
        let drill = &files.iter().find(|f| f.0.ends_with("PTH.drl")).unwrap().1;
        assert!(drill.contains("T1C1.000") || drill.contains("C1.000"), "{drill}");
        let z = zip(&files.iter().map(|(n, s)| (n.clone(), s.clone().into_bytes())).collect::<Vec<_>>());
        assert_eq!(&z[..4], b"PK\x03\x04");
        assert!(bom_csv(&p).contains("\"330\",\"R1\",\"R_0805\""));
        // BOM columns from the part's fields; parts that differ only in MPN are separate lines
        let mut q = p.clone();
        let r = q.parts.iter().position(|x| x.reference == "R1").unwrap();
        q.parts[r].mpn = "RC0805FR-07330RL".into();
        q.parts[r].manufacturer = "YAGEO".into();
        q.parts[r].digikey = "311-330CRCT-ND".into();
        q.parts[r].fields = vec![("Tolerance".into(), "1%".into()), ("LCSC".into(), "C17630".into())];
        let bom = bom_csv(&q);
        let head = bom.lines().next().unwrap();
        assert_eq!(head, "Comment,Designator,Footprint,LCSC,Quantity,Manufacturer,MPN,Digi-Key,Description,Datasheet,\"Tolerance\"");
        let line = bom.lines().find(|l| l.contains("\"R1\"")).unwrap();
        assert!(line.contains("\"C17630\",1,\"YAGEO\",\"RC0805FR-07330RL\",\"311-330CRCT-ND\""), "{line}");
        assert!(line.ends_with("\"1%\""), "{line}");
        assert!(cpl_csv(&p).contains("R1,15.0000mm,20.0000mm,Top,0"));
    }

    #[test]
    fn nested_pours_survive_the_outer_pours_hole() {
        // a VCC ring round a bigger-than-the-ring GND pour, on one layer
        let mut p = Pcb::new("nest");
        let sq = |a: f64, b: f64| vec![[a, a], [b, a], [b, b], [a, b]];
        p.board.zones.push(pcb::Zone { net: "GND".into(), layer: Layer::Top, outline: sq(5.0, 45.0), ..Default::default() });
        p.board.zones.push(pcb::Zone { net: "VCC".into(), layer: Layer::Top, outline: sq(0.0, 50.0), ..Default::default() });
        let mut hole = sq(4.0, 46.0);
        hole.reverse();
        let fills: Vec<Fill> = vec![vec![vec![sq(5.0, 45.0)]], vec![vec![sq(0.5, 49.5), hole]]];
        let g = copper_layer(&p, &[], &fills, Layer::Top);
        let gnd = g.find("X5000000Y5000000D02*").expect("the GND pour is drawn");
        let clear = g.rfind("%LPC*%").expect("the ring's hole is cleared");
        assert!(gnd > clear, "{g}");
    }

    #[test]
    fn studio_features_follow_their_parts() {
        use crate::doc::Op;
        let mut ws = crate::workspace::Workspace::default();
        ws.pcbs.push(crate::pcb::tests::led_board());
        let si = to_studio(&mut ws, 0).unwrap();
        let fid = |ws: &crate::workspace::Workspace, name: &str| ws.studios[si].doc.features.iter().find(|f| f.name == name).map(|f| f.id);
        let r1 = fid(&ws, "R1 330").unwrap();
        // a new value keeps the feature (assemblies use it), under its new title
        ws.pcbs[0].parts[0].value = "470".into();
        to_studio(&mut ws, 0).unwrap();
        assert_eq!(fid(&ws, "R1 470"), Some(r1));
        // two parts with one title are two bodies
        let mut twin = ws.pcbs[0].parts[0].clone();
        twin.id = 99;
        twin.at = [40.0, 30.0];
        ws.pcbs[0].parts.push(twin);
        to_studio(&mut ws, 0).unwrap();
        assert_eq!(ws.studios[si].doc.features.iter().filter(|f| f.name == "R1 470").count(), 2);
        // documents from before the ids are matched by title
        for f in &mut ws.studios[si].doc.features {
            if let Op::Custom { values, .. } = &mut f.op {
                values.remove(PART_KEY);
            }
        }
        let n = ws.studios[si].doc.features.len();
        to_studio(&mut ws, 0).unwrap();
        assert_eq!(ws.studios[si].doc.features.len(), n);
        assert_eq!(fid(&ws, "R1 470"), Some(r1));
    }

    #[test]
    fn renders_both_views() {
        let p = crate::pcb::tests::led_board();
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let list = board_scene(&p, &nl, &pads, &[], &[], &BoardView::default());
        let png_bytes = png(&list, p.board.bbox(), 400, [0, 0, 0]).unwrap();
        assert_eq!(&png_bytes[1..4], b"PNG");
        let s = schematic_scene(&p, &nl, None);
        assert!(s.len() > 20);
    }
}
