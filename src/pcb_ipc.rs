//! IPC-7351B footprint generator (Altium's IPC Compliant Footprint Wizard): land patterns
//! computed from the package's datasheet dimensions with the standard's toe, heel and side
//! fillet goals for density level M (most), N (nominal) or L (least), named the IPC way
//! (`RESC2012X50N`, `SOIC127P600X175-8N`, `QFN50P400X400X90-17N`, …).

use serde::{Deserialize, Serialize};

use crate::pcb_lib::{Footprint, Pad, PadShape, Solid, G, P};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    /// Two-terminal chips (resistors, capacitors, inductors, diodes, LEDs).
    #[default]
    Chip,
    /// Gull-wing leads on two sides (SOIC, SOP, TSSOP, MSOP).
    Sop,
    /// Gull-wing leads on four sides.
    Qfp,
    /// No-lead terminals on two sides (DFN, SON).
    Son,
    /// No-lead terminals on four sides.
    Qfn,
    /// Ball grid array.
    Bga,
}

/// Package dimensions in mm (nominal values; the tolerances are the full min-to-max range).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IpcSpec {
    pub family: Family,
    /// Chips: R, C, L, D or LED (sets the name prefix and colour).
    pub kind: String,
    /// Chips: an EIA size (0402, 0603, 0805, 1206, 1210, 2010, 2512) that fills body, lead and
    /// height.
    pub size: String,
    /// Leads in total (exposed pad not counted). BGA: rows × columns are used instead.
    pub pins: usize,
    pub pitch: f64,
    /// Body length (x) and width (y).
    #[serde(deserialize_with = "de_pair")]
    pub body: P,
    /// Toe-to-toe lead span across x and y (gull-wing; for no-lead parts the body is used).
    #[serde(deserialize_with = "de_pair")]
    pub span: P,
    /// Terminal (lead foot) length.
    pub lead: f64,
    /// Lead width.
    pub lead_w: f64,
    pub height: f64,
    /// Exposed thermal pad size (QFN, SON, some SOPs); [0, 0] for none.
    #[serde(deserialize_with = "de_pair")]
    pub ep: P,
    pub span_tol: f64,
    pub lead_tol: f64,
    pub width_tol: f64,
    /// BGA ball diameter, rows and columns.
    pub ball: f64,
    pub rows: usize,
    pub cols: usize,
    /// M, N or L.
    pub density: char,
}

impl Default for IpcSpec {
    fn default() -> Self {
        IpcSpec {
            family: Family::Chip,
            kind: "R".into(),
            size: String::new(),
            pins: 0,
            pitch: 0.0,
            body: [0.0; 2],
            span: [0.0; 2],
            lead: 0.0,
            lead_w: 0.0,
            height: 0.0,
            ep: [0.0; 2],
            span_tol: 0.2,
            lead_tol: 0.3,
            width_tol: 0.1,
            ball: 0.0,
            rows: 0,
            cols: 0,
            density: 'N',
        }
    }
}

/// A pair given as [x, y], [x] or x (a square).
fn de_pair<'de, D: serde::Deserializer<'de>>(d: D) -> Result<P, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        N(f64),
        L(Vec<f64>),
    }
    Ok(match V::deserialize(d)? {
        V::N(x) => [x, x],
        V::L(v) => match v.as_slice() {
            [] => [0.0, 0.0],
            [x] => [*x, 0.0],
            [x, y, ..] => [*x, *y],
        },
    })
}

/// Board fabrication and part placement tolerances (mm).
const F: f64 = 0.05;
const PL: f64 = 0.05;

