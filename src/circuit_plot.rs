//! Waveform plots of simulation results: panels by unit for a transient (volts, amps), gain and
//! phase for an AC sweep. Shared by the Simulate view and the PNG the agent tools return.

use crate::circuit::{Kind, Sig, SimOut, db, si};
use crate::pcb_export::{self, Draw, Prim};
use crate::pcb_lib::P;

pub const COLORS: [[u8; 3]; 8] = [[31, 119, 180], [214, 39, 40], [44, 160, 44], [255, 127, 14], [148, 103, 189], [23, 190, 207], [140, 86, 75], [227, 119, 194]];

pub struct Series {
    pub name: String,
    pub color: [u8; 3],
    pub y: Vec<f64>,
}

pub struct Panel {
    /// Axis unit (V, A, dB, °).
    pub unit: &'static str,
    pub series: Vec<Series>,
    pub lo: f64,
    pub hi: f64,
}

/// The panels to draw for some probed signals.
pub fn panels(out: &SimOut, probes: &[Sig]) -> Vec<Panel> {
    let mut v: Vec<Panel> = vec![];
    let mut put = |unit: &'static str, s: Series| match v.iter_mut().find(|p| p.unit == unit) {
        Some(p) => p.series.push(s),
        None => v.push(Panel { unit, series: vec![s], lo: 0.0, hi: 0.0 }),
    };
    for (k, s) in probes.iter().enumerate() {
        let color = COLORS[k % COLORS.len()];
        if out.kind == Kind::Ac {
            let g: Vec<f64> = s.re.iter().zip(&s.im).map(|(a, b)| db(*a, *b)).collect();
            // unwrapped phase
            let mut ph = vec![];
            for (a, b) in s.re.iter().zip(&s.im) {
                let mut p = b.atan2(*a).to_degrees();
                if let Some(&prev) = ph.last() {
                    while p - prev > 180.0 {
                        p -= 360.0;
                    }
                    while p - prev < -180.0 {
                        p += 360.0;
                    }
                }
                ph.push(p);
            }
            put("dB", Series { name: s.name.clone(), color, y: g });
            put("°", Series { name: s.name.clone(), color, y: ph });
        } else {
            put(s.unit, Series { name: s.name.clone(), color, y: s.re.clone() });
        }
    }
    for p in &mut v {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for s in &p.series {
            for y in &s.y {
                if y.is_finite() {
                    lo = lo.min(*y);
                    hi = hi.max(*y);
                }
            }
        }
        if !lo.is_finite() {
            (lo, hi) = (0.0, 1.0);
        }
        // gains can fall to nothing: keep 120 dB below the top
        if p.unit == "dB" {
            lo = lo.max(hi - 120.0);
        }
        let span = (hi - lo).max(hi.abs().max(lo.abs()) * 1e-3).max(1e-12);
        p.lo = lo - span * 0.06;
        p.hi = hi + span * 0.06;
        if p.unit != "dB" && p.unit != "°" && lo >= 0.0 && p.lo < 0.0 && lo < span * 0.2 {
            p.lo = 0.0 - span * 0.02;
        }
    }
    v
}

/// About `n` round tick values across [lo, hi].
pub fn ticks(lo: f64, hi: f64, n: usize) -> Vec<f64> {
    if !(hi > lo) {
        return vec![lo];
    }
    let raw = (hi - lo) / n.max(1) as f64;
    let step = crate::circuit::nice(raw);
    let mut t = (lo / step).ceil() * step;
    let mut v = vec![];
    while t <= hi + step * 1e-9 && v.len() < 100 {
        v.push(if t.abs() < step * 1e-9 { 0.0 } else { t });
        t += step;
    }
    v
}

/// Decades between lo and hi (both positive).
pub fn log_ticks(lo: f64, hi: f64) -> Vec<f64> {
    let mut v = vec![];
    let mut e = lo.log10().ceil();
    while 10f64.powf(e) <= hi * (1.0 + 1e-9) && v.len() < 40 {
        v.push(10f64.powf(e));
        e += 1.0;
    }
    v
}

pub fn x_label(kind: Kind, x: f64) -> String {
    if kind == Kind::Ac { si(x, "Hz") } else { si(x, "s") }
}

pub fn y_label(unit: &str, y: f64) -> String {
    match unit {
        "dB" => format!("{y:.0} dB"),
        "°" => format!("{y:.0}°"),
        u => si(y, u),
    }
}

/// The x position of a sample on a 0..1 axis.
pub fn x_frac(kind: Kind, x0: f64, x1: f64, x: f64) -> f64 {
    if kind == Kind::Ac { (x / x0).ln() / (x1 / x0).ln() } else { (x - x0) / (x1 - x0) }
}

/// The plot as a PNG.
pub fn png(out: &SimOut, probes: &[Sig], width: u32) -> Result<Vec<u8>, String> {
    let ps = panels(out, probes);
    if ps.is_empty() || out.x.len() < 2 {
        return Err("nothing to plot".into());
    }
    let (w, ph, gap, left, right) = (200.0, 62.0, 14.0, 24.0, 6.0);
    let h = ps.len() as f64 * (ph + gap) + 8.0;
    let (x0, x1) = (out.x[0], *out.x.last().unwrap());
    let mut list = vec![];
    let ink = [60, 60, 66, 255];
    let grid = [220, 220, 225, 255];
    let id = |p: P| p;
    for (k, p) in ps.iter().enumerate() {
        let top = h - 4.0 - k as f64 * (ph + gap);
        let bot = top - ph;
        let (l, r) = (left, w - right);
        let sx = |x: f64| l + (r - l) * x_frac(out.kind, x0, x1, x);
        let sy = |y: f64| bot + (top - bot) * (y - p.lo) / (p.hi - p.lo);
        // grid and labels
        for y in ticks(p.lo, p.hi, 5) {
            let yy = sy(y);
            list.push(Draw { rgba: grid, prim: Prim::Seg { a: [l, yy], b: [r, yy], w: 0.15 } });
            pcb_export::text(&mut list, &y_label(p.unit, y), [l - 1.5, yy - 1.0], 2.2, 2, 0.0, ink, &id);
        }
        let xt = if out.kind == Kind::Ac { log_ticks(x0, x1) } else { ticks(x0, x1, 8) };
        for x in xt {
            let xx = sx(x);
            list.push(Draw { rgba: grid, prim: Prim::Seg { a: [xx, bot], b: [xx, top], w: 0.15 } });
            if k + 1 == ps.len() {
                pcb_export::text(&mut list, &x_label(out.kind, x), [xx, bot - 4.5], 2.2, 1, 0.0, ink, &id);
            }
        }
        for (a, b) in [([l, bot], [r, bot]), ([r, bot], [r, top]), ([r, top], [l, top]), ([l, top], [l, bot])] {
            list.push(Draw { rgba: ink, prim: Prim::Seg { a, b, w: 0.2 } });
        }
        // traces
        for s in &p.series {
            let rgba = [s.color[0], s.color[1], s.color[2], 255];
            let pts: Vec<P> = out.x.iter().zip(&s.y).filter(|(_, y)| y.is_finite()).map(|(x, y)| [sx(*x), sy(y.clamp(p.lo, p.hi))]).collect();
            for seg in pts.windows(2) {
                list.push(Draw { rgba, prim: Prim::Seg { a: seg[0], b: seg[1], w: 0.35 } });
            }
        }
        // legend
        let mut lx = l + 2.0;
        for s in &p.series {
            let rgba = [s.color[0], s.color[1], s.color[2], 255];
            list.push(Draw { rgba, prim: Prim::Seg { a: [lx, top + 2.5], b: [lx + 4.0, top + 2.5], w: 0.6 } });
            pcb_export::text(&mut list, &s.name, [lx + 5.0, top + 1.5], 2.2, 0, 0.0, ink, &id);
            lx += 8.0 + crate::pcb_lib::text_width(&s.name, 2.2);
        }
    }
    pcb_export::png(&list, ([0.0, 0.0], [w, h]), width, [255, 255, 255])
}