/// (toe, heel, side, courtyard excess) fillet goals.
fn goals(fam: Family, pitch: f64, small_chip: bool, d: char) -> (f64, f64, f64, f64) {
    let k = match d {
        'M' => 0,
        'L' => 2,
        _ => 1,
    };
    let pick = |v: [f64; 3]| v[k];
    match fam {
        Family::Chip if small_chip => (pick([0.30, 0.20, 0.10]), 0.0, pick([0.05, 0.0, -0.05]), pick([0.2, 0.15, 0.1])),
        Family::Chip => (pick([0.55, 0.35, 0.15]), 0.0, pick([0.05, 0.0, -0.05]), pick([0.5, 0.25, 0.1])),
        Family::Sop | Family::Qfp if pitch > 0.625 => (pick([0.55, 0.35, 0.15]), pick([0.45, 0.35, 0.25]), pick([0.05, 0.03, 0.01]), pick([0.5, 0.25, 0.1])),
        Family::Sop | Family::Qfp => (pick([0.55, 0.35, 0.15]), pick([0.45, 0.35, 0.25]), pick([0.01, -0.02, -0.04]), pick([0.5, 0.25, 0.1])),
        Family::Son | Family::Qfn => (pick([0.4, 0.3, 0.2]), 0.0, -0.04, pick([0.5, 0.25, 0.1])),
        Family::Bga => (0.0, 0.0, 0.0, pick([1.0, 0.5, 0.25])),
    }
}

/// Land (outer edge Z, inner edge G, width X) from the lead span L, terminal T and width W.
fn land(l: f64, tl: f64, t: f64, tt: f64, w: f64, tw: f64, g: (f64, f64, f64, f64)) -> (f64, f64, f64) {
    let (lmin, lmax) = (l - tl / 2.0, l + tl / 2.0);
    let (tmin, tmax) = (t - tt / 2.0, t + tt / 2.0);
    let (smin, smax) = (lmin - 2.0 * tmax, lmax - 2.0 * tmin);
    let rms = |c: f64| (c * c + F * F + PL * PL).sqrt();
    let z = lmin + 2.0 * g.0 + rms(tl);
    let gg = (smax - 2.0 * g.1 - rms(smax - smin)).max(0.2);
    let x = (w - tw / 2.0) + 2.0 * g.2 + rms(tw);
    (r2(z), r2(gg), r2(x))
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Hundredths of a millimetre, as IPC names write dimensions.
fn h(v: f64) -> i64 {
    (v * 100.0).round() as i64
}

fn chip_size(s: &str) -> Option<(f64, f64, f64, f64)> {
    // (L, W, terminal, height)
    Some(match s {
        "0201" => (0.6, 0.3, 0.15, 0.3),
        "0402" => (1.0, 0.5, 0.25, 0.35),
        "0603" => (1.6, 0.8, 0.35, 0.45),
        "0805" => (2.0, 1.25, 0.4, 0.5),
        "1206" => (3.2, 1.6, 0.5, 0.55),
        "1210" => (3.2, 2.5, 0.5, 0.55),
        "2010" => (5.0, 2.5, 0.6, 0.55),
        "2512" => (6.4, 3.2, 0.6, 0.55),
        _ => return None,
    })
}

fn pad(num: impl ToString, at: P, size: P, shape: PadShape) -> Pad {
    Pad { num: num.to_string(), at: [r2(at[0] * 10.0) / 10.0, r2(at[1] * 10.0) / 10.0], size, shape, drill: 0.0 }
}

/// BGA row letters, skipping I, O, Q, S, X and Z (JEDEC).
fn row_name(i: usize) -> String {
    const L: &[u8] = b"ABCDEFGHJKLMNPRTUVWY";
    if i < L.len() {
        (L[i] as char).to_string()
    } else {
        format!("{}{}", L[i / L.len() - 1] as char, L[i % L.len()] as char)
    }
}

/// Build the land pattern.
pub fn generate(spec: &IpcSpec) -> Result<Footprint, String> {
    let mut s = spec.clone();
    let d = s.density.to_ascii_uppercase();
    if !matches!(d, 'M' | 'N' | 'L') {
        return Err("density is M (most), N (nominal) or L (least)".into());
    }
    let mut f = Footprint { name: String::new(), pads: vec![], silk: vec![], court: ([0.0; 2], [0.0; 2]), body: vec![], color: [36, 36, 40] };
    let need = |v: f64, what: &str| if v > 0.0 { Ok(()) } else { Err(format!("give {what}")) };
    let silk_w = 0.15;
    match s.family {
        Family::Chip => {
            if let Some((l, w, t, ht)) = chip_size(s.size.trim()) {
                if s.body == [0.0; 2] {
                    s.body = [l, w];
                }
                if s.lead == 0.0 {
                    s.lead = t;
                }
                if s.height == 0.0 {
                    s.height = ht;
                }
            } else if !s.size.trim().is_empty() {
                return Err(format!("unknown chip size {}; give 0201, 0402, 0603, 0805, 1206, 1210, 2010 or 2512, or body/lead/height", s.size));
            }
            need(s.body[0], "body [length, width]")?;
            need(s.lead, "lead (terminal length)")?;
            need(s.height, "height")?;
            let g = goals(Family::Chip, 0.0, s.body[0] < 1.6, d);
            let (z, gg, x) = land(s.body[0], s.span_tol.min(0.2), s.lead, s.lead_tol.min(0.2), s.body[1], s.width_tol.max(0.1), g);
            let y = r2((z - gg) / 2.0);
            let c = (z + gg) / 4.0;
            f.pads = vec![pad(1, [-c, 0.0], [y, x], PadShape::RoundRect), pad(2, [c, 0.0], [y, x], PadShape::RoundRect)];
            let pre = match s.kind.to_ascii_uppercase().as_str() {
                "C" => "CAPC",
                "L" => "INDC",
                "D" => "DIOC",
                "LED" => "LEDC",
                "F" => "FUSC",
                _ => "RESC",
            };
            f.color = match pre {
                "CAPC" => [176, 140, 96],
                "INDC" => [70, 70, 74],
                "LEDC" => [235, 235, 225],
                _ => [30, 30, 32],
            };
            f.name = format!("{pre}{:02}{:02}X{}{d}", (s.body[0] * 10.0 + 1e-6).floor() as i64, (s.body[1] * 10.0 + 1e-6).floor() as i64, h(s.height));
            let sy = x.max(s.body[1]) / 2.0 + silk_w;
            let sx = gg / 2.0 - 0.2;
            if sx > 0.2 {
                f.silk = vec![G::Line([-sx, sy], [sx, sy]), G::Line([-sx, -sy], [sx, -sy])];
            }
            if matches!(pre, "DIOC" | "LEDC") {
                // cathode (pad 1) bar
                let xx = -z / 2.0 - 0.2;
                f.silk = vec![G::Line([xx, sy], [sx.max(0.0), sy]), G::Line([xx, -sy], [sx.max(0.0), -sy]), G::Line([xx, -sy], [xx, sy])];
            }
            f.body = vec![Solid::Box { lo: [-s.body[0] / 2.0, -s.body[1] / 2.0, 0.0], hi: [s.body[0] / 2.0, s.body[1] / 2.0, s.height] }];
            court(&mut f, s.body, g.3);
        }
        Family::Sop | Family::Son | Family::Qfp | Family::Qfn => {
            need(s.pitch, "pitch")?;
            need(s.body[0], "body [length, width]")?;
            need(s.lead, "lead (terminal length)")?;
            need(s.lead_w, "lead_w (lead width)")?;
            need(s.height, "height")?;
            let quad = matches!(s.family, Family::Qfp | Family::Qfn);
            let sides = if quad { 4 } else { 2 };
            if s.pins == 0 || s.pins % sides != 0 {
                return Err(format!("pins must be a multiple of {sides}"));
            }
            if s.body[1] == 0.0 {
                s.body[1] = s.body[0];
            }
            let leaded = matches!(s.family, Family::Sop | Family::Qfp);
            // the span across the pads of each row pair: gull-wing toe to toe, no-lead the body
            let mut span = if leaded { s.span } else { s.body };
            if leaded {
                need(span[0], "span (toe-to-toe lead span)")?;
                if span[1] == 0.0 {
                    span[1] = span[0];
                }
            }
            let g = goals(s.family, s.pitch, false, d);
            let n = s.pins / sides;
            let mk = |l: f64| {
                let (z, gg, x) = land(l, if leaded { s.span_tol } else { s.span_tol.min(0.1) }, s.lead, if leaded { s.lead_tol } else { s.lead_tol.min(0.1) }, s.lead_w, s.width_tol, g);
                // keep at least 0.15 mm of copper-to-copper between neighbours
                (z, gg, x.min(r2(s.pitch - 0.15)))
            };
            // rows across x (left and right) use span[0]; across y (top and bottom) span[1]
            let (zx, mut gx, xx) = mk(span[0]);
            let (zy, mut gy, xy) = mk(span[1]);
            let run = (n - 1) as f64 * s.pitch;
            if quad {
                // the inner ends stay clear of the neighbouring rows' corner pads
                gx = gx.max(r2(run + xy + 0.32));
                gy = gy.max(r2(run + xx + 0.32));
            }
            let (yx, cx) = (r2((zx - gx) / 2.0), (zx + gx) / 4.0);
            let (yy, cy) = (r2((zy - gy) / 2.0), (zy + gy) / 4.0);
            let mut k = 1;
            // anticlockwise from pin 1 at the top of the left row
            for i in 0..n {
                f.pads.push(pad(k, [-cx, run / 2.0 - i as f64 * s.pitch], [yx, xx], PadShape::RoundRect));
                k += 1;
            }
            if quad {
                for i in 0..n {
                    f.pads.push(pad(k, [-run / 2.0 + i as f64 * s.pitch, -cy], [xy, yy], PadShape::RoundRect));
                    k += 1;
                }
            }
            for i in 0..n {
                f.pads.push(pad(k, [cx, -run / 2.0 + i as f64 * s.pitch], [yx, xx], PadShape::RoundRect));
                k += 1;
            }
            if quad {
                for i in 0..n {
                    f.pads.push(pad(k, [run / 2.0 - i as f64 * s.pitch, cy], [xy, yy], PadShape::RoundRect));
                    k += 1;
                }
            }
            let has_ep = s.ep[0] > 0.0 && s.ep[1] > 0.0;
            if has_ep {
                f.pads.push(pad(k, [0.0, 0.0], s.ep, PadShape::Rect));
            }
            let total = s.pins + has_ep as usize;
            f.name = match s.family {
                Family::Sop => format!("{}{}P{}X{}-{total}{d}", if (s.pitch - 1.27).abs() < 1e-6 { "SOIC" } else { "SOP" }, h(s.pitch), h(span[0]), h(s.height)),
                Family::Qfp => format!("QFP{}P{}X{}X{}-{total}{d}", h(s.pitch), h(span[0]), h(span[1]), h(s.height)),
                Family::Son => format!("SON{}P{}X{}X{}-{total}{d}", h(s.pitch), h(s.body[0]), h(s.body[1]), h(s.height)),
                _ => format!("QFN{}P{}X{}X{}-{total}{d}", h(s.pitch), h(s.body[0]), h(s.body[1]), h(s.height)),
            };
            // silkscreen: body outline clear of the pads, and a pin-1 dot
            let (bx, by) = (s.body[0] / 2.0 + silk_w / 2.0, s.body[1] / 2.0 + silk_w / 2.0);
            let pad_x = cx - yx / 2.0 - 0.2;
            let row_y = run / 2.0 + xx / 2.0 + 0.2;
            if quad {
                let row_x = run / 2.0 + xy / 2.0 + 0.2;
                let pad_y = cy - yy / 2.0 - 0.2;
                for (sx, sy) in [(-1.0, 1.0), (1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)] {
                    let c = [sx * bx.min(pad_x + 0.4), sy * by.min(pad_y + 0.4)];
                    f.silk.push(G::Poly(vec![[sx * row_x.min(c[0].abs()), c[1]], c, [c[0], sy * row_y.min(c[1].abs())]], false));
                }
            } else if leaded || bx < pad_x {
                let x0 = bx.min(pad_x);
                f.silk.push(G::Line([-x0, by.max(row_y)], [x0, by.max(row_y)]));
                f.silk.push(G::Line([-x0, -by.max(row_y)], [x0, -by.max(row_y)]));
            } else {
                f.silk.push(G::Line([-bx, by.max(row_y)], [bx, by.max(row_y)]));
                f.silk.push(G::Line([-bx, -by.max(row_y)], [bx, -by.max(row_y)]));
            }
            f.silk.push(G::Circle([-zx / 2.0 - 0.3, run / 2.0 + xx / 2.0 + 0.1], 0.15));
            f.body = vec![Solid::Box { lo: [-s.body[0] / 2.0, -s.body[1] / 2.0, if leaded { 0.1 } else { 0.0 }], hi: [s.body[0] / 2.0, s.body[1] / 2.0, s.height] }];
            if leaded {
                for p in f.pads.iter().take(s.pins) {
                    let (lo, hi) = (
                        [p.at[0].min(p.at[0].signum() * s.body[0] / 2.0), p.at[1] - s.lead_w / 2.0],
                        [p.at[0].max(p.at[0].signum() * s.body[0] / 2.0), p.at[1] + s.lead_w / 2.0],
                    );
                    let (lo, hi) = if p.size[0] < p.size[1] && quad {
                        ([p.at[0] - s.lead_w / 2.0, p.at[1].min(p.at[1].signum() * s.body[1] / 2.0)], [p.at[0] + s.lead_w / 2.0, p.at[1].max(p.at[1].signum() * s.body[1] / 2.0)])
                    } else {
                        (lo, hi)
                    };
                    f.body.push(Solid::Box { lo: [lo[0], lo[1], 0.0], hi: [hi[0], hi[1], 0.2] });
                }
            }
            court(&mut f, s.body, g.3);
        }
        Family::Bga => {
            need(s.pitch, "pitch")?;
            need(s.ball, "ball (ball diameter)")?;
            need(s.height, "height")?;
            if s.rows == 0 || s.cols == 0 {
                return Err("give rows and cols".into());
            }
            if s.body[0] == 0.0 {
                s.body = [s.cols as f64 * s.pitch + 1.0, s.rows as f64 * s.pitch + 1.0];
            }
            if s.body[1] == 0.0 {
                s.body[1] = s.body[0];
            }
            // non-solder-mask-defined lands a little smaller than the ball
            let k = match d {
                'M' => 0.85,
                'L' => 0.75,
                _ => 0.8,
            };
            let dpad = r2(s.ball * k);
            let (w, hh) = ((s.cols - 1) as f64 * s.pitch, (s.rows - 1) as f64 * s.pitch);
            for r in 0..s.rows {
                for c in 0..s.cols {
                    f.pads.push(pad(format!("{}{}", row_name(r), c + 1), [-w / 2.0 + c as f64 * s.pitch, hh / 2.0 - r as f64 * s.pitch], [dpad, dpad], PadShape::Round));
                }
            }
            f.name = format!("BGA{}C{}P{}X{}_{}X{}X{}{d}", s.rows * s.cols, h(s.pitch), s.cols, s.rows, h(s.body[0]), h(s.body[1]), h(s.height));
            let (bx, by) = (s.body[0] / 2.0, s.body[1] / 2.0);
            let ch = 1.0f64.min(bx / 3.0);
            f.silk = vec![G::Poly(vec![[-bx + ch, by], [bx, by], [bx, -by], [-bx, -by], [-bx, by - ch], [-bx + ch, by]], false)];
            f.body = vec![Solid::Box { lo: [-bx, -by, 0.0], hi: [bx, by, s.height] }];
            court(&mut f, s.body, goals(Family::Bga, 0.0, false, d).3);
        }
    }
    Ok(f)
}

/// Courtyard: pads and body grown by the excess, on a 0.05 mm grid.
fn court(f: &mut Footprint, body: P, excess: f64) {
    let mut lo = [-body[0] / 2.0, -body[1] / 2.0];
    let mut hi = [body[0] / 2.0, body[1] / 2.0];
    for p in &f.pads {
        lo[0] = lo[0].min(p.at[0] - p.size[0] / 2.0);
        lo[1] = lo[1].min(p.at[1] - p.size[1] / 2.0);
        hi[0] = hi[0].max(p.at[0] + p.size[0] / 2.0);
        hi[1] = hi[1].max(p.at[1] + p.size[1] / 2.0);
    }
    let g = |v: f64, up: bool| if up { (v / 0.05).ceil() * 0.05 } else { (v / 0.05).floor() * 0.05 };
    f.court = ([g(lo[0] - excess, false), g(lo[1] - excess, false)], [g(hi[0] + excess, true), g(hi[1] + excess, true)]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gap(a: &Pad, b: &Pad) -> f64 {
        let dx = (a.at[0] - b.at[0]).abs() - (a.size[0] + b.size[0]) / 2.0;
        let dy = (a.at[1] - b.at[1]).abs() - (a.size[1] + b.size[1]) / 2.0;
        dx.max(dy)
    }

    #[test]
    fn chips_match_the_ipc_tables() {
        let f = generate(&IpcSpec { size: "0805".into(), ..Default::default() }).unwrap();
        assert_eq!(f.name, "RESC2012X50N");
        // IPC-7351B nominal 0805: about 1.0 × 1.4 mm lands, 1.9 mm apart
        let p = &f.pads[1];
        assert!((p.size[0] - 1.0).abs() < 0.2 && (p.size[1] - 1.4).abs() < 0.15, "{:?}", p.size);
        assert!((2.0 * p.at[0] - 1.9).abs() < 0.2, "{:?}", p.at);
        let m = generate(&IpcSpec { size: "0805".into(), density: 'M', kind: "C".into(), ..Default::default() }).unwrap();
        assert_eq!(m.name, "CAPC2012X50M");
        assert!(m.pads[1].size[0] > p.size[0]);
    }

    #[test]
    fn soic_qfn_and_bga() {
        // SOIC-8: 1.27 pitch, 6.0 span, 0.835 feet, 0.41 leads, 1.75 high
        let f = generate(&IpcSpec { family: Family::Sop, pins: 8, pitch: 1.27, body: [3.9, 4.9], span: [6.0, 0.0], lead: 0.835, lead_w: 0.41, height: 1.75, ..Default::default() }).unwrap();
        assert_eq!(f.name, "SOIC127P600X175-8N");
        assert_eq!(f.pads.len(), 8);
        // close to the IPC land: about 1.55 × 0.6 at ±2.7
        assert!((f.pads[0].size[0] - 1.55).abs() < 0.3 && (f.pads[0].size[1] - 0.6).abs() < 0.1, "{:?}", f.pads[0]);
        assert!((f.pads[0].at[0] + 2.7).abs() < 0.2 && f.pads[0].at[1] > 0.0, "{:?}", f.pads[0].at);
        assert_eq!(f.pads[4].num, "5");
        assert!(f.pads[4].at[0] > 0.0 && f.pads[4].at[1] < 0.0);
        let q = generate(&IpcSpec { family: Family::Qfn, pins: 16, pitch: 0.5, body: [3.0, 3.0], lead: 0.4, lead_w: 0.25, height: 0.9, ep: [1.7, 1.7], ..Default::default() }).unwrap();
        assert_eq!(q.name, "QFN50P300X300X90-17N");
        assert_eq!(q.pads.len(), 17);
        for (i, a) in q.pads.iter().enumerate() {
            for b in q.pads.iter().skip(i + 1) {
                assert!(gap(a, b) >= 0.149, "{} {} {}", a.num, b.num, gap(a, b));
            }
        }
        let b = generate(&IpcSpec { family: Family::Bga, pitch: 0.8, ball: 0.4, rows: 10, cols: 10, height: 1.2, ..Default::default() }).unwrap();
        assert_eq!(b.pads.len(), 100);
        assert_eq!(b.pads[0].num, "A1");
        assert_eq!(b.pads[99].num, "K10");
        assert!(b.pads[0].at[0] < 0.0 && b.pads[0].at[1] > 0.0);
    }
}

/// An IPC land pattern for a KiCad standard package name, for symbols whose footprint was not
/// imported: `QFN-56-1EP_7x7mm_P0.4mm_EP3.2x3.2mm`, `SOIC-8_3.9x4.9mm_P1.27mm`,
/// `TSSOP-20_4.4x6.5mm_P0.65mm`, `LQFP-48_7x7mm_P0.5mm`, `DFN-8-1EP_3x3mm_P0.5mm_EP1.6x2.4mm`,
/// `R_0603_1608Metric`.
pub fn from_kicad_name(name: &str) -> Option<IpcSpec> {
    let mut parts = name.split('_');
    let head = parts.next()?;
    if let Some(size) = parts.clone().next().filter(|s| chip_size(s).is_some()) {
        let kind = match head {
            "R" | "C" | "L" | "D" | "LED" => head,
            _ => return None,
        };
        return Some(IpcSpec { kind: kind.into(), size: size.into(), ..Default::default() });
    }
    let mut hp = head.split('-');
    let pkg = hp.next()?.to_ascii_uppercase();
    let pins: usize = hp.next()?.parse().ok()?;
    let mut body = None;
    let mut pitch = None;
    let mut ep = [0.0; 2];
    let pair = |s: &str| -> Option<P> {
        let (a, b) = s.trim_end_matches("mm").split_once('x')?;
        Some([a.parse().ok()?, b.parse().ok()?])
    };
    for p in parts {
        if let Some(e) = p.strip_prefix("EP") {
            ep = pair(e)?;
        } else if let Some(v) = p.strip_prefix('P').and_then(|v| v.trim_end_matches("mm").parse::<f64>().ok()) {
            pitch = Some(v);
        } else if p.ends_with("mm") && body.is_none() {
            body = pair(p);
        }
    }
    let (body, pitch) = (body?, pitch?);
    let family = match pkg.as_str() {
        "QFN" | "VQFN" | "WQFN" | "UQFN" | "TQFN" => Family::Qfn,
        "DFN" | "SON" | "VSON" | "WSON" | "TDFN" | "UDFN" => Family::Son,
        "SOIC" | "SOP" | "SSOP" | "TSSOP" | "MSOP" | "HTSSOP" | "TSOP" => Family::Sop,
        "QFP" | "LQFP" | "TQFP" | "PQFP" => Family::Qfp,
        _ => return None,
    };
    let leaded = matches!(family, Family::Sop | Family::Qfp);
    let height = match pkg.as_str() {
        "SOIC" => 1.75,
        "LQFP" | "QFP" | "PQFP" => 1.6,
        "TQFP" | "TSSOP" | "HTSSOP" | "SSOP" | "TSOP" => 1.2,
        "MSOP" => 1.1,
        _ => 0.9,
    };
    Some(IpcSpec {
        family,
        pins,
        pitch,
        body,
        // gull-wing feet reach about 1 mm beyond the body on each side
        span: if leaded { [body[0] + 2.0, body[1] + 2.0] } else { [0.0; 2] },
        lead: if !leaded { 0.4 } else if pitch >= 1.27 { 0.835 } else { 0.6 },
        lead_w: (pitch * if leaded { 0.4 } else { 0.5 }).min(0.45),
        height,
        ep,
        ..Default::default()
    })
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn kicad_names_make_ipc_patterns() {
        let s = from_kicad_name("QFN-56-1EP_7x7mm_P0.4mm_EP3.2x3.2mm").unwrap();
        let f = generate(&s).unwrap();
        assert_eq!(f.name, "QFN40P700X700X90-57N");
        assert_eq!(f.pads.len(), 57);
        let f = generate(&from_kicad_name("SOIC-8_3.9x4.9mm_P1.27mm").unwrap()).unwrap();
        assert_eq!(f.name, "SOIC127P590X175-8N");
        assert_eq!(generate(&from_kicad_name("R_0603_1608Metric").unwrap()).unwrap().name, "RESC1608X45N");
        assert!(from_kicad_name("USB_C_Receptacle_GCT_USB4085").is_none());
    }
}
